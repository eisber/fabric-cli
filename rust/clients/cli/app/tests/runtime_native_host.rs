// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline host evidence; scripted Notebook execution is not MWC qualification.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, Url, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::runtime_v2::{
    OwnerKind, ReadErrorKind, ReadOptions, ResolvedTarget,
    host::{Destination, ExplicitResolver, acquire, prepare_destination},
    parse_args,
};
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin, TokenCredential,
    livy::LivyService,
    notebook_runtime::{NotebookRuntime, content::ContentLimits},
    runtime::{NativeEndpoint, NativeError, NativeExecutor, NativeRequest, NativeResponse},
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor, native_runtime::NativeFabricExecutor};
use fabric_sdk_transport::TokioTimer;
use serde_json::{Value, json};
use std::{
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const CONTENT: &str = "/Team.Workspace/Analysis.Notebook/src/content";
const SESSIONS: &str = "/Team.Workspace/Data.Lakehouse/sessions";

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn notebook() -> NotebookRuntime {
    NotebookRuntime::new(
        NativeEndpoint::new(Url::parse("https://selected.pbidedicated.windows.net/").unwrap())
            .unwrap(),
        "capacity",
        "workspace",
        "notebook",
        ContentLimits::default(),
        4 * 1024 * 1024,
        100,
    )
    .unwrap()
}

fn document() -> Value {
    json!({
        "nbformat":4, "nbformat_minor":5, "unknown":{"keep":null},
        "metadata":{"language_info":{"name":"python"}},
        "cells":[
            {"cell_type":"code","source":["print('snowman \u{2603}')\n","1"],
             "metadata":{"custom":false},"execution_count":7,"id":"original",
             "outputs":[{"output_type":"display_data","data":{"text/plain":["saved\n"]},
                         "metadata":{},"unknown":{"retained":true}}]},
            {"cell_type":"markdown","source":"# title","metadata":{},"attachments":{}}
        ]
    })
}

struct ScriptedNotebook {
    status: u16,
    body: Vec<u8>,
    calls: AtomicUsize,
}

impl NativeExecutor for ScriptedNotebook {
    fn execute<'a>(
        &'a self,
        request: &'a NativeRequest,
        max_response_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<NativeResponse, NativeError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(self.calls.fetch_add(1, Ordering::SeqCst), 0);
            assert_eq!(
                request.url().as_str(),
                concat!(
                    "https://selected.pbidedicated.windows.net/webapi/capacities/capacity/",
                    "workloads/Notebook/Data/Automatic/api/workspaces/workspace/artifacts/notebook/content"
                )
            );
            assert!(max_response_bytes <= 4 * 1024 * 1024);
            Ok(NativeResponse {
                status: self.status,
                body: self.body.clone(),
                content_type: Some("application/json".into()),
                headers: Default::default(),
            })
        })
    }
}

async fn content_read(status: u16, body: Vec<u8>) -> (Result<Value, ReadErrorKind>, usize) {
    let temp = tempfile::tempdir().unwrap();
    let input = parse_args(["fab-cli-v2", "get", CONTENT, "-o", "result.json"]).unwrap();
    let resolver = ExplicitResolver::new(
        input.path.owner.clone(),
        ResolvedTarget::Notebook(notebook()),
    )
    .unwrap();
    let executor = ScriptedNotebook {
        status,
        body,
        calls: AtomicUsize::new(0),
    };
    let destination = Destination::check("result.json", temp.path()).unwrap();
    let end = deadline();
    let result = match acquire(
        &input,
        &executor,
        &resolver,
        ReadOptions::default(),
        &destination,
        end,
        pending(),
    )
    .await
    {
        Ok(read) => read
            .deliver(destination, end, pending())
            .await
            .map(|receipt| receipt.data),
        Err(error) => Err(error),
    };
    let result = result.map_err(|error| error.kind);
    if let Ok(ref value) = result {
        let bytes = std::fs::read(temp.path().join("result.json")).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let receipt: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(&receipt["data"], value);
        assert_eq!(receipt["status"], "read_complete");
    } else {
        assert!(!temp.path().join("result.json").exists());
    }
    (result, executor.calls.load(Ordering::SeqCst))
}

#[tokio::test]
async fn notebook_content_full_json_fidelity_and_exact_native_route() {
    let expected = document();
    let (result, calls) = content_read(200, serde_json::to_vec(&expected).unwrap()).await;
    assert_eq!(result.unwrap(), expected);
    assert_eq!(calls, 1);
}

#[tokio::test]
async fn notebook_invalid_accepted_denied_and_empty_responses_never_deliver_success() {
    for (status, body) in [
        (202, b"{}".to_vec()),
        (403, b"private provider body".to_vec()),
        (200, Vec::new()),
        (200, b"{".to_vec()),
        (200, b"{}".to_vec()),
    ] {
        let (result, calls) = content_read(status, body).await;
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }
}

#[test]
fn notebook_facade_normalizes_identically_and_cannot_claim_foreign_owner() {
    assert_eq!(
        parse_args(["fab-cli-v2", "get", CONTENT]).unwrap(),
        parse_args(["fab-cli-v2", "artifact", "notebook", "get", CONTENT]).unwrap()
    );
    assert!(parse_args(["fab-cli-v2", "artifact", "notebook", "get", SESSIONS]).is_err());
}

#[tokio::test]
async fn no_clobber_is_checked_before_http_and_atomically_again_at_commit() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("result.json");
    std::fs::write(&path, "original").unwrap();
    assert!(Destination::check("result.json", temp.path()).is_err());
    std::fs::remove_file(&path).unwrap();
    let destination = Destination::check("result.json", temp.path()).unwrap();
    let input = parse_args(["fab-cli-v2", "get", CONTENT, "-o", "result.json"]).unwrap();
    let resolver = ExplicitResolver::new(
        input.path.owner.clone(),
        ResolvedTarget::Notebook(notebook()),
    )
    .unwrap();
    let executor = ScriptedNotebook {
        status: 200,
        body: serde_json::to_vec(&document()).unwrap(),
        calls: AtomicUsize::new(0),
    };
    let end = deadline();
    let read = acquire(
        &input,
        &executor,
        &resolver,
        ReadOptions::default(),
        &destination,
        end,
        pending(),
    )
    .await
    .unwrap();
    std::fs::write(&path, "racing owner").unwrap();
    let error = read.deliver(destination, end, pending()).await.unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::Delivery);
    assert!(error.read_completed);
    assert!(!error.file_created);
    let receipt = error.receipt.as_ref().unwrap();
    assert_eq!(receipt.data, document());
    assert_eq!(receipt.meta.http_status, 200);
    assert!(!format!("{error:?}").contains("snowman"));
    assert!(!error.to_string().contains("snowman"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "racing owner");
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

#[derive(Debug, Default)]
struct Credential(AtomicUsize);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert!(options.is_none());
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        Ok(AccessToken::new(
            "offline-test-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Wire {
    response: Mutex<Option<AsyncRawResponse>>,
    requests: Mutex<Vec<Request>>,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        let mut requests = self.requests.lock().unwrap();
        assert!(requests.is_empty(), "unexpected replay");
        requests.push(request.clone());
        Ok(self
            .response
            .lock()
            .unwrap()
            .take()
            .expect("unexpected HTTP replay"))
    }
}

fn production_stack(
    body: Vec<u8>,
    audience: Audience,
) -> (
    Result<NativeFabricExecutor, NativeError>,
    Arc<Credential>,
    Arc<Wire>,
) {
    production_response(
        AsyncRawResponse::from_bytes(200.into(), Headers::new(), body),
        audience,
    )
}

fn production_response(
    response: AsyncRawResponse,
    audience: Audience,
) -> (
    Result<NativeFabricExecutor, NativeError>,
    Arc<Credential>,
    Arc<Wire>,
) {
    let (status, mut headers, body) = response.deconstruct();
    headers.insert("content-type", "application/json");
    let response = AsyncRawResponse::new(status, headers, Box::pin(body));
    let credential = Arc::new(Credential::default());
    let wire = Arc::new(Wire {
        response: Mutex::new(Some(response)),
        requests: Mutex::new(Vec::new()),
    });
    let endpoint = EndpointBinding::public(ServiceOrigin::Fabric);
    let context = RequestContext::with_endpoint(
        credential.clone(),
        CredentialSelection::new("selected-account", "selected-tenant", audience).unwrap(),
        endpoint.clone(),
    );
    let http = HttpExecutor::new(
        wire.clone(),
        Arc::new(TokioTimer),
        ExecutionOptions::default(),
    )
    .unwrap()
    .with_endpoint(endpoint);
    (NativeFabricExecutor::new(context, http), credential, wire)
}

#[tokio::test]
async fn shared_host_uses_actual_sdk_authentication_and_http_for_supported_fabric_profile() {
    // This exercises the shared seam, not a new executable Livy leaf or MWC adapter.
    let input = parse_args(["fab-cli-v2", "list", SESSIONS, "--output", "result.json"]).unwrap();
    let service = LivyService::fabric_versioned(
        NativeEndpoint::new(Url::parse("https://api.fabric.microsoft.com/").unwrap()).unwrap(),
        "workspace",
        "lakehouse",
        "v1",
        "2023-12-01",
    )
    .unwrap();
    let resolver =
        ExplicitResolver::new(input.path.owner.clone(), ResolvedTarget::Lakehouse(service))
            .unwrap();
    let (executor, credential, wire) =
        production_stack(br#"{"sessions":[]}"#.to_vec(), Audience::Fabric);
    let executor = executor.unwrap();
    let temp = tempfile::tempdir().unwrap();
    let destination = Destination::check("result.json", temp.path()).unwrap();
    let end = deadline();
    let receipt = acquire(
        &input,
        &executor,
        &resolver,
        ReadOptions::default(),
        &destination,
        end,
        pending(),
    )
    .await
    .unwrap()
    .deliver(destination, end, pending())
    .await
    .unwrap();
    assert_eq!(receipt.data, json!([]));
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method(), Method::Get);
    assert_eq!(
        requests[0].url().as_str(),
        "https://api.fabric.microsoft.com/v1/workspaces/workspace/lakehouses/lakehouse/livyApi/versions/2023-12-01/sessions?%24top=100"
    );
}

#[tokio::test]
async fn notebook_cannot_send_fabric_token_to_mwc_or_fabric_workload_route() {
    let input = parse_args(["fab-cli-v2", "get", CONTENT, "-o", "result.json"]).unwrap();
    let resolver = ExplicitResolver::new(
        input.path.owner.clone(),
        ResolvedTarget::Notebook(notebook()),
    )
    .unwrap();
    let (executor, credential, wire) = production_stack(Vec::new(), Audience::Fabric);
    let executor = executor.unwrap();
    let temp = tempfile::tempdir().unwrap();
    let destination = Destination::check("result.json", temp.path()).unwrap();
    assert!(
        acquire(
            &input,
            &executor,
            &resolver,
            ReadOptions::default(),
            &destination,
            deadline(),
            pending()
        )
        .await
        .is_err()
    );
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());
    let (executor, credential, wire) = production_stack(Vec::new(), Audience::OneLake);
    assert!(executor.is_err());
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn changed_human_owner_and_wrong_type_fail_before_credentials_or_http() {
    let input = parse_args(["fab-cli-v2", "get", CONTENT, "-o", "result.json"]).unwrap();
    let mut other = input.path.owner.clone();
    other.workspace = "Foreign.Workspace".into();
    let resolver = ExplicitResolver::new(other, ResolvedTarget::Notebook(notebook())).unwrap();
    let mut wrong = input.path.owner.clone();
    wrong.kind = OwnerKind::Lakehouse;
    assert!(ExplicitResolver::new(wrong, ResolvedTarget::Notebook(notebook())).is_err());
    let (executor, credential, wire) = production_stack(Vec::new(), Audience::Fabric);
    let executor = executor.unwrap();
    let temp = tempfile::tempdir().unwrap();
    let destination = Destination::check("result.json", temp.path()).unwrap();
    assert!(
        acquire(
            &input,
            &executor,
            &resolver,
            ReadOptions::default(),
            &destination,
            deadline(),
            pending()
        )
        .await
        .is_err()
    );
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn original_deadline_and_latched_cancellation_prevent_preflight_and_acquisition() {
    let temp = tempfile::tempdir().unwrap();
    let expired = Instant::now() - Duration::from_secs(1);
    let result =
        prepare_destination("result.json".into(), temp.path().into(), expired, pending()).await;
    assert_eq!(result.err().unwrap().kind, ReadErrorKind::Timeout);
    let result = prepare_destination(
        "result.json".into(),
        temp.path().into(),
        deadline(),
        async {},
    )
    .await;
    assert_eq!(result.err().unwrap().kind, ReadErrorKind::Cancelled);
    assert!(!temp.path().join("result.json").exists());
}

#[tokio::test]
async fn expired_or_cancelled_delivery_does_not_create_file_or_replay_http() {
    for cancel in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let input = parse_args(["fab-cli-v2", "get", CONTENT, "-o", "result.json"]).unwrap();
        let resolver = ExplicitResolver::new(
            input.path.owner.clone(),
            ResolvedTarget::Notebook(notebook()),
        )
        .unwrap();
        let executor = ScriptedNotebook {
            status: 200,
            body: serde_json::to_vec(&document()).unwrap(),
            calls: AtomicUsize::new(0),
        };
        let destination = Destination::check("result.json", temp.path()).unwrap();
        let read = acquire(
            &input,
            &executor,
            &resolver,
            ReadOptions::default(),
            &destination,
            deadline(),
            pending(),
        )
        .await
        .unwrap();
        let end = if cancel {
            deadline()
        } else {
            Instant::now() - Duration::from_secs(1)
        };
        let error = read
            .deliver(destination, end, async move {
                if !cancel {
                    pending::<()>().await;
                }
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            if cancel {
                ReadErrorKind::Cancelled
            } else {
                ReadErrorKind::Timeout
            }
        );
        assert!(error.read_completed);
        assert!(!error.file_created);
        assert!(!temp.path().join("result.json").exists());
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn pending_sdk_future_is_dropped_on_cancellation_and_original_deadline() {
    struct Waiting(Arc<AtomicUsize>);
    impl NativeExecutor for Waiting {
        fn execute<'a>(
            &'a self,
            _: &'a NativeRequest,
            _: usize,
        ) -> Pin<Box<dyn Future<Output = Result<NativeResponse, NativeError>> + Send + 'a>>
        {
            Box::pin(async move {
                struct DropCount(Arc<AtomicUsize>);
                impl Drop for DropCount {
                    fn drop(&mut self) {
                        self.0.fetch_add(1, Ordering::SeqCst);
                    }
                }
                let _guard = DropCount(self.0.clone());
                pending().await
            })
        }
    }
    for cancel in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let input = parse_args(["fab-cli-v2", "get", CONTENT, "-o", "result.json"]).unwrap();
        let resolver = ExplicitResolver::new(
            input.path.owner.clone(),
            ResolvedTarget::Notebook(notebook()),
        )
        .unwrap();
        let dropped = Arc::new(AtomicUsize::new(0));
        let executor = Waiting(dropped.clone());
        let destination = Destination::check("result.json", temp.path()).unwrap();
        let end = Instant::now() + Duration::from_millis(if cancel { 500 } else { 20 });
        let signal = async move {
            if cancel {
                tokio::time::sleep(Duration::from_millis(10)).await;
            } else {
                pending::<()>().await;
            }
        };
        let error = acquire(
            &input,
            &executor,
            &resolver,
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
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(!temp.path().join("result.json").exists());
    }
}

#[tokio::test]
async fn wire_and_selected_data_budgets_reject_without_clipped_success() {
    for (options, max_bytes) in [
        (
            ReadOptions {
                max_response_bytes: 8,
                ..ReadOptions::default()
            },
            "1048576",
        ),
        (
            ReadOptions {
                max_data_bytes: 8,
                ..ReadOptions::default()
            },
            "8",
        ),
        (
            ReadOptions {
                max_receipt_bytes: 8,
                ..ReadOptions::default()
            },
            "1048576",
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let input = parse_args([
            "fab-cli-v2",
            "get",
            CONTENT,
            "-o",
            "result.json",
            "--max-bytes",
            max_bytes,
        ])
        .unwrap();
        let resolver = ExplicitResolver::new(
            input.path.owner.clone(),
            ResolvedTarget::Notebook(notebook()),
        )
        .unwrap();
        let executor = ScriptedNotebook {
            status: 200,
            body: serde_json::to_vec(&document()).unwrap(),
            calls: AtomicUsize::new(0),
        };
        let destination = Destination::check("result.json", temp.path()).unwrap();
        let error = acquire(
            &input,
            &executor,
            &resolver,
            options,
            &destination,
            deadline(),
            pending(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind, ReadErrorKind::BudgetExceeded);
        assert!(!temp.path().join("result.json").exists());
    }
}

#[tokio::test]
async fn actual_http_stream_bounds_errors_and_shapes_never_deliver_empty_success() {
    use azure_core::{Bytes, error::ErrorKind};
    use futures_util::stream;
    for (response, options, expected) in [
        (
            AsyncRawResponse::from_bytes(
                200.into(),
                Headers::new(),
                br#"{"sessions":[]}"#.to_vec(),
            ),
            ReadOptions {
                max_response_bytes: 8,
                ..ReadOptions::default()
            },
            ReadErrorKind::BudgetExceeded,
        ),
        (
            AsyncRawResponse::new(
                200.into(),
                Headers::new(),
                Box::pin(stream::iter([
                    Ok(Bytes::from_static(br#"{"sessions":["#)),
                    Err(azure_core::Error::with_message(
                        ErrorKind::Io,
                        "private-stream-error",
                    )),
                ])),
            ),
            ReadOptions::default(),
            ReadErrorKind::InvalidResponse,
        ),
        (
            AsyncRawResponse::from_bytes(200.into(), Headers::new(), b"{}".to_vec()),
            ReadOptions::default(),
            ReadErrorKind::InvalidResponse,
        ),
        (
            AsyncRawResponse::from_bytes(
                403.into(),
                Headers::new(),
                b"private-denied-body".to_vec(),
            ),
            ReadOptions::default(),
            ReadErrorKind::Http,
        ),
    ] {
        let input =
            parse_args(["fab-cli-v2", "list", SESSIONS, "--output", "result.json"]).unwrap();
        let service = LivyService::fabric(
            NativeEndpoint::new(Url::parse("https://api.fabric.microsoft.com/").unwrap()).unwrap(),
            "workspace",
            "lakehouse",
        )
        .unwrap();
        let resolver =
            ExplicitResolver::new(input.path.owner.clone(), ResolvedTarget::Lakehouse(service))
                .unwrap();
        let (executor, credential, wire) = production_response(response, Audience::Fabric);
        let executor = executor.unwrap();
        let temp = tempfile::tempdir().unwrap();
        let destination = Destination::check("result.json", temp.path()).unwrap();
        let error = acquire(
            &input,
            &executor,
            &resolver,
            options,
            &destination,
            deadline(),
            pending(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.kind, expected);
        assert_eq!(credential.0.load(Ordering::SeqCst), 1);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        assert!(!temp.path().join("result.json").exists());
    }
}

#[tokio::test]
async fn original_deadline_drops_actual_pending_http_body_without_retry() {
    use futures_util::stream;
    let response = AsyncRawResponse::new(200.into(), Headers::new(), Box::pin(stream::pending()));
    let (executor, credential, wire) = production_response(response, Audience::Fabric);
    let executor = executor.unwrap();
    let input = parse_args(["fab-cli-v2", "list", SESSIONS, "--output", "result.json"]).unwrap();
    let service = LivyService::fabric(
        NativeEndpoint::new(Url::parse("https://api.fabric.microsoft.com/").unwrap()).unwrap(),
        "workspace",
        "lakehouse",
    )
    .unwrap();
    let resolver =
        ExplicitResolver::new(input.path.owner.clone(), ResolvedTarget::Lakehouse(service))
            .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let destination = Destination::check("result.json", temp.path()).unwrap();
    let end = Instant::now() + Duration::from_millis(20);
    let error = acquire(
        &input,
        &executor,
        &resolver,
        ReadOptions::default(),
        &destination,
        end,
        pending(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, ReadErrorKind::Timeout);
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
    assert!(!temp.path().join("result.json").exists());
}

#[test]
fn invalid_scope_ids_fail_before_token() {
    for id in ["", "..", "notebook/foreign", "bad?query", "bad%2fescape"] {
        assert!(
            NotebookRuntime::new(
                NativeEndpoint::new(
                    Url::parse("https://selected.pbidedicated.windows.net/").unwrap()
                )
                .unwrap(),
                "capacity",
                "workspace",
                id,
                ContentLimits::default(),
                1024,
                100,
            )
            .is_err()
        );
    }
}

#[test]
fn cancelled_queued_delivery_is_aborted_before_file_creation() {
    struct Release(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let input = parse_args(["fab-cli-v2", "get", CONTENT, "-o", "result.json"]).unwrap();
        let resolver = ExplicitResolver::new(
            input.path.owner.clone(),
            ResolvedTarget::Notebook(notebook()),
        )
        .unwrap();
        let executor = ScriptedNotebook {
            status: 200,
            body: serde_json::to_vec(&document()).unwrap(),
            calls: AtomicUsize::new(0),
        };
        let destination = Destination::check("result.json", temp.path()).unwrap();
        let end = deadline();
        let read = acquire(
            &input,
            &executor,
            &resolver,
            ReadOptions::default(),
            &destination,
            end,
            pending(),
        )
        .await
        .unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let release = Release(Some(release));
        let (started, started_wait) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            wait.recv().unwrap();
        });
        started_wait.await.unwrap();
        let error = read
            .deliver(destination, end, async {
                tokio::time::sleep(Duration::from_millis(10)).await;
            })
            .await
            .unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::Cancelled);
        assert!(!error.file_created);
        drop(release);
        blocker.await.unwrap();
        tokio::task::spawn_blocking(|| ()).await.unwrap();
        assert!(!temp.path().join("result.json").exists());
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    });
    runtime.shutdown_timeout(Duration::from_millis(100));
}

#[tokio::test]
async fn accepted_receipt_survives_delivery_failure_without_claiming_read_completion() {
    let input = parse_args(["fab-cli-v2", "list", SESSIONS, "--output", "result.json"]).unwrap();
    let service = LivyService::fabric_versioned(
        NativeEndpoint::new(Url::parse("https://api.fabric.microsoft.com/").unwrap()).unwrap(),
        "workspace",
        "lakehouse",
        "v1",
        "2023-12-01",
    )
    .unwrap();
    let resolver =
        ExplicitResolver::new(input.path.owner.clone(), ResolvedTarget::Lakehouse(service))
            .unwrap();
    let (executor, credential, wire) = production_response(
        AsyncRawResponse::from_bytes(202.into(), Headers::new(), Vec::new()),
        Audience::Fabric,
    );
    let executor = executor.unwrap();
    let temp = tempfile::tempdir().unwrap();
    let destination = Destination::check("result.json", temp.path()).unwrap();
    let end = deadline();
    let read = acquire(
        &input,
        &executor,
        &resolver,
        ReadOptions::default(),
        &destination,
        end,
        pending(),
    )
    .await
    .unwrap();
    std::fs::write(temp.path().join("result.json"), "racing owner").unwrap();
    let error = read.deliver(destination, end, pending()).await.unwrap_err();
    assert!(!error.read_completed);
    assert_eq!(error.http_status, Some(202));
    let receipt = error.receipt.unwrap();
    assert_eq!(receipt.status, fabric_cli::runtime_v2::ReadStatus::Accepted);
    assert_eq!(receipt.data, Value::Null);
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("result.json")).unwrap(),
        "racing owner"
    );
}
