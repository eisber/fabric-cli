// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parser -> shared host -> SDK -> actual native credential/verifier/HTTP stack.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::Secret,
    error::ErrorKind,
    http::{AsyncRawResponse, HttpClient, Method, Request, headers::Headers},
};
use fabric_cli::runtime_v2::{
    ReadErrorKind, ReadOptions, ReadStatus, TypedPath,
    host::{acquire, prepare_destination},
    notebook::NotebookBinding,
    parse_args,
};
use fabric_identity::headless::{HeadlessCredential, HeadlessInput, ServiceIdentity};
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    runtime::{NativeDeadline, NativeError},
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_transport::TokioTimer;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::pending,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[path = "../../../../tests/fixtures/notebook-identity/fixture.rs"]
mod fixture;
use fixture::*;

const PATH: &str = "/Team.Workspace/Analysis.Notebook/src/content";

fn binding() -> Value {
    json!({
        "schemaVersion":1,
        "owner":{"workspace":"Team.Workspace","artifact":"Analysis.Notebook","kind":"notebook"},
        "environment":"production",
        "catalogOrigin":FABRIC,"clusterOrigin":CLUSTER,"workloadOrigin":WORKLOAD,
        "capacityId":CAPACITY,"workspaceId":WORKSPACE,"notebookId":NOTEBOOK,
        "account":CLIENT,"tenant":TENANT
    })
}

fn document() -> Value {
    json!({
        "nbformat":4,"nbformat_minor":5,"unknown":{"retained":null},"metadata":{"unknown":false},
        "cells":[
            {"cell_type":"code","id":"original","source":["print('snowman \u{2603}')\n","1"],
             "metadata":{"custom":42},"execution_count":7,
             "outputs":[{"output_type":"display_data","data":{"text/plain":["saved\n"]},
                         "metadata":{},"unknown":{"keep":true}}]},
            {"cell_type":"markdown","source":"# title","metadata":{},"attachments":{}}
        ]
    })
}

enum Reply {
    Json(u16, Value),
    Raw(u16, Vec<u8>),
    Pending,
    Partial,
}

impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reply(<offline>)")
    }
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
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP or replay");
        let mut headers = Headers::new();
        headers.insert("content-type", "application/json");
        match reply {
            Reply::Json(status, body) => Ok(AsyncRawResponse::from_bytes(
                status.into(),
                headers,
                serde_json::to_vec(&body).unwrap(),
            )),
            Reply::Raw(status, body) => {
                Ok(AsyncRawResponse::from_bytes(status.into(), headers, body))
            }
            Reply::Pending => pending().await,
            Reply::Partial => Ok(AsyncRawResponse::new(
                200.into(),
                headers,
                Box::pin(futures_util::stream::iter([
                    Ok(Bytes::from_static(b"{")),
                    Err(azure_core::Error::with_message(
                        ErrorKind::Io,
                        "private-stream-canary",
                    )),
                ])),
            )),
        }
    }
}

fn wire(last: Reply) -> Arc<Wire> {
    Arc::new(Wire {
        replies: Mutex::new(
            [
                Reply::Json(
                    200,
                    json!({"access_token":token(FABRIC),"token_type":"Bearer","expires_in":3500}),
                ),
                Reply::Json(200, jwks("offline-key")),
                Reply::Json(
                    200,
                    json!({"access_token":token(PBI),"token_type":"Bearer","expires_in":3500}),
                ),
                Reply::Json(200, json!({"backendUrl":CLUSTER})),
                Reply::Json(200, grant()),
                last,
            ]
            .into(),
        ),
        requests: Mutex::new(Vec::new()),
    })
}

fn connect(
    wire: Arc<Wire>,
    binding: NotebookBinding,
    deadline: Instant,
) -> fabric_cli::runtime_v2::notebook::NotebookConnection {
    let credential = HeadlessCredential::new_with_pbi_bootstrap(
        HeadlessInput::ClientSecret {
            identity: ServiceIdentity::new(TENANT, CLIENT).unwrap(),
            secret: Secret::new("synthetic-offline-client-secret"),
        },
        wire.clone(),
    )
    .unwrap();
    let selection = CredentialSelection::new(CLIENT, TENANT, Audience::Fabric).unwrap();
    let endpoint = EndpointBinding::public(ServiceOrigin::Fabric);
    let context = RequestContext::with_endpoint(credential, selection, endpoint.clone());
    let http = HttpExecutor::new(wire, Arc::new(TokioTimer), ExecutionOptions::default())
        .unwrap()
        .with_endpoint(endpoint);
    binding
        .connect(context, http, ReadOptions::default(), deadline)
        .unwrap()
}

fn assert_routes(wire: &Wire) {
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    for (index, scope) in [
        (0, "api.fabric.microsoft.com"),
        (2, "analysis.windows.net%2Fpowerbi%2Fapi"),
    ] {
        assert_eq!(requests[index].method(), Method::Post);
        assert_eq!(
            requests[index].url().as_str(),
            format!("https://login.microsoftonline.com/{TENANT}/oauth2/v2.0/token")
        );
        let body = String::from_utf8(Bytes::from(requests[index].body()).to_vec()).unwrap();
        assert!(body.contains(&format!("scope=https%3A%2F%2F{scope}%2F.default")));
        assert!(body.contains(&format!("client_id={CLIENT}")));
    }
    assert!(
        requests[1]
            .headers()
            .get_str(&"authorization".into())
            .is_err()
    );
    assert_eq!(
        requests[3].url().as_str(),
        format!("{FABRIC}/metadata/cluster")
    );
    assert_eq!(
        requests[4].url().as_str(),
        format!("{CLUSTER}/metadata/v201606/generatemwctoken")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&Bytes::from(requests[4].body())).unwrap(),
        json!({"capacityObjectId":CAPACITY,"workspaceObjectId":WORKSPACE,"workloadType":"Notebook","artifactObjectIds":[NOTEBOOK]})
    );
    assert_eq!(
        requests[5].url().as_str(),
        format!(
            "{WORKLOAD}/webapi/capacities/{CAPACITY}/workloads/Notebook/Data/Automatic/api/workspaces/{WORKSPACE}/artifacts/{NOTEBOOK}/content"
        )
    );
    assert_eq!(
        requests[5]
            .headers()
            .get_str(&"authorization".into())
            .unwrap(),
        format!("MwcToken {MWC}")
    );
}

#[tokio::test]
async fn canonical_and_facade_content_use_real_native_auth_and_preserve_full_notebook() {
    for facade in [false, true] {
        let mut args = vec!["fab-cli-v2"];
        if facade {
            args.extend(["artifact", "notebook"]);
        }
        args.extend(["get", PATH, "-o", "result.json"]);
        let input = parse_args(args).unwrap();
        let binding =
            NotebookBinding::parse(&serde_json::to_vec(&binding()).unwrap(), &input.path).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let end = Instant::now() + Duration::from_secs(30);
        let destination =
            prepare_destination("result.json".into(), temp.path().into(), end, pending())
                .await
                .unwrap();
        let wire = wire(Reply::Json(200, document()));
        let connection = connect(wire.clone(), binding, end);
        assert!(wire.requests.lock().unwrap().is_empty());
        let read = acquire(
            &input,
            &connection.executor,
            &connection.resolver,
            ReadOptions::default(),
            &destination,
            end,
            pending(),
        )
        .await
        .unwrap();
        let receipt = read.deliver(destination, end, pending()).await.unwrap();
        assert_eq!(receipt.status, ReadStatus::ReadComplete);
        assert_eq!(receipt.data, document());
        let bytes = std::fs::read(temp.path().join("result.json")).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["data"],
            document()
        );
        assert_routes(&wire);
        assert!(wire.replies.lock().unwrap().is_empty());
    }
}

#[test]
fn owner_origin_audience_identity_and_scope_mismatches_fail_before_credentials_or_http() {
    let path = TypedPath::parse(PATH).unwrap();
    let bytes = serde_json::to_vec(&binding()).unwrap();
    let valid = NotebookBinding::parse(&bytes, &path).unwrap();
    let wire = wire(Reply::Json(200, document()));
    for (account, tenant, audience) in [
        (OTHER, TENANT, Audience::Fabric),
        (CLIENT, OTHER, Audience::Fabric),
        (CLIENT, TENANT, Audience::OneLake),
    ] {
        assert!(
            valid
                .check_identity(
                    &CredentialSelection::new(account, tenant, audience).unwrap(),
                    &EndpointBinding::public(ServiceOrigin::Fabric)
                )
                .is_err()
        );
    }
    for (field, value) in [
        ("schemaVersion", json!(2)),
        ("capacityId", json!("")),
        ("workspaceId", json!("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaA")),
        ("notebookId", json!("../../foreign")),
        ("catalogOrigin", json!(CLUSTER)),
        ("clusterOrigin", json!("https://127.0.0.1")),
        ("workloadOrigin", json!("https://user@selected.example/")),
        ("account", json!("system-assigned")),
        ("tenant", json!("not-a-guid")),
        ("token", json!("forbidden")),
    ] {
        let mut config = binding();
        config[field] = value;
        assert!(
            NotebookBinding::parse(&serde_json::to_vec(&config).unwrap(), &path).is_err(),
            "{field}"
        );
    }
    assert!(
        NotebookBinding::parse(
            &bytes,
            &TypedPath::parse("/Other.Workspace/Analysis.Notebook/src/content").unwrap()
        )
        .is_err()
    );
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invalid_accepted_denied_partial_and_over_budget_reads_never_create_success_output() {
    for reply in [
        Reply::Json(202, json!({})),
        Reply::Raw(200, Vec::new()),
        Reply::Json(200, json!({})),
        Reply::Raw(200, b"{".to_vec()),
        Reply::Raw(403, b"private-denied-canary".to_vec()),
        Reply::Partial,
        Reply::Raw(200, vec![b' '; 4 * 1024 * 1024 + 1]),
    ] {
        let input = parse_args(["fab-cli-v2", "get", PATH, "-o", "result.json"]).unwrap();
        let binding =
            NotebookBinding::parse(&serde_json::to_vec(&binding()).unwrap(), &input.path).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let end = Instant::now() + Duration::from_secs(30);
        let destination =
            prepare_destination("result.json".into(), temp.path().into(), end, pending())
                .await
                .unwrap();
        let wire = wire(reply);
        let connection = connect(wire.clone(), binding, end);
        let error = acquire(
            &input,
            &connection.executor,
            &connection.resolver,
            ReadOptions::default(),
            &destination,
            end,
            pending(),
        )
        .await
        .err()
        .unwrap();
        assert!(!format!("{error:?}").contains("canary"));
        assert!(!temp.path().join("result.json").exists());
        assert_routes(&wire);
    }
}

#[tokio::test]
async fn original_deadline_and_cancellation_drop_pending_native_http_without_replay() {
    for cancel in [false, true] {
        let input = parse_args(["fab-cli-v2", "get", PATH, "-o", "result.json"]).unwrap();
        let binding =
            NotebookBinding::parse(&serde_json::to_vec(&binding()).unwrap(), &input.path).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let end = Instant::now() + Duration::from_secs(1);
        let destination =
            prepare_destination("result.json".into(), temp.path().into(), end, pending())
                .await
                .unwrap();
        let wire = wire(Reply::Pending);
        let connection = connect(wire.clone(), binding, end);
        let signal = async {
            if cancel {
                loop {
                    if wire.requests.lock().unwrap().len() == 6 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            } else {
                pending::<()>().await;
            }
        };
        let error = acquire(
            &input,
            &connection.executor,
            &connection.resolver,
            ReadOptions::default(),
            &destination,
            end,
            signal,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            error.kind,
            if cancel {
                ReadErrorKind::Cancelled
            } else {
                ReadErrorKind::Timeout
            }
        );
        assert!(!temp.path().join("result.json").exists());
        assert_routes(&wire);
    }
}

#[test]
fn native_deadline_conversion_never_restarts_original_expiry() {
    let end = Instant::now() + Duration::from_millis(100);
    let native = NativeDeadline::until(end).unwrap();
    let before = Instant::now();
    let remaining = native.remaining().unwrap();
    assert!(before + remaining <= end);
    assert_eq!(
        NativeDeadline::until(Instant::now() - Duration::from_secs(1)).err(),
        Some(NativeError::TimedOut)
    );
}
