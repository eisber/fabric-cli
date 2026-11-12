// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use super::*;
use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_sdk::{Audience, CredentialSelection};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_workspaces::mutations::ResourceOutcome;
use std::{
    collections::VecDeque,
    future::ready,
    sync::{Arc, Mutex},
};

const WS: &str = "11111111-1111-4111-8111-111111111111";
const MODEL: &str = "22222222-2222-4222-8222-222222222222";
const CONNECTION: &str = "33333333-3333-4333-8333-333333333333";
const OP: &str = "44444444-4444-4444-8444-444444444444";

fn path() -> String {
    format!("/{WS}.Workspace/{MODEL}.SemanticModel/connections")
}

fn body() -> Value {
    json!({"connectionBinding":{
        "id":CONNECTION,"connectivityType":"ShareableCloud",
        "connectionDetails":{"type":"SQL","path":"server.example.invalid;database"}
    }})
}

fn command(facade: bool, wait: bool) -> LinkCommand {
    LinkCommand {
        path: path(),
        input: "-".into(),
        facade: facade.then_some(LinkOwner::SemanticModel),
        wait,
    }
}

#[derive(Debug, Default)]
struct Credential(Mutex<usize>);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        *self.0.lock().unwrap() += 1;
        Ok(AccessToken::new(
            "synthetic-offline",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Network {
    replies: Mutex<VecDeque<AsyncRawResponse>>,
    requests: Mutex<Vec<(Method, String, Vec<u8>)>>,
    posted: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[async_trait]
impl HttpClient for Network {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push((
            request.method(),
            request.url().to_string(),
            Bytes::from(request.body()).to_vec(),
        ));
        if request.method() == Method::Post
            && let Some(posted) = self.posted.lock().unwrap().take()
        {
            posted.send(()).unwrap();
        }
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP or replay"))
    }
}

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay == TIMEOUT {
            Box::pin(pending())
        } else {
            Box::pin(ready(()))
        }
    }
}

struct Fixture {
    network: Arc<Network>,
    credential: Arc<Credential>,
    http: HttpExecutor,
    context: RequestContext,
}

impl Fixture {
    fn new(replies: Vec<AsyncRawResponse>) -> Self {
        let network = Arc::new(Network {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
            posted: Mutex::new(None),
        });
        let credential = Arc::new(Credential::default());
        let context = RequestContext::new(
            credential.clone(),
            CredentialSelection::new("offline-owner", "offline-tenant", Audience::Fabric).unwrap(),
        );
        let http = HttpExecutor::new(
            network.clone(),
            Arc::new(Timer),
            ExecutionOptions {
                timeout: TIMEOUT,
                max_delay: Duration::from_secs(60),
            },
        )
        .unwrap();
        Self {
            network,
            credential,
            http,
            context,
        }
    }

    async fn prepare(
        &self,
        command: &LinkCommand,
        bytes: Vec<u8>,
    ) -> Result<PreparedLink, MutationError> {
        let client = Client::with_context(self.context.clone());
        v2_link::prepare_command(
            command,
            &mut Resolver {
                client: &client,
                http: &self.http,
            },
            &mut Input {
                selector: command.input.clone(),
                bytes,
            },
            NonZeroUsize::new(INPUT_LIMIT).unwrap(),
        )
        .await
    }
}

fn reply(status: u16, body: &str, headers: &[(&str, &str)]) -> AsyncRawResponse {
    let mut response_headers = Headers::new();
    for (name, value) in headers {
        response_headers.insert((*name).to_owned(), (*value).to_owned());
    }
    AsyncRawResponse::from_bytes(
        StatusCode::from(status),
        response_headers,
        body.as_bytes().to_vec(),
    )
}

#[derive(Default)]
struct Policy {
    allow: bool,
    calls: Vec<Value>,
}

impl Approval for Policy {
    fn approve<'a>(
        &'a mut self,
        context: &'a ApprovalContext,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        assert_eq!(context.target.owner(), LinkOwner::SemanticModel);
        assert_eq!(context.target.scope(), &LinkScope::SemanticModelConnections);
        assert_eq!(
            context.effects,
            LinkEffects {
                remote_write: true,
                query_acceleration: false,
                update_item_sensitivity: false,
            }
        );
        assert_eq!(context.context.selection().account(), "offline-owner");
        assert_eq!(context.context.selection().tenant(), "offline-tenant");
        assert_eq!(context.context.selection().audience(), Audience::Fabric);
        assert_eq!(
            context.context.endpoint(),
            &EndpointBinding::public(ServiceOrigin::Fabric)
        );
        self.calls.push(context.request.clone());
        Box::pin(ready(Ok(self.allow)))
    }
}

#[tokio::test]
async fn root_and_facade_send_exact_native_bind_and_accept_empty_200() {
    for facade in [false, true] {
        let fixture = Fixture::new(vec![reply(200, "", &[])]);
        let prepared = fixture
            .prepare(
                &command(facade, false),
                serde_json::to_vec(&body()).unwrap(),
            )
            .await
            .unwrap();
        let mut policy = Policy {
            allow: true,
            ..Policy::default()
        };
        let receipt = execute(
            prepared,
            fixture.context.clone(),
            &fixture.http,
            &mut policy,
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(
            receipt.outcome,
            ResourceOutcome::Completed {
                status: 200,
                data: None
            }
        );
        assert_eq!(receipt.metadata()["status"], "succeeded");
        assert_eq!(policy.calls, vec![body()]);
        let requests = fixture.network.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, Method::Post);
        assert_eq!(
            requests[0].1,
            format!(
                "https://api.fabric.microsoft.com/v1/workspaces/{WS}/semanticModels/{MODEL}/bindConnection"
            )
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[0].2).unwrap(),
            body()
        );
    }
}

#[tokio::test]
async fn preflight_rejects_bad_owner_scope_json_and_native_input_without_http_or_approval() {
    let fixture = Fixture::new(vec![]);
    let mut inputs = vec![
        br#"{"connectionBinding":{},"connectionBinding":{}}"#.to_vec(),
        br#"{"connectionBinding":{"connectionDetails":{"path":"a","path":"b"}}}"#.to_vec(),
        b"{}{}".to_vec(),
        b"[]".to_vec(),
        vec![0xff],
        vec![b' '; INPUT_LIMIT + 1],
    ];
    for pointer in [
        "/",
        "/connectionBinding",
        "/connectionBinding/connectionDetails",
    ] {
        let mut value = body();
        let object = if pointer == "/" {
            &mut value
        } else {
            value.pointer_mut(pointer).unwrap()
        };
        object["unknown"] = json!(true);
        inputs.push(serde_json::to_vec(&value).unwrap());
    }
    for field in ["id", "connectivityType", "connectionDetails"] {
        let mut value = body();
        value["connectionBinding"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        inputs.push(serde_json::to_vec(&value).unwrap());
    }
    for bytes in inputs {
        assert!(
            fixture
                .prepare(&command(false, false), bytes)
                .await
                .is_err()
        );
    }
    for invalid in [
        path().replace("SemanticModel", "Lakehouse"),
        path().replace("/connections", ""),
        format!("{}/child", path()),
        path().replace("connections", "Files"),
        format!("/workspaces/{WS}/items/{MODEL}/connections"),
    ] {
        let mut command = command(false, false);
        command.path = invalid;
        assert!(
            fixture
                .prepare(&command, serde_json::to_vec(&body()).unwrap())
                .await
                .is_err()
        );
    }
    assert!(fixture.network.requests.lock().unwrap().is_empty());
    assert_eq!(*fixture.credential.0.lock().unwrap(), 0);
}

#[tokio::test]
async fn denied_approval_sends_nothing_and_service_denial_retains_native_error() {
    let fixture = Fixture::new(vec![reply(
        403,
        r#"{"errorCode":"Denied","message":"synthetic denial"}"#,
        &[],
    )]);
    let prepared = fixture
        .prepare(&command(true, false), serde_json::to_vec(&body()).unwrap())
        .await
        .unwrap();
    let failure = execute(
        prepared.clone(),
        fixture.context.clone(),
        &fixture.http,
        &mut Policy::default(),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.error.code, "ApprovalRequired");
    assert!(fixture.network.requests.lock().unwrap().is_empty());
    assert_eq!(*fixture.credential.0.lock().unwrap(), 0);
    let failure = execute(
        prepared,
        fixture.context.clone(),
        &fixture.http,
        &mut Policy {
            allow: true,
            ..Policy::default()
        },
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.error.operation.as_ref().unwrap().status, Some(403));
    let evidence = execution_failure(&failure).2;
    assert_eq!(evidence["httpStatus"], 403);
    assert!(evidence["serviceError"].is_object());
    assert_eq!(fixture.network.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn acceptance_is_not_completion_and_wait_observes_without_result_or_replay() {
    for wait in [false, true] {
        let accepted = reply(
            202,
            "",
            &[
                ("x-ms-operation-id", OP),
                ("retry-after", "0"),
                (
                    "location",
                    &format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
                ),
            ],
        );
        let mut replies = vec![accepted];
        if wait {
            replies.push(reply(200, r#"{"status":"Succeeded"}"#, &[]));
        }
        let fixture = Fixture::new(replies);
        let prepared = fixture
            .prepare(&command(true, wait), serde_json::to_vec(&body()).unwrap())
            .await
            .unwrap();
        let receipt = execute(
            prepared,
            fixture.context.clone(),
            &fixture.http,
            &mut Policy {
                allow: true,
                ..Policy::default()
            },
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(
            receipt.metadata()["status"],
            if wait { "succeeded" } else { "accepted" }
        );
        let requests = fixture.network.requests.lock().unwrap();
        assert_eq!(requests.len(), if wait { 2 } else { 1 });
        if wait {
            assert_eq!(requests[1].0, Method::Get);
            assert_eq!(
                requests[1].1,
                format!("https://api.fabric.microsoft.com/v1/operations/{OP}")
            );
        }
    }
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("synthetic writer failure"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("synthetic flush failure"))
    }
}

#[tokio::test]
async fn output_failure_retains_original_receipt_and_never_resubmits() {
    let fixture = Fixture::new(vec![reply(200, "", &[])]);
    let prepared = fixture
        .prepare(&command(true, false), serde_json::to_vec(&body()).unwrap())
        .await
        .unwrap();
    let receipt = execute(
        prepared,
        fixture.context.clone(),
        &fixture.http,
        &mut Policy {
            allow: true,
            ..Policy::default()
        },
        pending(),
    )
    .await
    .unwrap();
    let failure = deliver(&receipt, &mut BrokenWriter).unwrap_err();
    assert_eq!(failure.0, "OutputFailed");
    assert_eq!(failure.2, receipt.metadata());
    assert_eq!(
        receipt.outcome,
        ResourceOutcome::Completed {
            status: 200,
            data: None
        }
    );
    assert_eq!(fixture.network.requests.lock().unwrap().len(), 1);
}

struct PendingApproval;
impl Approval for PendingApproval {
    fn approve<'a>(
        &'a mut self,
        _: &'a ApprovalContext,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        Box::pin(pending())
    }
}

#[tokio::test]
async fn interrupted_approval_never_hangs_or_sends() {
    let fixture = Fixture::new(vec![]);
    let prepared = fixture
        .prepare(&command(true, false), serde_json::to_vec(&body()).unwrap())
        .await
        .unwrap();
    let failure = execute(
        prepared,
        fixture.context.clone(),
        &fixture.http,
        &mut PendingApproval,
        ready(()),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.error.code, "Cancelled");
    assert!(fixture.network.requests.lock().unwrap().is_empty());
    assert!(failure.error.operation.is_none());
}

#[tokio::test]
async fn cancellation_after_acceptance_retains_operation_and_uncertainty_without_replay() {
    let fixture = Fixture::new(vec![reply(
        202,
        "",
        &[
            ("x-ms-operation-id", OP),
            ("retry-after", "0"),
            (
                "location",
                &format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
            ),
        ],
    )]);
    let (posted, received) = tokio::sync::oneshot::channel();
    *fixture.network.posted.lock().unwrap() = Some(posted);
    let prepared = fixture
        .prepare(&command(true, true), serde_json::to_vec(&body()).unwrap())
        .await
        .unwrap();
    let failure = execute(
        prepared,
        fixture.context.clone(),
        &fixture.http,
        &mut Policy {
            allow: true,
            ..Policy::default()
        },
        async {
            received.await.unwrap();
        },
    )
    .await
    .unwrap_err();
    let native = failure.error.operation.as_ref().unwrap();
    assert_eq!(native.operation_id.as_deref(), Some(OP));
    assert!(native.mutation_may_have_occurred);
    assert_eq!(fixture.network.requests.lock().unwrap().len(), 1);
    assert_eq!(execution_failure(&failure).2["remoteReplayed"], false);
}

#[tokio::test]
async fn named_target_is_uniquely_resolved_once_and_pinned_for_approval_and_bind() {
    let workspaces = json!({"value":[{"id":WS,"displayName":"Team","type":"Workspace"}]});
    let models = json!({"value":[{"id":MODEL,"displayName":"Sales","type":"SemanticModel"}]});
    let fixture = Fixture::new(vec![
        reply(200, &workspaces.to_string(), &[]),
        reply(200, &models.to_string(), &[]),
        reply(200, "", &[]),
    ]);
    let mut command = command(true, false);
    command.path = "/Team.Workspace/Sales.SemanticModel/connections".into();
    let prepared = fixture
        .prepare(&command, serde_json::to_vec(&body()).unwrap())
        .await
        .unwrap();
    assert_eq!(prepared.target().workspace(), WS);
    assert_eq!(prepared.target().item(), MODEL);
    let receipt = execute(
        prepared,
        fixture.context.clone(),
        &fixture.http,
        &mut Policy {
            allow: true,
            ..Policy::default()
        },
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(receipt.target.item(), MODEL);
    let requests = fixture.network.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.0 == Method::Post)
            .count(),
        1
    );
    assert_eq!(requests.len(), 3);
}

#[tokio::test]
async fn ambiguous_or_denied_name_resolution_never_submits_a_bind() {
    let ambiguous = json!({"value":[
        {"id":WS,"displayName":"Team","type":"Workspace"},
        {"id":MODEL,"displayName":"Team","type":"Workspace"}
    ]});
    for response in [
        reply(200, &ambiguous.to_string(), &[]),
        reply(403, r#"{"errorCode":"Denied","message":"synthetic"}"#, &[]),
    ] {
        let fixture = Fixture::new(vec![response]);
        let mut command = command(true, false);
        command.path = "/Team.Workspace/Sales.SemanticModel/connections".into();
        assert!(
            fixture
                .prepare(&command, serde_json::to_vec(&body()).unwrap())
                .await
                .is_err()
        );
        assert!(
            fixture
                .network
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.0 == Method::Get)
        );
    }
}

#[test]
fn bounded_stdin_and_regular_file_capture_exact_bytes() {
    let command = command(true, false);
    let bytes = serde_json::to_vec(&body()).unwrap();
    let input = Input::read(&command, &mut bytes.as_slice()).unwrap();
    assert_eq!(input.bytes, bytes);
    assert!(Input::read(&command, &mut vec![b' '; INPUT_LIMIT + 1].as_slice()).is_err());
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("input.json");
    std::fs::write(&file, &bytes).unwrap();
    let mut command = command;
    command.input = file.to_str().unwrap().into();
    assert_eq!(
        Input::read(&command, &mut io::empty()).unwrap().bytes,
        bytes
    );
}
