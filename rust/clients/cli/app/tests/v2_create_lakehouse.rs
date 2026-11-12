// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual parser -> released host -> typed SDK -> existing engine -> scripted HTTP.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::{
    create_command::{
        CreateApproval, CreateApprovalError, CreateApprovalRequest, CreateBinding,
        CreateCommandError, CreateDelivery, CreateExecution, CreateTransportEvidence,
        PreparedCreateCommand, v2_create,
    },
    v2_create_lakehouse::LakehouseCreateBinding,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    items::{
        ItemExecutor,
        create::{
            CreateAction, CreateError, CreateTarget, LeafState, NativeId, Preflight, TargetScope,
        },
    },
};
use fabric_sdk_http::{
    ExecutionError, ExecutionFuture, ExecutionOptions, ExecutionTimer, ExecutionTrace, HttpExecutor,
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationRequest, OperationResponse,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending, poll_fn, ready},
    io::{self, Read, Write},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

const ORIGIN: &str = "https://api.fabric.microsoft.com";
const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
const ITEM: &str = "22222222-2222-4222-8222-222222222222";
const OPERATION: &str = "33333333-3333-4333-8333-333333333333";
const HTTP_TIMEOUT: Duration = Duration::from_secs(240);
const RAW: &[u8] =
    b" {\r\n\"displayName\":\"Lakehouse_1\",\"description\":\"raw source retained\"}\r\n";

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
            "synthetic-offline-lakehouse-create",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct RequestRecord {
    method: Method,
    url: String,
    body: Vec<u8>,
}

#[derive(Debug)]
struct Network {
    replies: Mutex<VecDeque<azure_core::Result<AsyncRawResponse>>>,
    requests: Mutex<Vec<RequestRecord>>,
    posts: AtomicUsize,
}

#[async_trait]
impl HttpClient for Network {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.url().origin().ascii_serialization(), ORIGIN);
        if request.method() == Method::Post {
            self.posts.fetch_add(1, Ordering::SeqCst);
        }
        self.requests.lock().unwrap().push(RequestRecord {
            method: request.method(),
            url: request.url().as_str().to_owned(),
            body: Bytes::from(request.body()).to_vec(),
        });
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected credential-bearing HTTP, SQL provisioning, or POST replay")
    }
}

struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay == HTTP_TIMEOUT {
            Box::pin(pending())
        } else {
            assert!(delay <= Duration::from_secs(3));
            Box::pin(ready(()))
        }
    }
}

struct OperationHttp<'a> {
    http: &'a HttpExecutor,
    timeout_poll: bool,
}

impl OperationExecutor for OperationHttp<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            if self.timeout_poll && request.url().path().contains("/operations/") {
                return Err(OperationExecutionError::TimedOut);
            }
            let method = match request.method() {
                fabric_sdk_workspaces::mutations::ResourceMethod::Get => Method::Get,
                fabric_sdk_workspaces::mutations::ResourceMethod::Post => Method::Post,
                fabric_sdk_workspaces::mutations::ResourceMethod::Patch => Method::Patch,
                fabric_sdk_workspaces::mutations::ResourceMethod::Delete => Method::Delete,
            };
            self.http
                .execute_fabric(
                    method,
                    request.url(),
                    request.token(),
                    request.body(),
                    request.max_response_bytes(),
                )
                .await
                .map(|response| OperationResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                })
                .map_err(operation_error)
        })
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(async move { self.http.wait_for(delay).await.map_err(operation_error) })
    }
}

fn operation_error(error: ExecutionError) -> OperationExecutionError {
    match error {
        ExecutionError::TimedOut => OperationExecutionError::TimedOut,
        ExecutionError::ResponseLimit => OperationExecutionError::ResponseLimit,
        ExecutionError::RetriesExhausted => OperationExecutionError::RetriesExhausted,
        ExecutionError::InvalidInput | ExecutionError::Transport | ExecutionError::RetryDelay => {
            OperationExecutionError::Failed
        }
    }
}

struct Approval {
    denied: bool,
    calls: usize,
    expect_write: bool,
}

impl CreateApproval for Approval {
    fn authorize<'a>(
        &'a mut self,
        request: CreateApprovalRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
        self.calls += 1;
        assert_eq!(request.plan().target().parent_id().as_str(), WORKSPACE);
        assert_eq!(request.plan().target().name(), "Lakehouse_1");
        assert_eq!(request.plan().target().item_type(), "Lakehouse");
        assert_eq!(request.context().selection().account(), "offline-account");
        assert_eq!(request.context().selection().tenant(), "offline-tenant");
        assert_eq!(request.context().selection().audience(), Audience::Fabric);
        assert_eq!(
            request.context().endpoint(),
            &EndpointBinding::public(ServiceOrigin::Fabric)
        );
        assert_eq!(request.raw_input(), Some(RAW));
        assert_eq!(request.remote_write(), self.expect_write);
        if let CreateAction::Create { input } = request.plan().action() {
            assert_eq!(
                input.supplied(),
                Some(&json!({"displayName":"Lakehouse_1","description":"raw source retained"}))
            );
        }
        Box::pin(ready(if self.denied {
            Err(CreateApprovalError::Denied)
        } else {
            Ok(())
        }))
    }
}

struct Fixture {
    network: Arc<Network>,
    credential: Arc<Credential>,
}

impl Fixture {
    fn new(replies: Vec<azure_core::Result<AsyncRawResponse>>) -> Self {
        Self {
            network: Arc::new(Network {
                replies: Mutex::new(replies.into()),
                requests: Mutex::new(Vec::new()),
                posts: AtomicUsize::new(0),
            }),
            credential: Arc::new(Credential::default()),
        }
    }

    fn http(&self) -> HttpExecutor {
        HttpExecutor::new(
            self.network.clone(),
            Arc::new(Timer),
            ExecutionOptions {
                timeout: HTTP_TIMEOUT,
                max_delay: Duration::from_secs(3),
            },
        )
        .unwrap()
    }

    fn client(&self, audience: Audience) -> Client {
        Client::with_context(RequestContext::with_endpoint(
            self.credential.clone(),
            CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap(),
            EndpointBinding::public(ServiceOrigin::Fabric),
        ))
    }
}

fn reply(
    status: u16,
    body: &str,
    headers: &[(&str, &str)],
) -> azure_core::Result<AsyncRawResponse> {
    let mut response_headers = Headers::new();
    for &(key, value) in headers {
        response_headers.insert(key.to_owned(), value.to_owned());
    }
    Ok(AsyncRawResponse::from_bytes(
        StatusCode::from(status),
        response_headers,
        body.as_bytes().to_vec(),
    ))
}

fn listing(rows: Value) -> azure_core::Result<AsyncRawResponse> {
    reply(200, &json!({"value":rows}).to_string(), &[])
}

fn created() -> Value {
    json!({
        "id":ITEM,"workspaceId":WORKSPACE,"displayName":"Lakehouse_1","type":"Lakehouse",
        "properties":{"sqlEndpointProperties":{"id":"sql-native-id","provisioningStatus":"InProgress"}},
        "futureNativeField":{"preserved":null}
    })
}

fn operation_url() -> String {
    format!("{ORIGIN}/v1/operations/{OPERATION}")
}

fn accepted() -> azure_core::Result<AsyncRawResponse> {
    reply(
        202,
        "",
        &[
            ("Location", &operation_url()),
            ("x-ms-operation-id", OPERATION),
            ("Retry-After", "1"),
        ],
    )
}

fn preflight(cwd: &std::path::Path, extra: &[&str]) -> PreparedCreateCommand {
    let path = format!("/{WORKSPACE}.Workspace/Lakehouse_1.Lakehouse");
    let mut args = vec!["create", path.as_str(), "--input", "-"];
    args.extend_from_slice(extra);
    let command = v2_create::parse_args(args).unwrap();
    let mut input = RAW;
    PreparedCreateCommand::preflight(
        command,
        &LakehouseCreateBinding::new().unwrap(),
        cwd,
        &mut input,
        false,
    )
    .unwrap()
}

async fn execute(
    prepared: PreparedCreateCommand,
    fixture: &Fixture,
    approval: &mut Approval,
    timeout_poll: bool,
    interrupted: impl Future<Output = ()> + Send,
) -> Result<CreateDelivery, CreateCommandError> {
    let http = fixture.http();
    let trace = ExecutionTrace::default();
    let operation_http = fixture.http().with_trace(trace.clone());
    let observe = || {
        let evidence = trace.snapshot();
        CreateTransportEvidence {
            http_status: evidence.status,
            operation_id: evidence.operation_id,
        }
    };
    prepared
        .execute(
            &LakehouseCreateBinding::new().unwrap(),
            &fixture.client(Audience::Fabric),
            &http as &dyn ItemExecutor,
            CreateExecution {
                executor: OperationHttp {
                    http: &operation_http,
                    timeout_poll,
                },
                observe: &observe,
            },
            approval,
            interrupted,
        )
        .await
}

fn approval(write: bool) -> Approval {
    Approval {
        denied: false,
        calls: 0,
        expect_write: write,
    }
}

#[tokio::test]
async fn native_201_full_host_preserves_raw_approval_body_and_actual_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let actual = created();
    let fixture = Fixture::new(vec![
        listing(json!([])),
        reply(201, &actual.to_string(), &[]),
    ]);
    let mut approval = approval(true);
    let delivery = execute(
        preflight(temp.path(), &[]),
        &fixture,
        &mut approval,
        false,
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(approval.calls, 1);
    let report = delivery.report();
    assert_eq!(report.exit_code(), 0);
    assert_eq!(report.value()["data"], actual);
    assert_eq!(report.value()["meta"]["resourceId"], ITEM);
    assert_eq!(report.value()["meta"]["operationHandle"], Value::Null);
    assert_eq!(report.value()["meta"]["completed"], true);
    let mut stdout = Vec::new();
    delivery.deliver(&mut stdout).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&stdout).unwrap()["data"],
        actual
    );
    let requests = fixture.network.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, Method::Get);
    assert_eq!(
        requests[0].url,
        format!("{ORIGIN}/v1/workspaces/{WORKSPACE}/items")
    );
    assert_eq!(requests[1].method, Method::Post);
    assert_eq!(
        requests[1].url,
        format!("{ORIGIN}/v1/workspaces/{WORKSPACE}/lakehouses")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[1].body).unwrap(),
        serde_json::from_slice::<Value>(RAW).unwrap()
    );
    assert_eq!(fixture.network.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn denied_approval_dispatches_no_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(vec![listing(json!([]))]);
    let mut approval = approval(true);
    approval.denied = true;
    assert!(matches!(
        execute(
            preflight(temp.path(), &[]),
            &fixture,
            &mut approval,
            false,
            pending()
        )
        .await,
        Err(CreateCommandError::Approval(CreateApprovalError::Denied))
    ));
    assert_eq!(approval.calls, 1);
    assert_eq!(fixture.network.posts.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.network.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn existing_skip_retains_identity_without_comparing_or_writing_content() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(vec![listing(json!([{
        "id":ITEM,"displayName":"Lakehouse_1","type":"Lakehouse","workspaceId":WORKSPACE
    }]))]);
    let mut approval = approval(false);
    let delivery = execute(
        preflight(temp.path(), &["--if-exists", "skip"]),
        &fixture,
        &mut approval,
        false,
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(delivery.report().value()["meta"]["status"], "unchanged");
    assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
    assert_eq!(
        delivery.report().value()["meta"]["contentEqualityClaimed"],
        false
    );
    assert_eq!(fixture.network.posts.load(Ordering::SeqCst), 0);
    assert_eq!(approval.calls, 1);
}

#[tokio::test]
async fn preflight_conflict_ambiguity_wrong_kind_and_wrong_workspace_never_approve_or_post() {
    let temp = tempfile::tempdir().unwrap();
    let existing = json!({
        "id":ITEM,"displayName":"Lakehouse_1","type":"Lakehouse","workspaceId":WORKSPACE
    });
    for rows in [
        json!([existing.clone()]),
        json!([existing.clone(), existing]),
        json!([{"id":ITEM,"displayName":"Lakehouse_1","type":"Notebook"}]),
        json!([{"id":ITEM,"displayName":"Lakehouse_1","type":"Lakehouse","workspaceId":OPERATION}]),
    ] {
        let fixture = Fixture::new(vec![listing(rows)]);
        let mut approval = approval(true);
        assert!(
            execute(
                preflight(temp.path(), &[]),
                &fixture,
                &mut approval,
                false,
                pending()
            )
            .await
            .is_err()
        );
        assert_eq!(approval.calls, 0);
        assert_eq!(fixture.network.posts.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.network.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn later_discovery_page_failure_never_treats_partial_listing_as_missing() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(vec![
        reply(200, r#"{"value":[],"continuationToken":"next-page"}"#, &[]),
        reply(403, r#"{"errorCode":"Denied","message":"synthetic"}"#, &[]),
    ]);
    let mut approval = approval(true);
    assert!(matches!(
        execute(
            preflight(temp.path(), &[]),
            &fixture,
            &mut approval,
            false,
            pending()
        )
        .await,
        Err(CreateCommandError::Discovery)
    ));
    assert_eq!(approval.calls, 0);
    assert_eq!(fixture.network.posts.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.network.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn conflict_race_is_native_failure_not_a_skip_retry_or_overwrite() {
    let temp = tempfile::tempdir().unwrap();
    let raw =
        json!({"errorCode":"ItemDisplayNameAlreadyInUse","message":"synthetic-private-diagnostic"});
    let fixture = Fixture::new(vec![listing(json!([])), reply(409, &raw.to_string(), &[])]);
    let mut approval = approval(true);
    let delivery = execute(
        preflight(temp.path(), &["--if-exists", "skip"]),
        &fixture,
        &mut approval,
        false,
        pending(),
    )
    .await
    .unwrap();
    let report = delivery.report();
    assert_eq!(report.exit_code(), 1);
    assert_eq!(report.value()["meta"]["status"], "failed");
    assert_eq!(report.value()["meta"]["action"], "create");
    assert_eq!(report.value()["meta"]["policy"], "skip");
    assert_eq!(
        report.value()["meta"]["serviceCode"],
        "ItemDisplayNameAlreadyInUse"
    );
    assert_eq!(report.value()["meta"]["serviceError"], raw);
    assert_eq!(report.value()["meta"]["remoteReplayed"], false);
    assert_eq!(fixture.network.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn wait_and_projection_preserve_native_resource_id_and_completed_operation_handle() {
    let temp = tempfile::tempdir().unwrap();
    let result_url = format!("{}/result", operation_url());
    let fixture = Fixture::new(vec![
        listing(json!([])),
        accepted(),
        reply(200, r#"{"status":"Running"}"#, &[("Retry-After", "1")]),
        reply(
            200,
            r#"{"status":"Succeeded"}"#,
            &[("Location", &result_url)],
        ),
        reply(200, &created().to_string(), &[]),
    ]);
    let mut approval = approval(true);
    let delivery = execute(
        preflight(temp.path(), &["--wait", "--project", "missingField"]),
        &fixture,
        &mut approval,
        false,
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    let mut stdout = Vec::new();
    delivery.deliver(&mut stdout).unwrap();
    let value: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(value["data"], Value::Null);
    assert_eq!(value["meta"]["completed"], true);
    assert_eq!(value["meta"]["resourceId"], ITEM);
    assert_eq!(
        value["meta"]["operationHandle"],
        format!("/operations/{OPERATION}")
    );
    assert_ne!(value["meta"]["resourceId"], OPERATION);
    assert_eq!(fixture.network.posts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.network.requests.lock().unwrap().len(), 5);
}

#[tokio::test]
async fn acceptance_timeout_and_cancellation_retain_handles_without_rollback_or_replay() {
    let temp = tempfile::tempdir().unwrap();
    for mode in ["accepted", "timeout", "cancelled"] {
        let fixture = Fixture::new(vec![listing(json!([])), accepted()]);
        let mut approval = approval(true);
        let network = fixture.network.clone();
        let interrupted = poll_fn(move |cx| {
            if mode == "cancelled" && network.posts.load(Ordering::SeqCst) != 0 {
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        });
        let delivery = execute(
            preflight(
                temp.path(),
                if mode == "accepted" { &[] } else { &["--wait"] },
            ),
            &fixture,
            &mut approval,
            mode == "timeout",
            interrupted,
        )
        .await
        .unwrap();
        let report = delivery.report();
        assert_eq!(
            report.value()["meta"]["operationHandle"],
            format!("/operations/{OPERATION}")
        );
        assert_eq!(report.value()["meta"]["resourceId"], Value::Null);
        assert_eq!(report.value()["meta"]["completed"], false);
        assert_eq!(report.value()["meta"]["remoteReplayed"], false);
        assert_eq!(report.value()["meta"]["remoteCancelled"], false);
        assert_eq!(report.value()["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(report.exit_code(), u8::from(mode != "accepted"));
        if mode != "accepted" {
            assert_eq!(report.value()["meta"]["status"], "unknown");
            assert_eq!(
                report.value()["error"]["code"],
                if mode == "timeout" {
                    "TimedOut"
                } else {
                    "Cancelled"
                }
            );
        }
        assert_eq!(fixture.network.posts.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.network.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn accepted_native_resource_identity_survives_later_poll_timeout_and_projection() {
    let temp = tempfile::tempdir().unwrap();
    let actual = created();
    let fixture = Fixture::new(vec![
        listing(json!([])),
        reply(
            202,
            &actual.to_string(),
            &[
                ("Location", &operation_url()),
                ("x-ms-operation-id", OPERATION),
                ("Retry-After", "1"),
            ],
        ),
    ]);
    let mut approval = approval(true);
    let delivery = execute(
        preflight(temp.path(), &["--wait", "--project", "missingField"]),
        &fixture,
        &mut approval,
        true,
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
    assert_eq!(delivery.report().value()["data"], actual);
    let mut stdout = Vec::new();
    delivery.deliver(&mut stdout).unwrap();
    let value: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(value["meta"]["resourceId"], ITEM);
    assert_eq!(
        value["meta"]["operationHandle"],
        format!("/operations/{OPERATION}")
    );
    assert_eq!(value["meta"]["completed"], false);
    assert_eq!(value["data"], Value::Null);
    assert_eq!(fixture.network.posts.load(Ordering::SeqCst), 1);
}

struct FailedWriter;

impl Write for FailedWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("synthetic delivery failure"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("synthetic flush failure"))
    }
}

#[tokio::test]
async fn output_failure_retains_acceptance_and_no_clobber_preserves_racing_file() {
    let temp = tempfile::tempdir().unwrap();
    for output_file in [false, true] {
        let fixture = Fixture::new(vec![listing(json!([])), accepted()]);
        let path = temp.path().join("receipt.json");
        let prepared = preflight(
            temp.path(),
            if output_file {
                &["--output", "receipt.json"]
            } else {
                &[]
            },
        );
        let mut approval = approval(true);
        let delivery = execute(prepared, &fixture, &mut approval, false, pending())
            .await
            .unwrap();
        if output_file {
            std::fs::write(&path, b"racing owner file").unwrap();
        }
        let error = delivery.deliver(&mut FailedWriter).unwrap_err();
        let CreateCommandError::Delivery { report, .. } = error else {
            panic!("local failure must retain original unprojected remote evidence");
        };
        assert_eq!(
            report.value()["meta"]["operationHandle"],
            format!("/operations/{OPERATION}")
        );
        assert_eq!(report.value()["meta"]["status"], "accepted");
        assert_eq!(report.value()["meta"]["remoteReplayed"], false);
        assert_eq!(fixture.network.posts.load(Ordering::SeqCst), 1);
        if output_file {
            assert_eq!(std::fs::read(path).unwrap(), b"racing owner file");
        }
    }
}

struct ForbiddenRead;

impl Read for ForbiddenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("this target must be rejected before explicit stdin access");
    }
}

#[test]
fn malformed_metadata_and_unsupported_policy_are_local_before_any_identity_or_http() {
    let temp = tempfile::tempdir().unwrap();
    let path = format!("/{WORKSPACE}.Workspace/Lakehouse_1.Lakehouse");
    let binding = LakehouseCreateBinding::new().unwrap();
    for raw in [
        b"".as_slice(),
        b"null",
        b"{}",
        br#"{"displayName":"other"}"#,
        br#"{"displayName":"Lakehouse_1","unknown":true}"#,
        br#"{"displayName":"Lakehouse_1","displayName":"Lakehouse_1"}"#,
    ] {
        let command = v2_create::parse_args([
            "create",
            path.as_str(),
            "--input",
            "-",
            "--if-exists",
            "skip",
        ])
        .unwrap();
        assert!(
            PreparedCreateCommand::preflight(command, &binding, temp.path(), &mut &raw[..], false)
                .is_err()
        );
    }
    for policy in ["overwrite", "duplicate"] {
        let command =
            v2_create::parse_args(["create", path.as_str(), "--if-exists", policy]).unwrap();
        assert!(matches!(
            PreparedCreateCommand::preflight(
                command,
                &binding,
                temp.path(),
                &mut ForbiddenRead,
                false
            ),
            Err(CreateCommandError::Policy(CreateError::UnsupportedPolicy))
        ));
        let command = v2_create::parse_args([
            "create",
            path.as_str(),
            "--if-exists",
            policy,
            "--input",
            "-",
        ])
        .unwrap();
        assert!(matches!(
            PreparedCreateCommand::preflight(
                command,
                &binding,
                temp.path(),
                &mut ForbiddenRead,
                false
            ),
            Err(CreateCommandError::Policy(CreateError::UnsupportedPolicy))
        ));
    }
    let command = v2_create::parse_args(["create", path.as_str(), "--force"]).unwrap();
    assert!(matches!(
        PreparedCreateCommand::preflight(command, &binding, temp.path(), &mut ForbiddenRead, false),
        Err(CreateCommandError::Policy(CreateError::UnsupportedPolicy))
    ));
}

#[test]
fn operation_rejects_raw_substitution_and_missing_source_before_execution() {
    let binding = LakehouseCreateBinding::new().unwrap();
    let command = v2_create::parse_args(["create", "/unused"]).unwrap();
    let target = CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WORKSPACE).unwrap(),
        "Lakehouse_1",
        "Lakehouse",
    )
    .unwrap();
    let plan = command
        .plan(
            target,
            Preflight::Leaf(LeafState::Missing),
            &binding.capabilities(),
            Some(serde_json::from_slice(RAW).unwrap()),
            &binding,
        )
        .unwrap();
    assert!(binding.operation(&plan, None, false).is_err());
    assert!(
        binding
            .operation(&plan, Some(br#"{"displayName":"Lakehouse_1"}"#), false)
            .is_err()
    );
    assert!(binding.operation(&plan, Some(RAW), false).is_ok());
}

#[tokio::test]
async fn selected_context_mismatch_or_interruption_before_discovery_has_zero_identity_and_send() {
    let temp = tempfile::tempdir().unwrap();
    for wrong_context in [false, true] {
        let fixture = Fixture::new(Vec::new());
        let http = fixture.http();
        let observe = || CreateTransportEvidence {
            http_status: None,
            operation_id: None,
        };
        let client = fixture.client(if wrong_context {
            Audience::Arm
        } else {
            Audience::Fabric
        });
        let mut approval = approval(true);
        let prepared = preflight(temp.path(), &[]);
        let result = if wrong_context {
            prepared
                .execute(
                    &LakehouseCreateBinding::new().unwrap(),
                    &client,
                    &http,
                    CreateExecution {
                        executor: OperationHttp {
                            http: &http,
                            timeout_poll: false,
                        },
                        observe: &observe,
                    },
                    &mut approval,
                    pending(),
                )
                .await
        } else {
            prepared
                .execute(
                    &LakehouseCreateBinding::new().unwrap(),
                    &client,
                    &http,
                    CreateExecution {
                        executor: OperationHttp {
                            http: &http,
                            timeout_poll: false,
                        },
                        observe: &observe,
                    },
                    &mut approval,
                    ready(()),
                )
                .await
        };
        assert!(result.is_err());
        assert_eq!(approval.calls, 0);
        assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 0);
        assert!(fixture.network.requests.lock().unwrap().is_empty());
    }
}

#[cfg(feature = "headless")]
mod executable {
    use super::*;
    use std::{
        process::{Command, Output, Stdio},
        thread,
        time::Instant,
    };

    fn invoke(binary: &str, args: &[&str], hold_stdin: bool) -> Output {
        let mut command = Command::new(binary);
        command
            .env_clear()
            .env("FAB_RUST_AUTH_MODE", "sensitive-invalid-mode")
            .env("FAB_RUST_CLIENT_SECRET", "synthetic-secret-never-used")
            .stdin(if hold_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .args(args);
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let stdout = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.take(32 * 1024 + 1).read_to_end(&mut bytes).unwrap();
            bytes
        });
        let stderr = thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.take(4 * 1024 + 1).read_to_end(&mut bytes).unwrap();
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("bounded create process did not terminate");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let result = Output {
            status: child.wait().unwrap(),
            stdout: stdout.join().unwrap(),
            stderr: stderr.join().unwrap(),
        };
        assert!(result.stdout.len() <= 32 * 1024);
        assert!(result.stderr.len() <= 4 * 1024);
        assert!(!String::from_utf8_lossy(&result.stdout).contains("synthetic-secret"));
        assert!(!String::from_utf8_lossy(&result.stderr).contains("sensitive-invalid-mode"));
        assert!(!String::from_utf8_lossy(&result.stderr).contains("synthetic-secret"));
        result
    }

    fn create(args: &[&str]) -> Output {
        invoke(env!("CARGO_BIN_EXE_fab-cli-v2"), args, false)
    }

    #[test]
    fn help_selects_lakehouse_without_identity_and_keeps_v1_alias() {
        for args in [vec!["--help"], vec!["create", "--help"]] {
            let result = create(&args);
            assert!(result.status.success());
            assert!(result.stderr.is_empty());
            let help = String::from_utf8_lossy(&result.stdout);
            assert!(help.contains("Lakehouse"));
            assert!(help.contains("create"));
            if args.first() == Some(&"create") {
                assert!(!help.contains("Notebook enables"));
            }
        }
        let result = invoke(env!("CARGO_BIN_EXE_fab-rs"), &["create", "--help"], false);
        assert!(result.status.success());
        assert!(result.stderr.is_empty());
        let help = String::from_utf8_lossy(&result.stdout);
        assert!(help.contains("mkdir"));
        assert!(!help.contains("Selected native owner"));
    }

    #[test]
    fn absent_terminal_approval_and_unsupported_policy_never_enter_identity() {
        let path = format!("/{WORKSPACE}.Workspace/Lakehouse_1.Lakehouse");
        let result = create(&["create", &path]);
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        let error: Value = serde_json::from_slice(&result.stderr).unwrap();
        assert_eq!(error["error"]["code"], "ApprovalRequired");
        for extra in [
            vec!["--force"],
            vec!["--if-exists", "overwrite"],
            vec!["--if-exists", "duplicate"],
        ] {
            let mut args = vec!["create", path.as_str()];
            args.extend(extra);
            let result = create(&args);
            assert_eq!(result.status.code(), Some(2));
            assert!(result.stdout.is_empty());
            let error: Value = serde_json::from_slice(&result.stderr).unwrap();
            assert_eq!(error["error"]["code"], "CreatePolicyRejected");
            assert_eq!(error["evidence"]["mutationMayHaveOccurred"], false);
        }
    }

    #[test]
    fn missing_parent_other_owner_facade_and_invented_flags_are_local_errors() {
        for args in [
            vec!["create", "Lakehouse_1.Lakehouse"],
            vec!["create", "/Name.Workspace/Lakehouse_1.Lakehouse"],
            vec![
                "create",
                "/11111111-1111-4111-8111-111111111111.Workspace/Other.Notebook",
            ],
            vec![
                "create",
                "/11111111-1111-4111-8111-111111111111.Workspace/Other.Report",
            ],
            vec!["create", "/11111111-1111-4111-8111-111111111111.Workspace"],
            vec![
                "create",
                "/11111111-1111-4111-8111-111111111111.Workspace/Lakehouse_1.Lakehouse",
                "--name",
                "invented",
            ],
        ] {
            let result = create(&args);
            assert_eq!(result.status.code(), Some(2));
            assert!(result.stdout.is_empty());
            let error: Value = serde_json::from_slice(&result.stderr).unwrap();
            assert!(matches!(
                error["error"]["code"].as_str(),
                Some("InvalidInput" | "InvalidArguments")
            ));
        }
    }

    #[test]
    fn blocked_stdin_preflight_obeys_total_timeout_without_identity_or_mutation() {
        let path = format!("/{WORKSPACE}.Workspace/Lakehouse_1.Lakehouse");
        let result = invoke(
            env!("CARGO_BIN_EXE_fab-cli-v2"),
            &["create", &path, "--input", "-", "--timeout", "10ms"],
            true,
        );
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        let error: Value = serde_json::from_slice(&result.stderr).unwrap();
        assert_eq!(error["error"]["code"], "TimedOut");
    }
}
