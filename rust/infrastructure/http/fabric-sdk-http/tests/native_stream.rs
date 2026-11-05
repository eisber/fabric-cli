// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Instrumented offline pulls through the real native HTTP streaming adapter.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenRequestOptions},
    error::ErrorKind,
    http::{AsyncRawResponse, HttpClient, Method, Request, Url, headers::Headers},
    time::OffsetDateTime,
};
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin, TokenCredential,
    runtime::{
        NativeDeadline, NativeEffect, NativeEndpoint, NativeError, NativeMethod, NativeRequest,
        NativeStreamExecutor, NativeStreamLimits, NativeStreamResponse,
    },
};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor,
    native_stream::NativeFabricStreamExecutor,
};
use futures_util::Stream;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending, poll_fn},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

const TOKEN: &str = "offline-native-stream-token";
const BASE: &str = "https://api.fabric.microsoft.com/v1/workspaces/ws";

#[derive(Debug, Default)]
enum CredentialMode {
    #[default]
    Ready,
    Failure,
    Pending,
    Late,
    InvalidToken,
}

#[derive(Debug, Default)]
struct Credential {
    mode: CredentialMode,
    scopes: Mutex<Vec<Vec<String>>>,
    dropped: AtomicUsize,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert!(options.is_none());
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|scope| (*scope).to_owned()).collect());
        match self.mode {
            CredentialMode::Failure => return Err(private_error()),
            CredentialMode::Pending => {
                let _guard = Dropped(&self.dropped);
                pending::<()>().await;
            }
            CredentialMode::Late => std::thread::sleep(Duration::from_millis(25)),
            _ => {}
        }
        Ok(AccessToken::new(
            if matches!(self.mode, CredentialMode::InvalidToken) {
                ""
            } else {
                TOKEN
            },
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug, Default)]
struct Timer {
    expired: Arc<AtomicBool>,
    waits: Mutex<Vec<Duration>>,
    dropped: AtomicUsize,
}

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        self.waits.lock().unwrap().push(delay);
        Box::pin(async move {
            let _guard = Dropped(&self.dropped);
            poll_fn(|_| {
                if self.expired.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;
        })
    }
}

#[derive(Debug)]
enum Reply {
    Response(AsyncRawResponse),
    Failure,
    ProtocolFailure,
    Pending,
    Late(AsyncRawResponse, Arc<AtomicBool>),
}

#[derive(Debug)]
struct Wire {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Request>>,
    dropped: AtomicUsize,
}

impl Wire {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            dropped: AtomicUsize::new(0),
        })
    }
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
            .expect("unexpected dispatch, retry, redirect or remote cancellation");
        match reply {
            Reply::Response(response) => Ok(response),
            Reply::Failure => Err(private_error()),
            Reply::ProtocolFailure => Err(protocol_error()),
            Reply::Pending => {
                let _guard = Dropped(&self.dropped);
                pending().await
            }
            Reply::Late(response, expired) => {
                expired.store(true, Ordering::SeqCst);
                Ok(response)
            }
        }
    }
}

struct Dropped<'a>(&'a AtomicUsize);

impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Default)]
struct Probe {
    polls: AtomicUsize,
    drops: AtomicUsize,
}

enum Chunk {
    Bytes(Bytes),
    Failure,
    ProtocolFailure,
    Pending,
    Late(Bytes, Arc<AtomicBool>),
}

struct Body {
    chunks: VecDeque<Chunk>,
    probe: Arc<Probe>,
}

impl Stream for Body {
    type Item = azure_core::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.probe.polls.fetch_add(1, Ordering::SeqCst);
        match self.chunks.pop_front() {
            Some(Chunk::Bytes(bytes)) => Poll::Ready(Some(Ok(bytes))),
            Some(Chunk::Failure) => Poll::Ready(Some(Err(private_error()))),
            Some(Chunk::ProtocolFailure) => Poll::Ready(Some(Err(protocol_error()))),
            Some(Chunk::Pending) => {
                self.chunks.push_front(Chunk::Pending);
                Poll::Pending
            }
            Some(Chunk::Late(bytes, expired)) => {
                expired.store(true, Ordering::SeqCst);
                Poll::Ready(Some(Ok(bytes)))
            }
            None => Poll::Ready(None),
        }
    }
}

impl Drop for Body {
    fn drop(&mut self) {
        self.probe.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn private_error() -> azure_core::Error {
    azure_core::Error::with_message(ErrorKind::Io, "private-token-body-url-provider-error")
}

fn protocol_error() -> azure_core::Error {
    azure_core::Error::with_message(ErrorKind::DataConversion, "private-protocol-decoding-error")
}

fn response(
    status: u16,
    headers: &[(&str, &str)],
    chunks: impl IntoIterator<Item = Chunk>,
) -> (Reply, Arc<Probe>) {
    let mut fields = Headers::new();
    for (name, value) in headers {
        fields.insert((*name).to_owned(), (*value).to_owned());
    }
    let probe = Arc::new(Probe::default());
    let body = Body {
        chunks: chunks.into_iter().collect(),
        probe: probe.clone(),
    };
    (
        Reply::Response(AsyncRawResponse::new(status.into(), fields, Box::pin(body))),
        probe,
    )
}

fn bytes(value: &'static [u8]) -> Chunk {
    Chunk::Bytes(Bytes::from_static(value))
}

fn binding(host: &str) -> EndpointBinding {
    EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap()
}

fn context(
    credential: Arc<Credential>,
    endpoint: EndpointBinding,
    audience: Audience,
) -> RequestContext {
    RequestContext::with_endpoint(
        credential,
        CredentialSelection::new("private-account", "private-tenant", audience).unwrap(),
        endpoint,
    )
}

fn executor(
    credential: Arc<Credential>,
    wire: Arc<Wire>,
    timer: Arc<Timer>,
) -> NativeFabricStreamExecutor {
    let endpoint = binding("api.fabric.microsoft.com");
    configured(
        credential,
        wire,
        timer,
        endpoint.clone(),
        endpoint,
        Audience::Fabric,
    )
    .unwrap()
}

fn configured(
    credential: Arc<Credential>,
    wire: Arc<Wire>,
    timer: Arc<Timer>,
    context_binding: EndpointBinding,
    http_binding: EndpointBinding,
    audience: Audience,
) -> Result<NativeFabricStreamExecutor, NativeError> {
    NativeFabricStreamExecutor::new(
        context(credential, context_binding, audience),
        HttpExecutor::new(wire, timer, ExecutionOptions::default())
            .unwrap()
            .with_endpoint(http_binding),
    )
}

fn request_at(base: &str, method: NativeMethod, body: Option<Value>) -> NativeRequest {
    NativeEndpoint::new(Url::parse(base).unwrap())
        .unwrap()
        .request(
            method,
            &["sessions"],
            &[],
            body,
            if matches!(method, NativeMethod::Get | NativeMethod::Head) {
                NativeEffect::Read
            } else {
                NativeEffect::Execute
            },
        )
        .unwrap()
}

fn request(method: NativeMethod) -> NativeRequest {
    request_at(BASE, method, None)
}

fn limits() -> NativeStreamLimits {
    NativeStreamLimits {
        max_fragment_bytes: 3,
        max_body_bytes: 16,
    }
}

fn deadline() -> NativeDeadline {
    NativeDeadline::new(Duration::from_secs(30)).unwrap()
}

fn failure<T>(result: Result<T, NativeError>) -> NativeError {
    match result {
        Ok(_) => panic!("expected native failure"),
        Err(error) => error,
    }
}

fn assert_no_io(credential: &Credential, wire: &Wire, timer: &Timer) {
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(wire.requests.lock().unwrap().is_empty());
    assert!(timer.waits.lock().unwrap().is_empty());
}

fn assert_pending<F: Future + ?Sized>(future: Pin<&mut F>) {
    assert!(
        future
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
}

#[tokio::test]
async fn real_open_preserves_public_daily_msit_wire_scope_metadata_without_reading_body() {
    for host in [
        "api.fabric.microsoft.com",
        "dailyapi.fabric.microsoft.com",
        "msitapi.fabric.microsoft.com",
    ] {
        let credential = Arc::new(Credential::default());
        let (reply, probe) = response(
            200,
            &[
                ("Content-Type", "application/vnd.provider+arrow"),
                ("ETag", "\"version\""),
            ],
            [bytes(b"abcdef")],
        );
        let wire = Wire::new([reply]);
        let timer = Arc::new(Timer::default());
        let endpoint = binding(host);
        let executor = configured(
            credential.clone(),
            wire.clone(),
            timer.clone(),
            endpoint.clone(),
            endpoint,
            Audience::Fabric,
        )
        .unwrap();
        let request = request_at(
            &format!("https://{host}/v1/workspaces/ws"),
            NativeMethod::Get,
            None,
        )
        .with_accept("application/vnd.provider+arrow")
        .unwrap()
        .with_header("If-None-Match", "\"previous\"")
        .unwrap();
        let deadline = deadline();
        let mut result = executor.open(&request, limits(), &deadline).await.unwrap();
        assert_eq!(result.status, 200);
        assert_eq!(
            result.content_type.as_deref(),
            Some("application/vnd.provider+arrow")
        );
        assert_eq!(result.headers["etag"], "\"version\"");
        assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), Method::Get);
        assert_eq!(requests[0].url(), request.url());
        assert!(Bytes::from(requests[0].body()).is_empty());
        for (name, value) in [
            ("accept", "application/vnd.provider+arrow"),
            ("content-type", "application/json"),
            ("if-none-match", "\"previous\""),
        ] {
            assert_eq!(requests[0].headers().get_str(&name.into()).unwrap(), value);
        }
        assert_eq!(
            requests[0]
                .headers()
                .get_str(&"authorization".into())
                .unwrap(),
            format!("Bearer {TOKEN}")
        );
        drop(requests);
        assert_eq!(
            *credential.scopes.lock().unwrap(),
            [vec![Audience::Fabric.scope().to_owned()]]
        );
        assert_eq!(
            result.body.read(2, &deadline).await.unwrap(),
            Some(b"ab".to_vec())
        );
        assert_eq!(
            result.body.read(99, &deadline).await.unwrap(),
            Some(b"cde".to_vec())
        );
        assert_eq!(probe.polls.load(Ordering::SeqCst), 1);
        drop(result);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn inline_post_is_dispatched_once_and_202_remains_acceptance_with_exact_payload() {
    for raw in [false, true] {
        let credential = Arc::new(Credential::default());
        let (reply, probe) = response(202, &[("location", "/accepted/7")], [bytes(b"receipt")]);
        let wire = Wire::new([reply]);
        let executor = executor(credential.clone(), wire.clone(), Arc::new(Timer::default()));
        let request = if raw {
            request(NativeMethod::Post)
                .with_bytes(b"\0\xffcode".to_vec(), "application/octet-stream", 16)
                .unwrap()
        } else {
            request_at(
                BASE,
                NativeMethod::Post,
                Some(json!({"code": "private-code"})),
            )
        }
        .with_header("If-Match", "\"version\"")
        .unwrap();
        let deadline = deadline();
        let result = executor.open(&request, limits(), &deadline).await.unwrap();
        assert_eq!(result.status, 202);
        assert!(result.content_type.is_none());
        assert_eq!(result.headers["location"], "/accepted/7");
        assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), Method::Post);
        let expected = if raw {
            b"\0\xffcode".to_vec()
        } else {
            br#"{"code":"private-code"}"#.to_vec()
        };
        assert_eq!(
            Bytes::from(requests[0].body()).as_ref(),
            expected.as_slice()
        );
        assert_eq!(
            requests[0]
                .headers()
                .get_str(&"content-type".into())
                .unwrap(),
            if raw {
                "application/octet-stream"
            } else {
                "application/json"
            }
        );
        assert_eq!(
            requests[0].headers().get_str(&"if-match".into()).unwrap(),
            "\"version\""
        );
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        drop(requests);
        drop(result);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn transport_fragments_split_without_read_ahead_and_exact_total_eof_disposes() {
    let (reply, probe) = response(200, &[], [bytes(b"abcdefgh")]);
    let wire = Wire::new([reply]);
    let executor = executor(
        Arc::new(Credential::default()),
        wire,
        Arc::new(Timer::default()),
    );
    let deadline = deadline();
    let mut result = executor
        .open(
            &request(NativeMethod::Get),
            NativeStreamLimits {
                max_fragment_bytes: 3,
                max_body_bytes: 8,
            },
            &deadline,
        )
        .await
        .unwrap();
    assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
    for expected in [b"abc".as_slice(), b"def", b"gh"] {
        assert_eq!(
            result.body.read(3, &deadline).await.unwrap().as_deref(),
            Some(expected)
        );
        assert_eq!(probe.polls.load(Ordering::SeqCst), 1);
    }
    assert_eq!(result.body.read(3, &deadline).await.unwrap(), None);
    assert_eq!(probe.polls.load(Ordering::SeqCst), 2);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert_eq!(result.body.read(3, &deadline).await.unwrap(), None);
    assert_eq!(probe.polls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn total_cap_rejects_whole_excess_transport_fragment_before_exposing_any_part() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        let (reply, probe) = response(200, &[], [bytes(b"abcdef")]);
        let executor = executor(
            Arc::new(Credential::default()),
            Wire::new([reply]),
            Arc::new(Timer::default()),
        );
        let deadline = deadline();
        let mut result = executor
            .open(
                &request(method),
                NativeStreamLimits {
                    max_fragment_bytes: 2,
                    max_body_bytes: 5,
                },
                &deadline,
            )
            .await
            .unwrap();
        let expected = if method == NativeMethod::Get {
            NativeError::Limit
        } else {
            NativeError::OutcomeUnknown
        };
        assert_eq!(failure(result.body.read(2, &deadline).await), expected);
        assert_eq!(probe.polls.load(Ordering::SeqCst), 1);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        assert_eq!(failure(result.body.read(2, &deadline).await), expected);
        assert_eq!(probe.polls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn total_budget_counts_fresh_chunks_once_and_rejects_cap_plus_one() {
    let (reply, probe) = response(200, &[], [bytes(b"abc"), bytes(b"de"), bytes(b"f")]);
    let executor = executor(
        Arc::new(Credential::default()),
        Wire::new([reply]),
        Arc::new(Timer::default()),
    );
    let deadline = deadline();
    let mut result = executor
        .open(
            &request(NativeMethod::Get),
            NativeStreamLimits {
                max_fragment_bytes: 2,
                max_body_bytes: 5,
            },
            &deadline,
        )
        .await
        .unwrap();
    for expected in [b"ab".as_slice(), b"c", b"de"] {
        assert_eq!(
            result.body.read(2, &deadline).await.unwrap().as_deref(),
            Some(expected)
        );
    }
    assert_eq!(
        failure(result.body.read(2, &deadline).await),
        NativeError::Limit
    );
    assert_eq!(probe.polls.load(Ordering::SeqCst), 3);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn zero_invalid_limits_and_all_request_preflight_failures_have_no_credential_or_network() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([]);
    let timer = Arc::new(Timer::default());
    let executor = executor(credential.clone(), wire.clone(), timer.clone());
    let deadline = deadline();
    for limits in [
        NativeStreamLimits {
            max_fragment_bytes: 0,
            max_body_bytes: 1,
        },
        NativeStreamLimits {
            max_fragment_bytes: 1,
            max_body_bytes: 0,
        },
        NativeStreamLimits {
            max_fragment_bytes: 2,
            max_body_bytes: 1,
        },
    ] {
        assert_eq!(
            failure(
                executor
                    .open(&request(NativeMethod::Get), limits, &deadline)
                    .await
            ),
            NativeError::InvalidInput
        );
    }
    let mut deep = Value::Null;
    for _ in 0..64 {
        deep = Value::Array(vec![deep]);
    }
    let mut many_headers = request(NativeMethod::Get);
    for index in 0..63 {
        many_headers = many_headers
            .with_header(&format!("x-{index}"), "v")
            .unwrap();
    }
    for request in [
        request_at(
            BASE,
            NativeMethod::Post,
            Some(json!({"x": "x".repeat(4 * 1024 * 1024)})),
        ),
        request_at(BASE, NativeMethod::Post, Some(json!({"x": deep}))),
        request(NativeMethod::Post)
            .with_bytes(
                vec![0; 4 * 1024 * 1024 + 1],
                "application/octet-stream",
                4 * 1024 * 1024 + 1,
            )
            .unwrap(),
        request(NativeMethod::Get)
            .with_header("x-native", &"x".repeat(16 * 1024))
            .unwrap(),
        request(NativeMethod::Get)
            .with_accept(&"x".repeat(16 * 1024))
            .unwrap(),
        many_headers,
        request_at(
            &format!("{BASE}/{}", "x".repeat(16 * 1024)),
            NativeMethod::Get,
            None,
        ),
    ] {
        assert_eq!(
            failure(executor.open(&request, limits(), &deadline).await),
            NativeError::Limit
        );
    }
    let request = request(NativeMethod::Get)
        .with_header("keep-alive", "override")
        .unwrap();
    assert_eq!(
        failure(executor.open(&request, limits(), &deadline).await),
        NativeError::InvalidInput
    );
    assert_no_io(&credential, &wire, &timer);
}

#[tokio::test]
async fn unselected_foreign_or_mwc_origins_and_prefixes_fail_before_credentials() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([]);
    let timer = Arc::new(Timer::default());
    let executor = executor(credential.clone(), wire.clone(), timer.clone());
    for (url, expected) in [
        (
            "https://dailyapi.fabric.microsoft.com/v1",
            NativeError::InvalidInput,
        ),
        (
            "https://api.fabric.microsoft.com:444/v1",
            NativeError::InvalidInput,
        ),
        (
            "https://api.fabric.microsoft.com/v10",
            NativeError::InvalidInput,
        ),
        (
            "https://api.fabric.microsoft.com/v1-other",
            NativeError::InvalidInput,
        ),
        (
            "https://custom.example.test/v1",
            NativeError::UnsupportedCapability,
        ),
        (
            "https://cluster.pbidedicated.windows.net/v1",
            NativeError::UnsupportedCapability,
        ),
    ] {
        assert_eq!(
            failure(
                executor
                    .open(
                        &request_at(url, NativeMethod::Get, None),
                        limits(),
                        &deadline()
                    )
                    .await
            ),
            expected
        );
    }
    assert_no_io(&credential, &wire, &timer);
}

#[test]
fn mismatched_binding_audience_and_non_fabric_service_fail_at_construction() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([]);
    let timer = Arc::new(Timer::default());
    for (context, http, audience, expected) in [
        (
            binding("api.fabric.microsoft.com"),
            binding("dailyapi.fabric.microsoft.com"),
            Audience::Fabric,
            NativeError::InvalidInput,
        ),
        (
            binding("api.fabric.microsoft.com"),
            binding("api.fabric.microsoft.com"),
            Audience::Arm,
            NativeError::InvalidInput,
        ),
        (
            EndpointBinding::public(ServiceOrigin::PowerBi),
            EndpointBinding::public(ServiceOrigin::PowerBi),
            Audience::Fabric,
            NativeError::UnsupportedCapability,
        ),
    ] {
        assert_eq!(
            failure(configured(
                credential.clone(),
                wire.clone(),
                timer.clone(),
                context,
                http,
                audience
            )),
            expected
        );
    }
    assert_no_io(&credential, &wire, &timer);
}

#[tokio::test]
async fn auth_failure_and_invalid_token_are_not_mislabeled_as_unknown_dispatch() {
    for (mode, expected) in [
        (CredentialMode::Failure, NativeError::InvalidResponse),
        (CredentialMode::InvalidToken, NativeError::InvalidInput),
    ] {
        let credential = Arc::new(Credential {
            mode,
            ..Default::default()
        });
        let wire = Wire::new([]);
        let executor = executor(credential.clone(), wire.clone(), Arc::new(Timer::default()));
        let error = failure(
            executor
                .open(&request(NativeMethod::Post), limits(), &deadline())
                .await,
        );
        assert_eq!(error, expected);
        assert!(!format!("{error:?}: {error}").contains("private"));
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        assert!(wire.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn get_and_inline_post_transport_loss_dispatch_once_without_retry() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        let credential = Arc::new(Credential::default());
        let wire = Wire::new([Reply::Failure]);
        let executor = executor(credential.clone(), wire.clone(), Arc::new(Timer::default()));
        assert_eq!(
            failure(executor.open(&request(method), limits(), &deadline()).await),
            if method == NativeMethod::Get {
                NativeError::InvalidResponse
            } else {
                NativeError::OutcomeUnknown
            }
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn protocol_failures_at_header_or_body_boundaries_are_not_retryable_io_evidence() {
    for method in [NativeMethod::Get, NativeMethod::Head, NativeMethod::Post] {
        let expected = if method == NativeMethod::Post {
            NativeError::OutcomeUnknown
        } else {
            NativeError::InvalidResponse
        };
        let wire = Wire::new([Reply::ProtocolFailure]);
        let header_executor = executor(
            Arc::new(Credential::default()),
            wire.clone(),
            Arc::new(Timer::default()),
        );
        assert_eq!(
            failure(
                header_executor
                    .open(&request(method), limits(), &deadline())
                    .await
            ),
            expected
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);

        let (reply, probe) = response(200, &[], [Chunk::ProtocolFailure]);
        let wire = Wire::new([reply]);
        let body_executor = executor(
            Arc::new(Credential::default()),
            wire.clone(),
            Arc::new(Timer::default()),
        );
        let deadline = deadline();
        let mut response = body_executor
            .open(&request(method), limits(), &deadline)
            .await
            .unwrap();
        let error = failure(response.body.read(3, &deadline).await);
        assert_eq!(error, expected);
        assert!(!format!("{error:?}: {error}").contains("private"));
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn status_failures_and_redirects_remain_actual_and_never_follow_or_retry() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        for status in [302, 307, 400, 413, 429, 503] {
            let (reply, probe) = response(
                status,
                &[
                    ("location", "https://foreign.invalid/"),
                    ("retry-after", "1"),
                ],
                [Chunk::Failure],
            );
            let wire = Wire::new([reply]);
            let executor = executor(
                Arc::new(Credential::default()),
                wire.clone(),
                Arc::new(Timer::default()),
            );
            let deadline = deadline();
            let mut result = executor
                .open(&request(method), limits(), &deadline)
                .await
                .unwrap();
            assert_eq!(result.status, status);
            assert_eq!(result.headers["location"], "https://foreign.invalid/");
            assert_eq!(
                failure(result.body.read(3, &deadline).await),
                NativeError::Http(status)
            );
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
            assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn stalled_auth_uses_the_absolute_watchdog_before_dispatch_and_drops_acquisition() {
    let credential = Arc::new(Credential {
        mode: CredentialMode::Pending,
        ..Default::default()
    });
    let wire = Wire::new([]);
    let timer = Arc::new(Timer::default());
    let executor = executor(credential.clone(), wire.clone(), timer.clone());
    let request = request(NativeMethod::Post);
    let deadline = deadline();
    let mut open = executor.open(&request, limits(), &deadline);
    assert_pending(open.as_mut());
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    assert!(wire.requests.lock().unwrap().is_empty());
    timer.expired.store(true, Ordering::SeqCst);
    assert_eq!(failure(open.await), NativeError::TimedOut);
    assert_eq!(credential.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(timer.dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn late_authentication_cannot_dispatch_after_the_absolute_deadline() {
    let credential = Arc::new(Credential {
        mode: CredentialMode::Late,
        ..Default::default()
    });
    let wire = Wire::new([]);
    let executor = executor(credential.clone(), wire.clone(), Arc::new(Timer::default()));
    let deadline = NativeDeadline::new(Duration::from_millis(20)).unwrap();
    assert_eq!(
        failure(
            executor
                .open(&request(NativeMethod::Post), limits(), &deadline)
                .await
        ),
        NativeError::TimedOut
    );
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stalled_headers_expire_pending_open_and_preserve_mutation_uncertainty() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        let wire = Wire::new([Reply::Pending]);
        let timer = Arc::new(Timer::default());
        let executor = executor(Arc::new(Credential::default()), wire.clone(), timer.clone());
        let request = request(method);
        let deadline = deadline();
        let mut open = executor.open(&request, limits(), &deadline);
        assert_pending(open.as_mut());
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        timer.expired.store(true, Ordering::SeqCst);
        assert_eq!(
            failure(open.await),
            if method == NativeMethod::Get {
                NativeError::TimedOut
            } else {
                NativeError::OutcomeUnknown
            }
        );
        assert_eq!(wire.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn late_header_returns_are_rejected_and_their_unread_stream_is_disposed() {
    let timer = Arc::new(Timer::default());
    let (Reply::Response(raw), probe) = response(202, &[], [bytes(b"unread")]) else {
        unreachable!()
    };
    let wire = Wire::new([Reply::Late(raw, timer.expired.clone())]);
    let executor = executor(Arc::new(Credential::default()), wire.clone(), timer);
    assert_eq!(
        failure(
            executor
                .open(&request(NativeMethod::Post), limits(), &deadline())
                .await
        ),
        NativeError::OutcomeUnknown
    );
    assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn stalled_and_late_body_reads_expire_without_exposing_a_fragment() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        for late in [false, true] {
            let timer = Arc::new(Timer::default());
            let chunk = if late {
                Chunk::Late(Bytes::from_static(b"x"), timer.expired.clone())
            } else {
                Chunk::Pending
            };
            let (reply, probe) = response(200, &[], [chunk]);
            let wire = Wire::new([reply]);
            let executor = executor(Arc::new(Credential::default()), wire.clone(), timer.clone());
            let deadline = deadline();
            let mut result = executor
                .open(&request(method), limits(), &deadline)
                .await
                .unwrap();
            let mut read = result.body.read(3, &deadline);
            if !late {
                assert_pending(read.as_mut());
                timer.expired.store(true, Ordering::SeqCst);
            }
            assert_eq!(
                failure(read.await),
                if method == NativeMethod::Get {
                    NativeError::TimedOut
                } else {
                    NativeError::OutcomeUnknown
                }
            );
            assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn one_budget_counts_open_reads_and_pauses_and_cannot_be_extended_with_a_new_deadline() {
    let timer = Arc::new(Timer::default());
    let (reply, probe) = response(200, &[], [bytes(b"abcdef")]);
    let executor = executor(
        Arc::new(Credential::default()),
        Wire::new([reply]),
        timer.clone(),
    );
    let original = NativeDeadline::new(Duration::from_secs(10)).unwrap();
    let mut result = executor
        .open(&request(NativeMethod::Get), limits(), &original)
        .await
        .unwrap();
    std::thread::sleep(Duration::from_millis(1));
    assert_eq!(
        result.body.read(3, &original).await.unwrap(),
        Some(b"abc".to_vec())
    );
    std::thread::sleep(Duration::from_millis(1));
    let replacement = NativeDeadline::new(Duration::from_secs(120)).unwrap();
    assert_eq!(
        result.body.read(3, &replacement).await.unwrap(),
        Some(b"def".to_vec())
    );
    let waits = timer.waits.lock().unwrap();
    assert_eq!(waits.len(), 3);
    assert!(waits[0] <= Duration::from_secs(10));
    assert!(waits[1] < waits[0] && waits[2] < waits[1]);
    assert_eq!(probe.polls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_expired_consumer_pause_disposes_without_reading_ahead_even_with_a_fresh_deadline() {
    let timer = Arc::new(Timer::default());
    let (reply, probe) = response(200, &[], [bytes(b"abcdef")]);
    let executor = executor(Arc::new(Credential::default()), Wire::new([reply]), timer);
    let original = NativeDeadline::new(Duration::from_millis(20)).unwrap();
    let mut result = executor
        .open(&request(NativeMethod::Get), limits(), &original)
        .await
        .unwrap();
    std::thread::sleep(Duration::from_millis(25));
    assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
    assert_eq!(
        failure(result.body.read(3, &deadline()).await),
        NativeError::TimedOut
    );
    assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn supplied_earlier_deadline_can_shorten_but_never_restart_a_body_budget() {
    let (reply, probe) = response(200, &[], [bytes(b"abcdef")]);
    let executor = executor(
        Arc::new(Credential::default()),
        Wire::new([reply]),
        Arc::new(Timer::default()),
    );
    let original = deadline();
    let mut result = executor
        .open(&request(NativeMethod::Get), limits(), &original)
        .await
        .unwrap();
    let shorter = NativeDeadline::new(Duration::from_millis(1)).unwrap();
    std::thread::sleep(Duration::from_millis(2));
    assert_eq!(
        failure(result.body.read(3, &shorter).await),
        NativeError::TimedOut
    );
    assert_eq!(
        failure(result.body.read(3, &original).await),
        NativeError::TimedOut
    );
    assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn malformed_and_overbudget_headers_are_rejected_without_polling_the_body() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        for (headers, expected) in [
            (
                vec![("x invalid".to_owned(), "v".to_owned())],
                NativeError::InvalidResponse,
            ),
            (
                vec![("content-type".to_owned(), "bad\r\nmedia".to_owned())],
                NativeError::InvalidResponse,
            ),
            (
                vec![("x-native".to_owned(), "x".repeat(16 * 1024))],
                NativeError::Limit,
            ),
            (
                (0..65)
                    .map(|i| (format!("x-{i}"), "v".to_owned()))
                    .collect(),
                NativeError::Limit,
            ),
        ] {
            let fields: Vec<_> = headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let (reply, probe) = response(200, &fields, [Chunk::Pending]);
            let executor = executor(
                Arc::new(Credential::default()),
                Wire::new([reply]),
                Arc::new(Timer::default()),
            );
            assert_eq!(
                failure(executor.open(&request(method), limits(), &deadline()).await),
                if method == NativeMethod::Get {
                    expected
                } else {
                    NativeError::OutcomeUnknown
                }
            );
            assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
            assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn exact_response_header_count_and_metadata_bytes_are_accepted() {
    for headers in [
        (0..64)
            .map(|i| (format!("x-{i}"), "v".to_owned()))
            .collect::<Vec<_>>(),
        vec![(
            "x-native".to_owned(),
            "x".repeat(16 * 1024 - "x-native".len()),
        )],
    ] {
        let fields: Vec<_> = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let (reply, probe) = response(200, &fields, []);
        let executor = executor(
            Arc::new(Credential::default()),
            Wire::new([reply]),
            Arc::new(Timer::default()),
        );
        let deadline = deadline();
        let mut result = executor
            .open(&request(NativeMethod::Get), limits(), &deadline)
            .await
            .unwrap();
        assert_eq!(result.headers.len(), headers.len());
        assert_eq!(result.body.read(3, &deadline).await.unwrap(), None);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn empty_fragments_stream_errors_and_zero_read_bounds_close_the_body() {
    for (chunk, max_bytes, expected) in [
        (bytes(b""), 3, NativeError::InvalidResponse),
        (Chunk::Failure, 3, NativeError::InvalidResponse),
        (bytes(b"abc"), 0, NativeError::InvalidInput),
    ] {
        let (reply, probe) = response(200, &[], [chunk]);
        let executor = executor(
            Arc::new(Credential::default()),
            Wire::new([reply]),
            Arc::new(Timer::default()),
        );
        let deadline = deadline();
        let mut result = executor
            .open(&request(NativeMethod::Get), limits(), &deadline)
            .await
            .unwrap();
        assert_eq!(
            failure(result.body.read(max_bytes, &deadline).await),
            expected
        );
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        assert_eq!(failure(result.body.read(3, &deadline).await), expected);
    }
}

#[tokio::test]
async fn invalid_statuses_are_not_exposed_and_valid_status_boundaries_are_preserved() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        for status in [99, 600] {
            let (reply, probe) = response(status, &[], []);
            let executor = executor(
                Arc::new(Credential::default()),
                Wire::new([reply]),
                Arc::new(Timer::default()),
            );
            assert_eq!(
                failure(executor.open(&request(method), limits(), &deadline()).await),
                if method == NativeMethod::Get {
                    NativeError::InvalidResponse
                } else {
                    NativeError::OutcomeUnknown
                }
            );
            assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
            assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        }
    }
    for status in [100, 599] {
        let (reply, probe) = response(status, &[], []);
        let executor = executor(
            Arc::new(Credential::default()),
            Wire::new([reply]),
            Arc::new(Timer::default()),
        );
        let result = executor
            .open(&request(NativeMethod::Get), limits(), &deadline())
            .await
            .unwrap();
        assert_eq!(result.status, status);
        drop(result);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn zero_or_already_expired_absolute_budget_precedes_credentials_and_timer_work() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([]);
    let timer = Arc::new(Timer::default());
    let executor = executor(credential.clone(), wire.clone(), timer.clone());
    assert_eq!(
        failure(NativeDeadline::new(Duration::ZERO)),
        NativeError::InvalidInput
    );
    let expired = NativeDeadline::new(Duration::from_millis(1)).unwrap();
    std::thread::sleep(Duration::from_millis(2));
    assert_eq!(
        failure(
            executor
                .open(&request(NativeMethod::Post), limits(), &expired)
                .await
        ),
        NativeError::TimedOut
    );
    assert_no_io(&credential, &wire, &timer);
}

#[tokio::test]
async fn header_failure_keeps_an_observed_http_failure_status() {
    let (reply, probe) = response(429, &[("content-type", "bad\r\nmedia")], []);
    let executor = executor(
        Arc::new(Credential::default()),
        Wire::new([reply]),
        Arc::new(Timer::default()),
    );
    assert_eq!(
        failure(
            executor
                .open(&request(NativeMethod::Post), limits(), &deadline())
                .await
        ),
        NativeError::Http(429)
    );
    assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropping_a_polled_read_closes_it_terminally_without_remote_cancellation() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        let (reply, probe) = response(200, &[], [Chunk::Pending]);
        let wire = Wire::new([reply]);
        let executor = executor(
            Arc::new(Credential::default()),
            wire.clone(),
            Arc::new(Timer::default()),
        );
        let deadline = deadline();
        let mut result = executor
            .open(&request(method), limits(), &deadline)
            .await
            .unwrap();
        let mut read = result.body.read(3, &deadline);
        assert_pending(read.as_mut());
        drop(read);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            failure(result.body.read(3, &deadline).await),
            if method == NativeMethod::Get {
                NativeError::Cancelled
            } else {
                NativeError::OutcomeUnknown
            }
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn dropping_unpolled_reads_or_remainders_has_no_prefetch_or_remote_side_effect() {
    let (reply, probe) = response(200, &[], [bytes(b"abcdef")]);
    let wire = Wire::new([reply]);
    let executor = executor(
        Arc::new(Credential::default()),
        wire.clone(),
        Arc::new(Timer::default()),
    );
    let deadline = deadline();
    let mut result = executor
        .open(&request(NativeMethod::Get), limits(), &deadline)
        .await
        .unwrap();
    drop(result.body.read(3, &deadline));
    assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 0);
    assert_eq!(
        result.body.read(3, &deadline).await.unwrap(),
        Some(b"abc".to_vec())
    );
    drop(result);
    assert_eq!(probe.polls.load(Ordering::SeqCst), 1);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[test]
fn dropping_open_releases_pending_credential_or_header_work() {
    for acquiring in [true, false] {
        let credential = Arc::new(Credential {
            mode: if acquiring {
                CredentialMode::Pending
            } else {
                CredentialMode::Ready
            },
            ..Default::default()
        });
        let wire = Wire::new(if acquiring {
            vec![]
        } else {
            vec![Reply::Pending]
        });
        let timer = Arc::new(Timer::default());
        let executor = executor(credential.clone(), wire.clone(), timer.clone());
        let request = request(NativeMethod::Post);
        let deadline = deadline();
        let mut open = executor.open(&request, limits(), &deadline);
        assert_pending(open.as_mut());
        drop(open);
        assert_eq!(
            credential.dropped.load(Ordering::SeqCst),
            usize::from(acquiring)
        );
        assert_eq!(wire.dropped.load(Ordering::SeqCst), usize::from(!acquiring));
        assert_eq!(wire.requests.lock().unwrap().len(), usize::from(!acquiring));
        assert_eq!(timer.dropped.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn debug_and_errors_never_expose_tokens_urls_headers_or_payloads() {
    let (reply, _) = response(
        200,
        &[
            ("content-type", "private-media"),
            ("x-private", "private-header"),
        ],
        [Chunk::Failure],
    );
    let executor = executor(
        Arc::new(Credential::default()),
        Wire::new([reply]),
        Arc::new(Timer::default()),
    );
    let deadline = deadline();
    let request = request_at(
        BASE,
        NativeMethod::Post,
        Some(json!({"code": "private-code"})),
    );
    let mut response: NativeStreamResponse =
        executor.open(&request, limits(), &deadline).await.unwrap();
    let diagnostic = format!("{executor:?} {request:?} {response:?}");
    for private in [
        TOKEN,
        BASE,
        "private-account",
        "private-tenant",
        "private-code",
        "private-media",
        "private-header",
    ] {
        assert!(!diagnostic.contains(private));
    }
    let error = failure(response.body.read(3, &deadline).await);
    assert_eq!(error, NativeError::OutcomeUnknown);
    assert!(!format!("{error:?} {error}").contains("private"));
}
