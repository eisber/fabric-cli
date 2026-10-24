// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::TokenRequestOptions,
    error::ErrorKind,
    http::{AsyncRawResponse, StatusCode, headers::Headers},
    time::{Duration as AzureDuration, OffsetDateTime},
};
use fabric_sdk::{
    Client, CredentialSelection, TokenCredential,
    notebooks::{DefinitionPart, NotebookDefinition, NotebookErrorKind, NotebookLimits},
    workspaces::{WorkspaceListErrorKind, WorkspaceListLimits},
};
use futures_util::{Stream, stream};
use std::{
    collections::VecDeque,
    future::pending,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Waker},
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const OPERATION: &str = "22222222-2222-2222-2222-222222222222";
const ITEM: &str = "33333333-3333-3333-3333-333333333333";

#[derive(Default)]
struct TestTimer {
    waits: Mutex<Vec<Duration>>,
    expired: AtomicBool,
}

impl ExecutionTimer for TestTimer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        self.waits.lock().unwrap().push(delay);
        Box::pin(poll_fn(move |_| {
            if delay == Duration::from_secs(240) && !self.expired.load(Ordering::SeqCst) {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        }))
    }
}

#[derive(Debug)]
struct ScriptedClient {
    replies: Mutex<VecDeque<azure_core::Result<AsyncRawResponse>>>,
    requests: Mutex<Vec<Request>>,
}

impl ScriptedClient {
    fn new(replies: impl IntoIterator<Item = AsyncRawResponse>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().map(Ok).collect()),
            requests: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl HttpClient for ScriptedClient {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP replay")
    }
}

#[derive(Debug)]
struct TestCredential;

#[async_trait]
impl TokenCredential for TestCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        Ok(token())
    }
}

fn token() -> AccessToken {
    AccessToken {
        token: "test-only-never-sent-to-a-service".into(),
        expires_on: OffsetDateTime::now_utc() + AzureDuration::hours(1),
    }
}

fn sdk() -> Client {
    Client::new(
        Arc::new(TestCredential),
        CredentialSelection::new("offline", "tenant", fabric_core::Audience::Fabric).unwrap(),
    )
}

fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> AsyncRawResponse {
    let mut result = Headers::new();
    for &(name, value) in headers {
        result.insert(name.to_owned(), value.to_owned());
    }
    AsyncRawResponse::from_bytes(StatusCode::from(status), result, body.as_bytes().to_vec())
}

fn executor(client: Arc<dyn HttpClient>, timer: Arc<TestTimer>) -> HttpExecutor {
    HttpExecutor::new(client, timer, ExecutionOptions::default()).unwrap()
}

async fn get(executor: &HttpExecutor, limit: usize) -> Result<WorkspaceResponse, ExecutionError> {
    executor
        .execute(
            Method::Get,
            &Url::parse("https://api.fabric.microsoft.com/v1/workspaces").unwrap(),
            &token(),
            &[],
            NonZeroUsize::new(limit).unwrap(),
        )
        .await
}

#[tokio::test]
async fn headers_token_query_and_exact_body_are_preserved() {
    let client = ScriptedClient::new([response(
        202,
        "",
        &[
            (
                "Location",
                "https://api.fabric.microsoft.com/v1/operations/test",
            ),
            ("x-ms-operation-id", "opaque"),
            ("Retry-After", "7"),
        ],
    )]);
    let exec = executor(client.clone(), Arc::new(TestTimer::default()));
    let result = exec
        .execute(
            Method::Post,
            &Url::parse(
                "https://api.fabric.microsoft.com/v1/workspaces/ws/items?updateMetadata=True",
            )
            .unwrap(),
            &token(),
            br#"{"definition":{}}"#,
            NonZeroUsize::new(10).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.status, 202);
    assert!(result.body.is_empty());
    assert_eq!(result.headers["retry-after"], "7");
    assert_eq!(result.headers["x-ms-operation-id"], "opaque");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method(), Method::Post);
    assert_eq!(requests[0].url().query(), Some("updateMetadata=True"));
    assert_eq!(
        requests[0]
            .headers()
            .get_str(&"authorization".into())
            .unwrap(),
        "Bearer test-only-never-sent-to-a-service"
    );
    assert_eq!(
        requests[0]
            .headers()
            .get_str(&"content-type".into())
            .unwrap(),
        "application/json"
    );
    assert_eq!(
        Bytes::from(requests[0].body()),
        br#"{"definition":{}}"#.as_slice()
    );
    assert!(!format!("{exec:?}").contains("test-only"));
}

#[tokio::test]
async fn read_gateway_retries_match_python_backoff_and_budget() {
    for status in [502, 503, 504] {
        let client = ScriptedClient::new([
            response(status, "", &[]),
            response(status, "", &[]),
            response(status, "", &[]),
            response(200, "ok", &[]),
        ]);
        let timer = Arc::new(TestTimer::default());
        assert_eq!(
            get(&executor(client.clone(), timer.clone()), 2)
                .await
                .unwrap()
                .body,
            b"ok"
        );
        assert_eq!(client.requests.lock().unwrap().len(), 4);
        assert_eq!(
            *timer.waits.lock().unwrap(),
            [
                Duration::from_secs(240),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ]
        );
    }
}

#[tokio::test]
async fn independent_python_retry_traces_match_shared_execution() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/http/retry-cases.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let headers = case["retry_after"]
            .as_str()
            .map(|value| vec![("Retry-After", value)])
            .unwrap_or_default();
        let statuses = case["statuses"].as_array().unwrap();
        let client = ScriptedClient::new(statuses.iter().map(|status| {
            response(
                u16::try_from(status.as_u64().unwrap()).unwrap(),
                "",
                &headers,
            )
        }));
        let timer = Arc::new(TestTimer::default());
        let exec = executor(client.clone(), timer.clone());
        let method = match case["method"].as_str().unwrap() {
            "GET" => Method::Get,
            "POST" => Method::Post,
            other => panic!("unsupported reference method: {other}"),
        };
        let observed = exec
            .execute(
                method,
                &Url::parse("https://api.fabric.microsoft.com/v1/workspaces").unwrap(),
                &token(),
                &[],
                NonZeroUsize::new(1).unwrap(),
            )
            .await;
        let outcome = match observed {
            Ok(_) => "response",
            Err(ExecutionError::RetriesExhausted) => "exhausted",
            Err(other) => panic!("{}: {other}", case["name"]),
        };
        assert_eq!(
            outcome,
            case["outcome"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        assert_eq!(
            client.requests.lock().unwrap().len(),
            usize::try_from(case["requests"].as_u64().unwrap()).unwrap(),
            "{}",
            case["name"]
        );
        let waits: Vec<_> = timer
            .waits
            .lock()
            .unwrap()
            .iter()
            .skip(1)
            .map(Duration::as_secs)
            .collect();
        let expected: Vec<_> = case["wait_seconds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|seconds| seconds.as_u64().unwrap())
            .collect();
        assert_eq!(waits, expected, "{}", case["name"]);
    }
}

#[tokio::test]
async fn retry_after_takes_precedence_for_adapter_statuses() {
    for status in [413, 429, 503] {
        let client = ScriptedClient::new([
            response(status, "", &[("rEtRy-AfTeR", " 3 ")]),
            response(200, "ok", &[]),
        ]);
        let timer = Arc::new(TestTimer::default());
        assert!(get(&executor(client, timer.clone()), 2).await.is_ok());
        assert_eq!(
            *timer.waits.lock().unwrap(),
            [Duration::from_secs(240), Duration::from_secs(3),]
        );
    }
}

#[tokio::test]
async fn headerless_throttle_uses_four_outer_attempts_and_ten_second_waits() {
    let client = ScriptedClient::new((0..4).map(|_| response(429, "", &[])));
    let timer = Arc::new(TestTimer::default());
    assert_eq!(
        get(&executor(client.clone(), timer.clone()), 1)
            .await
            .unwrap_err(),
        ExecutionError::RetriesExhausted
    );
    assert_eq!(client.requests.lock().unwrap().len(), 4);
    assert_eq!(
        *timer.waits.lock().unwrap(),
        [
            Duration::from_secs(240),
            Duration::from_secs(10),
            Duration::from_secs(10),
            Duration::from_secs(10),
            Duration::from_secs(10),
        ]
    );
}

#[tokio::test]
async fn adapter_exhaustion_is_explicit_and_has_no_fifth_attempt() {
    for status in [502, 503, 504, 429] {
        let client =
            ScriptedClient::new((0..4).map(|_| response(status, "", &[("Retry-After", "1")])));
        assert_eq!(
            get(&executor(client.clone(), Arc::new(TestTimer::default())), 1)
                .await
                .unwrap_err(),
            ExecutionError::RetriesExhausted
        );
        assert_eq!(client.requests.lock().unwrap().len(), 4);
    }
}

#[tokio::test]
async fn posts_never_replay_on_throttling_gateway_error_or_lost_response() {
    for status in [413, 429, 502, 503, 504] {
        let client = ScriptedClient::new([response(status, "", &[("Retry-After", "1")])]);
        let exec = executor(client.clone(), Arc::new(TestTimer::default()));
        assert_eq!(
            exec.execute(
                Method::Post,
                &Url::parse("https://api.fabric.microsoft.com/v1/workspaces/ws/items").unwrap(),
                &token(),
                b"{}",
                NonZeroUsize::new(1).unwrap()
            )
            .await
            .unwrap()
            .status,
            status
        );
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
    let client = ScriptedClient::new([]);
    client
        .replies
        .lock()
        .unwrap()
        .push_back(Err(azure_core::Error::with_message(
            ErrorKind::Io,
            "secret transport diagnostic",
        )));
    let exec = executor(client.clone(), Arc::new(TestTimer::default()));
    let error = sdk()
        .notebook_session(&exec, NotebookLimits::default(), pending())
        .create(WORKSPACE, "Test", "", &definition())
        .await
        .unwrap_err();
    assert!(error.mutation_may_have_occurred);
    assert_eq!(
        error.kind,
        NotebookErrorKind::Execution(NotebookExecutionError::Failed)
    );
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    assert!(!error.to_string().contains("secret"));
}

#[tokio::test]
async fn forbidden_redirect_and_nonretryable_statuses_remain_single_raw_responses() {
    for status in [
        200, 201, 202, 204, 301, 302, 307, 308, 400, 401, 403, 404, 413, 500,
    ] {
        let client = ScriptedClient::new([response(status, "", &[])]);
        assert_eq!(
            get(&executor(client.clone(), Arc::new(TestTimer::default())), 1)
                .await
                .unwrap()
                .status,
            status
        );
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn invalid_or_excessive_retry_intervals_fail_before_sleeping_or_replay() {
    for delay in ["-1", "invalid", "61", "18446744073709551616"] {
        let client = ScriptedClient::new([response(503, "", &[("Retry-After", delay)])]);
        let timer = Arc::new(TestTimer::default());
        assert_eq!(
            get(&executor(client.clone(), timer.clone()), 1)
                .await
                .unwrap_err(),
            ExecutionError::RetryDelay
        );
        assert_eq!(client.requests.lock().unwrap().len(), 1);
        assert_eq!(timer.waits.lock().unwrap().len(), 1);
    }
}

struct CountingBody {
    polls: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
    pending: bool,
}

impl Stream for CountingBody {
    type Item = azure_core::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        if self.pending {
            Poll::Pending
        } else {
            Poll::Ready(Some(Ok(Bytes::from_static(b"x"))))
        }
    }
}

impl Drop for CountingBody {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn response_limit_stops_infinite_stream_at_cap_plus_one_and_drops_it() {
    let polls = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let response = AsyncRawResponse::new(
        StatusCode::Ok,
        Headers::new(),
        Box::pin(CountingBody {
            polls: polls.clone(),
            dropped: dropped.clone(),
            pending: false,
        }),
    );
    let exec = executor(
        ScriptedClient::new([response]),
        Arc::new(TestTimer::default()),
    );
    let limits = WorkspaceListLimits {
        max_response_bytes: NonZeroUsize::new(5).unwrap(),
        ..Default::default()
    };
    let error = sdk()
        .list_workspaces(&exec, limits, pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::ResponseLimit);
    assert!(error.partial.value.is_empty());
    assert_eq!(polls.load(Ordering::SeqCst), 6);
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn exact_byte_boundary_succeeds_and_plus_one_fails() {
    for (body, expected) in [
        ("12345", Ok(())),
        ("123456", Err(ExecutionError::ResponseLimit)),
    ] {
        let exec = executor(
            ScriptedClient::new([response(200, body, &[])]),
            Arc::new(TestTimer::default()),
        );
        assert_eq!(get(&exec, 5).await.map(|_| ()), expected);
    }
}

#[tokio::test]
async fn stream_io_errors_are_sanitized_and_not_retried() {
    let raw = AsyncRawResponse::new(
        StatusCode::Ok,
        Headers::new(),
        Box::pin(stream::iter([Err(azure_core::Error::with_message(
            ErrorKind::Io,
            "secret body error",
        ))])),
    );
    let client = ScriptedClient::new([raw]);
    let error = get(&executor(client.clone(), Arc::new(TestTimer::default())), 5)
        .await
        .unwrap_err();
    assert_eq!(error, ExecutionError::Transport);
    assert!(!error.to_string().contains("secret"));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn deadline_covers_response_stream_and_drops_it_without_replay() {
    let dropped = Arc::new(AtomicBool::new(false));
    let raw = AsyncRawResponse::new(
        StatusCode::Ok,
        Headers::new(),
        Box::pin(CountingBody {
            polls: Arc::new(AtomicUsize::new(0)),
            dropped: dropped.clone(),
            pending: true,
        }),
    );
    let client = ScriptedClient::new([raw]);
    let timer = Arc::new(TestTimer::default());
    let exec = executor(client.clone(), timer.clone());
    let mut future = Box::pin(get(&exec, 5));
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    timer.expired.store(true, Ordering::SeqCst);
    assert_eq!(future.await.unwrap_err(), ExecutionError::TimedOut);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

struct DeadlineOnSecondPoll;

impl ExecutionTimer for DeadlineOnSecondPoll {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        let mut first = true;
        Box::pin(poll_fn(move |cx| {
            if first {
                first = false;
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        }))
    }
}

#[tokio::test]
async fn always_ready_empty_chunks_cannot_starve_the_deadline() {
    let polls = Arc::new(AtomicUsize::new(0));
    let seen = polls.clone();
    let raw = AsyncRawResponse::new(
        StatusCode::Ok,
        Headers::new(),
        Box::pin(stream::repeat_with(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(Bytes::new())
        })),
    );
    let exec = HttpExecutor::new(
        ScriptedClient::new([raw]),
        Arc::new(DeadlineOnSecondPoll),
        ExecutionOptions::default(),
    )
    .unwrap();
    assert_eq!(get(&exec, 5).await.unwrap_err(), ExecutionError::TimedOut);
    assert_eq!(polls.load(Ordering::SeqCst), 64);
}

#[tokio::test]
async fn cancellation_drops_body_and_preserves_workspace_progress() {
    let dropped = Arc::new(AtomicBool::new(false));
    let raw = AsyncRawResponse::new(
        StatusCode::Ok,
        Headers::new(),
        Box::pin(CountingBody {
            polls: Arc::new(AtomicUsize::new(0)),
            dropped: dropped.clone(),
            pending: true,
        }),
    );
    let client = ScriptedClient::new([
        response(200, r#"{"value":[],"continuationToken":"next"}"#, &[]),
        raw,
    ]);
    let exec = executor(client.clone(), Arc::new(TestTimer::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    let sdk = sdk();
    let mut future = Box::pin(sdk.list_workspaces(
        &exec,
        WorkspaceListLimits::default(),
        poll_fn(move |_| {
            if signal.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }),
    ));
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    cancel.store(true, Ordering::SeqCst);
    let error = future.await.unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::Cancelled);
    assert_eq!(error.partial.pages_received, 1);
    assert_eq!(error.partial.next_token.as_deref(), Some("next"));
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}

fn definition() -> NotebookDefinition {
    NotebookDefinition {
        format: None,
        parts: vec![DefinitionPart::inline("notebook-content.py", b"pass").unwrap()],
    }
}

#[tokio::test]
async fn notebook_empty_202_polls_retry_after_and_fetches_actual_result_through_shared_http() {
    let location = format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}");
    let result_location = format!("{location}/result");
    let item = format!(r#"{{"id":"{ITEM}","displayName":"Test","type":"Notebook"}}"#);
    let client = ScriptedClient::new([
        response(
            202,
            "",
            &[
                ("Location", &location),
                ("x-ms-operation-id", OPERATION),
                ("Retry-After", "2"),
            ],
        ),
        response(200, r#"{"status":"Running"}"#, &[("Retry-After", "3")]),
        response(
            200,
            r#"{"status":"Succeeded"}"#,
            &[("Location", &result_location)],
        ),
        response(200, &item, &[]),
    ]);
    let timer = Arc::new(TestTimer::default());
    let exec = executor(client.clone(), timer.clone());
    let sdk = sdk();
    let mut session = sdk.notebook_session(&exec, NotebookLimits::default(), pending());
    assert_eq!(
        session
            .create(WORKSPACE, "Test", "", &definition())
            .await
            .unwrap()
            .id,
        ITEM
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].method(), Method::Post);
    assert!(
        requests[1..]
            .iter()
            .all(|request| request.method() == Method::Get)
    );
    assert_eq!(requests[3].url().as_str(), result_location);
    let body: serde_json::Value = serde_json::from_slice(&Bytes::from(requests[0].body())).unwrap();
    assert_eq!(body["type"], "Notebook");
    assert_eq!(body["definition"]["parts"][0]["payload"], "cGFzcw==");
    let waits = timer.waits.lock().unwrap();
    assert_eq!(
        waits
            .iter()
            .filter(|&&delay| delay == Duration::from_secs(2))
            .count(),
        1
    );
    assert_eq!(
        waits
            .iter()
            .filter(|&&delay| delay == Duration::from_secs(3))
            .count(),
        1
    );
}

#[tokio::test]
async fn notebook_byte_limit_is_explicit_and_retains_mutation_uncertainty() {
    let client = ScriptedClient::new([response(201, "123456", &[])]);
    let exec = executor(client, Arc::new(TestTimer::default()));
    let limits = NotebookLimits {
        max_response_bytes: NonZeroUsize::new(5).unwrap(),
        ..Default::default()
    };
    let error = sdk()
        .notebook_session(&exec, limits, pending())
        .create(WORKSPACE, "Test", "", &definition())
        .await
        .unwrap_err();
    assert_eq!(error.kind, NotebookErrorKind::Limit);
    assert!(error.mutation_may_have_occurred);
}

#[tokio::test]
async fn workspace_pagination_retries_and_typed_rows_use_the_same_executor() {
    let client = ScriptedClient::new([
        response(503, "", &[]),
        response(
            200,
            r#"{"value":[{"id":"opaque","displayName":"A","type":"Future"}],"continuationToken":"a+/="}"#,
            &[],
        ),
        response(200, r#"{"value":[]}"#, &[]),
    ]);
    let exec = executor(client.clone(), Arc::new(TestTimer::default()));
    let value = sdk()
        .list_workspaces(&exec, WorkspaceListLimits::default(), pending())
        .await
        .unwrap();
    assert_eq!(value.pages_received, 2);
    assert_eq!(value.value.len(), 1);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].url(), requests[1].url());
    assert_eq!(
        requests[2].url().query_pairs().collect::<Vec<_>>(),
        vec![("continuationToken".into(), "a+/=".into())]
    );
}

#[tokio::test]
async fn invalid_config_origin_and_bearer_never_reach_http_client() {
    let client = ScriptedClient::new([]);
    let timer = Arc::new(TestTimer::default());
    assert!(matches!(
        HttpExecutor::new(
            client.clone(),
            timer.clone(),
            ExecutionOptions {
                timeout: Duration::ZERO,
                ..Default::default()
            }
        ),
        Err(ExecutionError::InvalidInput)
    ));
    let exec = executor(client.clone(), timer);
    for url in [
        "http://api.fabric.microsoft.com/v1/workspaces",
        "https://other.invalid/v1/workspaces",
        "https://user@api.fabric.microsoft.com/v1/workspaces",
        "https://api.fabric.microsoft.com:444/v1/workspaces",
    ] {
        assert_eq!(
            exec.execute(
                Method::Get,
                &Url::parse(url).unwrap(),
                &token(),
                &[],
                NonZeroUsize::new(1).unwrap()
            )
            .await
            .unwrap_err(),
            ExecutionError::InvalidInput
        );
    }
    let mut bad = token();
    bad.token = "token\r\ninjected: header".into();
    assert_eq!(
        exec.execute(
            Method::Get,
            &Url::parse("https://api.fabric.microsoft.com/v1/workspaces").unwrap(),
            &bad,
            &[],
            NonZeroUsize::new(1).unwrap()
        )
        .await
        .unwrap_err(),
        ExecutionError::InvalidInput
    );
    assert!(client.requests.lock().unwrap().is_empty());
}
