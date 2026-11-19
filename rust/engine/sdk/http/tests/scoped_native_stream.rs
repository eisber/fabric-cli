// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline injected credentials, HTTP and time through the scoped stream adapter.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    error::ErrorKind,
    http::{AsyncRawResponse, HttpClient, Method, Request, Url, headers::Headers},
    time::OffsetDateTime,
};
use fabric_sdk::runtime::{
    NativeDeadline, NativeEffect, NativeEndpoint, NativeError, NativeMethod, NativeRequest,
    NativeStreamExecutor, NativeStreamLimits,
};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionTimer,
    native_stream::{NativeScopeStreamExecutor, NativeStreamPreflight},
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

const BASE: &str = "https://cluster.example.test";
const SCOPE: &str = "https://audience.example.test/.default";
const TOKEN: &str = "offline-scoped-token";
type Events = Arc<Mutex<Vec<&'static str>>>;

#[derive(Debug, Default)]
enum CredentialMode {
    #[default]
    Ready,
    Error,
    Invalid(&'static str),
    Pending,
    Slow(Duration),
}

#[derive(Debug, Default)]
struct Credential {
    mode: CredentialMode,
    scopes: Mutex<Vec<Vec<String>>>,
    drops: AtomicUsize,
    events: Events,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert!(options.is_none());
        self.events.lock().unwrap().push("credential");
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|value| (*value).to_owned()).collect());
        match self.mode {
            CredentialMode::Error => return Err(private_error(ErrorKind::Credential)),
            CredentialMode::Pending => {
                let _guard = Dropped(&self.drops);
                pending::<()>().await;
            }
            CredentialMode::Slow(delay) => std::thread::sleep(delay),
            _ => {}
        }
        Ok(AccessToken::new(
            match self.mode {
                CredentialMode::Invalid(value) => value,
                _ => TOKEN,
            },
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug, Default)]
struct Timer {
    expired: Arc<AtomicBool>,
    waits: Mutex<Vec<Duration>>,
    drops: AtomicUsize,
}

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        self.waits.lock().unwrap().push(delay);
        Box::pin(async move {
            let _guard = Dropped(&self.drops);
            poll_fn(|_| {
                if self.expired.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await
        })
    }
}

#[derive(Debug)]
enum Reply {
    Response(AsyncRawResponse),
    Error(ErrorKind),
    Pending,
    Late(AsyncRawResponse, Arc<AtomicBool>),
}

#[derive(Debug, Default)]
struct Wire {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Request>>,
    drops: AtomicUsize,
    events: Events,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.events.lock().unwrap().push("send");
        self.requests.lock().unwrap().push(request.clone());
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected retry, redirect or remote cancellation");
        match reply {
            Reply::Response(response) => Ok(response),
            Reply::Error(kind) => Err(private_error(kind)),
            Reply::Pending => {
                let _guard = Dropped(&self.drops);
                pending().await
            }
            Reply::Late(response, expired) => {
                expired.store(true, Ordering::SeqCst);
                Ok(response)
            }
        }
    }
}

#[derive(Debug, Default)]
struct Probe {
    polls: AtomicUsize,
    drops: AtomicUsize,
}

enum Chunk {
    Bytes(Bytes),
    Error(ErrorKind),
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
            Some(Chunk::Error(kind)) => Poll::Ready(Some(Err(private_error(kind)))),
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

struct Dropped<'a>(&'a AtomicUsize);

impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct Preflight {
    reject_at: Option<(usize, NativeError)>,
    server_timeout: Option<Duration>,
    snapshots: Mutex<Vec<NativeRequest>>,
    events: Events,
}

impl NativeStreamPreflight for Preflight {
    fn validate(
        &self,
        request: &NativeRequest,
        deadline: &NativeDeadline,
    ) -> Result<(), NativeError> {
        self.events.lock().unwrap().push("preflight");
        let mut snapshots = self.snapshots.lock().unwrap();
        snapshots.push(request.clone());
        if let Some((at, error)) = self.reject_at
            && snapshots.len() == at
        {
            return Err(error);
        }
        if let Some(timeout) = self.server_timeout
            && timeout > deadline.remaining()?
        {
            return Err(NativeError::InvalidInput);
        }
        Ok(())
    }
}

struct Fixture {
    credential: Arc<Credential>,
    wire: Arc<Wire>,
    timer: Arc<Timer>,
    preflight: Arc<Preflight>,
    events: Events,
}

impl Fixture {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self::configured(replies, CredentialMode::Ready, None, None)
    }

    fn configured(
        replies: impl IntoIterator<Item = Reply>,
        mode: CredentialMode,
        reject_at: Option<(usize, NativeError)>,
        server_timeout: Option<Duration>,
    ) -> Self {
        let events = Events::default();
        Self {
            credential: Arc::new(Credential {
                mode,
                events: events.clone(),
                ..Credential::default()
            }),
            wire: Arc::new(Wire {
                replies: Mutex::new(replies.into_iter().collect()),
                events: events.clone(),
                ..Wire::default()
            }),
            timer: Arc::new(Timer::default()),
            preflight: Arc::new(Preflight {
                reject_at,
                server_timeout,
                events: events.clone(),
                ..Preflight::default()
            }),
            events,
        }
    }

    fn at(&self, base: &str, scope: &str) -> Result<NativeScopeStreamExecutor, NativeError> {
        NativeScopeStreamExecutor::new(
            endpoint(base),
            scope.to_owned(),
            self.credential.clone(),
            self.wire.clone(),
            self.timer.clone(),
            self.preflight.clone(),
        )
    }

    fn executor(&self) -> NativeScopeStreamExecutor {
        self.at(BASE, SCOPE).unwrap()
    }

    fn assert_no_acquisition(&self) {
        assert!(self.credential.scopes.lock().unwrap().is_empty());
        assert!(self.wire.requests.lock().unwrap().is_empty());
        assert!(self.timer.waits.lock().unwrap().is_empty());
    }
}

fn endpoint(base: &str) -> NativeEndpoint {
    NativeEndpoint::new(Url::parse(base).unwrap()).unwrap()
}

fn request_at(base: &str, method: NativeMethod, body: Option<Value>) -> NativeRequest {
    endpoint(base)
        .request(
            method,
            &["v2", "rest", "query"],
            &[],
            body,
            NativeEffect::Read,
        )
        .unwrap()
}

fn request(method: NativeMethod) -> NativeRequest {
    request_at(BASE, method, None)
}

fn deadline() -> NativeDeadline {
    NativeDeadline::new(Duration::from_secs(30)).unwrap()
}

fn limits() -> NativeStreamLimits {
    NativeStreamLimits {
        max_fragment_bytes: 3,
        max_body_bytes: 16,
    }
}

fn private_error(kind: ErrorKind) -> azure_core::Error {
    azure_core::Error::with_message(kind, "private-scope-token-query-provider-error")
}

fn response(
    status: u16,
    fields: &[(&str, &str)],
    chunks: impl IntoIterator<Item = Chunk>,
) -> (Reply, Arc<Probe>) {
    let mut headers = Headers::new();
    for (name, value) in fields {
        headers.insert((*name).to_owned(), (*value).to_owned());
    }
    let probe = Arc::new(Probe::default());
    let body = Body {
        chunks: chunks.into_iter().collect(),
        probe: probe.clone(),
    };
    (
        Reply::Response(AsyncRawResponse::new(
            status.into(),
            headers,
            Box::pin(body),
        )),
        probe,
    )
}

fn bytes(value: &'static [u8]) -> Chunk {
    Chunk::Bytes(Bytes::from_static(value))
}

fn failure<T>(result: Result<T, NativeError>) -> NativeError {
    match result {
        Ok(_) => panic!("expected scoped stream failure"),
        Err(error) => error,
    }
}

fn assert_pending<F: Future + ?Sized>(future: Pin<&mut F>) {
    assert!(
        future
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
}

#[test]
fn construction_rejects_unsafe_or_unbounded_literal_scopes_without_io() {
    let fixture = Fixture::new([]);
    for scope in [
        "",
        "https://audience.example.test/",
        "http://audience.example.test/.default",
        "https://user:password@audience.example.test/.default",
        "https://audience.example.test/.default?query=1",
        "https://audience.example.test/.default#fragment",
        "https://audience.example.test/%2f.default",
        "https://audience.example.test/.default other-scope",
        "https://audience.example.test/.default\n",
        "https:\\audience.example.test\\.default",
        "HTTPS://audience.example.test/.default",
    ] {
        assert_eq!(
            failure(fixture.at(BASE, scope)),
            NativeError::InvalidInput,
            "{scope:?}"
        );
    }
    assert_eq!(
        failure(fixture.at(BASE, &format!("{SCOPE}{}", "x".repeat(4096)))),
        NativeError::Limit
    );
    assert_eq!(
        failure(fixture.at("http://localhost:4444", SCOPE)),
        NativeError::InvalidInput
    );
    for base in [
        "https://cluster.example.test//",
        "https://cluster.example.test/owner//",
        "https://cluster.example.test/owner///",
    ] {
        assert_eq!(failure(fixture.at(base, SCOPE)), NativeError::InvalidInput);
    }
    assert_eq!(
        failure(fixture.at(&format!("{BASE}/{}", "x".repeat(16 * 1024)), SCOPE)),
        NativeError::Limit
    );
    fixture.assert_no_acquisition();
}

#[tokio::test]
async fn exact_literal_scope_origin_post_headers_and_utf8_bytes_are_immutable() {
    for raw in [false, true] {
        let (reply, probe) = response(
            202,
            &[("Content-Type", "application/json"), ("ETag", "\"v1\"")],
            [bytes(b"abcdef")],
        );
        let fixture = Fixture::new([reply]);
        let executor = fixture.executor();
        let payload = "{\"db\":\"synthetic\",\"csl\":\"print text='\u{03bb}'\"}".as_bytes();
        let _: &fabric_sdk_http::scoped_native_stream::NativeScopeStreamExecutor = &executor;
        let request = if raw {
            request(NativeMethod::Post)
                .with_bytes(payload.to_vec(), "application/json; charset=utf-8", 4096)
                .unwrap()
        } else {
            request_at(
                BASE,
                NativeMethod::Post,
                Some(json!({"db": "synthetic", "csl": "print value=1"})),
            )
        }
        .with_header("x-ms-client-request-id", "offline;correlation")
        .unwrap()
        .with_accept("application/json")
        .unwrap();
        let original = request.clone();
        let deadline = deadline();
        let mut result = executor.open(&request, limits(), &deadline).await.unwrap();
        assert_eq!(request, original);
        assert_eq!(
            *fixture.preflight.snapshots.lock().unwrap(),
            [original.clone(), original]
        );
        assert_eq!(
            *fixture.events.lock().unwrap(),
            ["preflight", "credential", "preflight", "send"]
        );
        assert_eq!(
            *fixture.credential.scopes.lock().unwrap(),
            [vec![SCOPE.to_owned()]]
        );
        assert_eq!(result.status, 202);
        assert_eq!(result.content_type.as_deref(), Some("application/json"));
        assert_eq!(result.headers["etag"], "\"v1\"");
        assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
        {
            let sent = fixture.wire.requests.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0].method(), Method::Post);
            assert_eq!(sent[0].url(), request.url());
            let expected = if raw {
                payload.to_vec()
            } else {
                serde_json::to_vec(request.body().unwrap()).unwrap()
            };
            assert_eq!(Bytes::from(sent[0].body()).as_ref(), expected);
            for (name, value) in [
                ("accept", "application/json"),
                ("content-type", request.content_type()),
                ("x-ms-client-request-id", "offline;correlation"),
            ] {
                assert_eq!(sent[0].headers().get_str(&name.into()).unwrap(), value);
            }
            assert_eq!(
                sent[0].headers().get_str(&"authorization".into()).unwrap(),
                format!("Bearer {TOKEN}")
            );
        }
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
async fn target_limits_headers_body_and_policy_fail_before_credentials() {
    let fixture = Fixture::new([]);
    let executor = fixture.executor();
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
                    .open(&request(NativeMethod::Post), limits, &deadline)
                    .await
            ),
            NativeError::InvalidInput
        );
    }
    for base in [
        "https://other.example.test",
        "https://cluster.example.test:4444",
        "http://localhost",
    ] {
        assert_eq!(
            failure(
                executor
                    .open(
                        &request_at(base, NativeMethod::Post, None),
                        limits(),
                        &deadline
                    )
                    .await
            ),
            NativeError::InvalidInput
        );
    }
    let prefix = fixture.at(&format!("{BASE}/owner"), SCOPE).unwrap();
    assert_eq!(
        failure(
            prefix
                .open(
                    &request_at(&format!("{BASE}/owner-other"), NativeMethod::Post, None),
                    limits(),
                    &deadline,
                )
                .await
        ),
        NativeError::InvalidInput
    );
    for header in [
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "te",
        "trailer",
        "upgrade",
    ] {
        let request = request(NativeMethod::Post)
            .with_header(header, "x")
            .unwrap();
        assert_eq!(
            failure(executor.open(&request, limits(), &deadline).await),
            NativeError::InvalidInput
        );
    }
    let mut many_headers = request(NativeMethod::Post);
    for index in 0..63 {
        many_headers = many_headers
            .with_header(&format!("x-{index}"), "v")
            .unwrap();
    }
    let mut deep = Value::Null;
    for _ in 0..64 {
        deep = Value::Array(vec![deep]);
    }
    for request in [
        many_headers,
        request(NativeMethod::Post)
            .with_header("x-large", &"x".repeat(16 * 1024))
            .unwrap(),
        request(NativeMethod::Post)
            .with_bytes(
                vec![0; 4 * 1024 * 1024 + 1],
                "application/json",
                5 * 1024 * 1024,
            )
            .unwrap(),
        request_at(BASE, NativeMethod::Post, Some(json!({"nested": deep}))),
        request_at(
            BASE,
            NativeMethod::Post,
            Some(json!({"data": "x".repeat(4 * 1024 * 1024)})),
        ),
        request_at(
            &format!("{BASE}/{}", "x".repeat(16 * 1024)),
            NativeMethod::Post,
            None,
        ),
    ] {
        assert_eq!(
            failure(executor.open(&request, limits(), &deadline).await),
            NativeError::Limit
        );
    }
    let invalid_effect = endpoint(BASE)
        .request(NativeMethod::Get, &[], &[], None, NativeEffect::Execute)
        .unwrap();
    assert_eq!(
        failure(executor.open(&invalid_effect, limits(), &deadline).await),
        NativeError::InvalidInput
    );
    for header in [
        "authorization",
        "proxy-authorization",
        "host",
        "cookie",
        "content-length",
        "connection",
        "accept",
        "content-type",
    ] {
        assert_eq!(
            failure(request(NativeMethod::Post).with_header(header, "override")),
            NativeError::InvalidInput
        );
    }
    assert_eq!(
        failure(
            request(NativeMethod::Post)
                .with_header("x-one", "a")
                .unwrap()
                .with_header("X-One", "b")
        ),
        NativeError::InvalidInput
    );
    assert_eq!(
        failure(request(NativeMethod::Post).with_header("x-one", "bad\r\nvalue")),
        NativeError::InvalidInput
    );
    fixture.assert_no_acquisition();
    assert!(fixture.preflight.snapshots.lock().unwrap().is_empty());

    let fixture = Fixture::configured(
        [],
        CredentialMode::Ready,
        Some((1, NativeError::UnsupportedCapability)),
        None,
    );
    assert_eq!(
        failure(
            fixture
                .executor()
                .open(&request(NativeMethod::Post), limits(), &deadline)
                .await
        ),
        NativeError::UnsupportedCapability
    );
    fixture.assert_no_acquisition();
    assert_eq!(fixture.preflight.snapshots.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn postcredential_policy_rejection_never_sends_or_reacquires() {
    let fixture = Fixture::configured(
        [],
        CredentialMode::Ready,
        Some((2, NativeError::InvalidInput)),
        None,
    );
    let executor = fixture.executor();
    let request = request(NativeMethod::Post);
    let original = request.clone();
    assert_eq!(
        failure(executor.open(&request, limits(), &deadline()).await),
        NativeError::InvalidInput
    );
    assert_eq!(
        *fixture.preflight.snapshots.lock().unwrap(),
        [original.clone(), original]
    );
    assert_eq!(
        *fixture.events.lock().unwrap(),
        ["preflight", "credential", "preflight"]
    );
    assert!(fixture.wire.requests.lock().unwrap().is_empty());
    assert_eq!(fixture.credential.scopes.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn expired_open_has_no_acquisition_and_explicit_https_port_and_prefix_are_retained() {
    let fixture = Fixture::new([]);
    let expired = NativeDeadline::new(Duration::from_millis(1)).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    assert_eq!(
        failure(
            fixture
                .executor()
                .open(&request(NativeMethod::Post), limits(), &expired)
                .await
        ),
        NativeError::TimedOut
    );
    fixture.assert_no_acquisition();

    let base = "https://cluster.example.test:4444/owner";
    let (reply, _) = response(200, &[], []);
    let fixture = Fixture::new([reply]);
    let request = request_at(base, NativeMethod::Post, None);
    fixture
        .at(base, SCOPE)
        .unwrap()
        .open(&request, limits(), &deadline())
        .await
        .unwrap();
    assert_eq!(
        fixture.wire.requests.lock().unwrap()[0].url(),
        request.url()
    );
}

#[tokio::test]
async fn slow_credential_makes_explicit_server_timeout_invalid_without_clamp_or_send() {
    let fixture = Fixture::configured(
        [],
        CredentialMode::Slow(Duration::from_millis(300)),
        None,
        Some(Duration::from_secs(1)),
    );
    let request = request_at(
        BASE,
        NativeMethod::Post,
        Some(json!({"serverTimeoutMs": 1000})),
    );
    let original = request.clone();
    let deadline = NativeDeadline::new(Duration::from_millis(1250)).unwrap();
    assert_eq!(
        failure(fixture.executor().open(&request, limits(), &deadline).await),
        NativeError::InvalidInput
    );
    assert_eq!(
        *fixture.preflight.snapshots.lock().unwrap(),
        [original.clone(), original]
    );
    assert!(fixture.wire.requests.lock().unwrap().is_empty());
    assert_eq!(fixture.credential.scopes.lock().unwrap().len(), 1);
    assert_eq!(request.body().unwrap()["serverTimeoutMs"], 1000);
}

#[tokio::test]
async fn credential_failure_invalid_token_and_expiry_remain_predispatch_errors() {
    for (mode, expected) in [
        (CredentialMode::Error, NativeError::InvalidResponse),
        (CredentialMode::Invalid(""), NativeError::InvalidInput),
        (
            CredentialMode::Invalid("bad\rtoken"),
            NativeError::InvalidInput,
        ),
    ] {
        let fixture = Fixture::configured([], mode, None, None);
        assert_eq!(
            failure(
                fixture
                    .executor()
                    .open(&request(NativeMethod::Post), limits(), &deadline())
                    .await
            ),
            expected
        );
        assert!(fixture.wire.requests.lock().unwrap().is_empty());
        assert_eq!(fixture.credential.scopes.lock().unwrap().len(), 1);
    }
    let fixture = Fixture::configured(
        [],
        CredentialMode::Slow(Duration::from_millis(30)),
        None,
        None,
    );
    let deadline = NativeDeadline::new(Duration::from_millis(10)).unwrap();
    assert_eq!(
        failure(
            fixture
                .executor()
                .open(&request(NativeMethod::Post), limits(), &deadline)
                .await
        ),
        NativeError::TimedOut
    );
    assert!(fixture.wire.requests.lock().unwrap().is_empty());
    assert_eq!(fixture.credential.scopes.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn pending_credential_and_header_waits_share_deadline_and_drop_local_futures() {
    for credential_wait in [true, false] {
        for cancel in [true, false] {
            let fixture = Fixture::configured(
                if credential_wait {
                    vec![]
                } else {
                    vec![Reply::Pending]
                },
                if credential_wait {
                    CredentialMode::Pending
                } else {
                    CredentialMode::Ready
                },
                None,
                None,
            );
            let executor = fixture.executor();
            let request = request(NativeMethod::Post);
            let deadline = deadline();
            let mut opening = executor.open(&request, limits(), &deadline);
            assert_pending(opening.as_mut());
            if !cancel {
                fixture.timer.expired.store(true, Ordering::SeqCst);
                assert_eq!(
                    failure(opening.as_mut().await),
                    if credential_wait {
                        NativeError::TimedOut
                    } else {
                        NativeError::OutcomeUnknown
                    }
                );
            }
            drop(opening);
            assert_eq!(fixture.credential.scopes.lock().unwrap().len(), 1);
            assert_eq!(
                fixture.wire.requests.lock().unwrap().len(),
                usize::from(!credential_wait)
            );
            assert_eq!(
                fixture.credential.drops.load(Ordering::SeqCst),
                usize::from(credential_wait)
            );
            assert_eq!(
                fixture.wire.drops.load(Ordering::SeqCst),
                usize::from(!credential_wait)
            );
            assert_eq!(fixture.timer.drops.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn statuses_redirects_and_transport_failures_are_never_retried() {
    for status in [200, 202, 302, 307, 400, 401, 403, 429, 500, 503] {
        let (reply, probe) = response(
            status,
            &[
                ("location", "https://other.example.test/next"),
                ("retry-after", "0"),
            ],
            [],
        );
        let fixture = Fixture::new([reply]);
        let result = fixture
            .executor()
            .open(&request(NativeMethod::Post), limits(), &deadline())
            .await
            .unwrap();
        assert_eq!(result.status, status);
        assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
        drop(result);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.wire.requests.lock().unwrap().len(), 1);
        assert_eq!(fixture.credential.scopes.lock().unwrap().len(), 1);
    }
    for method in [NativeMethod::Get, NativeMethod::Post] {
        for kind in [ErrorKind::Io, ErrorKind::DataConversion, ErrorKind::Other] {
            let expected = if method == NativeMethod::Post {
                NativeError::OutcomeUnknown
            } else if matches!(kind, ErrorKind::Io) {
                NativeError::Transport
            } else {
                NativeError::InvalidResponse
            };
            let fixture = Fixture::new([Reply::Error(kind)]);
            assert_eq!(
                failure(
                    fixture
                        .executor()
                        .open(&request(method), limits(), &deadline())
                        .await
                ),
                expected
            );
            assert_eq!(fixture.wire.requests.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn bounded_chunks_exact_eof_and_no_read_ahead_share_original_deadline() {
    let (reply, probe) = response(200, &[], [bytes(b"abcde"), bytes(b"fgh")]);
    let fixture = Fixture::new([reply]);
    let deadline = deadline();
    let mut result = fixture
        .executor()
        .open(
            &request(NativeMethod::Post),
            NativeStreamLimits {
                max_fragment_bytes: 3,
                max_body_bytes: 8,
            },
            &deadline,
        )
        .await
        .unwrap();
    assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
    for expected in [b"abc".as_slice(), b"de", b"fgh"] {
        assert_eq!(
            result.body.read(99, &deadline).await.unwrap().as_deref(),
            Some(expected)
        );
    }
    assert_eq!(probe.polls.load(Ordering::SeqCst), 2);
    assert_eq!(result.body.read(99, &deadline).await.unwrap(), None);
    assert_eq!(result.body.read(99, &deadline).await.unwrap(), None);
    assert_eq!(probe.polls.load(Ordering::SeqCst), 3);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    let waits = fixture.timer.waits.lock().unwrap();
    assert!(waits.windows(2).all(|pair| pair[1] <= pair[0]));
}

#[tokio::test]
async fn overflow_empty_chunks_zero_reads_and_body_errors_close_terminally() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        for (chunks, read_size, expected) in [
            (vec![bytes(b"abcdef")], 3, NativeError::Limit),
            (vec![bytes(b"")], 3, NativeError::InvalidResponse),
            (vec![bytes(b"x")], 0, NativeError::InvalidInput),
            (vec![Chunk::Error(ErrorKind::Io)], 3, NativeError::Transport),
            (
                vec![Chunk::Error(ErrorKind::DataConversion)],
                3,
                NativeError::InvalidResponse,
            ),
        ] {
            let (reply, probe) = response(200, &[], chunks);
            let fixture = Fixture::new([reply]);
            let deadline = deadline();
            let mut result = fixture
                .executor()
                .open(
                    &request(method),
                    NativeStreamLimits {
                        max_fragment_bytes: 3,
                        max_body_bytes: 5,
                    },
                    &deadline,
                )
                .await
                .unwrap();
            let expected = if method == NativeMethod::Post {
                NativeError::OutcomeUnknown
            } else {
                expected
            };
            assert_eq!(
                failure(result.body.read(read_size, &deadline).await),
                expected
            );
            let polls = probe.polls.load(Ordering::SeqCst);
            assert_eq!(failure(result.body.read(3, &deadline).await), expected);
            assert_eq!(probe.polls.load(Ordering::SeqCst), polls);
            assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        }
    }
    let (reply, probe) = response(429, &[], [Chunk::Error(ErrorKind::Io)]);
    let fixture = Fixture::new([reply]);
    let deadline = deadline();
    let mut result = fixture
        .executor()
        .open(&request(NativeMethod::Post), limits(), &deadline)
        .await
        .unwrap();
    assert_eq!(
        failure(result.body.read(3, &deadline).await),
        NativeError::Http(429)
    );
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cumulative_budget_rejects_the_entire_excess_chunk_before_exposure() {
    let (reply, probe) = response(200, &[], [bytes(b"abc"), bytes(b"de"), bytes(b"fg")]);
    let fixture = Fixture::new([reply]);
    let deadline = deadline();
    let mut result = fixture
        .executor()
        .open(
            &request(NativeMethod::Get),
            NativeStreamLimits {
                max_fragment_bytes: 2,
                max_body_bytes: 6,
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
async fn shorter_read_deadline_disposes_without_polling_and_unpolled_reads_do_not_cancel() {
    let (reply, probe) = response(200, &[], [bytes(b"abc")]);
    let fixture = Fixture::new([reply]);
    let deadline = deadline();
    let mut result = fixture
        .executor()
        .open(&request(NativeMethod::Get), limits(), &deadline)
        .await
        .unwrap();
    drop(result.body.read(3, &deadline));
    assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 0);
    let short = NativeDeadline::new(Duration::from_millis(1)).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    assert_eq!(
        failure(result.body.read(3, &short).await),
        NativeError::TimedOut
    );
    assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn pending_reads_cancel_or_timeout_and_cannot_resume_or_extend_deadline() {
    for cancel in [true, false] {
        let (reply, probe) = response(200, &[], [Chunk::Pending]);
        let fixture = Fixture::new([reply]);
        let deadline = deadline();
        let mut result = fixture
            .executor()
            .open(&request(NativeMethod::Get), limits(), &deadline)
            .await
            .unwrap();
        let mut reading = result.body.read(3, &deadline);
        assert_pending(reading.as_mut());
        if !cancel {
            fixture.timer.expired.store(true, Ordering::SeqCst);
            assert_eq!(failure(reading.as_mut().await), NativeError::TimedOut);
        }
        drop(reading);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            failure(result.body.read(3, &deadline).await),
            if cancel {
                NativeError::Cancelled
            } else {
                NativeError::TimedOut
            }
        );
    }
    let (reply, probe) = response(200, &[], [bytes(b"abc")]);
    let fixture = Fixture::new([reply]);
    let opening_deadline = NativeDeadline::new(Duration::from_millis(10)).unwrap();
    let mut result = fixture
        .executor()
        .open(&request(NativeMethod::Get), limits(), &opening_deadline)
        .await
        .unwrap();
    std::thread::sleep(Duration::from_millis(25));
    assert_eq!(
        failure(result.body.read(3, &deadline()).await),
        NativeError::TimedOut
    );
    assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn late_headers_and_fragments_preserve_postdispatch_uncertainty() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        let fixture = Fixture::new([]);
        let (Reply::Response(raw), probe) = response(200, &[], [bytes(b"x")]) else {
            unreachable!()
        };
        fixture
            .wire
            .replies
            .lock()
            .unwrap()
            .push_back(Reply::Late(raw, fixture.timer.expired.clone()));
        let expected = if method == NativeMethod::Post {
            NativeError::OutcomeUnknown
        } else {
            NativeError::TimedOut
        };
        assert_eq!(
            failure(
                fixture
                    .executor()
                    .open(&request(method), limits(), &deadline())
                    .await
            ),
            expected
        );
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);

        let fixture = Fixture::new([]);
        let (reply, probe) = response(
            200,
            &[],
            [Chunk::Late(
                Bytes::from_static(b"x"),
                fixture.timer.expired.clone(),
            )],
        );
        fixture.wire.replies.lock().unwrap().push_back(reply);
        let deadline = deadline();
        let mut result = fixture
            .executor()
            .open(&request(method), limits(), &deadline)
            .await
            .unwrap();
        assert_eq!(failure(result.body.read(3, &deadline).await), expected);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn metadata_bounds_and_debug_redaction_do_not_hide_failure_or_leak_data() {
    for fields in [
        vec![("content-type", "bad\r\nvalue".to_owned())],
        vec![("x-large", "x".repeat(16 * 1024))],
    ] {
        let borrowed: Vec<_> = fields
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let (reply, probe) = response(200, &borrowed, []);
        let fixture = Fixture::new([reply]);
        assert_eq!(
            failure(
                fixture
                    .executor()
                    .open(&request(NativeMethod::Post), limits(), &deadline())
                    .await
            ),
            NativeError::OutcomeUnknown
        );
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    }
    let fixture = Fixture::new([]);
    let debug = format!("{:?}", fixture.executor());
    for private in [BASE, SCOPE, TOKEN, "synthetic", "provider-error"] {
        assert!(!debug.contains(private));
    }
}
