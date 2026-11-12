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
        CreateCommandError, CreateDelivery, CreateEffectKind, CreateExecution,
        CreateTransportEvidence, MAX_BYTES, PreparedCreateCommand, v2_create,
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
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
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
    PendingBody(Headers, Arc<tokio::sync::Notify>),
    BrokenBody(Headers),
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
            Reply::PendingBody(headers, suspended) => {
                let body = futures_util::stream::poll_fn(move |_| {
                    suspended.notify_one();
                    std::task::Poll::<Option<azure_core::Result<azure_core::Bytes>>>::Pending
                });
                Ok(AsyncRawResponse::new(
                    StatusCode::Accepted,
                    headers,
                    Box::pin(body),
                ))
            }
            Reply::BrokenBody(headers) => Ok(AsyncRawResponse::new(
                StatusCode::Accepted,
                headers,
                Box::pin(futures_util::stream::iter([Err(
                    azure_core::Error::with_message(
                        azure_core::error::ErrorKind::Io,
                        "synthetic body failure",
                    ),
                )])),
            )),
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
    expected_name: Option<&'static str>,
    expected_write: Option<bool>,
    approved_bodies: Vec<Vec<u8>>,
    questions: Vec<String>,
    suspend: Option<Arc<tokio::sync::Notify>>,
}
impl CreateApproval for Approval {
    fn authorize<'a>(
        &'a mut self,
        request: CreateApprovalRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
        self.calls += 1;
        assert_eq!(
            request.plan().target().name(),
            self.expected_name.unwrap_or("Exact")
        );
        assert_eq!(request.remote_write(), self.expected_write.unwrap_or(true));
        assert_eq!(request.plan().target().parent_id().as_str(), WS);
        assert_eq!(request.context().selection().account(), "synthetic-owner");
        assert_eq!(request.context().selection().tenant(), WS);
        assert_eq!(request.context().selection().audience(), Audience::Fabric);
        assert_eq!(
            request.context().endpoint(),
            &EndpointBinding::public(ServiceOrigin::Fabric)
        );
        assert_eq!(request.plan().target().item_type(), "Warehouse");
        if request.remote_write() {
            let operation = request
                .operation()
                .expect("native create operation before approval");
            assert_eq!(operation.method(), ResourceMethod::Post);
            assert_eq!(operation.path(), format!("/v1/workspaces/{WS}/warehouses"));
            assert_eq!(
                request.url().unwrap().as_str(),
                format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/warehouses")
            );
            assert_eq!(
                operation.completion(),
                if request.command().wait {
                    CompletionPolicy::WaitForResult
                } else {
                    CompletionPolicy::ReturnAccepted
                }
            );
            self.approved_bodies.push(operation.body().to_vec());
            assert_eq!(request.anticipated_effects().len(), 1);
            let effect = &request.anticipated_effects()[0];
            assert_eq!(effect.kind(), CreateEffectKind::MeteredConsumption);
            assert_eq!(effect.resource_type(), Some("Warehouse"));
            assert!(effect.description().contains("no SQL"));
            assert!(
                effect
                    .contract()
                    .contains("c9d49756c2e184b61acde34d32f9aff63595db5b")
            );
        } else {
            assert!(request.operation().is_none());
            assert!(request.url().is_none());
            assert!(request.anticipated_effects().is_empty());
        }
        self.questions.push(request.approval_question());
        Box::pin(async move {
            if let Some(suspended) = &self.suspend {
                suspended.notify_one();
                return pending().await;
            }
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
    prepare_at(TARGET, input, options)
}

fn prepare_at(
    target: &str,
    input: Option<&[u8]>,
    options: &[&str],
) -> Result<PreparedCreateCommand, CreateCommandError> {
    let mut args = vec!["create", target];
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
    let mut paths = vec![("GET", format!("/v1/workspaces/{WS}/items?recursive=false"))];
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
        assert_eq!(approval.approved_bodies, [bytes.to_vec()]);
        let question = &approval.questions[0];
        assert!(question.contains("method=POST"));
        assert!(question.contains(&format!("/v1/workspaces/{WS}/warehouses")));
        assert!(question.contains(collation));
        assert!(question.contains(&String::from_utf8_lossy(bytes).escape_debug().to_string()));
        assert!(question.contains("MeteredConsumption"));
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
        ..Approval::default()
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
        let mut approval = Approval {
            expected_write: Some(false),
            ..Approval::default()
        };
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
    assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
    assert_eq!(
        receipt["meta"]["anticipatedEffects"][0]["kind"],
        "meteredConsumption"
    );
    assert_eq!(
        receipt["meta"]["anticipatedEffects"][0]["resourceType"],
        "Warehouse"
    );
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
    assert_eq!(
        delivery.report().value()["meta"]["submission"]["httpStatus"],
        202
    );
    assert_eq!(
        delivery.report().value()["meta"]["observation"]["httpStatus"],
        200
    );
    assert_eq!(delivery.report().value()["meta"]["httpStatus"], 200);
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
    assert_eq!(
        delivery.report().value()["meta"]["submission"]["httpStatus"],
        202
    );
    assert_eq!(
        delivery.report().value()["meta"]["submission"]["resource"]["id"],
        ITEM
    );
    assert_eq!(
        delivery.report().value()["meta"]["observation"]["httpStatus"],
        200
    );
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
    assert_eq!(report.value()["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(report.value()["meta"]["submission"]["resource"]["id"], ITEM);
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
            delivery.report().value()["meta"]["submission"]["httpStatus"],
            202
        );
        assert_eq!(
            delivery.report().value()["meta"]["submission"]["resource"]["id"],
            ITEM
        );
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

#[tokio::test]
async fn literal_guid_and_unicode_names_reach_only_the_warehouses_post_verbatim() {
    for name in [ITEM, "With spaces.and-dashes", "Caf\u{e9}"] {
        let target = format!("/{WS}.Workspace/{name}.Warehouse");
        let mut response = resource();
        response["displayName"] = json!(name);
        let harness = Harness::new(vec![
            listing(json!([])),
            reply(StatusCode::Created, response),
        ]);
        let mut approval = Approval {
            expected_name: Some(name),
            ..Approval::default()
        };
        let delivery = harness
            .execute(
                prepare_at(&target, None, &["--if-exists", "skip"]).unwrap(),
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
        assert_eq!(delivery.report().value()["meta"]["action"], "create");
        assert_eq!(approval.calls, 1);
        harness.assert_requests(&paths(true));
        let requests = harness.wire.requests.lock().unwrap();
        let Body::Bytes(bytes) = requests[1].body() else {
            panic!("native JSON bytes")
        };
        assert_eq!(
            serde_json::from_slice::<Value>(bytes).unwrap()["displayName"],
            name
        );
    }
}

#[tokio::test]
async fn empty_accepted_body_never_invents_a_resource_identity_or_completion() {
    let Reply::Http(status, headers, _) = accepted(Value::Null, "0") else {
        unreachable!()
    };
    let harness = Harness::new(vec![
        listing(json!([])),
        Reply::Http(status, headers, Vec::new()),
    ]);
    let delivery = harness
        .execute(
            prepare(None, &[]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    assert_eq!(delivery.report().value()["meta"]["status"], "accepted");
    assert_eq!(delivery.report().value()["meta"]["completed"], false);
    assert_eq!(delivery.report().value()["meta"]["resourceId"], Value::Null);
    assert_eq!(
        delivery.report().value()["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    harness.assert_requests(&paths(true));
}

#[tokio::test]
async fn accepted_headers_survive_suspended_or_failed_body_without_replay() {
    for cancelled in [false, true] {
        let Reply::Http(_, headers, _) = accepted(Value::Null, "0") else {
            unreachable!()
        };
        let suspended = Arc::new(tokio::sync::Notify::new());
        let response = if cancelled {
            Reply::PendingBody(headers, suspended.clone())
        } else {
            Reply::BrokenBody(headers)
        };
        let harness = Harness::new(vec![listing(json!([])), response]);
        let interrupted = async {
            if cancelled {
                suspended.notified().await;
            } else {
                pending().await
            }
        };
        let delivery = harness
            .execute(
                prepare(None, &[]).unwrap(),
                &mut Approval::default(),
                interrupted,
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(delivery.report().value()["meta"]["completed"], false);
        assert_eq!(delivery.report().value()["meta"]["httpStatus"], 202);
        assert_eq!(delivery.report().value()["meta"]["resourceId"], Value::Null);
        assert_eq!(
            delivery.report().value()["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(
            delivery.report().value()["meta"]["mutationMayHaveOccurred"],
            true
        );
        assert_eq!(delivery.report().value()["meta"]["remoteReplayed"], false);
        assert_eq!(delivery.report().value()["meta"]["remoteCancelled"], false);
        harness.assert_requests(&paths(true));
    }
}

#[tokio::test]
async fn defaulted_and_explicit_ci_bodies_are_visible_before_denial_or_cancelled_approval() {
    for ci in [false, true] {
        let input = ci.then(|| {
            serde_json::to_vec(&json!({
                "creationPayload":{"collationType":CASE_INSENSITIVE_COLLATION}
            }))
            .unwrap()
        });
        for cancel in [false, true] {
            let harness = Harness::new(vec![listing(json!([]))]);
            let suspended = Arc::new(tokio::sync::Notify::new());
            let mut approval = Approval {
                deny: !cancel,
                suspend: cancel.then(|| suspended.clone()),
                ..Approval::default()
            };
            let interrupted = async {
                if cancel {
                    suspended.notified().await;
                } else {
                    pending().await
                }
            };
            let result = harness
                .execute(
                    prepare(input.as_deref(), &[]).unwrap(),
                    &mut approval,
                    interrupted,
                )
                .await;
            if cancel {
                assert!(matches!(result, Err(CreateCommandError::Interrupted)));
            } else {
                assert!(matches!(
                    result,
                    Err(CreateCommandError::Approval(CreateApprovalError::Denied))
                ));
            }
            assert_eq!(approval.calls, 1);
            assert_eq!(approval.approved_bodies.len(), 1);
            assert_eq!(
                serde_json::from_slice::<Value>(&approval.approved_bodies[0]).unwrap(),
                json!({"displayName":"Exact","creationPayload":{
                    "collationType":if ci { CASE_INSENSITIVE_COLLATION } else { DEFAULT_COLLATION }
                }})
            );
            harness.assert_requests(&paths(false));
        }
    }
}

#[tokio::test]
async fn typed_same_name_unrelated_items_do_not_make_exact_warehouse_skip_ambiguous() {
    let harness = Harness::new(vec![listing(json!([
        resource(),
        {"id":OP,"workspaceId":WS,"displayName":"Exact","type":"Notebook"}
    ]))]);
    let mut approval = Approval {
        expected_write: Some(false),
        ..Approval::default()
    };
    let delivery = harness
        .execute(
            prepare(None, &["--if-exists", "skip"]).unwrap(),
            &mut approval,
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    assert_eq!(delivery.report().value()["meta"]["action"], "skip");
    assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
    assert_eq!(delivery.report().value()["meta"]["remoteWrite"], false);
    assert_eq!(
        delivery.report().value()["meta"]["anticipatedEffects"],
        json!([])
    );
    assert!(approval.approved_bodies.is_empty());
    assert!(approval.questions[0].contains("no native request (unchanged skip)"));
    harness.assert_requests(&paths(false));
}

#[tokio::test]
async fn malformed_final_and_cancelled_provider_state_keep_initial_submission_separate() {
    for malformed in [false, true] {
        let mut replies = vec![listing(json!([])), accepted(resource(), "0")];
        if malformed {
            replies.extend([
                reply(StatusCode::Ok, json!({"status":"Succeeded"})),
                Reply::Http(StatusCode::Ok, Headers::new(), b"{malformed".to_vec()),
            ]);
        } else {
            replies.push(reply(
                StatusCode::Ok,
                json!({"status":"Cancelled","error":{"errorCode":"NativeCancelled"}}),
            ));
        }
        let harness = Harness::new(replies);
        let delivery = harness
            .execute(
                prepare(None, &["--wait", "--project", "`null`"]).unwrap(),
                &mut Approval::default(),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        let mut stdout = Vec::new();
        delivery.deliver(&mut stdout).unwrap();
        let receipt: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(receipt["data"], Value::Null);
        assert_eq!(receipt["meta"]["completed"], false);
        assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
        assert_eq!(receipt["meta"]["resourceId"], ITEM);
        assert_eq!(
            receipt["meta"]["submission"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(receipt["meta"]["observation"]["httpStatus"], 200);
        assert_eq!(receipt["meta"]["httpStatus"], 200);
        assert_eq!(receipt["meta"]["remoteReplayed"], false);
        assert_eq!(receipt["meta"]["remoteCancelled"], false);
        assert_eq!(
            receipt["meta"]["anticipatedEffects"][0]["kind"],
            "meteredConsumption"
        );
        if malformed {
            use base64::Engine;
            assert_eq!(
                receipt["meta"]["unparsedResponseBodyBase64"],
                base64::engine::general_purpose::STANDARD.encode(b"{malformed")
            );
        }
        let mut expected = paths(true);
        expected.push(("GET", format!("/v1/operations/{OP}")));
        if malformed {
            expected.push(("GET", format!("/v1/operations/{OP}/result")));
        }
        harness.assert_requests(&expected);
    }
}

#[tokio::test]
async fn malformed_nonwaiting_acceptance_retains_bytes_headers_and_effects_without_completion() {
    let Reply::Http(status, headers, _) = accepted(Value::Null, "0") else {
        unreachable!()
    };
    let raw = b"{malformed";
    let harness = Harness::new(vec![
        listing(json!([])),
        Reply::Http(status, headers, raw.to_vec()),
    ]);
    let delivery = harness
        .execute(
            prepare(None, &[]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    let report = delivery.report();
    assert_eq!(report.exit_code(), 1);
    assert_eq!(report.value()["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(report.value()["meta"]["resourceId"], Value::Null);
    assert_eq!(
        report.value()["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(report.value()["meta"]["completed"], false);
    assert_eq!(
        report.value()["meta"]["anticipatedEffects"][0]["kind"],
        "meteredConsumption"
    );
    assert_eq!(
        report.value()["meta"]["anticipatedEffects"][0].get("id"),
        None
    );
    use base64::Engine;
    assert_eq!(
        report.value()["meta"]["unparsedResponseBodyBase64"],
        base64::engine::general_purpose::STANDARD.encode(raw)
    );
    harness.assert_requests(&paths(true));
}

#[tokio::test]
async fn explicit_root_or_omitted_folder_can_skip_but_nested_only_or_malformed_cannot() {
    for folder in [
        None,
        Some(Value::Null),
        Some(json!(OP)),
        Some(json!("invalid")),
        Some(json!("")),
        Some(json!(false)),
        Some(json!(1)),
        Some(json!({})),
        Some(json!([])),
    ] {
        let mut item = resource();
        if let Some(folder) = &folder {
            item["folderId"] = folder.clone();
        }
        let harness = Harness::new(vec![listing(json!([item]))]);
        let mut approval = Approval {
            expected_write: Some(false),
            ..Approval::default()
        };
        let result = harness
            .execute(
                prepare(None, &["--if-exists", "skip"]).unwrap(),
                &mut approval,
                pending(),
            )
            .await;
        if folder.is_none() || folder == Some(Value::Null) {
            let delivery = result.unwrap();
            assert_eq!(delivery.report().exit_code(), 0);
            assert_eq!(delivery.report().value()["meta"]["action"], "skip");
            assert_eq!(delivery.report().value()["meta"]["folderId"], Value::Null);
            assert_eq!(
                delivery.report().value()["meta"]["folderIdObserved"],
                folder.is_some()
            );
            assert_eq!(approval.calls, 1);
        } else {
            assert!(matches!(result, Err(CreateCommandError::Invalid)));
            assert_eq!(approval.calls, 0);
        }
        harness.assert_requests(&paths(false));
    }
}

#[tokio::test]
async fn root_and_nested_same_name_items_select_only_exact_typed_root() {
    for root_first in [false, true] {
        let mut root = resource();
        root["folderId"] = Value::Null;
        let nested = json!({
            "id":OP,"workspaceId":WS,"displayName":"Exact","type":"Warehouse","folderId":OP
        });
        let rows = if root_first {
            json!([root, nested])
        } else {
            json!([nested, root])
        };
        let harness = Harness::new(vec![listing(rows)]);
        let delivery = harness
            .execute(
                prepare(Some(b"{}"), &["--if-exists", "skip"]).unwrap(),
                &mut Approval {
                    expected_write: Some(false),
                    ..Approval::default()
                },
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 0);
        assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
        assert_eq!(delivery.report().value()["meta"]["folderIdObserved"], true);
        assert_eq!(delivery.report().value()["meta"]["folderId"], Value::Null);
        assert_eq!(delivery.report().value()["meta"]["remoteWrite"], false);
        harness.assert_requests(&paths(false));
    }
}

#[tokio::test]
async fn malformed_same_name_folder_rejects_even_alongside_a_valid_root_match() {
    let malformed = json!({
        "id":OP,"workspaceId":WS,"displayName":"Exact","type":"Warehouse","folderId":false
    });
    let harness = Harness::new(vec![listing(json!([resource(), malformed]))]);
    let mut approval = Approval {
        expected_write: Some(false),
        ..Approval::default()
    };
    assert!(matches!(
        harness
            .execute(
                prepare(None, &["--if-exists", "skip"]).unwrap(),
                &mut approval,
                pending()
            )
            .await,
        Err(CreateCommandError::Invalid)
    ));
    assert_eq!(approval.calls, 0);
    harness.assert_requests(&paths(false));
}

#[tokio::test]
async fn direct_workspace_query_is_preserved_on_every_discovery_page_before_create() {
    let harness = Harness::new(vec![
        reply(
            StatusCode::Ok,
            json!({
                "value":[{"id":OP,"workspaceId":WS,"displayName":"Other","type":"Warehouse"}],
                "continuationToken":"next +/"
            }),
        ),
        listing(json!([])),
        reply(StatusCode::Created, resource()),
    ]);
    let delivery = harness
        .execute(
            prepare(None, &[]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    harness.assert_requests(&[
        ("GET", format!("/v1/workspaces/{WS}/items?recursive=false")),
        (
            "GET",
            format!("/v1/workspaces/{WS}/items?recursive=false&continuationToken=next+%2B%2F"),
        ),
        ("POST", format!("/v1/workspaces/{WS}/warehouses")),
    ]);
}

#[tokio::test]
async fn incomplete_discovery_or_a_duplicate_root_on_later_pages_never_submits() {
    for page_limit in [false, true] {
        let first = if page_limit {
            json!([])
        } else {
            json!([resource()])
        };
        let mut replies = vec![reply(
            StatusCode::Ok,
            json!({"value":first,"continuationToken":"p1"}),
        )];
        if page_limit {
            replies.extend((1..10).map(|page| {
                reply(
                    StatusCode::Ok,
                    json!({"value":[],"continuationToken":format!("p{}",page+1)}),
                )
            }));
        } else {
            replies.push(listing(json!([resource()])));
        }
        let harness = Harness::new(replies);
        let mut approval = Approval {
            expected_write: Some(false),
            ..Approval::default()
        };
        let result = harness
            .execute(
                prepare(None, &["--if-exists", "skip"]).unwrap(),
                &mut approval,
                pending(),
            )
            .await;
        if page_limit {
            assert!(matches!(result, Err(CreateCommandError::Discovery)));
        } else {
            assert!(matches!(
                result,
                Err(CreateCommandError::Policy(CreateError::AmbiguousLeaf))
            ));
        }
        assert_eq!(approval.calls, 0);
        let mut expected = paths(false);
        for page in 1..if page_limit { 10 } else { 2 } {
            expected.push((
                "GET",
                format!("/v1/workspaces/{WS}/items?recursive=false&continuationToken=p{page}"),
            ));
        }
        harness.assert_requests(&expected);
    }
}

#[tokio::test]
async fn completed_native_responses_require_root_folder_and_preserve_observed_folder_metadata() {
    for wait in [false, true] {
        for folder in [
            None,
            Some(Value::Null),
            Some(json!(OP)),
            Some(json!("bad")),
            Some(json!(false)),
        ] {
            let mut item = resource();
            if let Some(folder) = &folder {
                item["folderId"] = folder.clone();
            }
            let mut replies = vec![listing(json!([]))];
            if wait {
                replies.extend([
                    accepted(Value::Null, "0"),
                    reply(StatusCode::Ok, json!({"status":"Succeeded"})),
                    reply(StatusCode::Ok, item),
                ]);
            } else {
                replies.push(reply(StatusCode::Created, item));
            }
            let harness = Harness::new(replies);
            let options = if wait {
                vec!["--wait", "--project", "`null`"]
            } else {
                vec!["--project", "`null`"]
            };
            let delivery = harness
                .execute(
                    prepare(None, &options).unwrap(),
                    &mut Approval::default(),
                    pending(),
                )
                .await
                .unwrap();
            let root = folder.is_none() || folder == Some(Value::Null);
            assert_eq!(delivery.report().exit_code(), u8::from(!root));
            let mut stdout = Vec::new();
            delivery.deliver(&mut stdout).unwrap();
            let receipt: Value = serde_json::from_slice(&stdout).unwrap();
            assert_eq!(receipt["data"], Value::Null);
            assert_eq!(receipt["meta"]["completed"], root);
            assert_eq!(receipt["meta"]["folderIdObserved"], folder.is_some());
            assert_eq!(
                receipt["meta"]["folderId"],
                folder.clone().unwrap_or(Value::Null)
            );
            assert_eq!(receipt["meta"]["resourceId"], ITEM);
            assert_eq!(
                receipt["meta"]["submission"]["httpStatus"],
                if wait { 202 } else { 201 }
            );
            if !wait {
                assert_eq!(
                    receipt["meta"]["submission"]["folderIdObserved"],
                    folder.is_some()
                );
                assert_eq!(
                    receipt["meta"]["submission"]["folderId"],
                    folder.unwrap_or(Value::Null)
                );
            }
            assert_eq!(receipt["meta"]["remoteReplayed"], false);
            let mut expected = paths(true);
            if wait {
                expected.extend([
                    ("GET", format!("/v1/operations/{OP}")),
                    ("GET", format!("/v1/operations/{OP}/result")),
                ]);
            }
            harness.assert_requests(&expected);
        }
    }
}

#[tokio::test]
async fn accepted_nested_folder_evidence_remains_submission_evidence_after_failed_poll() {
    let mut accepted_resource = resource();
    accepted_resource["folderId"] = json!(OP);
    let harness = Harness::new(vec![
        listing(json!([])),
        accepted(accepted_resource, "0"),
        reply(
            StatusCode::Ok,
            json!({"status":"Failed","error":{"errorCode":"NativeFailure"}}),
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
    let mut stdout = Vec::new();
    delivery.deliver(&mut stdout).unwrap();
    let receipt: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(receipt["data"], Value::Null);
    assert_eq!(receipt["meta"]["completed"], false);
    assert_eq!(receipt["meta"]["folderId"], OP);
    assert_eq!(receipt["meta"]["folderIdObserved"], true);
    assert_eq!(receipt["meta"]["submission"]["folderId"], OP);
    assert_eq!(receipt["meta"]["submission"]["folderIdObserved"], true);
    assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
    assert_eq!(receipt["meta"]["observation"]["httpStatus"], 200);
    assert_eq!(receipt["meta"]["remoteReplayed"], false);
    let mut expected = paths(true);
    expected.push(("GET", format!("/v1/operations/{OP}")));
    harness.assert_requests(&expected);
}
