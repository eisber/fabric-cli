// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline parser -> shared host -> SDK -> real HTTP adapter contracts.
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, Body, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::{
    create_command::{
        CreateApproval, CreateApprovalError, CreateApprovalRequest, CreateBinding,
        CreateCommandError, CreateDelivery, CreateExecution, CreateTransportEvidence, MAX_BYTES,
        PreparedCreateCommand, v2_create,
    },
    warehouse_create::{CASE_INSENSITIVE_COLLATION, DEFAULT_COLLATION, WarehouseCreateBinding},
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    items::create::{
        CreateError, CreateInputValidator, CreateSupport, CreateTarget, DuplicateSupport, NativeId,
        Preflight, SchemaRef, SkipSupport, TargetScope,
    },
};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, ExecutionTrace, HttpExecutor,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending},
    io::{self, Read},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OP: &str = "33333333-3333-3333-3333-333333333333";
const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Warehouse";

#[derive(Debug, Default)]
struct Credential(AtomicUsize);
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "offline-warehouse",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
enum Reply {
    Http(StatusCode, Headers, Vec<u8>),
    Pending,
}

fn reply(status: StatusCode, body: Value) -> Reply {
    Reply::Http(status, Headers::new(), serde_json::to_vec(&body).unwrap())
}

fn resource() -> Value {
    json!({"id":ITEM,"workspaceId":WS,"type":"Warehouse","displayName":"Exact"})
}

fn listing(items: Value) -> Reply {
    reply(StatusCode::Ok, json!({"value":items}))
}

fn accepted(body: Value, delay: &str) -> Reply {
    let mut headers = Headers::new();
    headers.insert(
        "location",
        format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
    );
    headers.insert("x-ms-operation-id", OP);
    headers.insert("retry-after", delay.to_owned());
    Reply::Http(
        StatusCode::Accepted,
        headers,
        serde_json::to_vec(&body).unwrap(),
    )
}

#[derive(Debug)]
struct Wire {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Request>>,
}
#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        let next = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected/replayed request");
        match next {
            Reply::Http(status, headers, body) => {
                Ok(AsyncRawResponse::from_bytes(status, headers, body))
            }
            Reply::Pending => pending().await,
        }
    }
}

#[derive(Debug, Default)]
struct Timer(Mutex<Vec<Duration>>);
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay == ExecutionOptions::default().timeout {
            return Box::pin(pending());
        }
        self.0.lock().unwrap().push(delay);
        Box::pin(async {})
    }
}

struct Harness {
    client: Client,
    executor: HttpExecutor,
    wire: Arc<Wire>,
    credential: Arc<Credential>,
    timer: Arc<Timer>,
}

impl Harness {
    fn new(replies: Vec<Reply>) -> Self {
        let wire = Arc::new(Wire {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
        });
        let credential = Arc::new(Credential::default());
        let timer = Arc::new(Timer::default());
        let endpoint = EndpointBinding::public(ServiceOrigin::Fabric);
        let client = Client::with_context(RequestContext::with_endpoint(
            credential.clone(),
            CredentialSelection::new("synthetic-owner", WS, Audience::Fabric).unwrap(),
            endpoint.clone(),
        ));
        let executor = HttpExecutor::new(wire.clone(), timer.clone(), ExecutionOptions::default())
            .unwrap()
            .with_endpoint(endpoint);
        Self {
            client,
            executor,
            wire,
            credential,
            timer,
        }
    }

    async fn execute(
        &self,
        prepared: PreparedCreateCommand,
        approval: &mut Approval,
        interrupted: impl Future<Output = ()> + Send,
    ) -> Result<CreateDelivery, CreateCommandError> {
        let trace = ExecutionTrace::default();
        let observe = || {
            let evidence = trace.snapshot();
            CreateTransportEvidence {
                http_status: evidence.status,
                operation_id: evidence.operation_id,
            }
        };
        tokio::time::timeout(
            Duration::from_secs(5),
            prepared.execute(
                &WarehouseCreateBinding,
                &self.client,
                &self.executor,
                CreateExecution {
                    executor: self.executor.clone().with_trace(trace.clone()),
                    observe: &observe,
                },
                approval,
                interrupted,
            ),
        )
        .await
        .expect("scripted Warehouse execution exceeded its fixture budget")
    }

    fn assert_requests(&self, expected: &[(&str, String)]) {
        let requests = self.wire.requests.lock().unwrap();
        assert_eq!(requests.len(), expected.len());
        for (request, (method, path)) in requests.iter().zip(expected) {
            assert_eq!(request.method().as_str(), *method);
            assert_eq!(
                request.url().as_str(),
                format!("https://api.fabric.microsoft.com{path}")
            );
        }
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method() == Method::Post)
                .count(),
            usize::from(expected.iter().any(|(method, _)| *method == "POST")),
            "never replay a mutation or provision another resource"
        );
    }
}

#[derive(Default)]
struct Approval {
    deny: bool,
    calls: usize,
}
impl CreateApproval for Approval {
    fn authorize<'a>(
        &'a mut self,
        request: CreateApprovalRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
        self.calls += 1;
        assert_eq!(request.plan().target().name(), "Exact");
        assert_eq!(request.plan().target().parent_id().as_str(), WS);
        assert_eq!(request.context().selection().account(), "synthetic-owner");
        assert_eq!(request.context().selection().tenant(), WS);
        assert_eq!(request.context().selection().audience(), Audience::Fabric);
        assert_eq!(
            request.context().endpoint(),
            &EndpointBinding::public(ServiceOrigin::Fabric)
        );
        assert_eq!(request.plan().target().item_type(), "Warehouse");
        Box::pin(async move {
            if self.deny {
                Err(CreateApprovalError::Denied)
            } else {
                Ok(())
            }
        })
    }
}

struct ForbiddenRead;
impl Read for ForbiddenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("unsupported input must not be read")
    }
}

fn prepare(
    input: Option<&[u8]>,
    options: &[&str],
) -> Result<PreparedCreateCommand, CreateCommandError> {
    let mut args = vec!["create", TARGET];
    if input.is_some() {
        args.extend(["--input", "-"]);
    }
    args.extend_from_slice(options);
    PreparedCreateCommand::preflight(
        v2_create::parse_args(args).unwrap(),
        &WarehouseCreateBinding,
        std::path::Path::new("."),
        &mut input.unwrap_or_default(),
        false,
    )
}

fn paths(post: bool) -> Vec<(&'static str, String)> {
    let mut paths = vec![("GET", format!("/v1/workspaces/{WS}/items"))];
    if post {
        paths.push(("POST", format!("/v1/workspaces/{WS}/warehouses")));
    }
    paths
}

#[test]
fn capabilities_are_native_create_and_exact_skip_only() {
    let capabilities = WarehouseCreateBinding.capabilities();
    assert_eq!(capabilities.create, CreateSupport::RejectExisting);
    assert_eq!(capabilities.skip, SkipSupport::ExactExistingUnchanged);
    assert_eq!(capabilities.replacement, None);
    assert_eq!(capabilities.duplicate, DuplicateSupport::Unsupported);
}

#[test]
fn native_operation_rejects_replaced_bytes_schema_and_owner_instead_of_trusting_callers() {
    let target = CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WS).unwrap(),
        "Exact",
        "Warehouse",
    )
    .unwrap();
    let binding = WarehouseCreateBinding;
    let command = v2_create::parse_args(["create", TARGET]).unwrap();
    let input = json!({"creationPayload":{"collationType":DEFAULT_COLLATION}});
    let plan = command
        .plan(
            target.clone(),
            Preflight::Leaf(fabric_sdk::items::create::LeafState::Missing),
            &binding.capabilities(),
            Some(input),
            &binding,
        )
        .unwrap();
    assert!(binding.operation(&plan, None, false).is_err());
    assert!(binding.operation(&plan, Some(b"{}"), false).is_err());
    assert!(
        binding
            .validate(&target, &SchemaRef::new("other/v1").unwrap(), None)
            .is_err()
    );
    let other = CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WS).unwrap(),
        "Exact",
        "Notebook",
    )
    .unwrap();
    assert!(
        binding
            .validate(&other, &binding.capabilities().schema, None)
            .is_err()
    );
}

#[tokio::test]
async fn default_and_both_finite_collations_submit_exact_native_body_once() {
    for (input, collation, description) in [
        (None, DEFAULT_COLLATION, None),
        (Some(json!({})), DEFAULT_COLLATION, None),
        (Some(json!({"creationPayload":{}})), DEFAULT_COLLATION, None),
        (
            Some(
                json!({"displayName":"Exact","creationPayload":{"collationType":DEFAULT_COLLATION}}),
            ),
            DEFAULT_COLLATION,
            None,
        ),
        (
            Some(
                json!({"description":"native description","creationPayload":{"collationType":CASE_INSENSITIVE_COLLATION}}),
            ),
            CASE_INSENSITIVE_COLLATION,
            Some("native description"),
        ),
    ] {
        let input = input.map(|value| serde_json::to_vec(&value).unwrap());
        let harness = Harness::new(vec![
            listing(json!([])),
            reply(StatusCode::Created, resource()),
        ]);
        let mut approval = Approval::default();
        let delivery = harness
            .execute(
                prepare(input.as_deref(), &[]).unwrap(),
                &mut approval,
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(
            delivery.report().exit_code(),
            0,
            "{}",
            delivery.report().value()
        );
        assert_eq!(delivery.report().value()["meta"]["status"], "completed");
        assert_eq!(approval.calls, 1);
        harness.assert_requests(&paths(true));
        let requests = harness.wire.requests.lock().unwrap();
        let Body::Bytes(bytes) = requests[1].body() else {
            panic!("native JSON bytes")
        };
        let mut expected =
            json!({"displayName":"Exact","creationPayload":{"collationType":collation}});
        if let Some(description) = description {
            expected["description"] = json!(description);
        }
        assert_eq!(serde_json::from_slice::<Value>(bytes).unwrap(), expected);
    }
}

#[test]
fn invalid_input_including_skip_has_zero_credentials_requests_or_input_side_effects() {
    for bytes in [
        b"".as_slice(), b"null", b"[]", b"{", b"{} {}", b"\xff",
        br#"{"displayName":"other"}"#, br#"{"displayName":null}"#,
        br#"{"type":"Warehouse"}"#, br#"{"type":"Lakehouse"}"#,
        br#"{"definition":{}}"#, br#"{"query":"SELECT 1"}"#, br#"{"sql":"SELECT 1"}"#,
        br#"{"folderId":null}"#, br#"{"sensitivityLabelSettings":{}}"#,
        br#"{"unknown":true}"#, br#"{"description":false}"#,
        br#"{"creationPayload":null}"#, br#"{"creationPayload":[]}"#,
        br#"{"creationPayload":{"collationType":"arbitrary"}}"#,
        br#"{"creationPayload":{"collationType":null}}"#,
        br#"{"creationPayload":{"defaultCollation":"Latin1_General_100_BIN2_UTF8"}}"#,
        br#"{"creationPayload":{"collationType":"Latin1_General_100_BIN2_UTF8","sql":"SELECT 1"}}"#,
        br#"{"displayName":"Exact","displayName":"Exact"}"#,
        br#"{"creationPayload":{"collationType":"Latin1_General_100_BIN2_UTF8","\u0063ollationType":"Latin1_General_100_BIN2_UTF8"}}"#,
    ] {
        let harness = Harness::new(vec![]);
        assert!(prepare(Some(bytes), &["--if-exists", "skip"]).is_err(), "{bytes:?}");
        assert_eq!(harness.credential.0.load(Ordering::SeqCst), 0);
        harness.assert_requests(&[]);
    }
    assert!(prepare(Some(&vec![b' '; MAX_BYTES + 1]), &[]).is_err());
    let long = serde_json::to_vec(&json!({"description":"a".repeat(257)})).unwrap();
    assert!(prepare(Some(&long), &[]).is_err());
}

#[test]
fn invalid_target_and_unsupported_policy_precede_input_io() {
    for target in [
        "/not-a-guid.Workspace/Exact.Warehouse",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Lakehouse",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.warehouse",
        "/11111111-1111-1111-1111-111111111111.Workspace/.Warehouse",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Warehouse/src/definition",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Warehouse/Tables",
        "/11111111-1111-1111-1111-111111111111.Workspace",
    ] {
        let command = v2_create::parse_args(["create", target, "--input", "-"]).unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &WarehouseCreateBinding,
                std::path::Path::new("."),
                &mut ForbiddenRead,
                false
            )
            .is_err()
        );
    }
    for policy in [
        vec!["--force"],
        vec!["--if-exists", "overwrite"],
        vec!["--if-exists", "duplicate"],
    ] {
        for input in ["-", "missing-input.json"] {
            let command = v2_create::parse_args(
                ["create", TARGET, "--input", input]
                    .into_iter()
                    .chain(policy.iter().copied()),
            )
            .unwrap();
            assert!(matches!(
                PreparedCreateCommand::preflight(
                    command,
                    &WarehouseCreateBinding,
                    std::path::Path::new("."),
                    &mut ForbiddenRead,
                    false
                ),
                Err(CreateCommandError::Policy(CreateError::UnsupportedPolicy))
            ));
        }
    }
}

#[tokio::test]
async fn approval_denial_makes_only_the_bounded_lookup() {
    let harness = Harness::new(vec![listing(json!([]))]);
    let mut approval = Approval {
        deny: true,
        calls: 0,
    };
    assert!(matches!(
        harness
            .execute(prepare(None, &[]).unwrap(), &mut approval, pending())
            .await,
        Err(CreateCommandError::Approval(CreateApprovalError::Denied))
    ));
    assert_eq!(approval.calls, 1);
    harness.assert_requests(&paths(false));
}

#[tokio::test]
async fn error_skip_and_ambiguity_never_mutate_an_existing_or_wrong_kind_target() {
    for (items, options, succeeds) in [
        (json!([resource()]), vec![], false),
        (json!([resource()]), vec!["--if-exists", "skip"], true),
        (
            json!([resource(), resource()]),
            vec!["--if-exists", "skip"],
            false,
        ),
        (
            json!([{"id":ITEM,"displayName":"Exact","type":"Notebook"}]),
            vec!["--if-exists", "skip"],
            false,
        ),
        (
            json!([{"id":ITEM,"displayName":"exact","type":"Warehouse"}]),
            vec!["--if-exists", "skip"],
            false,
        ),
        (
            json!([{"id":ITEM,"workspaceId":OP,"displayName":"Exact","type":"Warehouse"}]),
            vec!["--if-exists", "skip"],
            false,
        ),
    ] {
        let harness = Harness::new(vec![listing(items)]);
        let mut approval = Approval::default();
        let result = harness
            .execute(
                prepare(Some(b"{}"), &options).unwrap(),
                &mut approval,
                pending(),
            )
            .await;
        assert_eq!(result.is_ok(), succeeds);
        if let Ok(delivery) = result {
            assert_eq!(delivery.report().value()["meta"]["action"], "skip");
            assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
            assert_eq!(
                delivery.report().value()["meta"]["mutationMayHaveOccurred"],
                false
            );
            assert_eq!(
                delivery.report().value()["meta"]["contentEqualityClaimed"],
                false
            );
        }
        harness.assert_requests(&paths(false));
    }
}

#[tokio::test]
async fn provider_collision_throttle_and_service_errors_are_not_skip_or_replayed() {
    for (status, code) in [
        (StatusCode::Conflict, "ItemDisplayNameAlreadyInUse"),
        (StatusCode::TooManyRequests, "TooManyRequests"),
        (StatusCode::BadRequest, "CorruptedPayload"),
        (StatusCode::InternalServerError, "InternalError"),
    ] {
        let harness = Harness::new(vec![
            listing(json!([])),
            reply(
                status,
                json!({"errorCode":code,"message":"synthetic rejection"}),
            ),
        ]);
        let delivery = harness
            .execute(
                prepare(None, &["--if-exists", "skip"]).unwrap(),
                &mut Approval::default(),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(delivery.report().value()["meta"]["status"], "failed");
        assert_eq!(delivery.report().value()["meta"]["serviceCode"], code);
        harness.assert_requests(&paths(true));
    }
}

#[tokio::test]
async fn accepted_is_not_completed_and_projection_never_hides_identity_or_handle() {
    let harness = Harness::new(vec![listing(json!([])), accepted(resource(), "7")]);
    let delivery = harness
        .execute(
            prepare(None, &["--project", "`null`"]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    let mut stdout = Vec::new();
    delivery.deliver(&mut stdout).unwrap();
    let receipt: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(receipt["data"], Value::Null);
    assert_eq!(receipt["meta"]["status"], "accepted");
    assert_eq!(receipt["meta"]["completed"], false);
    assert_eq!(receipt["meta"]["resourceId"], ITEM);
    assert_eq!(
        receipt["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    harness.assert_requests(&paths(true));
    assert!(harness.timer.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn wait_uses_native_operation_result_and_retry_after_without_extra_resource_reads() {
    let harness = Harness::new(vec![
        listing(json!([])),
        accepted(Value::Null, "7"),
        reply(StatusCode::Ok, json!({"status":"Succeeded"})),
        reply(StatusCode::Ok, resource()),
    ]);
    let delivery = harness
        .execute(
            prepare(None, &["--wait"]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(
        delivery.report().exit_code(),
        0,
        "{}",
        delivery.report().value()
    );
    assert_eq!(delivery.report().value()["meta"]["completed"], true);
    let mut expected = paths(true);
    expected.extend([
        ("GET", format!("/v1/operations/{OP}")),
        ("GET", format!("/v1/operations/{OP}/result")),
    ]);
    harness.assert_requests(&expected);
    assert_eq!(*harness.timer.0.lock().unwrap(), [Duration::from_secs(7)]);
}

#[tokio::test]
async fn failed_poll_preserves_acceptance_body_and_handle_through_delivery_failure() {
    let harness = Harness::new(vec![
        listing(json!([])),
        accepted(resource(), "0"),
        reply(
            StatusCode::Ok,
            json!({"status":"Failed","error":{"errorCode":"NativeFailure","message":"synthetic"}}),
        ),
    ]);
    let delivery = harness
        .execute(
            prepare(None, &["--wait", "--project", "`null`"]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
    assert_eq!(delivery.report().value()["data"], resource());
    let (reader, mut writer) = io::pipe().unwrap();
    drop(reader);
    let Err(CreateCommandError::Delivery { report, .. }) = delivery.deliver(&mut writer) else {
        panic!("closed pipe must fail and retain the unprojected report")
    };
    assert_eq!(report.value()["meta"]["resourceId"], ITEM);
    assert_eq!(
        report.value()["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(report.value()["meta"]["remoteReplayed"], false);
    assert!(report.error().is_some());
    let mut expected = paths(true);
    expected.push(("GET", format!("/v1/operations/{OP}")));
    harness.assert_requests(&expected);
}

#[tokio::test]
async fn cancellation_and_timeout_after_acceptance_retain_evidence_without_replay() {
    for timeout in [false, true] {
        let harness = Harness::new(vec![
            listing(json!([])),
            accepted(resource(), "0"),
            Reply::Pending,
        ]);
        let interrupted = async {
            loop {
                if harness.wire.requests.lock().unwrap().len() == 3 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            if timeout {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        let delivery = harness
            .execute(
                prepare(None, &["--wait"]).unwrap(),
                &mut Approval::default(),
                interrupted,
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(delivery.report().value()["meta"]["completed"], false);
        assert_eq!(
            delivery.report().value()["meta"]["mutationMayHaveOccurred"],
            true
        );
        assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
        assert_eq!(
            delivery.report().value()["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        let mut expected = paths(true);
        expected.push(("GET", format!("/v1/operations/{OP}")));
        harness.assert_requests(&expected);
    }
}

#[tokio::test]
async fn poll_budget_and_invalid_delay_fail_closed_and_preserve_acceptance() {
    for delay in ["61", "invalid", "0"] {
        let mut replies = vec![listing(json!([])), accepted(resource(), delay)];
        if delay == "0" {
            replies.extend((0..99).map(|_| {
                let mut headers = Headers::new();
                headers.insert("retry-after", "0");
                Reply::Http(StatusCode::Ok, headers, br#"{"status":"Running"}"#.to_vec())
            }));
        }
        let harness = Harness::new(replies);
        let delivery = harness
            .execute(
                prepare(None, &["--wait"]).unwrap(),
                &mut Approval::default(),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
        assert_eq!(
            delivery.report().value()["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        let requests = harness.wire.requests.lock().unwrap();
        assert_eq!(requests.len(), if delay == "0" { 101 } else { 2 });
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method() == Method::Post)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn no_clobber_race_and_projection_failure_retain_the_original_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("receipt.json");
    let harness = Harness::new(vec![listing(json!([])), accepted(resource(), "0")]);
    let delivery = harness
        .execute(
            prepare(None, &["--output", output.to_str().unwrap()]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    std::fs::write(&output, b"preserve").unwrap();
    let Err(CreateCommandError::Delivery { report, .. }) = delivery.deliver(&mut Vec::new()) else {
        panic!("output race must not overwrite")
    };
    assert_eq!(std::fs::read(output).unwrap(), b"preserve");
    assert_eq!(
        report.value()["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    harness.assert_requests(&paths(true));

    let harness = Harness::new(vec![listing(json!([])), accepted(resource(), "0")]);
    let delivery = harness
        .execute(
            prepare(None, &["--project", "length(`1`)"]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    let Err(CreateCommandError::Delivery { report, .. }) = delivery.deliver(&mut Vec::new()) else {
        panic!("runtime projection type failure must retain the original report")
    };
    assert_eq!(report.value()["meta"]["resourceId"], ITEM);
    harness.assert_requests(&paths(true));
}

#[tokio::test]
async fn accepted_headers_survive_body_limit_and_invalid_result_identity() {
    let mut headers = Headers::new();
    headers.insert("x-ms-operation-id", OP);
    headers.insert(
        "location",
        format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
    );
    let harness = Harness::new(vec![
        listing(json!([])),
        Reply::Http(StatusCode::Accepted, headers, vec![b' '; MAX_BYTES + 1]),
    ]);
    let delivery = harness
        .execute(
            prepare(None, &[]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(delivery.report().value()["meta"]["httpStatus"], 202);
    assert_eq!(
        delivery.report().value()["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(delivery.report().value()["meta"]["resourceId"], Value::Null);
    harness.assert_requests(&paths(true));

    let mut wrong = resource();
    wrong["workspaceId"] = json!(OP);
    let harness = Harness::new(vec![listing(json!([])), reply(StatusCode::Created, wrong)]);
    let delivery = harness
        .execute(
            prepare(None, &[]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(delivery.report().value()["meta"]["completed"], false);
    assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
    harness.assert_requests(&paths(true));
}

#[tokio::test]
async fn foreign_operation_links_and_invalid_paging_never_authorize_extra_effects() {
    let mut headers = Headers::new();
    headers.insert(
        "location",
        format!("https://other.invalid/v1/operations/{OP}"),
    );
    headers.insert("x-ms-operation-id", OP);
    let harness = Harness::new(vec![
        listing(json!([])),
        Reply::Http(StatusCode::Accepted, headers, Vec::new()),
    ]);
    let delivery = harness
        .execute(
            prepare(None, &["--wait"]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(delivery.report().value()["meta"]["completed"], false);
    harness.assert_requests(&paths(true));

    let harness = Harness::new(vec![reply(
        StatusCode::Ok,
        json!({"value":[],"continuationToken":false}),
    )]);
    let mut approval = Approval::default();
    assert!(matches!(
        harness
            .execute(prepare(None, &[]).unwrap(), &mut approval, pending())
            .await,
        Err(CreateCommandError::Discovery)
    ));
    assert_eq!(approval.calls, 0);
    harness.assert_requests(&paths(false));
}

#[test]
fn literal_names_preserve_guid_shaped_and_unicode_spelling_and_bound_raw_bytes() {
    for name in [ITEM, "With spaces.and-dashes", "Caf\u{e9}"] {
        let target = format!("/{WS}.Workspace/{name}.Warehouse");
        let command = v2_create::parse_args(["create", &target]).unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &WarehouseCreateBinding,
                std::path::Path::new("."),
                &mut ForbiddenRead,
                false,
            )
            .is_ok()
        );
    }
    let target = format!("/{WS}.Workspace/{}.Warehouse", "a".repeat(257));
    assert!(
        PreparedCreateCommand::preflight(
            v2_create::parse_args(["create", &target]).unwrap(),
            &WarehouseCreateBinding,
            std::path::Path::new("."),
            &mut ForbiddenRead,
            false,
        )
        .is_err()
    );
}
