// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native-runtime requests dispatched through the production HTTP engine.

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
        NativeEffect, NativeEndpoint, NativeError, NativeExecutor, NativeMethod, NativeRequest,
    },
};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor,
    native_runtime::NativeFabricExecutor,
};
use futures_util::{Stream, stream};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{pending, poll_fn},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

const BODY_CAP: usize = 4 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(240);
const TEST_TOKEN: &str = "offline-native-adapter-token";
const BASE: &str = concat!(
    "https://api.fabric.microsoft.com/v1/workspaces/ws/",
    "lakehouses/lake/livyApi/versions/2024-04-01"
);

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
    fail: bool,
    token: Option<&'static str>,
    wait: bool,
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
        if self.wait {
            let _guard = Dropped(&self.dropped);
            pending::<()>().await;
        }
        if self.fail {
            return Err(private_error());
        }
        Ok(AccessToken::new(
            self.token.unwrap_or(TEST_TOKEN),
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Default)]
struct Timer {
    expired: AtomicBool,
    waits: Mutex<Vec<Duration>>,
    dropped: AtomicUsize,
}

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        self.waits.lock().unwrap().push(delay);
        Box::pin(async move {
            let _guard = Dropped(&self.dropped);
            poll_fn(|_| {
                if delay == DEADLINE && !self.expired.load(Ordering::SeqCst) {
                    Poll::Pending
                } else {
                    Poll::Ready(())
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
    Pending,
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
            .expect("unexpected dispatch or mutation replay");
        match reply {
            Reply::Response(response) => Ok(response),
            Reply::Failure => Err(private_error()),
            Reply::Pending => {
                let _guard = Dropped(&self.dropped);
                pending().await
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

fn private_error() -> azure_core::Error {
    azure_core::Error::with_message(
        ErrorKind::Io,
        "private-provider-body-token-url-must-not-be-logged",
    )
}

fn response(status: u16, body: &[u8], headers: &[(&str, &str)]) -> Reply {
    let mut values = Headers::new();
    for &(name, value) in headers {
        values.insert(name.to_owned(), value.to_owned());
    }
    Reply::Response(AsyncRawResponse::from_bytes(
        status.into(),
        values,
        body.to_vec(),
    ))
}

fn endpoint(host: &str) -> EndpointBinding {
    EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap()
}

fn context(
    credential: Arc<Credential>,
    endpoint: EndpointBinding,
    audience: Audience,
) -> RequestContext {
    RequestContext::with_endpoint(
        credential,
        CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap(),
        endpoint,
    )
}

fn http(wire: Arc<Wire>, timer: Arc<Timer>, endpoint: EndpointBinding) -> HttpExecutor {
    HttpExecutor::new(wire, timer, ExecutionOptions::default())
        .unwrap()
        .with_endpoint(endpoint)
}

fn adapter(
    credential: Arc<Credential>,
    wire: Arc<Wire>,
    timer: Arc<Timer>,
) -> NativeFabricExecutor {
    let endpoint = endpoint("api.fabric.microsoft.com");
    NativeFabricExecutor::new(
        context(credential, endpoint.clone(), Audience::Fabric),
        http(wire, timer, endpoint),
    )
    .unwrap()
}

fn request_at(
    base: &str,
    method: NativeMethod,
    body: Option<Value>,
    effect: NativeEffect,
) -> NativeRequest {
    NativeEndpoint::new(Url::parse(base).unwrap())
        .unwrap()
        .request(method, &["sessions"], &[], body, effect)
        .unwrap()
}

fn request(method: NativeMethod) -> NativeRequest {
    let effect = if matches!(method, NativeMethod::Get | NativeMethod::Head) {
        NativeEffect::Read
    } else {
        NativeEffect::Execute
    };
    request_at(BASE, method, None, effect)
}

fn error<T>(result: Result<T, NativeError>) -> NativeError {
    match result {
        Ok(_) => panic!("expected a sanitized native failure"),
        Err(error) => error,
    }
}

fn assert_no_io(credential: &Credential, wire: &Wire, timer: &Timer) {
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(wire.requests.lock().unwrap().is_empty());
    assert!(timer.waits.lock().unwrap().is_empty());
}

fn json_header_bytes() -> usize {
    "accept".len()
        + "application/json".len()
        + 4
        + "content-type".len()
        + "application/json".len()
        + 4
}

#[tokio::test]
async fn explicit_public_daily_and_msit_bindings_preserve_get_wire_and_scope() {
    for host in [
        "api.fabric.microsoft.com",
        "dailyapi.fabric.microsoft.com",
        "msitapi.fabric.microsoft.com",
    ] {
        let credential = Arc::new(Credential::default());
        let wire = Wire::new([response(
            200,
            b"native\x00\xffbody",
            &[
                (
                    "content-type",
                    "application/vnd.provider+json; charset=utf-8",
                ),
                ("ETag", "\"observed-version\""),
                ("x-native-result", "retained"),
            ],
        )]);
        let timer = Arc::new(Timer::default());
        let binding = endpoint(host);
        let executor = NativeFabricExecutor::new(
            context(credential.clone(), binding.clone(), Audience::Fabric),
            http(wire.clone(), timer.clone(), binding),
        )
        .unwrap();
        let base = Url::parse(&format!("https://{host}/v1/workspaces/ws")).unwrap();
        let request = NativeEndpoint::new(base)
            .unwrap()
            .request(
                NativeMethod::Get,
                &["sessions", "owner id"],
                &[("parts", "1&other=wrong".into())],
                None,
                NativeEffect::Read,
            )
            .unwrap()
            .with_accept("application/vnd.provider+json")
            .unwrap()
            .with_header("X-Native-Protocol", "1")
            .unwrap()
            .with_header("If-None-Match", "\"previous-version\"")
            .unwrap();
        let result = request.execute(&executor, 64).await.unwrap();
        assert_eq!(result.status, 200);
        assert_eq!(result.body, b"native\x00\xffbody");
        assert_eq!(
            result.content_type.as_deref(),
            Some("application/vnd.provider+json; charset=utf-8")
        );
        assert_eq!(result.header("etag").unwrap(), Some("\"observed-version\""));
        assert_eq!(result.headers["x-native-result"], "retained");
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let sent = &requests[0];
        assert_eq!(sent.method(), Method::Get);
        assert_eq!(sent.url(), request.url());
        assert_eq!(sent.url().path(), "/v1/workspaces/ws/sessions/owner%20id");
        assert_eq!(sent.url().query(), Some("parts=1%26other%3Dwrong"));
        assert!(Bytes::from(sent.body()).is_empty());
        for (name, value) in [
            ("accept", "application/vnd.provider+json"),
            ("content-type", "application/json"),
            ("x-native-protocol", "1"),
            ("if-none-match", "\"previous-version\""),
        ] {
            assert_eq!(sent.headers().get_str(&name.into()).unwrap(), value);
        }
        assert_eq!(
            sent.headers().get_str(&"authorization".into()).unwrap(),
            format!("Bearer {TEST_TOKEN}")
        );
        assert_eq!(
            *credential.scopes.lock().unwrap(),
            [vec![Audience::Fabric.scope().to_owned()]]
        );
        assert_eq!(*timer.waits.lock().unwrap(), [DEADLINE]);
    }
}

#[tokio::test]
async fn post_json_202_is_acceptance_not_completion_and_preserves_receipt_headers() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([response(
        202,
        br#"{"id":7,"state":"starting"}"#,
        &[
            ("Content-Type", "application/json"),
            ("Location", "/native/session/7"),
            ("Retry-After", "3"),
        ],
    )]);
    let timer = Arc::new(Timer::default());
    let executor = adapter(credential.clone(), wire.clone(), timer.clone());
    let body = json!({"code": "print(\"private\")\n", "kind": "pyspark"});
    let request = request_at(
        BASE,
        NativeMethod::Post,
        Some(body.clone()),
        NativeEffect::Execute,
    )
    .with_header("If-Match", "\"owner-version\"")
    .unwrap();
    let result = request.execute(&executor, 1024).await.unwrap();
    assert_eq!(result.status, 202);
    assert_eq!(result.body, br#"{"id":7,"state":"starting"}"#);
    assert_eq!(result.content_type.as_deref(), Some("application/json"));
    assert_eq!(
        result.header("location").unwrap(),
        Some("/native/session/7")
    );
    assert_eq!(result.header("retry-after").unwrap(), Some("3"));
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method(), Method::Post);
    assert_eq!(requests[0].url(), request.url());
    let expected_body = serde_json::to_vec(&body).unwrap();
    assert_eq!(
        Bytes::from(requests[0].body()).as_ref(),
        expected_body.as_slice()
    );
    assert_eq!(
        requests[0].headers().get_str(&"if-match".into()).unwrap(),
        "\"owner-version\""
    );
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    assert_eq!(*timer.waits.lock().unwrap(), [DEADLINE]);
}

#[tokio::test]
async fn finite_methods_preserve_raw_bytes_and_explicit_media_without_reencoding() {
    for (native, azure) in [
        (NativeMethod::Post, Method::Post),
        (NativeMethod::Put, Method::Put),
        (NativeMethod::Patch, Method::Patch),
        (NativeMethod::Delete, Method::Delete),
        (NativeMethod::Head, Method::Head),
    ] {
        let credential = Arc::new(Credential::default());
        let wire = Wire::new([response(204, b"", &[])]);
        let executor = adapter(credential.clone(), wire.clone(), Arc::new(Timer::default()));
        let mut request = request(native);
        if native != NativeMethod::Head {
            request = request
                .with_bytes(
                    b"\x00\xffnot-json".to_vec(),
                    "application/octet-stream",
                    1024,
                )
                .unwrap();
        }
        let result = request.execute(&executor, 1).await.unwrap();
        assert_eq!(result.status, 204);
        assert!(result.body.is_empty());
        assert!(result.content_type.is_none());
        assert!(result.headers.is_empty());
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), azure);
        assert_eq!(requests[0].url(), request.url());
        if native != NativeMethod::Head {
            assert_eq!(
                Bytes::from(requests[0].body()).as_ref(),
                b"\x00\xffnot-json"
            );
            assert_eq!(
                requests[0]
                    .headers()
                    .get_str(&"content-type".into())
                    .unwrap(),
                "application/octet-stream"
            );
        }
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn get_retries_share_one_token_and_one_deadline_but_head_and_writes_never_replay() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([
        response(503, b"retry", &[("retry-after", "2")]),
        response(200, b"done", &[]),
    ]);
    let timer = Arc::new(Timer::default());
    let executor = adapter(credential.clone(), wire.clone(), timer.clone());
    assert_eq!(
        request(NativeMethod::Get)
            .execute(&executor, 16)
            .await
            .unwrap()
            .body,
        b"done"
    );
    assert_eq!(wire.requests.lock().unwrap().len(), 2);
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    assert_eq!(
        *timer.waits.lock().unwrap(),
        [DEADLINE, Duration::from_secs(2)]
    );

    for method in [
        NativeMethod::Post,
        NativeMethod::Put,
        NativeMethod::Patch,
        NativeMethod::Delete,
        NativeMethod::Head,
    ] {
        for status in [307, 413, 429, 503] {
            let wire = Wire::new([response(
                status,
                b"actual-service-body",
                &[
                    ("retry-after", "1"),
                    ("location", "https://foreign.invalid/"),
                ],
            )]);
            let timer = Arc::new(Timer::default());
            let executor = adapter(Arc::new(Credential::default()), wire.clone(), timer.clone());
            let result = request(method).execute(&executor, 64).await.unwrap();
            assert_eq!(result.status, status);
            assert_eq!(result.body, b"actual-service-body");
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
            assert_eq!(*timer.waits.lock().unwrap(), [DEADLINE]);
        }
    }
}

#[tokio::test]
async fn get_retry_budget_delay_validation_and_redirect_status_are_not_reimplemented() {
    for (replies, expected, sends) in [
        (
            (0..4).map(|_| response(503, b"", &[])).collect::<Vec<_>>(),
            NativeError::InvalidResponse,
            4,
        ),
        (
            vec![response(429, b"", &[("retry-after", "61")])],
            NativeError::InvalidResponse,
            1,
        ),
        (
            vec![response(429, b"", &[("retry-after", "malformed")])],
            NativeError::InvalidResponse,
            1,
        ),
    ] {
        let credential = Arc::new(Credential::default());
        let wire = Wire::new(replies);
        let executor = adapter(credential.clone(), wire.clone(), Arc::new(Timer::default()));
        assert_eq!(
            error(request(NativeMethod::Get).execute(&executor, 16).await),
            expected
        );
        assert_eq!(wire.requests.lock().unwrap().len(), sends);
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    }
    let wire = Wire::new([response(
        302,
        b"",
        &[("location", "https://foreign.invalid/")],
    )]);
    let executor = adapter(
        Arc::new(Credential::default()),
        wire.clone(),
        Arc::new(Timer::default()),
    );
    assert_eq!(
        request(NativeMethod::Get)
            .execute(&executor, 16)
            .await
            .unwrap()
            .status,
        302
    );
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn lost_transport_and_stream_errors_preserve_mutation_uncertainty_without_replay() {
    for method in [
        NativeMethod::Get,
        NativeMethod::Head,
        NativeMethod::Post,
        NativeMethod::Put,
        NativeMethod::Patch,
        NativeMethod::Delete,
    ] {
        for streaming in [false, true] {
            let reply = if streaming {
                Reply::Response(AsyncRawResponse::new(
                    200.into(),
                    Headers::new(),
                    Box::pin(stream::iter([Err(private_error())])),
                ))
            } else {
                Reply::Failure
            };
            let wire = Wire::new([reply]);
            let executor = adapter(
                Arc::new(Credential::default()),
                wire.clone(),
                Arc::new(Timer::default()),
            );
            let failure = error(request(method).execute(&executor, 16).await);
            let expected = if matches!(method, NativeMethod::Get | NativeMethod::Head) {
                NativeError::InvalidResponse
            } else {
                NativeError::OutcomeUnknown
            };
            assert_eq!(failure, expected);
            assert!(!format!("{failure:?}: {failure}").contains("private"));
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn post_declared_as_a_provider_read_still_never_replays_or_hides_a_lost_response() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([response(429, b"throttled", &[]), Reply::Failure]);
    let executor = adapter(credential.clone(), wire.clone(), Arc::new(Timer::default()));
    let request = request_at(BASE, NativeMethod::Post, None, NativeEffect::Read);
    assert_eq!(request.execute(&executor, 16).await.unwrap().status, 429);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
    assert_eq!(
        error(request.execute(&executor, 16).await),
        NativeError::OutcomeUnknown
    );
    assert_eq!(wire.requests.lock().unwrap().len(), 2);
    assert_eq!(credential.scopes.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn existing_deadline_drops_pending_transport_and_reports_uncertain_writes() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        let credential = Arc::new(Credential::default());
        let wire = Wire::new([Reply::Pending]);
        let timer = Arc::new(Timer::default());
        let executor = adapter(credential, wire.clone(), timer.clone());
        let request = request(method);
        let mut future = executor.execute(&request, 16);
        assert!(
            future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        timer.expired.store(true, Ordering::SeqCst);
        assert_eq!(
            error(future.await),
            if method == NativeMethod::Get {
                NativeError::TimedOut
            } else {
                NativeError::OutcomeUnknown
            }
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        assert_eq!(wire.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(timer.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(*timer.waits.lock().unwrap(), [DEADLINE]);
    }
}

#[tokio::test]
async fn already_expired_http_deadline_does_not_send_or_create_a_second_timer() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        let credential = Arc::new(Credential::default());
        let wire = Wire::new([]);
        let timer = Arc::new(Timer::default());
        timer.expired.store(true, Ordering::SeqCst);
        let executor = adapter(credential.clone(), wire.clone(), timer.clone());
        assert_eq!(
            error(request(method).execute(&executor, 1).await),
            if method == NativeMethod::Get {
                NativeError::TimedOut
            } else {
                NativeError::OutcomeUnknown
            }
        );
        assert!(wire.requests.lock().unwrap().is_empty());
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        assert_eq!(*timer.waits.lock().unwrap(), [DEADLINE]);
    }
}

struct Body {
    polls: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
    pending: bool,
}

impl Stream for Body {
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

impl Drop for Body {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn streaming_cap_stops_at_bound_plus_one_and_deadline_covers_response_body() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        for wait in [false, true] {
            let polls = Arc::new(AtomicUsize::new(0));
            let dropped = Arc::new(AtomicBool::new(false));
            let wire = Wire::new([Reply::Response(AsyncRawResponse::new(
                202.into(),
                Headers::new(),
                Box::pin(Body {
                    polls: polls.clone(),
                    dropped: dropped.clone(),
                    pending: wait,
                }),
            ))]);
            let timer = Arc::new(Timer::default());
            let executor = adapter(Arc::new(Credential::default()), wire.clone(), timer.clone());
            let request = request(method);
            let mut future = executor.execute(&request, 5);
            if wait {
                assert!(
                    future
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                        .is_pending()
                );
                timer.expired.store(true, Ordering::SeqCst);
            }
            let expected = if method != NativeMethod::Get {
                NativeError::OutcomeUnknown
            } else if wait {
                NativeError::TimedOut
            } else {
                NativeError::Limit
            };
            assert_eq!(error(future.await), expected);
            assert_eq!(polls.load(Ordering::SeqCst), if wait { 1 } else { 6 });
            assert!(dropped.load(Ordering::SeqCst));
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn response_header_bounds_remain_explicit_for_reads_and_uncertain_for_writes() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        for excessive_count in [false, true] {
            let mut headers = Headers::new();
            if excessive_count {
                for index in 0..65 {
                    headers.insert(format!("x-{index}"), "v".to_owned());
                }
            } else {
                headers.insert("content-type", "x".repeat(16 * 1024 + 1));
            }
            let wire = Wire::new([Reply::Response(AsyncRawResponse::from_bytes(
                202.into(),
                headers,
                vec![],
            ))]);
            let executor = adapter(
                Arc::new(Credential::default()),
                wire.clone(),
                Arc::new(Timer::default()),
            );
            assert_eq!(
                error(request(method).execute(&executor, 1).await),
                if method == NativeMethod::Get {
                    NativeError::Limit
                } else {
                    NativeError::OutcomeUnknown
                }
            );
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
        }
    }
}

#[test]
fn mismatched_binding_audience_or_service_is_rejected_without_io() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([]);
    let timer = Arc::new(Timer::default());
    let public = endpoint("api.fabric.microsoft.com");
    for (context_endpoint, http_endpoint, audience, expected) in [
        (
            public.clone(),
            public.clone(),
            Audience::Arm,
            NativeError::InvalidInput,
        ),
        (
            endpoint("dailyapi.fabric.microsoft.com"),
            public.clone(),
            Audience::Fabric,
            NativeError::InvalidInput,
        ),
        (
            public.clone(),
            endpoint("msitapi.fabric.microsoft.com"),
            Audience::Fabric,
            NativeError::InvalidInput,
        ),
        (
            EndpointBinding::public(ServiceOrigin::PowerBi),
            EndpointBinding::public(ServiceOrigin::PowerBi),
            Audience::Fabric,
            NativeError::UnsupportedCapability,
        ),
        (
            EndpointBinding::public(ServiceOrigin::Arm),
            EndpointBinding::public(ServiceOrigin::Arm),
            Audience::Arm,
            NativeError::UnsupportedCapability,
        ),
        (
            EndpointBinding::public(ServiceOrigin::OneLake),
            EndpointBinding::public(ServiceOrigin::OneLake),
            Audience::OneLake,
            NativeError::UnsupportedCapability,
        ),
        (
            public,
            EndpointBinding::public(ServiceOrigin::PowerBi),
            Audience::Fabric,
            NativeError::UnsupportedCapability,
        ),
    ] {
        assert_eq!(
            error(NativeFabricExecutor::new(
                context(credential.clone(), context_endpoint, audience),
                http(wire.clone(), timer.clone(), http_endpoint),
            )),
            expected
        );
    }
    assert_no_io(&credential, &wire, &timer);
}

#[tokio::test]
async fn foreign_origin_port_scheme_and_wrong_prefix_fail_before_credentials() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([]);
    let timer = Arc::new(Timer::default());
    let executor = adapter(credential.clone(), wire.clone(), timer.clone());
    for (base, expected) in [
        (
            "https://dailyapi.fabric.microsoft.com/v1",
            NativeError::InvalidInput,
        ),
        (
            "https://msitapi.fabric.microsoft.com/v1",
            NativeError::InvalidInput,
        ),
        ("https://api.powerbi.com/v1", NativeError::InvalidInput),
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
            "https://api.fabric.microsoft.com/",
            NativeError::InvalidInput,
        ),
        (
            "https://api.fabric.microsoft.com.example.test/v1",
            NativeError::UnsupportedCapability,
        ),
        (
            "https://native.example.test/v1",
            NativeError::UnsupportedCapability,
        ),
        (
            "https://cluster.pbidedicated.windows.net/v1",
            NativeError::UnsupportedCapability,
        ),
        (
            "http://127.0.0.1:9000/v1",
            NativeError::UnsupportedCapability,
        ),
    ] {
        let request = request_at(base, NativeMethod::Get, None, NativeEffect::Read);
        assert_eq!(error(executor.execute(&request, 1).await), expected);
    }
    assert_no_io(&credential, &wire, &timer);
}

#[tokio::test]
async fn zero_bounds_and_get_declared_mutations_do_not_acquire_or_send() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([]);
    let timer = Arc::new(Timer::default());
    let executor = adapter(credential.clone(), wire.clone(), timer.clone());
    assert_eq!(
        error(executor.execute(&request(NativeMethod::Post), 0).await),
        NativeError::InvalidInput
    );
    assert_eq!(
        error(request(NativeMethod::Get).execute(&executor, 0).await),
        NativeError::InvalidInput
    );
    for method in [NativeMethod::Get, NativeMethod::Head] {
        let request = request_at(BASE, method, None, NativeEffect::Execute);
        assert_eq!(
            error(executor.execute(&request, 1).await),
            NativeError::InvalidInput
        );
    }
    assert_no_io(&credential, &wire, &timer);
}

#[tokio::test]
async fn auth_host_and_hop_header_overrides_are_rejected_before_credentials() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([]);
    let timer = Arc::new(Timer::default());
    let executor = adapter(credential.clone(), wire.clone(), timer.clone());
    for name in [
        "Authorization",
        "hOsT",
        "Proxy-Authorization",
        "Cookie",
        "Content-Length",
        "Connection",
        "Transfer-Encoding",
        "Content-Type",
        "Accept",
    ] {
        assert_eq!(
            error(request(NativeMethod::Post).with_header(name, "override")),
            NativeError::InvalidInput
        );
    }
    for name in [
        "Proxy-Authenticate",
        "Set-Cookie",
        "Upgrade",
        "TE",
        "Trailer",
        "Keep-Alive",
        "Proxy-Connection",
    ] {
        let request = request(NativeMethod::Post)
            .with_header(name, "override")
            .unwrap();
        assert_eq!(
            error(executor.execute(&request, 1).await),
            NativeError::InvalidInput
        );
    }
    for (name, value) in [("", "v"), ("x invalid", "v"), ("x-native", "bad\r\nvalue")] {
        assert_eq!(
            error(request(NativeMethod::Post).with_header(name, value)),
            NativeError::InvalidInput
        );
    }
    assert_eq!(
        error(request(NativeMethod::Get).with_accept("bad\nmedia")),
        NativeError::InvalidInput
    );
    assert_no_io(&credential, &wire, &timer);
}

#[tokio::test]
async fn url_header_and_raw_body_budgets_are_preflighted_before_credentials() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([]);
    let timer = Arc::new(Timer::default());
    let executor = adapter(credential.clone(), wire.clone(), timer.clone());
    let mut many_headers = request(NativeMethod::Post);
    for index in 0..63 {
        many_headers = many_headers
            .with_header(&format!("x-{index}"), "v")
            .unwrap();
    }
    let prefix = "https://api.fabric.microsoft.com/v1/";
    let oversized_url = request_at(
        &format!(
            "{prefix}{}",
            "x".repeat(16 * 1024 + 1 - prefix.len() - "/sessions".len())
        ),
        NativeMethod::Get,
        None,
        NativeEffect::Read,
    );
    assert_eq!(oversized_url.url().as_str().len(), 16 * 1024 + 1);
    for request in [
        many_headers,
        oversized_url,
        request(NativeMethod::Post)
            .with_header(
                "x-native",
                &"x".repeat(16 * 1024 + 1 - json_header_bytes() - "x-native".len() - 4),
            )
            .unwrap(),
        request(NativeMethod::Post)
            .with_accept(&"x".repeat(16 * 1024))
            .unwrap(),
        request(NativeMethod::Post)
            .with_bytes(
                vec![0; BODY_CAP + 1],
                "application/octet-stream",
                BODY_CAP + 1,
            )
            .unwrap(),
        request(NativeMethod::Post)
            .with_bytes(vec![], &"x".repeat(16 * 1024), 1)
            .unwrap(),
    ] {
        assert_eq!(
            error(executor.execute(&request, 1).await),
            NativeError::Limit
        );
    }
    assert_no_io(&credential, &wire, &timer);
}

#[tokio::test]
async fn json_wire_size_escaping_depth_and_node_budgets_are_checked_before_allocation_and_token() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new([]);
    let timer = Arc::new(Timer::default());
    let executor = adapter(credential.clone(), wire.clone(), timer.clone());
    let mut deep = Value::Null;
    for _ in 0..63 {
        deep = Value::Array(vec![deep]);
    }
    for body in [
        json!({"x": "x".repeat(BODY_CAP - 7)}),
        json!({"x": format!("{}xxx", "\0".repeat((BODY_CAP - 8) / 6))}),
        json!({"x": deep}),
        json!({"x": vec![Value::Null; 65_535]}),
        json!({("x".repeat(BODY_CAP + 1)): 0}),
    ] {
        let request = request_at(BASE, NativeMethod::Post, Some(body), NativeEffect::Execute);
        assert_eq!(
            error(executor.execute(&request, 1).await),
            NativeError::Limit
        );
    }
    assert_no_io(&credential, &wire, &timer);
}

#[tokio::test]
async fn exact_request_body_url_header_count_and_metadata_byte_budgets_are_accepted() {
    let credential = Arc::new(Credential::default());
    let wire = Wire::new((0..7).map(|_| response(200, b"x", &[])));
    let executor = adapter(credential.clone(), wire.clone(), Arc::new(Timer::default()));
    let json = json!({"x": "x".repeat(BODY_CAP - 8)});
    assert_eq!(serde_json::to_vec(&json).unwrap().len(), BODY_CAP);
    let escaped = json!({"x": format!("{}xx", "\0".repeat((BODY_CAP - 8) / 6))});
    assert_eq!(serde_json::to_vec(&escaped).unwrap().len(), BODY_CAP);
    let mut headers = request(NativeMethod::Post);
    for index in 0..62 {
        headers = headers.with_header(&format!("x-{index}"), "v").unwrap();
    }
    let metadata = request(NativeMethod::Post)
        .with_header(
            "x-native",
            &"x".repeat(16 * 1024 - json_header_bytes() - "x-native".len() - 4),
        )
        .unwrap();
    let prefix = "https://api.fabric.microsoft.com/v1/";
    let exact_url = format!(
        "{prefix}{}",
        "x".repeat(16 * 1024 - prefix.len() - "/sessions".len())
    );
    let long_url = request_at(&exact_url, NativeMethod::Get, None, NativeEffect::Read);
    assert_eq!(long_url.url().as_str().len(), 16 * 1024);
    let mut deep = Value::Null;
    for _ in 0..62 {
        deep = Value::Array(vec![deep]);
    }
    for request in [
        request_at(BASE, NativeMethod::Post, Some(json), NativeEffect::Execute),
        request_at(
            BASE,
            NativeMethod::Post,
            Some(escaped),
            NativeEffect::Execute,
        ),
        request(NativeMethod::Put)
            .with_bytes(vec![0xff; BODY_CAP], "application/octet-stream", BODY_CAP)
            .unwrap(),
        headers,
        metadata,
        long_url,
        request_at(
            BASE,
            NativeMethod::Post,
            Some(json!({"x": deep})),
            NativeEffect::Execute,
        ),
    ] {
        assert_eq!(request.execute(&executor, 1).await.unwrap().body, b"x");
    }
    assert_eq!(wire.requests.lock().unwrap().len(), 7);
    assert_eq!(credential.scopes.lock().unwrap().len(), 7);
}

#[tokio::test]
async fn exact_json_node_budget_is_accepted() {
    let wire = Wire::new([response(200, b"", &[])]);
    let executor = adapter(
        Arc::new(Credential::default()),
        wire.clone(),
        Arc::new(Timer::default()),
    );
    let request = request_at(
        BASE,
        NativeMethod::Post,
        Some(json!({"x": vec![Value::Null; 65_534]})),
        NativeEffect::Execute,
    );
    request.execute(&executor, 1).await.unwrap();
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn failed_or_malformed_caller_credentials_are_sanitized_and_never_sent() {
    for credential in [
        Credential {
            fail: true,
            ..Default::default()
        },
        Credential {
            token: Some(""),
            ..Default::default()
        },
        Credential {
            token: Some("invalid\nbearer"),
            ..Default::default()
        },
    ] {
        let credential = Arc::new(credential);
        let wire = Wire::new([]);
        let timer = Arc::new(Timer::default());
        let executor = adapter(credential.clone(), wire.clone(), timer.clone());
        let failure = error(request(NativeMethod::Post).execute(&executor, 1).await);
        assert_eq!(
            failure,
            if credential.fail {
                NativeError::InvalidResponse
            } else {
                NativeError::InvalidInput
            }
        );
        assert!(!format!("{failure:?}: {failure}").contains("private"));
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        assert!(wire.requests.lock().unwrap().is_empty());
        assert!(timer.waits.lock().unwrap().is_empty());
    }
}

#[test]
fn dropping_acquisition_or_dispatch_future_is_local_only_and_debug_is_redacted() {
    for acquiring in [true, false] {
        let credential = Arc::new(Credential {
            wait: acquiring,
            ..Default::default()
        });
        let wire = Wire::new(if acquiring {
            vec![]
        } else {
            vec![Reply::Pending]
        });
        let timer = Arc::new(Timer::default());
        let executor = adapter(credential.clone(), wire.clone(), timer.clone());
        let request = request_at(
            BASE,
            NativeMethod::Post,
            Some(json!({"code": "private-code"})),
            NativeEffect::Execute,
        );
        let mut future = executor.execute(&request, 16);
        assert!(
            future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        drop(future);
        assert_eq!(wire.requests.lock().unwrap().len(), usize::from(!acquiring));
        assert_eq!(
            credential.dropped.load(Ordering::SeqCst),
            usize::from(acquiring)
        );
        assert_eq!(wire.dropped.load(Ordering::SeqCst), usize::from(!acquiring));
        assert_eq!(
            timer.dropped.load(Ordering::SeqCst),
            usize::from(!acquiring)
        );
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        let debug = format!("{executor:?} {request:?}");
        for secret in [
            BASE,
            TEST_TOKEN,
            "private-code",
            "offline-account",
            "offline-tenant",
        ] {
            assert!(!debug.contains(secret));
        }
    }
}

#[test]
fn dropping_a_response_stream_future_does_not_send_remote_cancellation() {
    let credential = Arc::new(Credential::default());
    let dropped = Arc::new(AtomicBool::new(false));
    let wire = Wire::new([Reply::Response(AsyncRawResponse::new(
        202.into(),
        Headers::new(),
        Box::pin(Body {
            polls: Arc::new(AtomicUsize::new(0)),
            dropped: dropped.clone(),
            pending: true,
        }),
    ))]);
    let timer = Arc::new(Timer::default());
    let executor = adapter(credential.clone(), wire.clone(), timer.clone());
    let request = request(NativeMethod::Post);
    let mut future = executor.execute(&request, 16);
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    drop(future);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    assert_eq!(timer.dropped.load(Ordering::SeqCst), 1);
}
