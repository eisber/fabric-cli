// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real parser/leaf/host/SDK/native credential/RS256/MWC stack with scripted HTTP.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::Secret,
    error::ErrorKind,
    http::{AsyncRawResponse, HttpClient, Method, Request, headers::Headers},
};
use fabric_cli::runtime_v2::{
    ReadCoverage, ReadErrorKind, ReadOptions, ReadReceipt, ReadStatus, Representation,
    host::{self, HostError, prepare_destination},
    native_runtime_notebook_list as leaf,
    notebook::{NotebookBinding, NotebookConnection},
    parse_args,
};
use fabric_cli_output::OutputStream;
use fabric_identity::headless::{HeadlessCredential, HeadlessInput, ServiceIdentity};
use fabric_sdk::{Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_transport::TokioTimer;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::pending,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[path = "../../../../tests/fixtures/notebook-identity/fixture.rs"]
mod fixture;
use fixture::*;

const PATH: &str = "/Team.Workspace/Analysis.Notebook/sessions";

fn binding() -> Value {
    json!({
        "schemaVersion":1,
        "owner":{"workspace":"Team.Workspace","artifact":"Analysis.Notebook","kind":"notebook"},
        "environment":"production","catalogOrigin":FABRIC,"clusterOrigin":CLUSTER,
        "workloadOrigin":WORKLOAD,"capacityId":CAPACITY,"workspaceId":WORKSPACE,
        "notebookId":NOTEBOOK,"account":CLIENT,"tenant":TENANT
    })
}

enum Reply {
    Json(u16, Value),
    Raw(u16, Vec<u8>),
    Partial,
    PendingBody,
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
            Reply::Partial => Ok(AsyncRawResponse::new(
                200.into(),
                headers,
                Box::pin(futures_util::stream::iter([
                    Ok(Bytes::from_static(b"[")),
                    Err(azure_core::Error::with_message(
                        ErrorKind::Io,
                        "private-stream-canary",
                    )),
                ])),
            )),
            Reply::PendingBody => Ok(AsyncRawResponse::new(
                200.into(),
                headers,
                Box::pin(futures_util::stream::pending::<azure_core::Result<Bytes>>()),
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
    options: ReadOptions,
    end: Instant,
) -> NotebookConnection {
    let credential = HeadlessCredential::new_with_pbi_bootstrap(
        HeadlessInput::ClientSecret {
            identity: ServiceIdentity::new(TENANT, CLIENT).unwrap(),
            secret: Secret::new("synthetic-offline-client-secret"),
        },
        wire.clone(),
    )
    .unwrap();
    let endpoint = EndpointBinding::public(ServiceOrigin::Fabric);
    let context = RequestContext::with_endpoint(
        credential,
        CredentialSelection::new(CLIENT, TENANT, Audience::Fabric).unwrap(),
        endpoint.clone(),
    );
    let http = HttpExecutor::new(wire, Arc::new(TokioTimer), ExecutionOptions::default())
        .unwrap()
        .with_endpoint(endpoint);
    binding.connect(context, http, options, end).unwrap()
}

fn assert_routes(wire: &Wire) {
    let requests = wire.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        6,
        "one bootstrap and collection, no retry/history/readiness"
    );
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
    assert_eq!(requests[3].method(), Method::Get);
    assert_eq!(
        requests[3].url().as_str(),
        format!("{FABRIC}/metadata/cluster")
    );
    assert_eq!(requests[4].method(), Method::Post);
    assert_eq!(
        requests[4].url().as_str(),
        format!("{CLUSTER}/metadata/v201606/generatemwctoken")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&Bytes::from(requests[4].body())).unwrap(),
        json!({"capacityObjectId":CAPACITY,"workspaceObjectId":WORKSPACE,
               "workloadType":"Notebook","artifactObjectIds":[NOTEBOOK]})
    );
    assert_eq!(requests[5].method(), Method::Get);
    assert_eq!(
        requests[5].url().as_str(),
        format!(
            "{WORKLOAD}/webapi/capacities/{CAPACITY}/workloads/Notebook/Data/Automatic/api/workspaces/{WORKSPACE}/artifacts/{NOTEBOOK}/jupyterApi/versions/1/api/sessions"
        )
    );
    assert_eq!(requests[5].url().query(), None);
    assert!(Bytes::from(requests[5].body()).is_empty());
    assert_eq!(
        requests[5]
            .headers()
            .get_str(&"authorization".into())
            .unwrap(),
        format!("MwcToken {MWC}")
    );
}

async fn run(
    facade: bool,
    extra: &[&str],
    wire: Arc<Wire>,
    options: ReadOptions,
) -> (Result<ReadReceipt, HostError>, Option<Vec<u8>>) {
    let mut args = vec!["fab-cli-v2"];
    if facade {
        args.extend(["artifact", "notebook"]);
    }
    args.extend(["list", PATH, "--output", "result.json"]);
    args.extend_from_slice(extra);
    let input = parse_args(args).unwrap();
    let binding =
        NotebookBinding::parse(&serde_json::to_vec(&binding()).unwrap(), &input.path).unwrap();
    let root = tempfile::tempdir().unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    let signal = pending::<()>();
    tokio::pin!(signal);
    let destination =
        prepare_destination(input.output.clone(), root.path().into(), end, &mut signal)
            .await
            .unwrap();
    let connection = connect(wire.clone(), binding, options, end);
    assert!(
        wire.requests.lock().unwrap().is_empty(),
        "local setup must not access credentials/HTTP"
    );
    let result = match leaf::acquire(
        &input,
        &connection.executor,
        &connection.resolver,
        options,
        &destination,
        end,
        &mut signal,
    )
    .await
    {
        Ok(read) => read.deliver(destination, end, &mut signal).await,
        Err(error) => Err(error),
    };
    let bytes = match std::fs::read(root.path().join("result.json")) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("unexpected output access error: {error}"),
    };
    (result, bytes)
}

#[tokio::test]
async fn collection_and_facade_use_real_principal_mwc_sdk_http_and_preserve_every_row() {
    let rows = json!([
        {"id":"failed","state":"Failed","kernel":{"id":"kernel","execution_state":"future-state"},
         "error":{"message":"provider-data"},"unknown":[null,[],{}]},
        {"id":"unknown","state":{"future":true},"kernel":null}
    ]);
    let mut receipts = Vec::new();
    let mut deliveries = Vec::new();
    for facade in [false, true] {
        let wire = wire(Reply::Json(200, rows.clone()));
        let (receipt, bytes) = run(
            facade,
            &["--limit", "2"],
            wire.clone(),
            ReadOptions::default(),
        )
        .await;
        let receipt = receipt.unwrap();
        assert_eq!(receipt.data, rows);
        assert_eq!(receipt.status, ReadStatus::ReadComplete);
        assert_eq!(receipt.meta.representation, Representation::NotebookSession);
        assert_eq!(receipt.meta.coverage, ReadCoverage::ReturnedCollection);
        assert_eq!(receipt.meta.continuation, None);
        assert_eq!(receipt.meta.workload_observation, None);
        let bytes = bytes.unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_eq!(
            serde_json::from_slice::<ReadReceipt>(&bytes).unwrap(),
            receipt
        );
        assert_routes(&wire);
        assert!(wire.replies.lock().unwrap().is_empty());
        receipts.push(receipt);
        deliveries.push(bytes);
    }
    assert_eq!(receipts[0], receipts[1]);
    assert_eq!(deliveries[0], deliveries[1]);
}

#[tokio::test]
async fn empty_and_singleton_are_arrays_not_unavailable_or_unwrapped() {
    for rows in [json!([]), json!([{"id":"one","future":null}])] {
        let wire = wire(Reply::Json(200, rows.clone()));
        let (receipt, _) = run(false, &[], wire.clone(), ReadOptions::default()).await;
        assert_eq!(receipt.unwrap().data, rows);
        assert_routes(&wire);
    }
}

#[tokio::test]
async fn malformed_202_denied_partial_and_continuation_never_deliver_empty_success() {
    for reply in [
        Reply::Json(202, json!([])),
        Reply::Json(206, json!([])),
        Reply::Raw(200, Vec::new()),
        Reply::Raw(200, b"[".to_vec()),
        Reply::Json(200, json!([{"id":false}])),
        Reply::Raw(403, b"private-denied-canary".to_vec()),
        Reply::Partial,
        Reply::Json(200, json!({"value":[],"continuationToken":"more"})),
        Reply::Json(200, json!({"value":[],"continuationUri":"more"})),
        Reply::Json(200, json!({"value":[],"nextLink":"more"})),
    ] {
        let wire = wire(reply);
        let (result, bytes) = run(false, &[], wire.clone(), ReadOptions::default()).await;
        let error = result.unwrap_err();
        assert!(!format!("{error} {error:?}").contains("canary"));
        assert!(bytes.is_none());
        assert_routes(&wire);
    }
}

#[tokio::test]
async fn exact_whole_page_record_data_and_stream_caps_do_not_clip_or_invent_query() {
    let rows = json!([{"id":"one","future":[null,"keep"]},{"id":"two","state":"Failed"}]);
    let size = serde_json::to_vec(&rows).unwrap().len();
    for (limit, bytes, succeeds) in [(2, size, true), (1, size, false), (2, size - 1, false)] {
        let limit = limit.to_string();
        let bytes = bytes.to_string();
        let wire = wire(Reply::Json(200, rows.clone()));
        let (result, output) = run(
            false,
            &["--limit", &limit, "--max-bytes", &bytes],
            wire.clone(),
            ReadOptions::default(),
        )
        .await;
        if succeeds {
            assert_eq!(result.unwrap().data, rows);
        } else {
            assert_eq!(result.unwrap_err().kind, ReadErrorKind::BudgetExceeded);
            assert!(output.is_none());
        }
        assert_routes(&wire);
    }
    let wire = wire(Reply::Raw(200, vec![b' '; 4097]));
    let (result, bytes) = run(
        false,
        &[],
        wire.clone(),
        ReadOptions {
            max_response_bytes: 4096,
            ..ReadOptions::default()
        },
    )
    .await;
    assert_eq!(result.unwrap_err().kind, ReadErrorKind::BudgetExceeded);
    assert!(bytes.is_none());
    assert_routes(&wire);
}

#[tokio::test]
async fn original_deadline_and_cancel_drop_actual_pending_body_without_replay() {
    for cancel in [false, true] {
        let input = parse_args(["fab-cli-v2", "list", PATH, "--output", "result.json"]).unwrap();
        let binding =
            NotebookBinding::parse(&serde_json::to_vec(&binding()).unwrap(), &input.path).unwrap();
        let root = tempfile::tempdir().unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        let destination =
            prepare_destination(input.output.clone(), root.path().into(), end, pending())
                .await
                .unwrap();
        let wire = wire(Reply::PendingBody);
        let connection = connect(wire.clone(), binding, ReadOptions::default(), end);
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
        let error = leaf::acquire(
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
        assert!(!root.path().join("result.json").exists());
        assert_routes(&wire);
    }
}

#[test]
fn owner_binding_account_tenant_audience_and_catalog_mismatch_are_zero_send() {
    let input = parse_args(["fab-cli-v2", "list", PATH]).unwrap();
    let wire = wire(Reply::Json(200, json!([])));
    let valid =
        NotebookBinding::parse(&serde_json::to_vec(&binding()).unwrap(), &input.path).unwrap();
    for (account, tenant, audience) in [
        (OTHER, TENANT, Audience::Fabric),
        (CLIENT, OTHER, Audience::Fabric),
        (CLIENT, TENANT, Audience::OneLake),
    ] {
        assert!(
            valid
                .check_identity(
                    &CredentialSelection::new(account, tenant, audience).unwrap(),
                    &EndpointBinding::public(ServiceOrigin::Fabric),
                )
                .is_err()
        );
    }
    for (field, value) in [
        (
            "owner",
            json!({"workspace":"Foreign.Workspace","artifact":"Analysis.Notebook","kind":"notebook"}),
        ),
        ("catalogOrigin", json!(CLUSTER)),
        ("capacityId", json!("invalid")),
        ("workloadOrigin", json!("http://127.0.0.1")),
        ("token", json!("not-allowed")),
    ] {
        let mut document = binding();
        document[field] = value;
        assert!(
            NotebookBinding::parse(&serde_json::to_vec(&document).unwrap(), &input.path).is_err()
        );
    }
    assert!(wire.requests.lock().unwrap().is_empty());
}

async fn pipe_child(mode: &str, root: &Path) -> u8 {
    let rows = json!([{"id":"failed","state":"Failed","unknown":"x".repeat(512 * 1024)}]);
    let input = parse_args(["fab-cli-v2", "list", PATH]).unwrap();
    let binding =
        NotebookBinding::parse(&serde_json::to_vec(&binding()).unwrap(), &input.path).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    let (send, receive) = tokio::sync::oneshot::channel();
    let mut send = Some(send);
    let signal = async {
        receive.await.unwrap();
    };
    tokio::pin!(signal);
    let destination = prepare_destination("-".into(), root.into(), end, &mut signal)
        .await
        .unwrap();
    let wire = wire(if mode == "stderr" {
        Reply::Raw(403, b"private-denied-canary".to_vec())
    } else {
        Reply::Json(200, rows.clone())
    });
    let connection = connect(wire.clone(), binding, ReadOptions::default(), end);
    let result = match leaf::acquire(
        &input,
        &connection.executor,
        &connection.resolver,
        ReadOptions::default(),
        &destination,
        end,
        &mut signal,
    )
    .await
    {
        Ok(read) => {
            if mode == "cancel" {
                let send = send.take().unwrap();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    send.send(()).unwrap();
                });
            }
            read.deliver(destination, end, &mut signal).await
        }
        Err(error) => Err(error),
    };
    let error = result.unwrap_err();
    assert_routes(&wire);
    if mode != "stderr" {
        assert!(error.read_completed);
        assert_eq!(error.receipt.as_ref().unwrap().data, rows);
    } else {
        let mut blocked = false;
        for _ in 0..128 {
            if host::write_stream(OutputStream::Stderr, vec![b'x'; 16 * 1024], end)
                .await
                .is_err()
            {
                blocked = true;
                break;
            }
        }
        assert!(blocked, "stderr must stop within fixture byte/time bounds");
    }
    let diagnostic = host::diagnostic(&error, end).await;
    if mode == "cancel" {
        assert!(diagnostic.is_ok());
    } else {
        assert!(diagnostic.is_err());
    }
    std::fs::write(
        root.join("evidence.json"),
        serde_json::to_vec(&json!({
            "requests":wire.requests.lock().unwrap().len(),"kind":error.kind,
            "readCompleted":error.read_completed,"receiptRetained":error.receipt.is_some(),
        }))
        .unwrap(),
    )
    .unwrap();
    1
}

#[test]
fn actual_native_stack_child_pipes_exit_bounded_nonzero_without_replaying_bootstrap_or_get() {
    const MODE: &str = "FAB_NOTEBOOK_LIST_WIRE_CHILD";
    const ROOT: &str = "FAB_NOTEBOOK_LIST_WIRE_ROOT";
    if let Some(mode) = std::env::var_os(MODE) {
        let root = std::path::PathBuf::from(std::env::var_os(ROOT).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(2)
            .build()
            .unwrap();
        let exit = runtime.block_on(pipe_child(mode.to_str().unwrap(), &root));
        runtime.shutdown_timeout(Duration::from_millis(100));
        std::process::exit(i32::from(exit));
    }
    for mode in ["stdout", "stderr", "cancel"] {
        let root = tempfile::tempdir().unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact",
            "actual_native_stack_child_pipes_exit_bounded_nonzero_without_replaying_bootstrap_or_get",
            "--nocapture",
        ]).env_clear().env(MODE,mode).env(ROOT,root.path())
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let mut child = command.spawn().unwrap();
        let end = Instant::now() + Duration::from_secs(12);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= end {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("actual native stack blocked pipe exceeded process watchdog");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(1));
        let evidence: Value =
            serde_json::from_slice(&std::fs::read(root.path().join("evidence.json")).unwrap())
                .unwrap();
        assert_eq!(evidence["requests"], 6);
        assert_eq!(
            evidence["kind"],
            match mode {
                "stdout" => "timeout",
                "cancel" => "cancelled",
                _ => "http",
            }
        );
        assert_eq!(evidence["readCompleted"], mode != "stderr");
        assert_eq!(evidence["receiptRetained"], mode != "stderr");
    }
}
