// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline native HTTP and delivery contracts for the bounded DataAgent host.
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::v2_link_dataagent_host::{self as host, NativeFiles};
use fabric_cli_mutations::{
    CommandFuture, MutationError,
    v2_link::{LinkApproval, LinkEffects, PreparedLink, ResolvedLinkTarget},
};
use fabric_sdk::{Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_operations::{OperationLimits, OperationSession};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{pending, poll_fn},
    io::{self, Cursor, Write},
    sync::{Arc, Mutex},
    task::{Poll, Waker},
    time::Duration,
};

const WS: &str = "11111111-1111-4111-8111-111111111111";
const AGENT: &str = "22222222-2222-4222-8222-222222222222";
const SOURCE: &str = "33333333-3333-4333-8333-333333333333";
const DS: &str = "44444444-4444-4444-8444-444444444444";
const OP: &str = "55555555-5555-4555-8555-555555555555";
const TARGET: &str = "/11111111-1111-4111-8111-111111111111.Workspace/22222222-2222-4222-8222-222222222222.DataAgent/settings/staging/datasources";

#[test]
fn root_dispatch_claims_only_its_own_typed_owner() {
    let args = ["link", TARGET, "--input", "-"].map(Into::into);
    assert!(host::handles(&args));
    for kind in [
        "Lakehouse",
        "KQLDatabase",
        "SemanticModel",
        "Ontology",
        "Notebook",
    ] {
        let target = TARGET.replace("DataAgent", kind);
        let args = ["link", &target, "--input", "-"].map(Into::into);
        assert!(!host::handles(&args));
    }
    assert!(host::handles(&["link", "--help"].map(Into::into)));
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
            "synthetic-offline-dataagent",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct RequestEvidence {
    method: Method,
    url: String,
    body: Vec<u8>,
}

#[derive(Debug, Default)]
struct Wire {
    replies: Mutex<VecDeque<(StatusCode, Headers, Vec<u8>)>>,
    requests: Mutex<Vec<RequestEvidence>>,
    cancellation_waker: Mutex<Option<Waker>>,
    fail_at: Option<usize>,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(RequestEvidence {
            method: request.method(),
            url: request.url().to_string(),
            body: Bytes::from(request.body()).to_vec(),
        });
        if let Some(waker) = self.cancellation_waker.lock().unwrap().take() {
            waker.wake();
        }
        if self.fail_at == Some(self.requests.lock().unwrap().len()) {
            return Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Other,
                "Synthetic lost DataAgent mutation response",
            ));
        }
        let (status, headers, body) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected request, fallback or POST replay");
        Ok(AsyncRawResponse::from_bytes(status, headers, body))
    }
}

#[derive(Debug)]
struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay >= Duration::from_secs(30) {
            Box::pin(pending())
        } else {
            Box::pin(async {})
        }
    }
}

struct Approval {
    expected: PreparedLink,
    allowed: bool,
    calls: usize,
}

impl LinkApproval for Approval {
    fn approve<'a>(
        &'a mut self,
        target: &'a ResolvedLinkTarget,
        effects: LinkEffects,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        assert_eq!(target, self.expected.target());
        assert_eq!(effects, self.expected.effects());
        assert!(effects.remote_write);
        assert!(!effects.query_acceleration);
        assert!(!effects.update_item_sensitivity);
        self.calls += 1;
        Box::pin(async move { Ok(self.allowed) })
    }
}

fn input(kind: &str) -> Value {
    let key = if kind == "LakehouseTables" {
        "lakehouseReference"
    } else {
        "itemReference"
    };
    json!({"type":kind,(key):{"referenceType":"ById","workspaceId":WS,"itemId":SOURCE}})
}

async fn prepared(kind: &str, wait: bool) -> PreparedLink {
    prepared_input(&input(kind), wait).await
}

async fn prepared_input(input: &Value, wait: bool) -> PreparedLink {
    let mut args = vec!["link", TARGET, "--input", "-"];
    if wait {
        args.push("--wait");
    }
    let command = host::parse(args.into_iter().map(Into::into).collect()).unwrap();
    host::prepare(
        &command,
        &mut NativeFiles::new(&mut Cursor::new(serde_json::to_vec(input).unwrap())),
    )
    .await
    .unwrap()
}

fn response(status: StatusCode, value: Value) -> (StatusCode, Headers, Vec<u8>) {
    (status, Headers::new(), serde_json::to_vec(&value).unwrap())
}

fn metadata(id: &str, kind: &str) -> (StatusCode, Headers, Vec<u8>) {
    response(
        StatusCode::Ok,
        json!({"id":id,"type":kind,"workspaceId":WS}),
    )
}

async fn run(
    prepared: PreparedLink,
    replies: Vec<(StatusCode, Headers, Vec<u8>)>,
    approved: Option<bool>,
) -> (host::Dispatch, Arc<Wire>, Arc<Credential>, usize) {
    run_scenario(prepared, replies, approved, None, None).await
}

async fn run_scenario(
    prepared: PreparedLink,
    replies: Vec<(StatusCode, Headers, Vec<u8>)>,
    approved: Option<bool>,
    cancel_after: Option<usize>,
    fail_at: Option<usize>,
) -> (host::Dispatch, Arc<Wire>, Arc<Credential>, usize) {
    let wire = Arc::new(Wire {
        replies: Mutex::new(replies.into()),
        fail_at,
        ..Wire::default()
    });
    let credential = Arc::new(Credential::default());
    let endpoint = EndpointBinding::public(ServiceOrigin::Fabric);
    let context = RequestContext::with_endpoint(
        credential.clone(),
        CredentialSelection::new("synthetic-owner", WS, Audience::Fabric).unwrap(),
        endpoint.clone(),
    );
    let http = HttpExecutor::new(
        wire.clone(),
        Arc::new(Timer),
        ExecutionOptions {
            timeout: Duration::from_secs(60),
            max_delay: Duration::from_secs(3),
        },
    )
    .unwrap()
    .with_endpoint(endpoint);
    let cancelled = poll_fn(|context| {
        let requests = wire.requests.lock().unwrap();
        if cancel_after.is_some_and(|count| requests.len() >= count) {
            Poll::Ready(())
        } else {
            *wire.cancellation_waker.lock().unwrap() = Some(context.waker().clone());
            Poll::Pending
        }
    });
    let mut operations =
        OperationSession::with_context(&context, http, OperationLimits::default(), cancelled);
    let mut approval = Approval {
        expected: prepared.clone(),
        allowed: approved.unwrap_or(false),
        calls: 0,
    };
    let dispatch = host::dispatch(
        prepared,
        &mut operations,
        approved.map(|_| &mut approval as &mut dyn LinkApproval),
    )
    .await;
    drop(operations);
    (dispatch, wire, credential, approval.calls)
}

#[tokio::test]
async fn cross_workspace_source_keeps_its_own_reference_and_metadata_route() {
    let source_workspace = "66666666-6666-4666-8666-666666666666";
    let mut body = input("FabricItem");
    body["itemReference"]["workspaceId"] = json!(source_workspace);
    let (dispatch, wire, _, _) = run(
        prepared_input(&body, false).await,
        vec![
            metadata(AGENT, "DataAgent"),
            response(
                StatusCode::Ok,
                json!({"id":SOURCE,"type":"Warehouse","workspaceId":source_workspace}),
            ),
            response(StatusCode::Created, json!({"id":DS})),
        ],
        Some(true),
    )
    .await;
    assert_eq!(dispatch.exit_code(), 0);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(
        requests[1].url,
        format!("https://api.fabric.microsoft.com/v1/workspaces/{source_workspace}/items/{SOURCE}")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[2].body).unwrap(),
        body
    );
}

#[tokio::test]
async fn cancellation_before_each_read_or_post_prevents_mutation() {
    for cancel_after in 0..3 {
        let (dispatch, wire, _, _) = run_scenario(
            prepared("LakehouseTables", false).await,
            vec![metadata(AGENT, "DataAgent"), metadata(SOURCE, "Lakehouse")],
            Some(true),
            Some(cancel_after),
            None,
        )
        .await;
        assert_eq!(dispatch.exit_code(), 1);
        assert_eq!(
            dispatch.value["error"]["code"],
            "LongRunningOperationCancelled"
        );
        assert_eq!(dispatch.value["mutationMayHaveOccurred"], false);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), cancel_after);
        assert!(requests.iter().all(|request| request.method == Method::Get));
    }
}

#[tokio::test]
async fn facade_and_root_share_exact_native_staging_post_and_body() {
    for kind in ["LakehouseTables", "FabricItem"] {
        let root = prepared(kind, false).await;
        let facade = host::parse(
            ["artifact", "data-agent", "link", TARGET, "--input", "-"]
                .into_iter()
                .map(Into::into)
                .collect(),
        )
        .unwrap();
        let facade = host::prepare(
            &facade,
            &mut NativeFiles::new(&mut Cursor::new(serde_json::to_vec(&input(kind)).unwrap())),
        )
        .await
        .unwrap();
        assert_eq!(root.operation().path(), facade.operation().path());
        assert_eq!(root.operation().body(), facade.operation().body());
        let (dispatch, wire, credential, approvals) = run(
            facade,
            vec![
                metadata(AGENT, "DataAgent"),
                metadata(
                    SOURCE,
                    if kind == "LakehouseTables" {
                        "Lakehouse"
                    } else {
                        "Warehouse"
                    },
                ),
                response(StatusCode::Created, json!({"id":DS})),
            ],
            Some(true),
        )
        .await;
        assert_eq!(dispatch.value["status"], "completed");
        assert_eq!(dispatch.value["httpStatus"], 201);
        assert_eq!(approvals, 1);
        assert_eq!(*credential.0.lock().unwrap(), 3);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].method, Method::Get);
        assert_eq!(
            requests[0].url,
            format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/items/{AGENT}")
        );
        assert_eq!(
            requests[1].url,
            format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/items/{SOURCE}")
        );
        assert_eq!(requests[2].method, Method::Post);
        assert_eq!(
            requests[2].url,
            format!(
                "https://api.fabric.microsoft.com/v1/workspaces/{WS}/dataAgents/{AGENT}/staging/datasources"
            )
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[2].body).unwrap(),
            input(kind)
        );
    }
}

#[tokio::test]
async fn missing_or_declined_approval_sends_nothing_and_acquires_no_token() {
    for approval in [None, Some(false)] {
        let (dispatch, wire, credential, calls) =
            run(prepared("FabricItem", false).await, vec![], approval).await;
        assert_eq!(dispatch.value["error"]["code"], "ApprovalRequired");
        assert_eq!(dispatch.exit_code(), 1);
        assert!(wire.requests.lock().unwrap().is_empty());
        assert_eq!(*credential.0.lock().unwrap(), 0);
        assert_eq!(calls, usize::from(approval.is_some()));
    }
}

#[tokio::test]
async fn wrong_owner_source_identity_or_type_never_posts() {
    for replies in [
        vec![metadata(AGENT, "Notebook")],
        vec![metadata(SOURCE, "DataAgent")],
        vec![metadata(AGENT, "DataAgent"), metadata(SOURCE, "Notebook")],
        vec![metadata(AGENT, "DataAgent"), metadata(DS, "Lakehouse")],
        vec![metadata(AGENT, "DataAgent"), metadata(SOURCE, "Warehouse")],
    ] {
        let (dispatch, wire, _, _) = run(
            prepared("LakehouseTables", false).await,
            replies,
            Some(true),
        )
        .await;
        assert_eq!(dispatch.exit_code(), 1);
        assert_eq!(dispatch.value["mutationMayHaveOccurred"], false);
        assert!(
            wire.requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.method == Method::Get)
        );
    }
}

#[tokio::test]
async fn service_permission_failures_are_explicit_and_never_replayed() {
    for failure_at in 0..3 {
        let mut replies = vec![metadata(AGENT, "DataAgent"), metadata(SOURCE, "Lakehouse")];
        replies.truncate(failure_at);
        replies.push(response(
            StatusCode::Forbidden,
            json!({"errorCode":"InsufficientPrivileges","message":"synthetic"}),
        ));
        let (dispatch, wire, _, _) = run(
            prepared("LakehouseTables", false).await,
            replies,
            Some(true),
        )
        .await;
        assert_eq!(dispatch.exit_code(), 1);
        assert_eq!(dispatch.value["httpStatus"], 403);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), failure_at + 1);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == Method::Post)
                .count(),
            usize::from(failure_at == 2)
        );
    }
}

#[tokio::test]
async fn acceptance_waiting_malformed_completion_and_unknown_retain_real_evidence() {
    let mut headers = Headers::new();
    headers.insert(
        "location",
        format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
    );
    headers.insert("x-ms-operation-id", OP);
    headers.insert("retry-after", "0");
    for wait in [false, true] {
        let mut replies = vec![
            metadata(AGENT, "DataAgent"),
            metadata(SOURCE, "Lakehouse"),
            (StatusCode::Accepted, headers.clone(), vec![]),
        ];
        if wait {
            replies.push(response(StatusCode::Ok, json!({"status":"Succeeded"})));
            replies.push(response(StatusCode::Ok, json!({"id":DS})));
        }
        let (dispatch, wire, _, _) =
            run(prepared("LakehouseTables", wait).await, replies, Some(true)).await;
        assert_eq!(dispatch.exit_code(), 0);
        assert_eq!(
            dispatch.value["status"],
            if wait { "completed" } else { "accepted" }
        );
        if !wait {
            assert_eq!(dispatch.value["operationId"], OP);
        }
        assert_eq!(
            wire.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.method == Method::Post)
                .count(),
            1
        );
    }
    let (dispatch, wire, _, _) = run(
        prepared("LakehouseTables", false).await,
        vec![
            metadata(AGENT, "DataAgent"),
            metadata(SOURCE, "Lakehouse"),
            response(StatusCode::Created, json!({"noIdentity":true})),
        ],
        Some(true),
    )
    .await;
    assert_eq!(dispatch.exit_code(), 1);
    assert_eq!(dispatch.value["status"], "partial");
    assert_eq!(dispatch.value["mutationMayHaveOccurred"], true);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
    assert!(dispatch.result.unwrap_err().observed.is_some());

    let (dispatch, wire, _, _) = run_scenario(
        prepared("LakehouseTables", false).await,
        vec![
            metadata(AGENT, "DataAgent"),
            metadata(SOURCE, "Lakehouse"),
            response(StatusCode::Created, json!({"id":DS})),
        ],
        Some(true),
        None,
        Some(3),
    )
    .await;
    assert_eq!(dispatch.exit_code(), 1);
    assert_eq!(dispatch.value["status"], "unknown");
    assert_eq!(dispatch.value["error"]["code"], "ExecutionFailed");
    assert_eq!(dispatch.value["mutationMayHaveOccurred"], true);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
    assert_eq!(wire.replies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn bare_acceptance_is_not_promoted_to_completion_or_replayed() {
    let (dispatch, wire, _, _) = run(
        prepared("LakehouseTables", false).await,
        vec![
            metadata(AGENT, "DataAgent"),
            metadata(SOURCE, "Lakehouse"),
            response(StatusCode::Accepted, Value::Null),
        ],
        Some(true),
    )
    .await;
    assert_eq!(dispatch.exit_code(), 0);
    assert_eq!(dispatch.value["status"], "accepted");
    assert_eq!(dispatch.value["operationId"], Value::Null);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn strict_input_and_target_validation_precede_all_effects() {
    let valid = serde_json::to_vec(&input("LakehouseTables")).unwrap();
    let invalid = vec![
        br#"{"type":"FabricItem","type":"LakehouseTables"}"#.to_vec(),
        br#"{"type":"FabricItem","itemReference":{"referenceType":"ById","workspaceId":"x","itemId":"x","itemId":"y"}}"#.to_vec(),
        vec![0xff],
        vec![b' ';host::MAX_INPUT_BYTES+1],
        br#"{"type":"FabricItem","fabricItemType":"Warehouse","itemReference":{}}"#.to_vec(),
        br#"{"type":"LakehouseTables","lakehouseReference":{"referenceType":"ByVariable","variableReference":"$(x)"}}"#.to_vec(),
        br#"{} {}"#.to_vec(),
    ];
    let command = host::parse(
        ["link", TARGET, "--input", "-"]
            .into_iter()
            .map(Into::into)
            .collect(),
    )
    .unwrap();
    let mut exact_limit = valid.clone();
    exact_limit.resize(host::MAX_INPUT_BYTES, b' ');
    assert!(
        host::prepare(
            &command,
            &mut NativeFiles::new(&mut Cursor::new(exact_limit))
        )
        .await
        .is_ok()
    );
    let mut oversized = Cursor::new(vec![b' '; host::MAX_INPUT_BYTES * 2]);
    assert!(
        host::prepare(&command, &mut NativeFiles::new(&mut oversized))
            .await
            .is_err()
    );
    assert_eq!(oversized.position(), (host::MAX_INPUT_BYTES + 1) as u64);
    for bytes in invalid {
        assert!(
            host::prepare(&command, &mut NativeFiles::new(&mut Cursor::new(bytes)))
                .await
                .is_err()
        );
    }
    for target in [
        TARGET.replace("staging", "published"),
        TARGET.replace("DataAgent", "Lakehouse"),
        TARGET.replace(WS, "named"),
        format!("{TARGET}/{DS}"),
        TARGET.replace("/settings/staging/datasources", ""),
        format!("{TARGET}/"),
    ] {
        let mut command = command.clone();
        command.path = target;
        command.input = "does-not-exist".into();
        let error = host::prepare(&command, &mut NativeFiles::new(&mut Cursor::new(&valid)))
            .await
            .unwrap_err();
        assert_eq!(error.code, "InvalidTarget");
    }
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), &valid).unwrap();
    let mut command = command.clone();
    command.input = file.path().to_str().unwrap().into();
    assert!(
        host::prepare(&command, &mut NativeFiles::new(&mut Cursor::new(vec![])))
            .await
            .is_ok()
    );
    command.input = file
        .path()
        .with_file_name("nonexistent-dataagent-input.json")
        .to_str()
        .unwrap()
        .into();
    assert_eq!(
        host::prepare(&command, &mut NativeFiles::new(&mut Cursor::new(vec![])))
            .await
            .unwrap_err()
            .code,
        "InputFailed"
    );
    command.input = file.path().parent().unwrap().to_str().unwrap().into();
    assert!(
        host::prepare(&command, &mut NativeFiles::new(&mut Cursor::new(vec![])))
            .await
            .is_err()
    );
}

struct BrokenWriter {
    remaining: usize,
    flush: bool,
}
impl Write for BrokenWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::other("synthetic output failure"));
        }
        let count = self.remaining.min(bytes.len());
        self.remaining -= count;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.flush {
            Err(io::Error::other("synthetic flush failure"))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn output_failure_retains_original_execution_and_never_posts_again() {
    for (remaining, flush) in [(7, false), (usize::MAX, true)] {
        let (dispatch, wire, _, _) = run(
            prepared("LakehouseTables", false).await,
            vec![
                metadata(AGENT, "DataAgent"),
                metadata(SOURCE, "Lakehouse"),
                response(StatusCode::Created, json!({"id":DS})),
            ],
            Some(true),
        )
        .await;
        let error = dispatch
            .write_to(&mut BrokenWriter { remaining, flush })
            .unwrap_err();
        assert!(error.dispatch.result.is_ok());
        assert_eq!(error.evidence()["execution"]["status"], "completed");
        assert_eq!(error.evidence()["remoteReplayed"], false);
        assert!(error.written >= 7);
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
    }
}
