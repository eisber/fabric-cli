// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Independent request-to-CLI acceptance cases using only injected offline I/O.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, Body, HttpClient, Request, StatusCode, Url, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::{
    delete::{
        DeleteDispatch, DeleteOutputFiles, PreparedDelete, dispatch, preflight, prepare_output,
        render, write_dispatch,
    },
    parser::{DeleteCommand, Invocation, parse_args},
};
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    items::delete::{DeleteApproval, OperationStatus},
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_operations::{OperationLimits, OperationSession};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending, poll_fn},
    io::{self, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
    time::Duration,
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";
const TARGET: &str =
    "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook";
const HOST: &str = "dailyapi.fabric.microsoft.com";
const ROOT: &str = "https://dailyapi.fabric.microsoft.com";
const TOKEN: &str = "synthetic-delete-acceptance-only";
const PRIVATE: &str = "private-provider-delete-diagnostic";
const HTTP_TIMEOUT: Duration = Duration::from_secs(240);

#[derive(Debug, Default)]
struct Credential(Mutex<Vec<Vec<String>>>);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0
            .lock()
            .unwrap()
            .push(scopes.iter().map(|scope| (*scope).to_owned()).collect());
        Ok(AccessToken::new(TOKEN, OffsetDateTime::UNIX_EPOCH))
    }
}

#[derive(Debug)]
struct WireRequest {
    method: String,
    url: Url,
    body: Vec<u8>,
}

#[derive(Debug)]
struct Reply {
    status: u16,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

fn reply(status: u16, body: Value) -> Reply {
    Reply {
        status,
        body: if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&body).unwrap()
        },
        headers: Vec::new(),
    }
}

fn accepted() -> Reply {
    Reply {
        headers: vec![
            ("x-ms-operation-id".into(), OPERATION.into()),
            (
                "Location".into(),
                format!("{ROOT}/v1/operations/{OPERATION}"),
            ),
            ("Retry-After".into(), "1".into()),
        ],
        ..reply(202, Value::Null)
    }
}

#[derive(Debug)]
struct Wire {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<WireRequest>>,
    after_acceptance: Arc<AtomicBool>,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(
            request.headers().get_str(&"authorization".into()).unwrap(),
            format!("Bearer {TOKEN}")
        );
        assert!(request.headers().get_str(&"if-match".into()).is_err());
        let Body::Bytes(body) = request.body() else {
            panic!("delete must not stream a request body")
        };
        self.requests.lock().unwrap().push(WireRequest {
            method: request.method().to_string(),
            url: request.url().clone(),
            body: body.to_vec(),
        });
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request, lookup, fallback, or mutation replay");
        let mut headers = Headers::new();
        for (name, value) in reply.headers {
            headers.insert(name, value);
        }
        if reply.status == 202 {
            self.after_acceptance.store(true, Ordering::SeqCst);
        }
        Ok(AsyncRawResponse::from_bytes(
            StatusCode::from(reply.status),
            headers,
            reply.body,
        ))
    }
}

struct Clock {
    expired: Arc<AtomicBool>,
}

impl ExecutionTimer for Clock {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay == HTTP_TIMEOUT {
            Box::pin(poll_fn(|_| {
                if self.expired.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }))
        } else {
            Box::pin(std::future::ready(()))
        }
    }
}

struct Harness {
    credential: Arc<Credential>,
    context: RequestContext,
    wire: Arc<Wire>,
    http: HttpExecutor,
}

impl Harness {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self::with_expiry(replies, Arc::new(AtomicBool::new(false)), false)
    }

    fn with_expiry(
        replies: impl IntoIterator<Item = Reply>,
        expired: Arc<AtomicBool>,
        expire_after_acceptance: bool,
    ) -> Self {
        let credential = Arc::new(Credential::default());
        let endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, HOST).unwrap();
        let context = RequestContext::with_endpoint(
            credential.clone(),
            CredentialSelection::new(
                "offline@example.invalid",
                "offline-tenant",
                Audience::Fabric,
            )
            .unwrap(),
            endpoint.clone(),
        );
        let wire = Arc::new(Wire {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::default(),
            after_acceptance: if expire_after_acceptance {
                expired.clone()
            } else {
                Arc::new(AtomicBool::new(false))
            },
        });
        let http = HttpExecutor::new(
            wire.clone(),
            Arc::new(Clock { expired }),
            ExecutionOptions::default(),
        )
        .unwrap()
        .with_endpoint(endpoint);
        Self {
            credential,
            context,
            wire,
            http,
        }
    }

    fn prepared(&self, options: &[&str]) -> PreparedDelete {
        preflight(&command(options), None)
            .unwrap()
            .with_selection(self.context.selection(), self.context.endpoint())
            .unwrap()
    }

    fn assert_wire(&self, polls: usize) {
        let requests = self.wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1 + polls);
        let delete = &requests[0];
        assert_eq!(delete.method, "DELETE");
        assert_eq!(delete.url.scheme(), "https");
        assert_eq!(delete.url.host_str(), Some(HOST));
        assert_eq!(
            delete.url.path(),
            format!("/v1/workspaces/{WORKSPACE}/notebooks/{ITEM}")
        );
        assert_eq!(delete.url.query(), Some("hardDelete=true"));
        assert!(delete.body.is_empty());
        assert!(delete.url.fragment().is_none());
        for poll in &requests[1..] {
            assert_eq!(poll.method, "GET");
            assert_eq!(
                poll.url.as_str(),
                format!("{ROOT}/v1/operations/{OPERATION}")
            );
            assert!(poll.body.is_empty());
        }
        assert_eq!(
            *self.credential.0.lock().unwrap(),
            vec![vec![Audience::Fabric.scope().to_owned()]; requests.len()]
        );
        assert!(self.wire.replies.lock().unwrap().is_empty());
    }

    fn assert_untouched(&self) {
        assert!(self.wire.requests.lock().unwrap().is_empty());
        assert!(self.credential.0.lock().unwrap().is_empty());
    }
}

struct StdoutOnly;

impl DeleteOutputFiles for StdoutOnly {
    fn reserve_new(&mut self, _: &Path) -> io::Result<Box<dyn Write + Send>> {
        panic!("stdout delete must not reserve any local path")
    }
}

struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn command(options: &[&str]) -> DeleteCommand {
    let Invocation::Delete(command) = parse_args(
        ["fab-rs", "delete", TARGET]
            .into_iter()
            .chain(options.iter().copied()),
    )
    .unwrap() else {
        panic!("expected the v2 delete leaf")
    };
    command
}

fn snapshot(dispatch: &DeleteDispatch) -> Value {
    serde_json::from_slice(&render(dispatch).unwrap()).unwrap()
}

fn assert_metadata(snapshot: &Value, prepared: &PreparedDelete) {
    let metadata = serde_json::to_value(prepared.assessment().metadata()).unwrap();
    assert_eq!(snapshot["schemaVersion"], "0.1");
    assert_eq!(
        snapshot["meta"]["target"],
        json!({
            "path": TARGET,
            "id": ITEM,
            "type": "Notebook",
            "scope": {"workspaceId": WORKSPACE}
        })
    );
    assert_eq!(snapshot["meta"]["operation"], metadata);
    assert_eq!(
        snapshot["meta"]["assessment"],
        serde_json::to_value(prepared.assessment()).unwrap()
    );
    assert_eq!(
        snapshot["meta"]["context"],
        json!({
            "account": "offline@example.invalid",
            "tenant": "offline-tenant",
            "endpoint": format!("{ROOT}/")
        })
    );
    assert_eq!(snapshot["meta"]["context"], metadata["context"]);
    assert_eq!(metadata["target"]["workspace_id"], WORKSPACE);
    assert_eq!(metadata["target"]["item_id"], ITEM);
    assert_eq!(metadata["target"]["resource_type"], "notebook");
    assert_eq!(metadata["deletion_mode"], "permanent");
    assert_eq!(metadata["traversal"], "exact_target");
    assert_eq!(metadata["precondition"], "unconditional");
    assert_eq!(
        metadata["descriptor"]["target_type_enforcement"],
        "service_endpoint"
    );
    assert_eq!(
        metadata["descriptor"]["observation_support"],
        "terminal_response_only"
    );
    assert_eq!(metadata["descriptor"]["required_workspace_role"], "admin");
    assert_eq!(
        snapshot["meta"]["effects"]["declared"],
        json!([
            {"id": "notebook-lifecycle", "kind": "notebook_lifecycle"},
            {"id": "notebook-content", "kind": "notebook_content"}
        ])
    );
    assert!(snapshot["meta"]["effects"]["observed"].is_null());
    assert!(snapshot["meta"]["coverage"]["complete"].is_null());
    assert!(!snapshot.to_string().contains(TOKEN));
    assert!(!snapshot.to_string().contains(PRIVATE));
}

fn assert_uncertain(snapshot: &Value, diagnostic: &str) {
    assert_eq!(snapshot["status"], "unknown");
    assert!(snapshot["data"].is_null());
    assert_eq!(snapshot["error"]["code"], diagnostic);
    assert_eq!(snapshot["meta"]["evidence"]["status"], "unknown");
    assert_eq!(snapshot["meta"]["evidence"]["completion"], "unknown");
    assert_eq!(snapshot["meta"]["evidence"]["operation_id"], OPERATION);
    assert_eq!(
        snapshot["meta"]["evidence"]["mutation_may_have_occurred"],
        true
    );
    assert_eq!(snapshot["meta"]["resourceError"]["operationId"], OPERATION);
    assert_eq!(
        snapshot["meta"]["resourceError"]["mutationMayHaveOccurred"],
        true
    );
    for effect in snapshot["meta"]["evidence"]["effects"].as_array().unwrap() {
        assert_eq!(effect["status"], "unknown");
    }
}

fn cancellation(flag: Arc<AtomicBool>) -> impl Future<Output = ()> + Send {
    poll_fn(move |_| {
        if flag.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
}

#[tokio::test]
async fn terminal_http_receipt_reaches_cli_with_scalar_projection_and_no_replay() {
    let host = Harness::new([reply(200, Value::Null)]);
    let prepared = host.prepared(&["--permanent", "-q", "resource.id"]);
    let approval = DeleteApproval::approve(prepared.assessment());
    let mut operations = OperationSession::with_context(
        &host.context,
        host.http.clone(),
        OperationLimits::default(),
        pending(),
    );
    let output = prepare_output(&prepared, &mut StdoutOnly).unwrap();
    let dispatched = dispatch(&mut operations, &prepared, Some(&approval), output)
        .await
        .unwrap();
    let receipt = dispatched.result.as_ref().unwrap();
    assert_eq!(receipt.status(), OperationStatus::Succeeded);
    assert_eq!(receipt.metadata(), prepared.assessment().metadata());
    assert_eq!(dispatched.exit_code(), 0);
    let before_projection = serde_json::to_value(dispatched.envelope()).unwrap();
    assert!(before_projection["data"]["state"].is_null());
    assert!(before_projection["data"]["changes"].is_null());
    assert_eq!(before_projection["data"]["action"], "delete");
    let projected = snapshot(&dispatched);
    assert_eq!(projected["status"], "succeeded");
    assert_eq!(projected["data"], ITEM);
    assert!(projected["error"].is_null());
    assert_eq!(projected["meta"], before_projection["meta"]);
    assert_metadata(&projected, &prepared);
    assert_eq!(
        projected["meta"]["receipt"]["metadata"],
        projected["meta"]["operation"]
    );
    assert_eq!(projected["meta"]["evidence"]["http_status"], 200);
    assert_eq!(projected["meta"]["evidence"]["completion"], "completed");
    assert_eq!(
        projected["meta"]["evidence"]["dispatch"],
        "response_observed"
    );
    assert!(projected["meta"]["evidence"]["operation_id"].is_null());
    assert_eq!(
        projected["meta"]["evidence"]["effects"],
        json!([
            {"effect_id": "notebook-lifecycle", "status": "succeeded"},
            {"effect_id": "notebook-content", "status": "succeeded"}
        ])
    );
    let mut bytes = Vec::new();
    write_dispatch(dispatched, &mut bytes).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), projected);
    assert!(bytes.ends_with(b"\n"));
    let output = prepare_output(&prepared, &mut StdoutOnly).unwrap();
    let repeated = dispatch(&mut operations, &prepared, Some(&approval), output)
        .await
        .unwrap();
    assert_eq!(repeated.result.unwrap_err().code(), "AlreadyExecuted");
    host.assert_wire(0);
}

#[tokio::test]
async fn unsupported_modes_and_absent_or_stale_approval_do_no_http_or_credential_work() {
    let host = Harness::new([]);
    for options in [
        vec![],
        vec!["--permanent", "--recursive"],
        vec!["--permanent", "--if-match", "private-etag"],
    ] {
        let error = preflight(&command(&options), None).unwrap_err();
        assert_eq!(error.code(), "UnsupportedCapability");
        let envelope = serde_json::to_value(error.envelope(None)).unwrap();
        assert_eq!(envelope["meta"]["evidence"]["dispatch"], "not_dispatched");
        assert_eq!(
            envelope["meta"]["evidence"]["mutation_may_have_occurred"],
            false
        );
        host.assert_untouched();
    }
    for stale in [false, true] {
        let unbound = preflight(&command(&["--permanent"]), None).unwrap();
        let old_approval = DeleteApproval::approve(unbound.assessment());
        let prepared = unbound
            .with_selection(host.context.selection(), host.context.endpoint())
            .unwrap();
        let mut operations = OperationSession::with_context(
            &host.context,
            host.http.clone(),
            OperationLimits::default(),
            pending(),
        );
        let output = prepare_output(&prepared, &mut StdoutOnly).unwrap();
        let result = dispatch(
            &mut operations,
            &prepared,
            stale.then_some(&old_approval),
            output,
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code(), 1);
        assert_eq!(
            result.result.as_ref().unwrap_err().code(),
            if stale {
                "ApprovalMismatch"
            } else {
                "ApprovalRequired"
            }
        );
        let envelope = snapshot(&result);
        assert_metadata(&envelope, &prepared);
        assert_eq!(envelope["status"], "failed");
        assert_eq!(envelope["meta"]["evidence"]["dispatch"], "not_dispatched");
        host.assert_untouched();
    }
}

#[tokio::test]
async fn defensive_http_acceptance_is_not_completion_even_when_output_fails() {
    let host = Harness::new([accepted()]);
    let prepared = host.prepared(&["--permanent"]);
    let approval = DeleteApproval::approve(prepared.assessment());
    let output = prepare_output(&prepared, &mut StdoutOnly).unwrap();
    let mut operations = OperationSession::with_context(
        &host.context,
        host.http.clone(),
        OperationLimits::default(),
        pending(),
    );
    let result = dispatch(&mut operations, &prepared, Some(&approval), output)
        .await
        .unwrap();
    assert_eq!(result.exit_code(), 0);
    let envelope = snapshot(&result);
    assert_metadata(&envelope, &prepared);
    assert_eq!(envelope["status"], "accepted");
    assert!(envelope["data"]["state"].is_null());
    assert!(envelope["data"]["changes"].is_null());
    assert_eq!(envelope["meta"]["evidence"]["status"], "accepted");
    assert_eq!(envelope["meta"]["evidence"]["completion"], "unknown");
    assert_eq!(envelope["meta"]["evidence"]["operation_id"], OPERATION);
    assert_eq!(envelope["meta"]["evidence"]["http_status"], 202);
    let failure = write_dispatch(result, &mut BrokenWriter).unwrap_err();
    assert_eq!(failure.exit_code(), 1);
    assert_eq!(
        failure.dispatch.result.as_ref().unwrap().status(),
        OperationStatus::Accepted
    );
    assert_eq!(snapshot(&failure.dispatch), envelope);
    assert_eq!(
        serde_json::to_value(failure.envelope()).unwrap()["meta"]["dispatch"],
        envelope
    );
    host.assert_wire(0);
}

#[tokio::test]
async fn forbidden_poll_after_acceptance_is_unknown_not_a_denied_delete_and_is_sanitized() {
    for after_acceptance in [false, true] {
        let forbidden = reply(403, json!({"errorCode": "Forbidden", "message": PRIVATE}));
        let host = Harness::new(if after_acceptance {
            vec![accepted(), forbidden]
        } else {
            vec![forbidden]
        });
        let prepared = host.prepared(&["--permanent", "--wait"]);
        let approval = DeleteApproval::approve(prepared.assessment());
        let output = prepare_output(&prepared, &mut StdoutOnly).unwrap();
        let mut operations = OperationSession::with_context(
            &host.context,
            host.http.clone(),
            OperationLimits::default(),
            pending(),
        );
        let result = dispatch(&mut operations, &prepared, Some(&approval), output)
            .await
            .unwrap();
        assert_eq!(result.exit_code(), 1);
        let error = result.result.as_ref().unwrap_err();
        let original = error.resource_error().unwrap();
        assert_eq!(original.status, Some(403));
        assert!(
            original
                .raw_service_error
                .as_ref()
                .unwrap()
                .to_string()
                .contains(PRIVATE)
        );
        let envelope = snapshot(&result);
        assert_metadata(&envelope, &prepared);
        if after_acceptance {
            assert_uncertain(&envelope, "AccessDenied");
            let message = envelope["error"]["message"].as_str().unwrap();
            assert!(message.contains("observation"));
            assert!(message.contains("deletion outcome is unknown"));
            assert!(!message.contains("deletion was denied"));
        } else {
            assert_eq!(envelope["status"], "failed");
            assert_eq!(envelope["error"]["code"], "AccessDenied");
            assert!(envelope["meta"]["evidence"]["operation_id"].is_null());
        }
        host.assert_wire(usize::from(after_acceptance));
    }
}

#[tokio::test]
async fn injected_cancellation_and_deadlines_preserve_acceptance_without_second_delete() {
    for timeout in [false, true] {
        let expired = Arc::new(AtomicBool::new(false));
        let host = Harness::with_expiry([accepted()], expired, timeout);
        let prepared = host.prepared(&["--permanent", "--wait"]);
        let approval = DeleteApproval::approve(prepared.assessment());
        let cancelled = host.wire.after_acceptance.clone();
        let mut operations = OperationSession::with_context(
            &host.context,
            host.http.clone(),
            OperationLimits::default(),
            async move {
                if timeout {
                    pending::<()>().await;
                } else {
                    cancellation(cancelled).await;
                }
            },
        );
        let output = prepare_output(&prepared, &mut StdoutOnly).unwrap();
        let result = dispatch(&mut operations, &prepared, Some(&approval), output)
            .await
            .unwrap();
        assert_eq!(result.exit_code(), 1);
        let envelope = snapshot(&result);
        assert_metadata(&envelope, &prepared);
        assert_uncertain(&envelope, if timeout { "Timeout" } else { "Cancelled" });
        assert_eq!(operations.operation_id(), Some(OPERATION));
        host.assert_wire(0);
    }
    let host = Harness::with_expiry([], Arc::new(AtomicBool::new(true)), false);
    let prepared = host.prepared(&["--permanent"]);
    let approval = DeleteApproval::approve(prepared.assessment());
    let mut operations = OperationSession::with_context(
        &host.context,
        host.http.clone(),
        OperationLimits::default(),
        pending(),
    );
    let output = prepare_output(&prepared, &mut StdoutOnly).unwrap();
    let result = dispatch(&mut operations, &prepared, Some(&approval), output)
        .await
        .unwrap();
    assert_eq!(result.result.as_ref().unwrap_err().code(), "Timeout");
    assert!(host.wire.requests.lock().unwrap().is_empty());
    assert!(operations.operation_id().is_none());
    assert!(snapshot(&result)["meta"]["evidence"]["operation_id"].is_null());
}
