// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![cfg(feature = "notebook-workload")]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenRequestOptions},
    error::ErrorKind,
    http::{AsyncRawResponse, HttpClient, Method, Request, Url, headers::Headers},
    time::OffsetDateTime,
};
use fabric_core::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin, TokenCredential,
    principal::{EntraJwtVerifier, JWKS_LIMIT},
};
use fabric_sdk::{
    notebook_runtime::{NotebookRuntime, SessionId, content::ContentLimits},
    runtime::{
        NativeDeadline, NativeEffect, NativeEndpoint, NativeError, NativeExecutor, NativeMethod,
        NativeRequest,
    },
};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor,
    notebook_workload::{
        NativeNotebookExecutor, NotebookEnvironment, NotebookOrigin, NotebookReadScope,
        NotebookWorkloadSelection,
    },
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending, ready},
    pin::{Pin, pin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

#[path = "../../../../../tests/fixtures/notebook-identity/fixture.rs"]
mod fixture;
use fixture::*;

#[derive(Debug)]
struct Credential {
    tokens: Mutex<VecDeque<String>>,
    scopes: Mutex<Vec<Vec<String>>>,
    pending: bool,
    dropped: AtomicUsize,
}

impl Credential {
    fn new(fabric: String, pbi: String) -> Arc<Self> {
        Arc::new(Self {
            tokens: Mutex::new([fabric, pbi].into()),
            scopes: Mutex::new(Vec::new()),
            pending: false,
            dropped: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|value| (*value).to_owned()).collect());
        if self.pending {
            let _guard = Dropped(&self.dropped);
            pending::<()>().await;
        }
        Ok(AccessToken::new(
            self.tokens
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected token access"),
            OffsetDateTime::now_utc() + azure_core::time::Duration::seconds(3500),
        ))
    }
}

#[derive(Default)]
struct Timer(AtomicBool);
impl ExecutionTimer for Timer {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(std::future::poll_fn(|_| {
            if self.0.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }))
    }
}

#[derive(Debug)]
enum Reply {
    Body(u16, Vec<u8>),
    Pending,
    Partial,
    Failure,
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
            .expect("unexpected HTTP or replay");
        match reply {
            Reply::Body(status, bytes) => Ok(AsyncRawResponse::from_bytes(
                status.into(),
                Headers::new(),
                bytes,
            )),
            Reply::Pending => {
                let _guard = Dropped(&self.dropped);
                pending().await
            }
            Reply::Failure => Err(private_error()),
            Reply::Partial => {
                let body = stream::iter([Ok(Bytes::from_static(b"partial")), Err(private_error())])
                    .boxed();
                Ok(AsyncRawResponse::new(200.into(), Headers::new(), body))
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
    azure_core::Error::with_message(ErrorKind::Io, "private-token-body-must-not-leak")
}
fn response(value: Value) -> Reply {
    Reply::Body(200, serde_json::to_vec(&value).unwrap())
}
fn owner() -> CredentialSelection {
    CredentialSelection::new(CLIENT, TENANT, Audience::Fabric).unwrap()
}
fn scope() -> NotebookReadScope {
    NotebookReadScope::new(CAPACITY, WORKSPACE, NOTEBOOK).unwrap()
}
fn selection(owner: CredentialSelection) -> NotebookWorkloadSelection {
    NotebookWorkloadSelection::new(
        owner,
        NotebookEnvironment::Production,
        NotebookOrigin::new(FABRIC).unwrap(),
        NotebookOrigin::new(CLUSTER).unwrap(),
        NotebookOrigin::new(WORKLOAD).unwrap(),
        scope(),
    )
    .unwrap()
}
fn http(wire: Arc<Wire>, timer: Arc<Timer>) -> HttpExecutor {
    HttpExecutor::new(wire, timer, ExecutionOptions::default()).unwrap()
}
fn executor(
    credential: Arc<Credential>,
    wire: Arc<Wire>,
    timer: Arc<Timer>,
) -> NativeNotebookExecutor {
    NativeNotebookExecutor::new(
        RequestContext::new(credential, owner()),
        http(wire, timer),
        selection(owner()),
        NativeDeadline::new(Duration::from_secs(60)).unwrap(),
    )
    .unwrap()
}
fn runtime(workload: &str, capacity: &str, workspace: &str, notebook: &str) -> NotebookRuntime {
    NotebookRuntime::new(
        NativeEndpoint::new(Url::parse(workload).unwrap()).unwrap(),
        capacity,
        workspace,
        notebook,
        ContentLimits::default(),
        1024 * 1024,
        100,
    )
    .unwrap()
}
fn content() -> NativeRequest {
    runtime(WORKLOAD, CAPACITY, WORKSPACE, NOTEBOOK)
        .content_get_request()
        .unwrap()
}
fn replies() -> Vec<Reply> {
    vec![
        response(jwks("offline-key")),
        response(json!({"backendUrl": CLUSTER})),
        response(grant()),
        response(json!({"observed": true})),
    ]
}
fn poll<T>(future: Pin<&mut impl Future<Output = T>>) -> Poll<T> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

#[tokio::test]
async fn signed_selected_context_uses_exact_frozen_get_content_and_session_routes() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/notebook-identity/protocol.json"
    ))
    .unwrap();
    let runtime = runtime(WORKLOAD, CAPACITY, WORKSPACE, NOTEBOOK);
    let session = SessionId::new("existing-session").unwrap();
    let requests = [
        runtime.content_get_request().unwrap(),
        runtime.sessions_request(false).unwrap(),
        runtime.sessions_request(true).unwrap(),
        runtime.session_get_request(&session, false).unwrap(),
        runtime.session_get_request(&session, true).unwrap(),
    ];
    for (request, suffix) in requests
        .iter()
        .zip(oracle["readSuffixes"].as_array().unwrap())
    {
        let fabric = token(FABRIC);
        let pbi = token(PBI);
        let credential = Credential::new(fabric.clone(), pbi.clone());
        let wire = Wire::new(replies());
        let executor = executor(credential.clone(), wire.clone(), Arc::new(Timer::default()));
        assert_eq!(executor.execute(request, 4096).await.unwrap().status, 200);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests[0].url().as_str(),
            format!("https://login.microsoftonline.com/{TENANT}/discovery/keys")
        );
        assert!(
            requests[0]
                .headers()
                .get_str(&"authorization".into())
                .is_err()
        );
        assert_eq!(
            requests[1].url().path(),
            oracle["catalogPath"].as_str().unwrap()
        );
        assert_eq!(
            requests[2].url().path(),
            oracle["exchangePath"].as_str().unwrap()
        );
        assert_eq!(requests[1].method(), Method::Get);
        assert_eq!(requests[2].method(), Method::Post);
        assert_eq!(
            serde_json::from_slice::<Value>(&Bytes::from(requests[2].body())).unwrap(),
            oracle["exchangeBody"]
        );
        for request in &requests[1..3] {
            assert_eq!(
                request.headers().get_str(&"authorization".into()).unwrap(),
                format!("Bearer {pbi}")
            );
            assert!(
                !request
                    .headers()
                    .get_str(&"authorization".into())
                    .unwrap()
                    .contains(&fabric)
            );
        }
        assert_eq!(
            requests[3]
                .headers()
                .get_str(&"authorization".into())
                .unwrap(),
            format!("MwcToken {MWC}")
        );
        assert_eq!(requests[3].method(), Method::Get);
        assert!(
            requests[3]
                .url()
                .as_str()
                .ends_with(suffix.as_str().unwrap())
        );
        assert_eq!(
            *credential.scopes.lock().unwrap(),
            vec![
                vec![Audience::Fabric.scope()],
                vec![Audience::PowerBiBootstrap.scope()]
            ]
        );
    }
}

#[tokio::test]
async fn wrong_origin_resource_scope_method_headers_and_query_reject_without_token_or_http() {
    let credential = Credential::new(token(FABRIC), token(PBI));
    let wire = Wire::new([]);
    let executor = executor(credential.clone(), wire.clone(), Arc::new(Timer::default()));
    let root = NativeEndpoint::new(Url::parse(WORKLOAD).unwrap()).unwrap();
    let invalid = [
        runtime(WORKLOAD, OTHER, WORKSPACE, NOTEBOOK)
            .content_get_request()
            .unwrap(),
        runtime(WORKLOAD, CAPACITY, OTHER, NOTEBOOK)
            .content_get_request()
            .unwrap(),
        runtime(WORKLOAD, CAPACITY, WORKSPACE, OTHER)
            .content_get_request()
            .unwrap(),
        runtime("https://other.example.com", CAPACITY, WORKSPACE, NOTEBOOK)
            .content_get_request()
            .unwrap(),
        runtime(WORKLOAD, CAPACITY, WORKSPACE, NOTEBOOK)
            .content_head_request()
            .unwrap(),
        content().with_header("x-unselected", "forbidden").unwrap(),
        content().with_accept("text/plain").unwrap(),
        root.request(
            NativeMethod::Post,
            &["content"],
            &[],
            None,
            NativeEffect::Read,
        )
        .unwrap(),
        root.request(
            NativeMethod::Get,
            &["content"],
            &[("leak", "value".into())],
            None,
            NativeEffect::Read,
        )
        .unwrap(),
    ];
    for request in invalid {
        assert_eq!(
            executor.execute(&request, 4096).await.err(),
            Some(NativeError::InvalidInput)
        );
    }
    assert_eq!(
        executor.execute(&content(), 0).await.err(),
        Some(NativeError::InvalidInput)
    );
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[test]
fn mismatched_owner_tenant_audience_and_catalog_reject_during_local_composition() {
    for other in [
        CredentialSelection::new(OTHER, TENANT, Audience::Fabric).unwrap(),
        CredentialSelection::new(CLIENT, OTHER, Audience::Fabric).unwrap(),
        CredentialSelection::new(CLIENT, TENANT, Audience::PowerBiBootstrap).unwrap(),
    ] {
        let credential = Credential::new(token(FABRIC), token(PBI));
        let wire = Wire::new([]);
        assert!(
            NativeNotebookExecutor::new(
                RequestContext::new(credential.clone(), other),
                http(wire.clone(), Arc::new(Timer::default())),
                selection(owner()),
                NativeDeadline::new(Duration::from_secs(60)).unwrap()
            )
            .is_err()
        );
        assert!(credential.scopes.lock().unwrap().is_empty());
        assert!(wire.requests.lock().unwrap().is_empty());
    }
    assert!(
        NotebookWorkloadSelection::new(
            owner(),
            NotebookEnvironment::Daily,
            NotebookOrigin::new(FABRIC).unwrap(),
            NotebookOrigin::new(CLUSTER).unwrap(),
            NotebookOrigin::new(WORKLOAD).unwrap(),
            scope()
        )
        .is_err()
    );
}

#[test]
fn exact_origins_reject_raw_authority_and_path_tricks_and_non_guid_scope() {
    for origin in [
        "http://selected.example.com",
        "https://127.0.0.1",
        "https://[::1]",
        "https://2130706433",
        "https://user@selected.example.com",
        "https://selected.example.com:444",
        "https://selected.example.com/path",
        "https://selected.example.com/../",
        "https://selected.example.com?",
        "https://selected.example.com#",
        "https://selected.example.com.",
        "https://selected.example.com\\",
        "https://%73elected.example.com",
        "https://SELECTED.example.com",
        "https://selected.example.com:0443",
        "https://selected.example.com//",
    ] {
        assert!(NotebookOrigin::new(origin).is_err(), "{origin}");
    }
    for value in [
        "",
        "opaque",
        "00000000-0000-0000-0000-000000000000",
        "../escape",
        "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA",
    ] {
        assert!(NotebookReadScope::new(value, WORKSPACE, NOTEBOOK).is_err());
    }
}

async fn reject_tokens(fabric: String, pbi: String, responses: Vec<Reply>) -> (usize, usize) {
    let credential = Credential::new(fabric, pbi);
    let wire = Wire::new(responses);
    let executor = executor(credential.clone(), wire.clone(), Arc::new(Timer::default()));
    assert_eq!(
        executor.execute(&content(), 4096).await.err(),
        Some(NativeError::InvalidResponse)
    );
    let tokens = credential.scopes.lock().unwrap().len();
    let requests = wire.requests.lock().unwrap().len();
    (tokens, requests)
}

#[tokio::test]
async fn jwt_wrong_algorithm_key_header_and_opaque_formats_reject_before_keys() {
    for alg in ["none", "HS256", "RS512", "ES256"] {
        let mut h = header("offline-key");
        h["alg"] = json!(alg);
        assert_eq!(
            reject_tokens(signed(&h, &claims(FABRIC)), token(PBI), vec![]).await,
            (1, 0)
        );
    }
    for name in ["jku", "jwk", "x5u", "crit", "b64"] {
        let mut h = header("offline-key");
        h[name] = json!("untrusted");
        assert_eq!(
            reject_tokens(signed(&h, &claims(FABRIC)), token(PBI), vec![]).await,
            (1, 0)
        );
    }
    for kid in ["", "bad kid", "\n", &"x".repeat(129)] {
        assert_eq!(
            reject_tokens(signed(&header(kid), &claims(FABRIC)), token(PBI), vec![]).await,
            (1, 0)
        );
    }
    for text in ["opaque", "a.b", "a.b.c.d", "a..b", "é.b.c"] {
        assert_eq!(reject_tokens(text.into(), token(PBI), vec![]).await, (1, 0));
    }
}

#[tokio::test]
async fn jwt_wrong_issuer_audience_tenant_object_and_lifetimes_reject_before_keys() {
    let now = OffsetDateTime::now_utc().unix_timestamp();
    for (name, value) in [
        ("iss", json!("https://evil.example/")),
        ("tid", json!(OTHER)),
        ("aud", json!(PBI)),
        ("oid", json!("opaque")),
        ("exp", json!(now - 1)),
        ("exp", json!(now + 30)),
        ("exp", json!(now + 90000)),
        ("nbf", json!(now + 60)),
        ("iat", json!(now + 60)),
        ("exp", json!(true)),
        ("ver", json!("3.0")),
    ] {
        let mut value_claims = claims(FABRIC);
        value_claims[name] = value;
        assert_eq!(
            reject_tokens(
                signed(&header("offline-key"), &value_claims),
                token(PBI),
                vec![]
            )
            .await,
            (1, 0)
        );
    }
}

#[tokio::test]
async fn jwt_signature_is_checked_and_never_uses_an_injected_decode_only_proof() {
    let original = token(FABRIC);
    let mut parts: Vec<_> = original.split('.').map(str::to_owned).collect();
    let mut changed = claims(FABRIC);
    changed["oid"] = json!(OTHER);
    use base64::Engine as _;
    parts[1] = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&changed).unwrap());
    assert_eq!(
        reject_tokens(
            parts.join("."),
            token(PBI),
            vec![response(jwks("offline-key"))]
        )
        .await,
        (1, 1)
    );
}

#[tokio::test]
async fn separately_signed_pbi_wrong_principal_account_or_tenant_never_bootstraps() {
    for (name, value) in [
        ("oid", json!(OTHER)),
        ("appid", json!(OTHER)),
        ("tid", json!(OTHER)),
        ("aud", json!(FABRIC)),
    ] {
        let mut pbi = claims(PBI);
        pbi[name] = value;
        assert_eq!(
            reject_tokens(
                token(FABRIC),
                signed(&header("offline-key"), &pbi),
                vec![response(jwks("offline-key"))]
            )
            .await,
            (2, 1)
        );
    }
}

#[tokio::test]
async fn unsupported_application_identity_format_is_not_claimed_verified() {
    for name in ["idtyp", "appid"] {
        let mut c = claims(FABRIC);
        c.as_object_mut().unwrap().remove(name);
        assert_eq!(
            reject_tokens(
                signed(&header("offline-key"), &c),
                token(PBI),
                vec![response(jwks("offline-key"))]
            )
            .await,
            (1, 1)
        );
    }
}

#[tokio::test]
async fn jwks_rotation_is_bounded_and_unknown_key_is_not_refetched_without_limit() {
    let credential = Credential::new(token(FABRIC), token(PBI));
    let wire = Wire::new([
        response(jwks("previous-key")),
        response(jwks("offline-key")),
        response(json!({"backendUrl": CLUSTER})),
        response(grant()),
        response(json!({})),
    ]);
    let executor = executor(credential, wire.clone(), Arc::new(Timer::default()));
    assert!(executor.execute(&content(), 4096).await.is_ok());
    assert_eq!(wire.requests.lock().unwrap().len(), 5);

    let credential = Credential::new(token(FABRIC), token(FABRIC));
    let wire = Wire::new([response(jwks("other-key")), response(jwks("other-key"))]);
    let verifier = EntraJwtVerifier::new(
        owner(),
        Arc::new(http(wire.clone(), Arc::new(Timer::default()))),
    )
    .unwrap();
    assert!(verifier.acquire_pbi(credential.as_ref()).await.is_err());
    assert!(verifier.acquire_pbi(credential.as_ref()).await.is_err());
    assert_eq!(wire.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn jwks_issuer_algorithm_usage_key_bounds_and_duplicate_ids_fail_closed() {
    for (name, value) in [
        ("issuer", json!("https://evil.example/")),
        ("kty", json!("EC")),
        ("alg", json!("HS256")),
        ("use", json!("enc")),
        ("e", json!("Aw")),
        ("n", json!("AQAB")),
        ("key_ops", json!(["sign"])),
    ] {
        let mut keys = jwks("offline-key");
        keys["keys"][0][name] = value;
        assert_eq!(
            reject_tokens(token(FABRIC), token(PBI), vec![response(keys)]).await,
            (1, 1)
        );
    }
    let mut duplicate = jwks("offline-key");
    duplicate["keys"]
        .as_array_mut()
        .unwrap()
        .push(jwks("offline-key")["keys"][0].clone());
    for keys in [
        json!({"keys": []}),
        duplicate,
        json!({"keys": vec![jwks("offline-key")["keys"][0].clone(); 33]}),
    ] {
        assert_eq!(
            reject_tokens(token(FABRIC), token(PBI), vec![response(keys)]).await,
            (1, 1)
        );
    }
}

#[tokio::test]
async fn jwks_stream_byte_limit_and_provider_error_are_sanitized_without_bootstrap() {
    for response in [
        Reply::Body(200, vec![b' '; JWKS_LIMIT + 1]),
        Reply::Failure,
        Reply::Body(302, b"private-token-body".to_vec()),
    ] {
        assert_eq!(
            reject_tokens(token(FABRIC), token(PBI), vec![response]).await,
            (1, 1)
        );
    }
}

#[tokio::test]
async fn catalog_returned_origin_never_replaces_selected_cluster() {
    for backend in [
        "https://other-cluster.example.com",
        "http://selected-cluster.analysis.windows.net",
        "https://selected-cluster.analysis.windows.net/path",
        "https://user@selected-cluster.analysis.windows.net",
        "https://127.0.0.1",
    ] {
        assert_eq!(
            reject_tokens(
                token(FABRIC),
                token(PBI),
                vec![
                    response(jwks("offline-key")),
                    response(json!({"backendUrl": backend}))
                ]
            )
            .await,
            (2, 2)
        );
    }
}

#[tokio::test]
async fn required_grant_token_host_capacity_expiry_are_exact_and_bounded() {
    for (name, value) in [
        ("Token", json!("")),
        ("Token", json!("token\ninjection")),
        ("TargetUriHost", json!("https://other-workload.example.com")),
        (
            "TargetUriHost",
            json!("https://selected-notebook.fabric.microsoft.com?leak"),
        ),
        ("CapacityObjectId", json!(OTHER)),
        ("Expiry", json!("1970-01-01T00:00:00Z")),
        ("Expiry", json!("2099-01-01T00:00:00Z")),
        ("Expiry", json!("invalid")),
    ] {
        let mut value_grant = grant();
        value_grant[name] = value;
        assert_eq!(
            reject_tokens(
                token(FABRIC),
                token(PBI),
                vec![
                    response(jwks("offline-key")),
                    response(json!({"backendUrl": CLUSTER})),
                    response(value_grant)
                ]
            )
            .await,
            (2, 3)
        );
    }
    for name in ["Token", "TargetUriHost", "CapacityObjectId", "Expiry"] {
        let mut value = grant();
        value.as_object_mut().unwrap().remove(name);
        assert_eq!(
            reject_tokens(
                token(FABRIC),
                token(PBI),
                vec![
                    response(jwks("offline-key")),
                    response(json!({"backendUrl": CLUSTER})),
                    response(value)
                ]
            )
            .await,
            (2, 3)
        );
    }
}

#[tokio::test]
async fn no_redirect_status_retry_or_exchange_replay_at_any_phase() {
    for phase in 1..=3 {
        for status in [301, 302, 307, 308, 429, 503] {
            let credential = Credential::new(token(FABRIC), token(PBI));
            let mut replies = replies();
            replies.truncate(phase + 1);
            replies[phase] = Reply::Body(status, b"private-token-body".to_vec());
            let wire = Wire::new(replies);
            let executor = executor(credential, wire.clone(), Arc::new(Timer::default()));
            let result = executor.execute(&content(), 4096).await;
            if phase < 3 || status < 400 {
                assert_eq!(result.err(), Some(NativeError::Http(status)));
            } else {
                assert_eq!(result.unwrap().status, status);
            }
            assert_eq!(wire.requests.lock().unwrap().len(), phase + 1);
        }
    }
}

#[tokio::test]
async fn partial_workload_body_fails_without_replay_or_empty_success() {
    let credential = Credential::new(token(FABRIC), token(PBI));
    let mut responses = replies();
    responses[3] = Reply::Partial;
    let wire = Wire::new(responses);
    let executor = executor(credential, wire.clone(), Arc::new(Timer::default()));
    assert_eq!(
        executor.execute(&content(), 4096).await.err(),
        Some(NativeError::Transport)
    );
    assert_eq!(wire.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn response_body_bound_is_enforced_by_shared_stream_engine() {
    let credential = Credential::new(token(FABRIC), token(PBI));
    let wire = Wire::new(replies());
    let executor = executor(credential, wire.clone(), Arc::new(Timer::default()));
    assert_eq!(
        executor.execute(&content(), 1).await.err(),
        Some(NativeError::Limit)
    );
    assert_eq!(wire.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn pre_cancel_and_expired_original_deadline_touch_no_token_or_http() {
    let credential = Credential::new(token(FABRIC), token(PBI));
    let wire = Wire::new([]);
    let executor = executor(credential.clone(), wire.clone(), Arc::new(Timer::default()));
    assert_eq!(
        executor
            .execute_cancelled(&content(), 4096, pin!(ready(())))
            .await
            .err(),
        Some(NativeError::Cancelled)
    );
    let deadline = NativeDeadline::new(Duration::from_nanos(1)).unwrap();
    assert!(
        NativeNotebookExecutor::new(
            RequestContext::new(credential.clone(), owner()),
            http(wire.clone(), Arc::new(Timer::default())),
            selection(owner()),
            deadline
        )
        .is_err()
    );
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn original_deadline_drops_pending_authentication_without_http() {
    let credential = Arc::new(Credential {
        tokens: Mutex::new(VecDeque::new()),
        scopes: Mutex::new(Vec::new()),
        pending: true,
        dropped: AtomicUsize::new(0),
    });
    let wire = Wire::new([]);
    let timer = Arc::new(Timer::default());
    let executor = executor(credential.clone(), wire.clone(), timer.clone());
    let request = content();
    let mut future = Box::pin(executor.execute(&request, 4096));
    assert!(poll(future.as_mut()).is_pending());
    timer.0.store(true, Ordering::SeqCst);
    assert_eq!(
        poll(future.as_mut()).map(|result| result.err()),
        Poll::Ready(Some(NativeError::TimedOut))
    );
    assert_eq!(credential.dropped.load(Ordering::SeqCst), 1);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dropping_operation_cancels_pending_jwks_bootstrap_and_workload_io() {
    for phase in 0..=3 {
        let credential = Credential::new(token(FABRIC), token(PBI));
        let mut responses = replies();
        responses.truncate(phase + 1);
        responses[phase] = Reply::Pending;
        let wire = Wire::new(responses);
        let executor = executor(credential, wire.clone(), Arc::new(Timer::default()));
        let request = content();
        let mut future = Box::pin(executor.execute(&request, 4096));
        assert!(poll(future.as_mut()).is_pending());
        drop(future);
        assert_eq!(wire.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(wire.requests.lock().unwrap().len(), phase + 1);
    }
}

#[test]
fn debug_and_errors_never_format_tokens_selected_ids_origins_or_provider_data() {
    let credential = Credential::new(token(FABRIC), token(PBI));
    let wire = Wire::new([]);
    let executor = executor(credential, wire.clone(), Arc::new(Timer::default()));
    let debug = format!(
        "{executor:?} {:?} {:?} {:?}",
        selection(owner()),
        scope(),
        NotebookOrigin::new(WORKLOAD).unwrap()
    );
    for secret in [
        CLIENT,
        TENANT,
        CAPACITY,
        WORKSPACE,
        NOTEBOOK,
        WORKLOAD,
        CLUSTER,
        MWC,
        "private-token-body",
    ] {
        assert!(!debug.contains(secret));
        assert!(!NativeError::InvalidResponse.to_string().contains(secret));
    }
    assert!(wire.requests.lock().unwrap().is_empty());
}
