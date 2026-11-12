// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public create parser -> shared host -> typed SDK -> offline Azure transport.

#![cfg(feature = "v2-create")]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    error::ErrorKind,
    http::{AsyncRawResponse, Body, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::{
    create_command::{
        CreateApproval, CreateApprovalError, CreateApprovalRequest, CreateBinding, CreateCommand,
        CreateCommandError, CreateDelivery, CreateExecution, CreateTransportEvidence, MAX_BYTES,
        PreparedCreateCommand, v2_create,
    },
    create_notebook::NotebookBinding,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, RequestContext,
    items::create::{
        CreateAction, CreateCapabilities, CreateError, CreateInputValidator, CreatePlan,
        CreateTarget, DuplicateSupport, InputDisposition, LeafState, NativeId, Preflight,
        ReplacementUnit, ResourceIdentity, SchemaRef, TargetScope,
    },
    notebook_create,
};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, ExecutionTrace, HttpExecutor,
};
use fabric_sdk_operations::{FabricOperation, OperationErrorKind};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending, ready},
    io::{self, Cursor, Read, Write},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const WS: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OP: &str = "99999999-8888-7777-6666-555555555555";
const OTHER_WS: &str = "00000000-1111-2222-3333-444444444444";
const TARGET: &str = "/11111111-2222-3333-4444-555555555555.Workspace/Literal.name.Notebook";
const RAW: &[u8] = br#" {
  "displayName": "Literal.name",
  "description": "exact \u0062ytes",
  "definition": {"format":"ipynb","parts":[
    {"path":"notebook-content.ipynb","payload":"e30=","payloadType":"InlineBase64"},
    {"path":".platform","payload":"e30=","payloadType":"InlineBase64"}
  ]}
}
"#;

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
            "offline-create-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
enum ReplyBody {
    Bytes(Vec<u8>),
    Lost,
    Pending,
    BodyPending,
    BodyFailure,
}

#[derive(Debug)]
struct Reply {
    method: Method,
    path: String,
    status: u16,
    headers: Headers,
    body: ReplyBody,
}

impl Reply {
    fn bytes(method: Method, path: String, status: u16, body: Vec<u8>) -> Self {
        Self {
            method,
            path,
            status,
            headers: Headers::new(),
            body: ReplyBody::Bytes(body),
        }
    }

    fn json(method: Method, path: String, status: u16, value: Value) -> Self {
        Self::bytes(method, path, status, serde_json::to_vec(&value).unwrap())
    }
}

#[derive(Debug, Default)]
struct Wire {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Request>>,
    pending_started: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP: create must not replay");
        assert_eq!(request.method(), reply.method);
        assert_eq!(request.url().path(), reply.path);
        assert_eq!(request.url().host_str(), Some("api.fabric.microsoft.com"));
        assert!(request.headers().get_str(&"authorization".into()).is_ok());
        match reply.body {
            ReplyBody::Bytes(body) => Ok(AsyncRawResponse::from_bytes(
                StatusCode::from(reply.status),
                reply.headers,
                body,
            )),
            ReplyBody::Lost => Err(azure_core::Error::with_message(
                ErrorKind::Io,
                "offline response lost after possible acceptance",
            )),
            ReplyBody::Pending => {
                self.pending_started.notify_one();
                pending().await
            }
            ReplyBody::BodyPending => {
                let started = self.pending_started.clone();
                let body = futures_util::stream::poll_fn(move |_| {
                    started.notify_one();
                    std::task::Poll::Pending
                });
                Ok(AsyncRawResponse::new(
                    StatusCode::from(reply.status),
                    reply.headers,
                    Box::pin(body),
                ))
            }
            ReplyBody::BodyFailure => {
                let body = futures_util::stream::iter([
                    Ok(azure_core::Bytes::from_static(b"{\"incomplete\":")),
                    Err(azure_core::Error::with_message(
                        ErrorKind::Io,
                        "offline response stream failed after headers",
                    )),
                ]);
                Ok(AsyncRawResponse::new(
                    StatusCode::from(reply.status),
                    reply.headers,
                    Box::pin(body),
                ))
            }
        }
    }
}

#[derive(Debug)]
struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(tokio::time::sleep(delay))
    }
}

struct Fixture {
    root: PathBuf,
    credential: Arc<Credential>,
    client: Client,
    wire: Arc<Wire>,
    http: HttpExecutor,
    trace: ExecutionTrace,
}

impl Fixture {
    fn new(replies: Vec<Reply>) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::current_dir().unwrap().join(format!(
            ".create-v2-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&root).unwrap();
        let credential = Arc::new(Credential::default());
        let client = Client::with_context(RequestContext::new(
            credential.clone(),
            CredentialSelection::new("offline-account", WS, Audience::Fabric).unwrap(),
        ));
        let wire = Arc::new(Wire {
            replies: Mutex::new(replies.into()),
            ..Wire::default()
        });
        let http =
            HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default()).unwrap();
        Self {
            root,
            credential,
            client,
            wire,
            http,
            trace: ExecutionTrace::default(),
        }
    }

    fn prepare(&self, extra: &[&str], raw: Option<&[u8]>) -> PreparedCreateCommand {
        self.prepare_target(TARGET, extra, raw)
    }

    fn prepare_target(
        &self,
        target: &str,
        extra: &[&str],
        raw: Option<&[u8]>,
    ) -> PreparedCreateCommand {
        let command = command(target, extra, raw.is_some());
        PreparedCreateCommand::preflight(
            command,
            &NotebookBinding,
            &self.root,
            &mut Cursor::new(raw.unwrap_or_default()),
            false,
        )
        .unwrap()
    }

    async fn execute(
        &self,
        prepared: PreparedCreateCommand,
        approval: &mut dyn CreateApproval,
    ) -> Result<CreateDelivery, CreateCommandError> {
        self.execute_interrupted(prepared, approval, pending())
            .await
    }

    async fn execute_interrupted(
        &self,
        prepared: PreparedCreateCommand,
        approval: &mut dyn CreateApproval,
        interrupted: impl Future<Output = ()> + Send,
    ) -> Result<CreateDelivery, CreateCommandError> {
        self.execute_bound_interrupted(&NotebookBinding, prepared, approval, interrupted)
            .await
    }

    fn prepare_bound(
        &self,
        binding: &dyn CreateBinding,
        extra: &[&str],
    ) -> Result<PreparedCreateCommand, CreateCommandError> {
        PreparedCreateCommand::preflight(
            command(TARGET, extra, true),
            binding,
            &self.root,
            &mut Cursor::new(RAW),
            false,
        )
    }

    async fn execute_bound(
        &self,
        binding: &dyn CreateBinding,
        prepared: PreparedCreateCommand,
        approval: &mut dyn CreateApproval,
    ) -> Result<CreateDelivery, CreateCommandError> {
        self.execute_bound_interrupted(binding, prepared, approval, pending())
            .await
    }

    async fn execute_bound_interrupted(
        &self,
        binding: &dyn CreateBinding,
        prepared: PreparedCreateCommand,
        approval: &mut dyn CreateApproval,
        interrupted: impl Future<Output = ()> + Send,
    ) -> Result<CreateDelivery, CreateCommandError> {
        let observe = || {
            let evidence = self.trace.snapshot();
            CreateTransportEvidence {
                http_status: evidence.status,
                operation_id: evidence.operation_id,
            }
        };
        prepared
            .execute(
                binding,
                &self.client,
                &self.http,
                CreateExecution {
                    executor: self.http.clone().with_trace(self.trace.clone()),
                    observe: &observe,
                },
                approval,
                interrupted,
            )
            .await
    }

    fn assert_requests(&self, methods: &[Method]) {
        let requests = self.wire.requests.lock().unwrap();
        assert_eq!(
            requests.iter().map(Request::method).collect::<Vec<_>>(),
            methods
        );
        assert!(self.wire.replies.lock().unwrap().is_empty());
        assert_eq!(self.credential.0.load(Ordering::SeqCst), requests.len());
    }

    fn assert_post_body(&self, expected: &[u8]) {
        self.assert_post_body_at(WS, expected);
    }

    fn assert_post_body_at(&self, workspace: &str, expected: &[u8]) {
        let requests = self.wire.requests.lock().unwrap();
        let posts: Vec<_> = requests
            .iter()
            .filter(|request| request.method() == Method::Post)
            .collect();
        assert_eq!(posts.len(), 1);
        assert_eq!(
            posts[0].url().path(),
            format!("/v1/workspaces/{workspace}/notebooks")
        );
        assert_eq!(posts[0].url().query(), None);
        let Body::Bytes(body) = posts[0].body() else {
            panic!("native JSON must retain bytes");
        };
        assert_eq!(body.as_ref(), expected);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[derive(Debug)]
struct ApprovalSnapshot {
    raw: Option<Vec<u8>>,
    command: CreateCommand,
    remote_write: bool,
    existing_id: Option<String>,
}

#[derive(Default)]
struct Approval {
    workspace: Option<String>,
    denied: bool,
    block: bool,
    calls: Vec<ApprovalSnapshot>,
    started: Arc<tokio::sync::Notify>,
}

impl CreateApproval for Approval {
    fn authorize<'a>(
        &'a mut self,
        request: CreateApprovalRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
        assert_eq!(
            request.plan().target().parent_id().as_str(),
            self.workspace.as_deref().unwrap_or(WS)
        );
        assert_eq!(request.plan().target().name(), "Literal.name");
        assert_eq!(request.plan().target().item_type(), "Notebook");
        assert_eq!(request.plan().policy(), request.command().policy);
        assert_eq!(request.context().selection().account(), "offline-account");
        assert_eq!(request.context().selection().tenant(), WS);
        assert_eq!(request.context().selection().audience(), Audience::Fabric);
        assert_eq!(
            request.context().endpoint().host(),
            "api.fabric.microsoft.com"
        );
        let existing_id = match request.plan().action() {
            CreateAction::Skip { existing } => Some(existing.native_id().as_str().to_owned()),
            CreateAction::Create { .. } => None,
            _ => panic!("Notebook must not approve replacement or duplicate"),
        };
        self.calls.push(ApprovalSnapshot {
            raw: request.raw_input().map(<[u8]>::to_vec),
            command: request.command().clone(),
            remote_write: request.remote_write(),
            existing_id,
        });
        Box::pin(async move {
            self.started.notify_one();
            if self.block {
                return pending().await;
            }
            if self.denied {
                Err(CreateApprovalError::Denied)
            } else {
                Ok(())
            }
        })
    }
}

fn command(target: &str, extra: &[&str], input: bool) -> CreateCommand {
    let mut args = vec!["create", target];
    if input {
        args.extend(["--input", "-"]);
    }
    args.extend_from_slice(extra);
    v2_create::parse_args(args).unwrap()
}

fn create_path() -> String {
    format!("/v1/workspaces/{WS}/notebooks")
}

fn list_path() -> String {
    format!("/v1/workspaces/{WS}/items")
}

fn operation_path() -> String {
    format!("/v1/operations/{OP}")
}

fn resource() -> Value {
    json!({
        "id":ITEM, "workspaceId":WS, "type":"Notebook", "displayName":"Literal.name",
        "description":"provider result", "providerExtra":{"retained":true}
    })
}

fn listing(items: Vec<Value>) -> Reply {
    Reply::json(Method::Get, list_path(), 200, json!({"value":items}))
}

fn created() -> Reply {
    Reply::json(Method::Post, create_path(), 201, resource())
}

fn accepted() -> Reply {
    let mut reply = Reply::bytes(Method::Post, create_path(), 202, Vec::new());
    reply.headers.insert("x-ms-operation-id", OP);
    reply.headers.insert(
        "location",
        format!("https://api.fabric.microsoft.com{}", operation_path()),
    );
    reply.headers.insert("retry-after", "0");
    reply
}

fn state(status: &str) -> Reply {
    let mut reply = Reply::json(Method::Get, operation_path(), 200, json!({"status":status}));
    reply.headers.insert("retry-after", "0");
    if status == "Succeeded" {
        reply.headers.insert(
            "location",
            format!(
                "https://api.fabric.microsoft.com{}/result",
                operation_path()
            ),
        );
    }
    reply
}

fn deliver(delivery: CreateDelivery) -> Value {
    let expected = delivery.report().value().clone();
    let mut bytes = Vec::new();
    assert!(delivery.deliver(&mut bytes).unwrap().is_none());
    assert_eq!(bytes.last(), Some(&b'\n'));
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value, expected);
    value
}

fn assert_no_replay(value: &Value) {
    assert_eq!(value["meta"]["remoteReplayed"], false);
    assert_eq!(value["meta"]["remoteCancelled"], false);
    assert_eq!(value["meta"]["workspaceId"], WS);
    assert_eq!(value["meta"]["type"], "Notebook");
}

#[tokio::test]
async fn parser_host_sdk_http_preserve_explicit_native_bytes_and_201_resource() {
    let fixture = Fixture::new(vec![listing(vec![]), created()]);
    let mut approval = Approval::default();
    let delivery = fixture
        .execute(fixture.prepare(&[], Some(RAW)), &mut approval)
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    assert!(delivery.report().error().is_none());
    let value = deliver(delivery);
    assert_eq!(value["data"], resource());
    assert_eq!(value["meta"]["resourceId"], ITEM);
    assert_eq!(value["meta"]["status"], "completed");
    assert_eq!(value["meta"]["completed"], true);
    assert_eq!(value["meta"]["operationHandle"], Value::Null);
    assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
    assert_no_replay(&value);
    assert_eq!(approval.calls.len(), 1);
    assert_eq!(approval.calls[0].raw.as_deref(), Some(RAW));
    assert!(approval.calls[0].remote_write);
    assert_eq!(approval.calls[0].command.destination, TARGET);
    assert!(!approval.calls[0].command.wait);
    assert!(approval.calls[0].existing_id.is_none());
    fixture.assert_requests(&[Method::Get, Method::Post]);
    fixture.assert_post_body(RAW);
}

#[tokio::test]
async fn omitted_input_is_a_shell_not_an_explicit_empty_or_null_request() {
    let fixture = Fixture::new(vec![listing(vec![]), created()]);
    let mut approval = Approval::default();
    let delivery = fixture
        .execute(fixture.prepare(&[], None), &mut approval)
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    assert_eq!(approval.calls[0].raw, None);
    fixture.assert_post_body(br#"{"displayName":"Literal.name"}"#);
    fixture.assert_requests(&[Method::Get, Method::Post]);
}

#[tokio::test]
async fn accepted_without_wait_retains_handle_and_never_fetches_state_or_result() {
    let fixture = Fixture::new(vec![listing(vec![]), accepted()]);
    let delivery = fixture
        .execute(fixture.prepare(&[], Some(RAW)), &mut Approval::default())
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    let value = deliver(delivery);
    assert_eq!(value["data"], Value::Null);
    assert_eq!(value["meta"]["status"], "accepted");
    assert_eq!(value["meta"]["completed"], false);
    assert_eq!(
        value["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
    assert_no_replay(&value);
    fixture.assert_requests(&[Method::Get, Method::Post]);
    fixture.assert_post_body(RAW);
}

#[tokio::test]
async fn wait_reconciles_202_state_and_result_with_one_mutation() {
    let fixture = Fixture::new(vec![
        listing(vec![]),
        accepted(),
        state("Running"),
        state("Succeeded"),
        Reply::json(
            Method::Get,
            format!("{}/result", operation_path()),
            200,
            resource(),
        ),
    ]);
    let mut approval = Approval::default();
    let delivery = fixture
        .execute(fixture.prepare(&["--wait"], Some(RAW)), &mut approval)
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    let value = deliver(delivery);
    assert_eq!(value["data"], resource());
    assert_eq!(value["meta"]["completed"], true);
    assert_eq!(
        value["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert!(approval.calls[0].command.wait);
    assert_no_replay(&value);
    fixture.assert_requests(&[
        Method::Get,
        Method::Post,
        Method::Get,
        Method::Get,
        Method::Get,
    ]);
    fixture.assert_post_body(RAW);
}

#[tokio::test]
async fn failed_poll_retains_service_error_handle_and_uncertain_effect() {
    let service_error = json!({"errorCode":"NotebookCreateFailed","message":"offline failure"});
    let fixture = Fixture::new(vec![
        listing(vec![]),
        accepted(),
        Reply::json(
            Method::Get,
            operation_path(),
            200,
            json!({"status":"Failed","error":service_error}),
        ),
    ]);
    let delivery = fixture
        .execute(
            fixture.prepare(&["--wait"], Some(RAW)),
            &mut Approval::default(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    let error = delivery.report().error().unwrap();
    assert!(matches!(
        &error.kind,
        OperationErrorKind::OperationFailed { status, .. } if status == "Failed"
    ));
    assert_eq!(error.raw_service_error.as_deref(), Some(&service_error));
    assert_eq!(error.operation_id.as_deref(), Some(OP));
    assert!(error.mutation_may_have_occurred);
    let value = deliver(delivery);
    assert_eq!(
        value["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(value["meta"]["completed"], false);
    assert_no_replay(&value);
    fixture.assert_requests(&[Method::Get, Method::Post, Method::Get]);
}

#[tokio::test]
async fn malformed_or_mismatched_created_resource_is_not_confirmed_success() {
    let mut wrong_workspace = resource();
    wrong_workspace["workspaceId"] = json!(OTHER_WS);
    let mut wrong_type = resource();
    wrong_type["type"] = json!("Lakehouse");
    let mut wrong_name = resource();
    wrong_name["displayName"] = json!("Different name");
    let mut missing_id = resource();
    missing_id.as_object_mut().unwrap().remove("id");
    for body in [
        b"{malformed".to_vec(),
        br#"{"id":"one","id":"two"}"#.to_vec(),
        serde_json::to_vec(&wrong_workspace).unwrap(),
        serde_json::to_vec(&wrong_type).unwrap(),
        serde_json::to_vec(&wrong_name).unwrap(),
        serde_json::to_vec(&missing_id).unwrap(),
    ] {
        let fixture = Fixture::new(vec![
            listing(vec![]),
            Reply::bytes(Method::Post, create_path(), 201, body.clone()),
        ]);
        let delivery = fixture
            .execute(fixture.prepare(&[], Some(RAW)), &mut Approval::default())
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(
            delivery.report().error().unwrap().kind,
            OperationErrorKind::InvalidResponse
        );
        let value = deliver(delivery);
        assert_eq!(value["meta"]["completed"], false);
        assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
        if let Ok(observed) = serde_json::from_slice::<Value>(&body)
            && observed.get("workspaceId").is_some()
        {
            assert_eq!(value["data"], observed);
            assert_eq!(
                value["meta"]["observedResource"]["workspaceId"],
                observed["workspaceId"]
            );
            assert_eq!(value["meta"]["observedResource"]["type"], observed["type"]);
        }
        assert_no_replay(&value);
        fixture.assert_requests(&[Method::Get, Method::Post]);
    }
}

#[tokio::test]
async fn approval_denial_is_after_bounded_discovery_but_before_post() {
    let fixture = Fixture::new(vec![listing(vec![])]);
    let mut approval = Approval {
        denied: true,
        ..Approval::default()
    };
    let result = fixture
        .execute(fixture.prepare(&[], Some(RAW)), &mut approval)
        .await;
    assert!(matches!(
        result,
        Err(CreateCommandError::Approval(CreateApprovalError::Denied))
    ));
    assert_eq!(approval.calls.len(), 1);
    assert_eq!(approval.calls[0].raw.as_deref(), Some(RAW));
    fixture.assert_requests(&[Method::Get]);
}

struct Unreadable;

impl Read for Unreadable {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("invalid target or contested stdin must not be read");
    }
}

#[test]
fn owner_stdin_and_exact_metadata_target_rejections_are_entirely_local() {
    let fixture = Fixture::new(vec![]);
    let targets = [
        "/Named.Workspace/Literal.name.Notebook",
        "/11111111-2222-3333-4444-555555555555.Workspace",
        "/11111111-2222-3333-4444-555555555555.Workspace/notebooks",
        "/11111111-2222-3333-4444-555555555555.Workspace/Literal.name.Lakehouse",
        "/11111111-2222-3333-4444-555555555555.Workspace/Literal.name.Notebook/src/definition",
        "/11111111-2222-3333-4444-555555555555.Workspace/Literal.name.Notebook/Files/content",
        "/workspaces/11111111-2222-3333-4444-555555555555/items/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
    ];
    for target in targets {
        assert!(
            PreparedCreateCommand::preflight(
                command(target, &[], true),
                &NotebookBinding,
                &fixture.root,
                &mut Unreadable,
                false,
            )
            .is_err(),
            "{target}"
        );
    }
    assert!(
        PreparedCreateCommand::preflight(
            command(TARGET, &[], true),
            &NotebookBinding,
            &fixture.root,
            &mut Unreadable,
            true,
        )
        .is_err()
    );
    assert!(
        PreparedCreateCommand::preflight(
            command(TARGET, &["--profile", "unavailable"], true),
            &NotebookBinding,
            &fixture.root,
            &mut Unreadable,
            false,
        )
        .is_err()
    );
    fixture.assert_requests(&[]);
}

#[test]
fn malformed_null_unknown_duplicate_and_invalid_native_inputs_send_nothing_even_for_skip() {
    let fixture = Fixture::new(vec![]);
    for raw in [
        b"".as_slice(),
        b"null",
        b"[]",
        b"{}",
        b"{",
        br#"{"displayName":"Literal.name"} {}"#,
        br#"{"displayName":"Literal.name","unknown":true}"#,
        br#"{"displayName":"Literal.name","displayName":"Literal.name"}"#,
        br#"{"displayName":"Literal.name","description":null}"#,
        br#"{"displayName":"Literal.name","definition":null}"#,
        br#"{"displayName":"Different"}"#,
        br#"{"displayName":"Literal.name","definition":{"parts":[]}}"#,
        br#"{"displayName":"Literal.name","definition":{"format":"unknown","parts":[]}}"#,
        br#"{"displayName":"Literal.name","definition":{"parts":[{"path":"n","payload":"!","payloadType":"InlineBase64"}]}}"#,
        br#"{"displayName":"Literal.name","definition":{"parts":[{"path":"n","path":"n","payload":"e30=","payloadType":"InlineBase64"}]}}"#,
        br#"{"displayName":"Literal.name","folderId":"00000000-1111-2222-3333-444444444444"}"#,
        b"\xff",
    ] {
        for extra in [vec![], vec!["--if-exists", "skip"]] {
            assert!(
                PreparedCreateCommand::preflight(
                    command(TARGET, &extra, true),
                    &NotebookBinding,
                    &fixture.root,
                    &mut Cursor::new(raw),
                    false,
                )
                .is_err(),
                "{}",
                String::from_utf8_lossy(raw)
            );
        }
    }
    fixture.assert_requests(&[]);
}

#[test]
fn unsupported_overwrite_force_and_duplicate_reject_before_any_discovery() {
    let fixture = Fixture::new(vec![]);
    // Neither remote state can be observed: the native policy is unsupported.
    for extra in [
        vec!["--if-exists", "overwrite"],
        vec!["--force"],
        vec!["--if-exists", "duplicate"],
    ] {
        let command = command(TARGET, &extra, true);
        for state in [
            LeafState::Missing,
            LeafState::Existing(
                ResourceIdentity::new(
                    NativeId::new(ITEM).unwrap(),
                    NativeId::new(WS).unwrap(),
                    "Notebook",
                )
                .unwrap(),
            ),
        ] {
            let result = command.plan(
                CreateTarget::new(
                    TargetScope::NamedLeaf,
                    NativeId::new(WS).unwrap(),
                    "Literal.name",
                    "Notebook",
                )
                .unwrap(),
                Preflight::Leaf(state),
                &notebook_create::capabilities(),
                Some(serde_json::from_slice(RAW).unwrap()),
                &NotebookBinding,
            );
            assert!(matches!(result, Err(CreateError::UnsupportedPolicy)));
        }
        assert!(matches!(
            PreparedCreateCommand::preflight(
                command,
                &NotebookBinding,
                &fixture.root,
                &mut Cursor::new(RAW),
                false,
            ),
            Err(CreateCommandError::Policy(_))
        ));
    }
    fixture.assert_requests(&[]);
}

#[tokio::test]
async fn skip_existing_retains_identity_without_post_or_content_equality_claim() {
    let fixture = Fixture::new(vec![listing(vec![resource()])]);
    let mut approval = Approval::default();
    let delivery = fixture
        .execute(
            fixture.prepare(&["--if-exists", "skip"], Some(RAW)),
            &mut approval,
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    let value = deliver(delivery);
    assert_eq!(value["data"], Value::Null);
    assert_eq!(value["meta"]["action"], "skip");
    assert_eq!(value["meta"]["status"], "unchanged");
    assert_eq!(value["meta"]["resourceId"], ITEM);
    assert_eq!(value["meta"]["mutationMayHaveOccurred"], false);
    assert_eq!(value["meta"]["contentEqualityClaimed"], false);
    assert_eq!(approval.calls.len(), 1);
    assert!(!approval.calls[0].remote_write);
    assert_eq!(approval.calls[0].existing_id.as_deref(), Some(ITEM));
    assert_eq!(approval.calls[0].raw.as_deref(), Some(RAW));
    assert_no_replay(&value);
    fixture.assert_requests(&[Method::Get]);
}

#[tokio::test]
async fn skip_missing_still_creates_once_and_default_existing_is_not_approved() {
    let fixture = Fixture::new(vec![listing(vec![]), created()]);
    let delivery = fixture
        .execute(
            fixture.prepare(&["--if-exists", "skip"], Some(RAW)),
            &mut Approval::default(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().value()["meta"]["action"], "create");
    fixture.assert_requests(&[Method::Get, Method::Post]);
    fixture.assert_post_body(RAW);

    let fixture = Fixture::new(vec![listing(vec![resource()])]);
    let mut approval = Approval::default();
    assert!(matches!(
        fixture
            .execute(fixture.prepare(&[], Some(RAW)), &mut approval)
            .await,
        Err(CreateCommandError::Policy(_))
    ));
    assert!(approval.calls.is_empty());
    fixture.assert_requests(&[Method::Get]);
}

#[tokio::test]
async fn complete_paged_discovery_replans_before_approval_or_mutation() {
    let mut first = listing(vec![]);
    first.body = ReplyBody::Bytes(
        serde_json::to_vec(&json!({"value":[],"continuationToken":"next+/="})).unwrap(),
    );
    let fixture = Fixture::new(vec![first, listing(vec![resource()])]);
    let mut approval = Approval::default();
    let delivery = fixture
        .execute(
            fixture.prepare(&["--if-exists", "skip"], Some(RAW)),
            &mut approval,
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().value()["meta"]["action"], "skip");
    assert_eq!(approval.calls.len(), 1);
    let requests = fixture.wire.requests.lock().unwrap();
    assert_eq!(requests[0].url().query(), None);
    assert_eq!(
        requests[1].url().query(),
        Some("continuationToken=next%2B%2F%3D")
    );
    drop(requests);
    fixture.assert_requests(&[Method::Get, Method::Get]);
}

#[tokio::test]
async fn ambiguous_wrong_kind_or_wrong_parent_discovery_never_approves() {
    let mut wrong_type = resource();
    wrong_type["type"] = json!("Lakehouse");
    let mut wrong_parent = resource();
    wrong_parent["workspaceId"] = json!(OTHER_WS);
    let mut invalid_id = resource();
    invalid_id["id"] = json!("not-an-id");
    let mut casing_match = resource();
    casing_match["displayName"] = json!("literal.NAME");
    for items in [
        vec![resource(), casing_match],
        vec![wrong_type],
        vec![wrong_parent],
        vec![invalid_id],
    ] {
        let fixture = Fixture::new(vec![listing(items)]);
        let mut approval = Approval::default();
        assert!(
            fixture
                .execute(
                    fixture.prepare(&["--if-exists", "skip"], Some(RAW)),
                    &mut approval,
                )
                .await
                .is_err()
        );
        assert!(approval.calls.is_empty());
        fixture.assert_requests(&[Method::Get]);
    }
}

#[tokio::test]
async fn create_time_name_conflict_is_a_single_rejection_not_a_policy_retry() {
    let fixture = Fixture::new(vec![
        listing(vec![]),
        Reply::json(
            Method::Post,
            create_path(),
            409,
            json!({"errorCode":"ItemDisplayNameAlreadyInUse","message":"racing create"}),
        ),
    ]);
    let delivery = fixture
        .execute(
            fixture.prepare(&["--if-exists", "skip"], Some(RAW)),
            &mut Approval::default(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    assert!(matches!(
        &delivery.report().error().unwrap().kind,
        OperationErrorKind::Http { status:409, code:Some(code), .. }
            if code == "ItemDisplayNameAlreadyInUse"
    ));
    let value = deliver(delivery);
    assert_ne!(value["meta"]["action"], "skip");
    assert_eq!(value["meta"]["completed"], false);
    assert_no_replay(&value);
    fixture.assert_requests(&[Method::Get, Method::Post]);
}

#[tokio::test]
async fn cancellation_before_discovery_has_zero_total_http() {
    let fixture = Fixture::new(vec![]);
    let mut approval = Approval::default();
    let result = fixture
        .execute_interrupted(fixture.prepare(&[], Some(RAW)), &mut approval, ready(()))
        .await;
    assert!(result.is_err());
    assert!(approval.calls.is_empty());
    fixture.assert_requests(&[]);
}

#[tokio::test]
async fn cancellation_while_async_approval_is_pending_prevents_submission() {
    let fixture = Fixture::new(vec![listing(vec![])]);
    let mut approval = Approval {
        block: true,
        ..Approval::default()
    };
    let approval_started = approval.started.clone();
    let result = fixture
        .execute_interrupted(
            fixture.prepare(&[], Some(RAW)),
            &mut approval,
            approval_started.notified(),
        )
        .await;
    assert!(matches!(result, Err(CreateCommandError::Interrupted)));
    assert_eq!(approval.calls.len(), 1);
    assert!(approval.calls[0].remote_write);
    fixture.assert_requests(&[Method::Get]);
}

#[tokio::test]
async fn cancellation_during_submission_retains_uncertain_effect_without_inventing_handle() {
    let mut post = created();
    post.body = ReplyBody::Pending;
    let fixture = Fixture::new(vec![listing(vec![]), post]);
    let delivery = fixture
        .execute_interrupted(
            fixture.prepare(&[], Some(RAW)),
            &mut Approval::default(),
            fixture.wire.pending_started.notified(),
        )
        .await
        .unwrap();
    let error = delivery.report().error().unwrap();
    assert_eq!(error.kind, OperationErrorKind::Cancelled);
    assert!(error.mutation_may_have_occurred);
    assert_eq!(error.operation_id, None);
    let value = deliver(delivery);
    assert_eq!(value["meta"]["completed"], false);
    assert_eq!(value["meta"]["operationHandle"], Value::Null);
    assert_no_replay(&value);
    fixture.assert_requests(&[Method::Get, Method::Post]);
}

#[tokio::test]
async fn cancellation_after_acceptance_retains_handle_and_never_cancels_remotely() {
    let mut poll = state("Running");
    poll.body = ReplyBody::Pending;
    let fixture = Fixture::new(vec![listing(vec![]), accepted(), poll]);
    let delivery = fixture
        .execute_interrupted(
            fixture.prepare(&["--wait"], Some(RAW)),
            &mut Approval::default(),
            fixture.wire.pending_started.notified(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    let error = delivery.report().error().unwrap();
    assert_eq!(error.kind, OperationErrorKind::Cancelled);
    assert_eq!(error.operation_id.as_deref(), Some(OP));
    assert!(error.mutation_may_have_occurred);
    let value = deliver(delivery);
    assert_eq!(
        value["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(value["meta"]["completed"], false);
    assert_no_replay(&value);
    fixture.assert_requests(&[Method::Get, Method::Post, Method::Get]);
}

#[tokio::test]
async fn lost_submission_response_retains_uncertain_effect_and_never_replays() {
    let mut reply = created();
    reply.body = ReplyBody::Lost;
    let fixture = Fixture::new(vec![listing(vec![]), reply]);
    let delivery = fixture
        .execute(fixture.prepare(&[], Some(RAW)), &mut Approval::default())
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    assert!(matches!(
        delivery.report().error().unwrap().kind,
        OperationErrorKind::Execution(_)
    ));
    assert!(
        delivery
            .report()
            .error()
            .unwrap()
            .mutation_may_have_occurred
    );
    let value = deliver(delivery);
    assert_eq!(value["meta"]["operationHandle"], Value::Null);
    assert_eq!(value["meta"]["completed"], false);
    assert_no_replay(&value);
    fixture.assert_requests(&[Method::Get, Method::Post]);
    fixture.assert_post_body(RAW);
}

struct BrokenWriter {
    flush_only: bool,
    bytes: Vec<u8>,
}

impl Write for BrokenWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.flush_only {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "offline write"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "offline flush"))
    }
}

#[tokio::test]
async fn writer_write_and_flush_failures_retain_unprojected_success_receipt() {
    for flush_only in [false, true] {
        let fixture = Fixture::new(vec![listing(vec![]), created()]);
        let delivery = fixture
            .execute(
                fixture.prepare(&["--project", "displayName"], Some(RAW)),
                &mut Approval::default(),
            )
            .await
            .unwrap();
        let original = delivery.report().value().clone();
        let mut writer = BrokenWriter {
            flush_only,
            bytes: Vec::new(),
        };
        let CreateCommandError::Delivery { report, source } =
            delivery.deliver(&mut writer).unwrap_err()
        else {
            panic!("delivery failure discarded remote evidence");
        };
        assert!(matches!(*source, CreateCommandError::Io(_)));
        assert_eq!(report.value(), &original);
        assert_eq!(report.value()["data"], resource());
        assert_eq!(report.exit_code(), 0);
        if flush_only {
            let written: Value = serde_json::from_slice(&writer.bytes).unwrap();
            assert_eq!(written["data"], "Literal.name");
        }
        fixture.assert_requests(&[Method::Get, Method::Post]);
    }
}

#[tokio::test]
async fn null_projection_cannot_hide_resource_handle_or_effect_metadata() {
    for wait in [false, true] {
        let mut replies = vec![listing(vec![]), accepted()];
        if wait {
            replies.extend([
                state("Succeeded"),
                Reply::json(
                    Method::Get,
                    format!("{}/result", operation_path()),
                    200,
                    resource(),
                ),
            ]);
        }
        let fixture = Fixture::new(replies);
        let extra = if wait {
            vec!["--wait", "--project", "missing"]
        } else {
            vec!["--project", "missing"]
        };
        let delivery = fixture
            .execute(fixture.prepare(&extra, Some(RAW)), &mut Approval::default())
            .await
            .unwrap();
        let original_meta = delivery.report().value()["meta"].clone();
        let mut bytes = Vec::new();
        delivery.deliver(&mut bytes).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["data"], Value::Null);
        assert_eq!(value["meta"], original_meta);
        assert_eq!(
            value["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
        if wait {
            assert_eq!(value["meta"]["resourceId"], ITEM);
            assert_eq!(value["meta"]["observedResource"]["workspaceId"], WS);
        }
        assert_no_replay(&value);
        fixture.assert_requests(if wait {
            &[Method::Get, Method::Post, Method::Get, Method::Get]
        } else {
            &[Method::Get, Method::Post]
        });
    }
}

#[tokio::test]
async fn output_no_clobber_race_retains_remote_report_and_original_owner_bytes() {
    let fixture = Fixture::new(vec![listing(vec![]), created()]);
    let output = fixture.root.join("receipt.json");
    let prepared = fixture.prepare(&["--output", output.to_str().unwrap()], Some(RAW));
    assert!(
        !output.exists(),
        "preflight must not reserve a visible output"
    );
    let delivery = fixture
        .execute(prepared, &mut Approval::default())
        .await
        .unwrap();
    std::fs::write(&output, b"racing owner bytes").unwrap();
    let CreateCommandError::Delivery { report, source } =
        delivery.deliver(&mut Vec::new()).unwrap_err()
    else {
        panic!("no-clobber failure must retain the report");
    };
    assert!(matches!(*source, CreateCommandError::File(_)));
    assert_eq!(report.exit_code(), 0);
    assert_eq!(report.value()["data"], resource());
    assert_eq!(std::fs::read(output).unwrap(), b"racing owner bytes");
    fixture.assert_requests(&[Method::Get, Method::Post]);
}

#[test]
fn file_preflight_rejects_existing_output_missing_input_and_directory_without_http() {
    let fixture = Fixture::new(vec![]);
    let output = fixture.root.join("receipt.json");
    std::fs::write(&output, b"original receipt").unwrap();
    assert!(matches!(
        PreparedCreateCommand::preflight(
            command(TARGET, &["--output", output.to_str().unwrap()], true),
            &NotebookBinding,
            &fixture.root,
            &mut Cursor::new(RAW),
            false,
        ),
        Err(CreateCommandError::File(_))
    ));
    assert_eq!(std::fs::read(output).unwrap(), b"original receipt");
    for input in ["missing.json", "."] {
        assert!(
            PreparedCreateCommand::preflight(
                command(TARGET, &["--input", input], false),
                &NotebookBinding,
                &fixture.root,
                &mut Unreadable,
                false,
            )
            .is_err()
        );
    }
    fixture.assert_requests(&[]);
}

#[tokio::test]
async fn file_input_is_resolved_from_injected_cwd_and_preserved_on_wire() {
    let fixture = Fixture::new(vec![listing(vec![]), created()]);
    std::fs::write(fixture.root.join("native.json"), RAW).unwrap();
    let prepared = PreparedCreateCommand::preflight(
        command(TARGET, &["--input", "native.json"], false),
        &NotebookBinding,
        &fixture.root,
        &mut Unreadable,
        true,
    )
    .unwrap();
    let delivery = fixture
        .execute(prepared, &mut Approval::default())
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    fixture.assert_post_body(RAW);
    fixture.assert_requests(&[Method::Get, Method::Post]);
}

#[test]
fn input_limit_accepts_exactly_four_mib_and_reads_at_most_one_extra_byte() {
    let fixture = Fixture::new(vec![]);
    let mut bytes = br#"{"displayName":"Literal.name"}"#.to_vec();
    bytes.resize(MAX_BYTES, b' ');
    assert!(
        PreparedCreateCommand::preflight(
            command(TARGET, &[], true),
            &NotebookBinding,
            &fixture.root,
            &mut Cursor::new(&bytes),
            false,
        )
        .is_ok()
    );
    bytes.extend_from_slice(b"  ");
    let mut input = Cursor::new(&bytes);
    assert!(
        PreparedCreateCommand::preflight(
            command(TARGET, &[], true),
            &NotebookBinding,
            &fixture.root,
            &mut input,
            false,
        )
        .is_err()
    );
    assert_eq!(input.position(), (MAX_BYTES + 1) as u64);
    let input_file = fixture.root.join("too-large.json");
    std::fs::write(&input_file, &bytes).unwrap();
    assert!(
        PreparedCreateCommand::preflight(
            command(TARGET, &["--input", "too-large.json"], false),
            &NotebookBinding,
            &fixture.root,
            &mut Unreadable,
            false,
        )
        .is_err()
    );
    fixture.assert_requests(&[]);
}

#[tokio::test]
async fn oversized_discovery_or_mutation_response_is_bounded_without_replay() {
    let oversized = vec![b' '; MAX_BYTES + 1];
    let fixture = Fixture::new(vec![Reply::bytes(
        Method::Get,
        list_path(),
        200,
        oversized.clone(),
    )]);
    let mut approval = Approval::default();
    assert!(matches!(
        fixture
            .execute(fixture.prepare(&[], Some(RAW)), &mut approval)
            .await,
        Err(CreateCommandError::Discovery)
    ));
    assert!(approval.calls.is_empty());
    fixture.assert_requests(&[Method::Get]);

    let fixture = Fixture::new(vec![
        listing(vec![]),
        Reply::bytes(Method::Post, create_path(), 201, oversized),
    ]);
    let delivery = fixture
        .execute(fixture.prepare(&[], Some(RAW)), &mut Approval::default())
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(
        delivery.report().error().unwrap().kind,
        OperationErrorKind::Limit
    );
    assert!(
        delivery
            .report()
            .error()
            .unwrap()
            .mutation_may_have_occurred
    );
    fixture.assert_requests(&[Method::Get, Method::Post]);
}

#[tokio::test]
async fn incomplete_discovery_at_page_limit_cannot_authorize_create_or_skip() {
    let replies = (0..10)
        .map(|page| {
            Reply::json(
                Method::Get,
                list_path(),
                200,
                json!({"value":[],"continuationToken":format!("page-{}", page + 1)}),
            )
        })
        .collect();
    let fixture = Fixture::new(replies);
    let mut approval = Approval::default();
    assert!(matches!(
        fixture
            .execute(
                fixture.prepare(&["--if-exists", "skip"], Some(RAW)),
                &mut approval,
            )
            .await,
        Err(CreateCommandError::Discovery)
    ));
    assert!(approval.calls.is_empty());
    fixture.assert_requests(&[Method::Get; 10]);
}

#[tokio::test]
async fn operation_request_limit_retains_acceptance_and_stops_without_a_second_post() {
    let mut replies = vec![listing(vec![]), accepted()];
    replies.extend((0..99).map(|_| state("Running")));
    let fixture = Fixture::new(replies);
    let delivery = fixture
        .execute(
            fixture.prepare(&["--wait"], Some(RAW)),
            &mut Approval::default(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    let error = delivery.report().error().unwrap();
    assert_eq!(error.kind, OperationErrorKind::Limit);
    assert_eq!(error.operation_id.as_deref(), Some(OP));
    assert!(error.mutation_may_have_occurred);
    let value = deliver(delivery);
    assert_eq!(
        value["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_no_replay(&value);
    let mut methods = vec![Method::Get, Method::Post];
    methods.extend([Method::Get; 99]);
    fixture.assert_requests(&methods);
    fixture.assert_post_body(RAW);
}

#[tokio::test]
async fn malformed_acceptance_cannot_claim_completion_or_follow_foreign_location() {
    for with_foreign_location in [false, true] {
        let mut reply = Reply::bytes(Method::Post, create_path(), 202, Vec::new());
        if with_foreign_location {
            reply.headers.insert("x-ms-operation-id", OP);
            reply.headers.insert(
                "location",
                format!("https://example.invalid/v1/operations/{OP}"),
            );
        }
        let fixture = Fixture::new(vec![listing(vec![]), reply]);
        let delivery = fixture
            .execute(
                fixture.prepare(&["--wait"], Some(RAW)),
                &mut Approval::default(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert!(matches!(
            delivery.report().error().unwrap().kind,
            OperationErrorKind::InvalidResponse | OperationErrorKind::InvalidLink
        ));
        let value = deliver(delivery);
        assert_eq!(value["meta"]["completed"], false);
        assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
        assert_no_replay(&value);
        fixture.assert_requests(&[Method::Get, Method::Post]);
    }
}

#[tokio::test]
async fn exact_output_file_commits_projected_data_without_consuming_stdout() {
    let fixture = Fixture::new(vec![listing(vec![]), created()]);
    let output = fixture.root.join("receipt.json");
    let delivery = fixture
        .execute(
            fixture.prepare(
                &[
                    "--output",
                    output.to_str().unwrap(),
                    "--project",
                    "displayName",
                ],
                Some(RAW),
            ),
            &mut Approval::default(),
        )
        .await
        .unwrap();
    let meta = delivery.report().value()["meta"].clone();
    let mut stdout = Vec::new();
    assert!(delivery.deliver(&mut stdout).unwrap().is_some());
    assert!(stdout.is_empty());
    let value: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    assert_eq!(value["data"], "Literal.name");
    assert_eq!(value["meta"], meta);
    fixture.assert_requests(&[Method::Get, Method::Post]);
}

#[tokio::test]
async fn bounded_receipt_serialization_failure_retains_complete_remote_success() {
    let mut observed = resource();
    observed["padding"] = json!("");
    let overhead = serde_json::to_vec(&observed).unwrap().len();
    observed["padding"] = json!("x".repeat(MAX_BYTES - overhead));
    let response = serde_json::to_vec(&observed).unwrap();
    assert_eq!(response.len(), MAX_BYTES);
    let fixture = Fixture::new(vec![
        listing(vec![]),
        Reply::bytes(Method::Post, create_path(), 201, response),
    ]);
    let delivery = fixture
        .execute(fixture.prepare(&[], Some(RAW)), &mut Approval::default())
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    let mut stdout = Vec::new();
    let CreateCommandError::Delivery { report, source } =
        delivery.deliver(&mut stdout).unwrap_err()
    else {
        panic!("receipt bound must preserve the remote report");
    };
    assert!(matches!(*source, CreateCommandError::Invalid));
    assert_eq!(report.value()["data"], observed);
    assert_eq!(report.value()["meta"]["completed"], true);
    assert_eq!(report.exit_code(), 0);
    assert!(stdout.is_empty());
    fixture.assert_requests(&[Method::Get, Method::Post]);
}

#[tokio::test]
async fn cancellation_during_202_body_retains_observed_headers_and_original_error() {
    let mut reply = accepted();
    reply.body = ReplyBody::BodyPending;
    let fixture = Fixture::new(vec![listing(vec![]), reply]);
    let delivery = fixture
        .execute_interrupted(
            fixture.prepare(&[], Some(RAW)),
            &mut Approval::default(),
            fixture.wire.pending_started.notified(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    let error = delivery.report().error().unwrap();
    assert_eq!(error.kind, OperationErrorKind::Cancelled);
    assert_eq!(error.operation_id.as_deref(), Some(OP));
    assert!(error.mutation_may_have_occurred);
    let value = deliver(delivery);
    assert_eq!(value["data"], Value::Null);
    assert_eq!(value["meta"]["httpStatus"], 202);
    assert_eq!(
        value["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(value["meta"]["status"], "unknown");
    assert_eq!(value["meta"]["completed"], false);
    assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
    assert_no_replay(&value);
    let trace = fixture.trace.snapshot();
    assert_eq!(
        trace.requests, 1,
        "discovery must not contaminate operation evidence"
    );
    assert_eq!(trace.status, Some(202));
    assert_eq!(trace.operation_id.as_deref(), Some(OP));
    assert_eq!(trace.phase, "response_body");
    fixture.assert_requests(&[Method::Get, Method::Post]);
    fixture.assert_post_body(RAW);
}

#[tokio::test]
async fn oversized_or_failed_202_body_retains_known_handle_without_upgrading_failure() {
    for (body, expected) in [
        (
            ReplyBody::Bytes(vec![b' '; MAX_BYTES + 1]),
            OperationErrorKind::Limit,
        ),
        (
            ReplyBody::BodyFailure,
            OperationErrorKind::Execution(fabric_sdk_operations::OperationExecutionError::Failed),
        ),
    ] {
        let mut reply = accepted();
        reply.body = body;
        let fixture = Fixture::new(vec![listing(vec![]), reply]);
        let delivery = fixture
            .execute(fixture.prepare(&[], Some(RAW)), &mut Approval::default())
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        let error = delivery.report().error().unwrap();
        assert_eq!(error.kind, expected);
        assert_eq!(error.operation_id.as_deref(), Some(OP));
        assert!(error.mutation_may_have_occurred);
        let value = deliver(delivery);
        assert_eq!(value["data"], Value::Null);
        assert_eq!(value["meta"]["httpStatus"], 202);
        assert_eq!(
            value["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(value["meta"]["status"], "unknown");
        assert_eq!(value["meta"]["completed"], false);
        assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
        assert_no_replay(&value);
        let trace = fixture.trace.snapshot();
        assert_eq!(trace.requests, 1);
        assert_eq!(trace.status, Some(202));
        assert_eq!(trace.operation_id.as_deref(), Some(OP));
        fixture.assert_requests(&[Method::Get, Method::Post]);
        fixture.assert_post_body(RAW);
    }
}

#[tokio::test]
async fn single_case_different_existing_name_is_not_an_exact_skip() {
    let mut existing = resource();
    existing["displayName"] = json!("literal.name");
    let fixture = Fixture::new(vec![listing(vec![existing])]);
    let mut approval = Approval::default();
    assert!(matches!(
        fixture
            .execute(
                fixture.prepare(&["--if-exists", "skip"], Some(RAW)),
                &mut approval,
            )
            .await,
        Err(CreateCommandError::Policy(CreateError::NameChanged))
    ));
    assert!(approval.calls.is_empty());
    fixture.assert_requests(&[Method::Get]);
}

#[tokio::test]
async fn alphabetic_workspace_guid_case_is_preserved_in_routes_but_not_identity_comparison() {
    for workspace in [
        "AaBbCcDd-eEfF-1122-3344-aAbBcCdDeEfF",
        "AABBCCDD-EEFF-1122-3344-AABBCCDDEEFF",
    ] {
        let target = format!("/{workspace}.Workspace/Literal.name.Notebook");
        let list_path = format!("/v1/workspaces/{workspace}/items");
        let create_path = format!("/v1/workspaces/{workspace}/notebooks");
        let mut observed = resource();
        observed["workspaceId"] = json!(workspace.to_ascii_lowercase());
        let fixture = Fixture::new(vec![
            Reply::json(Method::Get, list_path.clone(), 200, json!({"value":[]})),
            Reply::json(Method::Post, create_path, 201, observed.clone()),
        ]);
        let mut approval = Approval {
            workspace: Some(workspace.into()),
            ..Approval::default()
        };
        let delivery = fixture
            .execute(
                fixture.prepare_target(&target, &[], Some(RAW)),
                &mut approval,
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 0);
        assert!(delivery.report().error().is_none());
        let value = deliver(delivery);
        assert_eq!(value["data"], observed);
        assert_eq!(value["meta"]["workspaceId"], workspace);
        assert_eq!(
            value["meta"]["observedResource"]["workspaceId"],
            workspace.to_ascii_lowercase()
        );
        assert_eq!(value["meta"]["completed"], true);
        fixture.assert_post_body_at(workspace, RAW);
        fixture.assert_requests(&[Method::Get, Method::Post]);

        for extra in [vec!["--if-exists", "skip"], vec![]] {
            let fixture = Fixture::new(vec![Reply::json(
                Method::Get,
                list_path.clone(),
                200,
                json!({"value":[observed]}),
            )]);
            let mut approval = Approval {
                workspace: Some(workspace.into()),
                ..Approval::default()
            };
            let result = fixture
                .execute(
                    fixture.prepare_target(&target, &extra, Some(RAW)),
                    &mut approval,
                )
                .await;
            if extra.is_empty() {
                assert!(matches!(
                    result,
                    Err(CreateCommandError::Policy(CreateError::AlreadyExists))
                ));
                assert!(approval.calls.is_empty());
            } else {
                let delivery = result.unwrap();
                assert_eq!(delivery.report().exit_code(), 0);
                let value = deliver(delivery);
                assert_eq!(value["meta"]["action"], "skip");
                assert_eq!(value["meta"]["workspaceId"], workspace);
                assert_eq!(value["meta"]["resourceId"], ITEM);
                assert_eq!(value["meta"]["mutationMayHaveOccurred"], false);
                assert_eq!(approval.calls.len(), 1);
                assert!(!approval.calls[0].remote_write);
            }
            fixture.assert_requests(&[Method::Get]);
        }
    }
}

#[tokio::test]
async fn alphabetic_guid_case_does_not_weaken_unknown_workspace_rejection() {
    let workspace = "AaBbCcDd-eEfF-1122-3344-aAbBcCdDeEfF";
    let target = format!("/{workspace}.Workspace/Literal.name.Notebook");
    let list_path = format!("/v1/workspaces/{workspace}/items");
    let mut observed = resource();
    observed["workspaceId"] = json!(OTHER_WS);
    let fixture = Fixture::new(vec![Reply::json(
        Method::Get,
        list_path.clone(),
        200,
        json!({"value":[observed]}),
    )]);
    let mut approval = Approval::default();
    assert!(matches!(
        fixture
            .execute(
                fixture.prepare_target(&target, &["--if-exists", "skip"], Some(RAW)),
                &mut approval,
            )
            .await,
        Err(CreateCommandError::Invalid)
    ));
    assert!(approval.calls.is_empty());
    fixture.assert_requests(&[Method::Get]);

    let fixture = Fixture::new(vec![
        Reply::json(Method::Get, list_path, 200, json!({"value":[]})),
        Reply::json(
            Method::Post,
            format!("/v1/workspaces/{workspace}/notebooks"),
            201,
            observed.clone(),
        ),
    ]);
    let mut approval = Approval {
        workspace: Some(workspace.into()),
        ..Approval::default()
    };
    let delivery = fixture
        .execute(
            fixture.prepare_target(&target, &[], Some(RAW)),
            &mut approval,
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(
        delivery.report().error().unwrap().kind,
        OperationErrorKind::InvalidResponse
    );
    let value = deliver(delivery);
    assert_eq!(value["data"], observed);
    assert_eq!(value["meta"]["observedResource"]["workspaceId"], OTHER_WS);
    assert_eq!(value["meta"]["workspaceId"], workspace);
    assert_eq!(value["meta"]["completed"], false);
    assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
    fixture.assert_requests(&[Method::Get, Method::Post]);
}

#[derive(Clone, Copy)]
enum SyntheticNotebook {
    SkipOnly,
    Replacement,
}

fn replacement_unit() -> ReplacementUnit {
    ReplacementUnit::new("offline.notebook.complete-definition").unwrap()
}

impl CreateInputValidator for SyntheticNotebook {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        notebook_create::NotebookCreateValidator.validate(target, schema, supplied)?;
        if supplied.is_none() {
            return Err(CreateError::InputRejected);
        }
        Ok(match self {
            Self::SkipOnly => InputDisposition::SkipOnly,
            Self::Replacement => InputDisposition::CreateAndReplace(replacement_unit()),
        })
    }
}

impl CreateBinding for SyntheticNotebook {
    fn item_type(&self) -> &'static str {
        "Notebook"
    }

    fn capabilities(&self) -> CreateCapabilities {
        let mut capabilities = notebook_create::capabilities();
        if matches!(self, Self::Replacement) {
            capabilities.replacement = Some(replacement_unit());
            capabilities.duplicate = DuplicateSupport::FreshIdentityWithRepeatedNames;
        }
        capabilities
    }

    fn operation(
        &self,
        _: &CreatePlan,
        _: Option<&[u8]>,
        _: bool,
    ) -> Result<FabricOperation, CreateError> {
        panic!("skip-only or unsupported replacement must never dispatch");
    }
}

#[tokio::test]
async fn initial_202_resource_survives_failed_poll_and_null_data_projection() {
    let mut initial = accepted();
    initial.body = ReplyBody::Bytes(serde_json::to_vec(&resource()).unwrap());
    let service_error = json!({"errorCode":"NotebookCreateFailed","message":"offline failure"});
    let fixture = Fixture::new(vec![
        listing(vec![]),
        initial,
        Reply::json(
            Method::Get,
            operation_path(),
            200,
            json!({"status":"Failed","error":service_error}),
        ),
    ]);
    let delivery = fixture
        .execute(
            fixture.prepare(&["--wait", "--project", "missing"], Some(RAW)),
            &mut Approval::default(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    let error = delivery.report().error().unwrap();
    assert!(matches!(
        &error.kind,
        OperationErrorKind::OperationFailed { status, .. } if status == "Failed"
    ));
    assert_eq!(error.operation_id.as_deref(), Some(OP));
    assert_eq!(error.raw_service_error.as_deref(), Some(&service_error));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(delivery.report().value()["data"], resource());
    let meta = delivery.report().value()["meta"].clone();
    assert_eq!(meta["resourceId"], ITEM);
    assert_eq!(meta["workspaceId"], WS);
    assert_eq!(meta["observedResource"]["id"], ITEM);
    assert_eq!(meta["observedResource"]["workspaceId"], WS);
    assert_eq!(meta["observedResource"]["type"], "Notebook");
    assert_eq!(meta["observedResource"]["displayName"], "Literal.name");
    assert_eq!(meta["operationHandle"], format!("/operations/{OP}"));
    assert_eq!(meta["completed"], false);
    let mut output = Vec::new();
    delivery.deliver(&mut output).unwrap();
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["data"], Value::Null);
    assert_eq!(value["meta"], meta);
    assert_no_replay(&value);
    fixture.assert_requests(&[Method::Get, Method::Post, Method::Get]);
    fixture.assert_post_body(RAW);
}

#[tokio::test]
async fn skip_only_valid_input_preflights_and_skips_existing_without_native_operation() {
    let binding = SyntheticNotebook::SkipOnly;
    let fixture = Fixture::new(vec![listing(vec![resource()])]);
    let prepared = fixture
        .prepare_bound(&binding, &["--if-exists", "skip"])
        .expect("preflight must not fabricate a missing target for valid skip-only input");
    assert!(fixture.wire.requests.lock().unwrap().is_empty());
    assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 0);
    let mut approval = Approval::default();
    let delivery = fixture
        .execute_bound(&binding, prepared, &mut approval)
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    let value = deliver(delivery);
    assert_eq!(value["meta"]["action"], "skip");
    assert_eq!(value["meta"]["resourceId"], ITEM);
    assert_eq!(value["meta"]["mutationMayHaveOccurred"], false);
    assert_eq!(value["meta"]["contentEqualityClaimed"], false);
    assert_eq!(approval.calls.len(), 1);
    assert!(!approval.calls[0].remote_write);
    assert_eq!(approval.calls[0].raw.as_deref(), Some(RAW));
    fixture.assert_requests(&[Method::Get]);
}

#[tokio::test]
async fn skip_only_input_missing_target_fails_after_discovery_before_approval_or_post() {
    let binding = SyntheticNotebook::SkipOnly;
    let fixture = Fixture::new(vec![listing(vec![])]);
    let prepared = fixture
        .prepare_bound(&binding, &["--if-exists", "skip"])
        .unwrap();
    let mut approval = Approval::default();
    assert!(matches!(
        fixture
            .execute_bound(&binding, prepared, &mut approval)
            .await,
        Err(CreateCommandError::Policy(CreateError::InputCannotCreate))
    ));
    assert!(approval.calls.is_empty());
    fixture.assert_requests(&[Method::Get]);
}

#[tokio::test]
async fn replacement_advertisement_cannot_enable_host_overwrite_or_duplicate_dispatch() {
    let binding = SyntheticNotebook::Replacement;
    for existing in [false, true] {
        let fixture = Fixture::new(vec![listing(if existing {
            vec![resource()]
        } else {
            vec![]
        })]);
        for extra in [
            vec!["--if-exists", "overwrite"],
            vec!["--force"],
            vec!["--if-exists", "duplicate"],
        ] {
            assert!(
                matches!(
                    fixture.prepare_bound(&binding, &extra),
                    Err(CreateCommandError::Policy(CreateError::UnsupportedPolicy))
                ),
                "unsupported host policy must fail before discovering existing={existing}"
            );
        }
        for extra in [vec![], vec!["--if-exists", "skip"]] {
            assert!(fixture.prepare_bound(&binding, &extra).is_ok());
        }
        assert!(fixture.wire.requests.lock().unwrap().is_empty());
        assert_eq!(fixture.credential.0.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.wire.replies.lock().unwrap().len(), 1);
    }
    for extra in [vec![], vec!["--if-exists", "skip"]] {
        let fixture = Fixture::new(vec![listing(vec![resource()])]);
        let prepared = fixture.prepare_bound(&binding, &extra).unwrap();
        let mut approval = Approval::default();
        let result = fixture
            .execute_bound(&binding, prepared, &mut approval)
            .await;
        if extra.is_empty() {
            assert!(matches!(
                result,
                Err(CreateCommandError::Policy(CreateError::AlreadyExists))
            ));
            assert!(approval.calls.is_empty());
        } else {
            let delivery = result.unwrap();
            assert_eq!(delivery.report().exit_code(), 0);
            assert_eq!(delivery.report().value()["meta"]["action"], "skip");
            assert_eq!(approval.calls.len(), 1);
            assert!(!approval.calls[0].remote_write);
        }
        fixture.assert_requests(&[Method::Get]);
    }
}
