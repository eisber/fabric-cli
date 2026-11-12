// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parser -> Livy leaf -> shared native host -> SDK -> production HTTP, all offline.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, Url, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::runtime_v2::{
    ReadCoverage, ReadErrorKind, ReadOptions, ReadReceipt, ReadStatus,
    host::{self, Destination, HostError},
    livy_list::{self, FABRIC_API_VERSION, PreparedList},
};
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin, TokenCredential,
    livy::LivyService,
    runtime::{NativeEndpoint, NativeError, NativeExecutor, NativeRequest, NativeResponse},
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use futures_util::Stream;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    ffi::OsString,
    future::{Future, pending},
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

const LAKEHOUSE: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Lakehouse/sessions";
const NATIVE: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Native.LivyService/sessions";
const BASE: &str = "https://msitapi.fabric.microsoft.com/v1/workspaces/11111111-1111-1111-1111-111111111111/lakehouses/22222222-2222-2222-2222-222222222222/livyApi/versions/2023-12-01/sessions";

fn end() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn prepared(path: &str, limit: &str, options: ReadOptions) -> PreparedList {
    PreparedList::parse(
        [
            "fab-cli-v2",
            "list",
            path,
            "--limit",
            limit,
            "--output",
            "result.json",
        ],
        options,
    )
    .unwrap()
}

fn endpoint() -> EndpointBinding {
    EndpointBinding::with_host(ServiceOrigin::Fabric, "msitapi.fabric.microsoft.com").unwrap()
}

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
    waiting: bool,
    dropped: Arc<AtomicUsize>,
}

struct Dropped(Arc<AtomicUsize>);

impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        assert!(options.is_none());
        if self.waiting {
            let _guard = Dropped(self.dropped.clone());
            pending::<()>().await;
        }
        Ok(AccessToken::new(
            "offline-livy-test-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(async move {
            if delay == ExecutionOptions::default().timeout {
                pending::<()>().await;
            }
        })
    }
}

#[derive(Debug)]
enum Reply {
    Bytes(u16, Vec<u8>),
    Stream(Body),
    Waiting(Arc<AtomicUsize>),
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
            .expect("unexpected HTTP replay");
        let mut headers = Headers::new();
        headers.insert("content-type", "application/json");
        Ok(match reply {
            Reply::Bytes(status, bytes) => {
                AsyncRawResponse::from_bytes(status.into(), headers, bytes)
            }
            Reply::Stream(body) => AsyncRawResponse::new(200.into(), headers, Box::pin(body)),
            Reply::Waiting(dropped) => {
                let _guard = Dropped(dropped);
                pending().await
            }
        })
    }
}

#[derive(Debug)]
struct Body {
    polls: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    waiting: bool,
}

impl Stream for Body {
    type Item = azure_core::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        if self.waiting {
            Poll::Pending
        } else {
            Poll::Ready(Some(Ok(Bytes::from_static(b"0123456789"))))
        }
    }
}

impl Drop for Body {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

struct Stack {
    credential: Arc<Credential>,
    wire: Arc<Wire>,
}

impl Stack {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self {
            credential: Arc::new(Credential::default()),
            wire: Arc::new(Wire {
                replies: Mutex::new(replies.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
            }),
        }
    }

    fn json(status: u16, value: Value) -> Self {
        Self::new([Reply::Bytes(status, serde_json::to_vec(&value).unwrap())])
    }

    fn context(&self, audience: Audience, endpoint: EndpointBinding) -> RequestContext {
        RequestContext::with_endpoint(
            self.credential.clone(),
            CredentialSelection::new("selected-account", "selected-tenant", audience).unwrap(),
            endpoint,
        )
    }

    fn http(&self) -> HttpExecutor {
        HttpExecutor::new(
            self.wire.clone(),
            Arc::new(Timer),
            ExecutionOptions::default(),
        )
        .unwrap()
        .with_endpoint(endpoint())
    }

    async fn read(&self, prepared: PreparedList, cwd: &Path) -> Result<ReadReceipt, HostError> {
        let deadline = end();
        let destination = host::prepare_destination(
            prepared.invocation().output.clone(),
            cwd.into(),
            deadline,
            pending(),
        )
        .await?;
        prepared
            .fabric(self.context(Audience::Fabric, endpoint()), self.http())?
            .acquire(&destination, deadline, pending())
            .await?
            .deliver(destination, deadline, pending())
            .await
    }

    fn calls(&self) -> usize {
        self.wire.requests.lock().unwrap().len()
    }

    fn no_io(&self) {
        assert_eq!(self.credential.calls.load(Ordering::SeqCst), 0);
        assert_eq!(self.calls(), 0);
    }
}

#[test]
fn root_admission_claims_only_the_livy_collection_and_its_help() {
    for path in [LAKEHOUSE, NATIVE] {
        assert!(livy_list::claims(&args(&["list", path])));
        assert!(livy_list::claims(&args(&[
            "list",
            "--output",
            "result.json",
            path,
            "--help"
        ])));
        assert!(livy_list::claims(&args(&[
            "list",
            path,
            "--continuation",
            "unsupported"
        ])));
    }
    assert!(livy_list::claims(&args(&["list", "--help"])));
    for values in [
        vec!["get", LAKEHOUSE],
        vec!["list", "/Team.Workspace/N.Notebook/sessions"],
        vec!["artifact", "notebook", "list", LAKEHOUSE],
        vec!["list", "/Team.Workspace/N.Lakehouse/sessions/0"],
        vec!["list", "/Team.Workspace/N.Lakehouse/livy-history"],
        vec![
            "list",
            "/Team.Workspace/N.Lakehouse/sessions",
            "--output",
            LAKEHOUSE,
            "--bad",
        ],
        vec![
            "list",
            "--output",
            LAKEHOUSE,
            "/Team.Workspace/N.Notebook/sessions",
        ],
    ] {
        assert!(!livy_list::claims(&args(&values)), "{values:?}");
    }
}

#[test]
fn unsupported_options_and_invalid_selections_fail_before_binding_or_http() {
    let stack = Stack::json(200, json!({"sessions":[]}));
    for extra in [
        vec!["--limit", "0"],
        vec!["--limit", "1001"],
        vec!["--limit", "2147483648"],
        vec!["--max-bytes", "0"],
        vec!["--max-bytes", "1048577"],
        vec!["--continuation", "opaque"],
        vec!["--profile", "fabric"],
        vec!["--timeout", "30"],
        vec!["--all"],
    ] {
        let mut values = vec!["fab-cli-v2", "list", LAKEHOUSE];
        values.extend(extra);
        assert!(PreparedList::parse(values, ReadOptions::default()).is_err());
    }
    for path in [
        "/Team.Workspace/N.Notebook/sessions",
        "/Team.Workspace/N.Lakehouse/sessions/0",
        "/Team.Workspace/N.Lakehouse/sessions/../sessions",
        "/Team.Workspace/N.Lakehouse/sessions?size=2",
    ] {
        assert!(PreparedList::parse(["fab-cli-v2", "list", path], ReadOptions::default()).is_err());
    }
    assert!(PreparedList::parse(["fab-cli-v2", "get", LAKEHOUSE], ReadOptions::default()).is_err());
    stack.no_io();
}

#[test]
fn fabric_binding_requires_exact_profile_origin_guid_owner_and_audience() {
    let stack = Stack::json(200, json!({"sessions":[]}));
    for path in [
        "/Team.Workspace/N.Lakehouse/sessions",
        "/11111111-1111-1111-1111-111111111111.Workspace/N.Lakehouse/sessions",
        "/11111111-1111-1111-1111-111111111111.Workspace.Workspace/22222222-2222-2222-2222-222222222222.Lakehouse/sessions",
        NATIVE,
    ] {
        assert!(
            prepared(path, "1", ReadOptions::default())
                .bind_fabric(&endpoint())
                .is_err()
        );
    }
    assert!(
        prepared(LAKEHOUSE, "1", ReadOptions::default())
            .bind_fabric(&EndpointBinding::public(ServiceOrigin::PowerBi))
            .is_err()
    );
    for (audience, selected) in [
        (Audience::OneLake, endpoint()),
        (
            Audience::Fabric,
            EndpointBinding::public(ServiceOrigin::Fabric),
        ),
    ] {
        assert!(
            prepared(LAKEHOUSE, "1", ReadOptions::default())
                .fabric(stack.context(audience, selected), stack.http())
                .is_err()
        );
    }
    let service = LivyService::fabric(
        NativeEndpoint::new(endpoint().base_url()).unwrap(),
        "ws",
        "lake",
    )
    .unwrap();
    assert!(
        prepared(NATIVE, "1", ReadOptions::default())
            .bind(service)
            .is_err()
    );
    stack.no_io();
}

#[tokio::test]
async fn fabric_zero_single_and_multiple_rows_preserve_order_raw_state_and_provider_page() {
    for rows in [
        json!([]),
        json!([{"id":"7","state":"future_state","unknown":{"keep":null}}]),
        json!([{"id":7,"state":"error","errorInfo":[{"raw":["x"]}]},
               {"id":"3","state":"idle","future":[false,null]}]),
    ] {
        let stack = Stack::json(
            200,
            json!({"items":rows,"totalCountOfMatchedItems":47,"pageSize":2}),
        );
        let temp = tempfile::tempdir().unwrap();
        let receipt = stack
            .read(
                prepared(LAKEHOUSE, "2", ReadOptions::default()),
                temp.path(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.data, rows);
        assert_eq!(receipt.status, ReadStatus::ReadComplete);
        assert_eq!(receipt.meta.coverage, ReadCoverage::ProviderPage);
        assert_eq!(receipt.meta.provider_total, Some(47));
        assert_eq!(receipt.meta.continuation, None);
        assert_eq!(receipt.meta.workload_observation, None);
        assert_eq!(receipt.meta.workload_state, None);
        assert_eq!(receipt.target.native_url, format!("{BASE}?$top=2"));
        let bytes = std::fs::read(temp.path().join("result.json")).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let encoded: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(encoded["data"].is_array());
        assert_eq!(encoded["data"], rows);
        let requests = stack.wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), Method::Get);
        assert_eq!(requests[0].url().as_str(), format!("{BASE}?$top=2"));
        assert_eq!(
            requests[0]
                .headers()
                .get_optional_str(&"accept-encoding".into()),
            Some("identity")
        );
        assert!(
            requests[0]
                .headers()
                .get_optional_str(&"x-requested-by".into())
                .is_none()
        );
        assert_eq!(stack.credential.calls.load(Ordering::SeqCst), 1);
        assert_eq!(FABRIC_API_VERSION, "2023-12-01");
    }
}

#[derive(Default)]
struct NativeScript {
    requests: Mutex<Vec<NativeRequest>>,
    body: Value,
}

impl NativeExecutor for NativeScript {
    fn execute<'a>(
        &'a self,
        request: &'a NativeRequest,
        max_response_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<NativeResponse, NativeError>> + Send + 'a>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            let body = serde_json::to_vec(&self.body).unwrap();
            assert!(body.len() <= max_response_bytes);
            Ok(NativeResponse {
                status: 200,
                body,
                content_type: Some("application/json".into()),
                headers: Default::default(),
            })
        })
    }
}

#[tokio::test]
async fn native_lakehouse_and_livyservice_are_one_list_operation_with_size_and_native_headers() {
    for path in [LAKEHOUSE, NATIVE] {
        for rows in [
            json!([]),
            json!([{"id":0,"state":"future"}]),
            json!([{"id":2},{"id":"1","unknown":true}]),
        ] {
            let input = prepared(path, "2", ReadOptions::default());
            let service = LivyService::native(
                NativeEndpoint::new(
                    Url::parse("https://selected.example.invalid/classic/").unwrap(),
                )
                .unwrap(),
            );
            let resolver = input.bind(service).unwrap();
            let script = NativeScript {
                body: json!({"sessions":rows,"total":19}),
                ..Default::default()
            };
            let temp = tempfile::tempdir().unwrap();
            let destination = Destination::check("result.json", temp.path()).unwrap();
            let deadline = end();
            let receipt = host::acquire(
                input.invocation(),
                &script,
                &resolver,
                ReadOptions::default(),
                &destination,
                deadline,
                pending(),
            )
            .await
            .unwrap()
            .deliver(destination, deadline, pending())
            .await
            .unwrap();
            assert_eq!(receipt.data, rows);
            assert_eq!(receipt.meta.provider_total, Some(19));
            assert_eq!(receipt.meta.continuation, None);
            assert_eq!(receipt.operation_id.id(), "fabric.runtime.list");
            let requests = script.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(
                requests[0].url().as_str(),
                "https://selected.example.invalid/classic/sessions?size=2"
            );
            assert_eq!(
                requests[0].headers()["x-requested-by"],
                "fabric-sdk-rust-livy"
            );
            assert_eq!(requests[0].headers()["accept-encoding"], "identity");
        }
    }
}

#[tokio::test]
async fn missing_provider_total_is_null_without_fabricated_continuation_or_history_claim() {
    let stack = Stack::json(
        200,
        json!({"sessions": [], "nextLink": "opaque-provider-page"}),
    );
    let temp = tempfile::tempdir().unwrap();
    let receipt = stack
        .read(
            prepared(LAKEHOUSE, "1", ReadOptions::default()),
            temp.path(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.meta.provider_total, None);
    assert_eq!(receipt.meta.continuation, None);
    assert_eq!(receipt.meta.coverage, ReadCoverage::ProviderPage);
    assert_eq!(receipt.data, json!([]));
    assert_eq!(stack.calls(), 1);
}

#[tokio::test]
async fn injected_native_origin_cannot_borrow_fabric_authorization_or_change_human_owner() {
    use fabric_sdk_http::native_runtime::NativeFabricExecutor;

    let stack = Stack::json(200, json!({"sessions": []}));
    let input = prepared(LAKEHOUSE, "1", ReadOptions::default());
    let temp = tempfile::tempdir().unwrap();
    let destination = Destination::check("result.json", temp.path()).unwrap();
    let executor =
        NativeFabricExecutor::new(stack.context(Audience::Fabric, endpoint()), stack.http())
            .unwrap();
    for (owner, origin) in [
        (
            LAKEHOUSE.to_owned(),
            "https://foreign.pbidedicated.windows.net/classic",
        ),
        (
            LAKEHOUSE.to_owned(),
            "https://api.fabric.microsoft.com/classic",
        ),
        (
            LAKEHOUSE.replace("22222222", "33333333"),
            "https://msitapi.fabric.microsoft.com/classic",
        ),
    ] {
        let resolver = prepared(&owner, "1", ReadOptions::default())
            .bind(LivyService::native(
                NativeEndpoint::new(Url::parse(origin).unwrap()).unwrap(),
            ))
            .unwrap();
        let error = host::acquire(
            input.invocation(),
            &executor,
            &resolver,
            ReadOptions::default(),
            &destination,
            end(),
            pending(),
        )
        .await
        .err()
        .unwrap();
        assert!(matches!(
            error.kind,
            ReadErrorKind::UnsupportedCapability | ReadErrorKind::InvalidInput
        ));
        stack.no_io();
    }
}

#[tokio::test]
async fn over_limit_page_is_rejected_whole_instead_of_clipped_or_written() {
    let stack = Stack::json(200, json!({"sessions":[{"id":1},{"id":2}]}));
    let temp = tempfile::tempdir().unwrap();
    let error = stack
        .read(
            prepared(LAKEHOUSE, "1", ReadOptions::default()),
            temp.path(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::BudgetExceeded);
    assert!(!temp.path().join("result.json").exists());
    assert_eq!(stack.calls(), 1);
}

#[tokio::test]
async fn malformed_shapes_and_known_wrong_types_retain_status_without_empty_success() {
    for body in [
        b"{".to_vec(),
        b"{}".to_vec(),
        b"[]".to_vec(),
        br#"{"sessions":null}"#.to_vec(),
        br#"{"sessions":[1]}"#.to_vec(),
        br#"{"sessions":[{"state":false}]}"#.to_vec(),
        br#"{"sessions":[{"errorInfo":{"raw":["x"]}}]}"#.to_vec(),
        br#"{"sessions":[],"total":-1}"#.to_vec(),
    ] {
        let stack = Stack::new([Reply::Bytes(200, body)]);
        let temp = tempfile::tempdir().unwrap();
        let error = stack
            .read(
                prepared(LAKEHOUSE, "2", ReadOptions::default()),
                temp.path(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::InvalidResponse);
        assert_eq!(error.http_status, Some(200));
        assert!(error.receipt.is_none());
        assert!(!temp.path().join("result.json").exists());
        assert_eq!(stack.calls(), 1);
    }
}

#[tokio::test]
async fn accepted_empty_body_stays_null_not_completed_collection_or_readiness() {
    let stack = Stack::new([Reply::Bytes(202, Vec::new())]);
    let temp = tempfile::tempdir().unwrap();
    let receipt = stack
        .read(
            prepared(LAKEHOUSE, "1", ReadOptions::default()),
            temp.path(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.status, ReadStatus::Accepted);
    assert_eq!(receipt.meta.coverage, ReadCoverage::Accepted);
    assert_eq!(receipt.meta.http_status, 202);
    assert_eq!(receipt.meta.workload_observation, None);
    assert_eq!(receipt.meta.provider_total, None);
    assert_eq!(receipt.meta.continuation, None);
    assert_eq!(receipt.data, Value::Null);
    assert_eq!(stack.calls(), 1);
}

#[tokio::test]
async fn denied_and_throttled_reads_retain_real_status_and_shared_bounded_retry_policy() {
    for status in [401, 403, 429] {
        let attempts = if status == 429 { 4 } else { 1 };
        let stack = Stack::new(
            (0..attempts).map(|_| Reply::Bytes(status, b"private-provider-body".to_vec())),
        );
        let temp = tempfile::tempdir().unwrap();
        let error = stack
            .read(
                prepared(LAKEHOUSE, "1", ReadOptions::default()),
                temp.path(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.http_status, Some(status));
        assert!(!error.read_completed);
        assert!(!format!("{error:?}: {error}").contains("private-provider-body"));
        assert_eq!(stack.calls(), attempts);
        assert_eq!(stack.credential.calls.load(Ordering::SeqCst), 1);
        assert!(!temp.path().join("result.json").exists());
    }
}

#[tokio::test]
async fn data_and_complete_receipt_bounds_never_create_partial_success() {
    for options in [
        ReadOptions {
            max_data_bytes: 1048576,
            ..ReadOptions::default()
        },
        ReadOptions {
            max_receipt_bytes: 64,
            ..ReadOptions::default()
        },
    ] {
        let stack = Stack::json(
            200,
            json!({"sessions":[{"id":1,"unknown":"retained data"}]}),
        );
        let temp = tempfile::tempdir().unwrap();
        let input = if options.max_receipt_bytes == 64 {
            prepared(LAKEHOUSE, "1", options)
        } else {
            PreparedList::parse(
                [
                    "fab-cli-v2",
                    "list",
                    LAKEHOUSE,
                    "--max-bytes",
                    "2",
                    "--output",
                    "result.json",
                ],
                options,
            )
            .unwrap()
        };
        let error = stack.read(input, temp.path()).await.unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::BudgetExceeded);
        assert_eq!(error.http_status, Some(200));
        assert_eq!(stack.calls(), 1);
        assert!(!temp.path().join("result.json").exists());
    }
}

#[tokio::test]
async fn streaming_response_bound_stops_without_reading_or_delivering_the_whole_body() {
    let polls = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let stack = Stack::new([Reply::Stream(Body {
        polls: polls.clone(),
        dropped: dropped.clone(),
        waiting: false,
    })]);
    let temp = tempfile::tempdir().unwrap();
    let options = ReadOptions {
        max_response_bytes: 16,
        ..ReadOptions::default()
    };
    let error = stack
        .read(prepared(LAKEHOUSE, "1", options), temp.path())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::BudgetExceeded);
    assert_eq!(error.http_status, Some(200));
    assert!(polls.load(Ordering::SeqCst) <= 2);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(stack.calls(), 1);
    assert!(!temp.path().join("result.json").exists());
}

#[tokio::test]
async fn original_deadline_and_cancellation_drop_credential_http_or_body_without_remote_cancel() {
    for stage in ["credential", "http", "body"] {
        for cancel in [false, true] {
            let dropped = Arc::new(AtomicUsize::new(0));
            let mut stack = match stage {
                "http" => Stack::new([Reply::Waiting(dropped.clone())]),
                "body" => Stack::new([Reply::Stream(Body {
                    polls: Arc::default(),
                    dropped: dropped.clone(),
                    waiting: true,
                })]),
                _ => Stack::new([]),
            };
            if stage == "credential" {
                stack.credential = Arc::new(Credential {
                    waiting: true,
                    dropped: dropped.clone(),
                    ..Default::default()
                });
            }
            let temp = tempfile::tempdir().unwrap();
            let destination = Destination::check("result.json", temp.path()).unwrap();
            let deadline = Instant::now() + Duration::from_millis(40);
            let interrupted = async {
                if cancel {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                } else {
                    pending::<()>().await;
                }
            };
            let error = prepared(LAKEHOUSE, "1", ReadOptions::default())
                .fabric(stack.context(Audience::Fabric, endpoint()), stack.http())
                .unwrap()
                .acquire(&destination, deadline, interrupted)
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
            assert_eq!(
                error.http_status,
                if stage == "body" { Some(200) } else { None }
            );
            assert_eq!(dropped.load(Ordering::SeqCst), 1);
            assert_eq!(stack.calls(), usize::from(stage != "credential"));
            assert!(!temp.path().join("result.json").exists());
        }
    }
}

#[tokio::test]
async fn preflight_and_atomic_delivery_no_clobber_do_not_replay_http() {
    let stack = Stack::json(200, json!({"sessions":[]}));
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("result.json");
    std::fs::write(&path, "original owner").unwrap();
    let error = stack
        .read(
            prepared(LAKEHOUSE, "1", ReadOptions::default()),
            temp.path(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::OutputUnavailable);
    stack.no_io();
    std::fs::remove_file(&path).unwrap();
    let destination = Destination::check("result.json", temp.path()).unwrap();
    let deadline = end();
    let pending_read = prepared(LAKEHOUSE, "1", ReadOptions::default())
        .fabric(stack.context(Audience::Fabric, endpoint()), stack.http())
        .unwrap()
        .acquire(&destination, deadline, pending())
        .await
        .unwrap();
    std::fs::write(&path, "racing owner").unwrap();
    let error = pending_read
        .deliver(destination, deadline, pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::Delivery);
    assert!(error.read_completed);
    assert_eq!(error.http_status, Some(200));
    assert_eq!(error.receipt.as_ref().unwrap().data, json!([]));
    assert!(!error.file_created);
    assert_eq!(error.bytes_written, 0);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "racing owner");
    assert_eq!(stack.calls(), 1);
}
