// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real service adapters over the public native transport, using loopback CONNECT.
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{HttpClient, Method, Request, Url},
    time::OffsetDateTime,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, RequestContext,
    notebooks::{DefinitionPart, NotebookDefinition, NotebookLimits},
    runtime::{
        NativeDeadline, NativeEffect, NativeEndpoint, NativeError, NativeMethod,
        NativeStreamExecutor, NativeStreamLimits,
    },
    workspaces::{WorkspaceExecutionError, WorkspaceListErrorKind, WorkspaceListLimits},
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor, native_stream::NativeFabricStreamExecutor};
use fabric_sdk_transport::*;
use flate2::{Compression, write::GzEncoder};
use futures_util::{StreamExt, future::pending};
use std::{
    io::Write,
    num::NonZeroUsize,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio_rustls::TlsAcceptor;

#[path = "../../../core/http/transport/src/test_server.rs"]
mod test_server;
use test_server::Reply;

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "33333333-3333-3333-3333-333333333333";
const OPERATION: &str = "22222222-2222-2222-2222-222222222222";

struct Server {
    inner: test_server::Server,
    hide_connect: bool,
}

impl std::ops::Deref for Server {
    type Target = test_server::Server;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Server {
    async fn start(replies: Vec<Reply>, tls: Option<TlsAcceptor>) -> Self {
        Self {
            inner: test_server::Server::proxy(replies, tls.expect("TLS fixture")).await,
            hide_connect: true,
        }
    }

    async fn proxy(replies: Vec<Reply>, tls: TlsAcceptor) -> Self {
        Self {
            inner: test_server::Server::proxy(replies, tls).await,
            hide_connect: false,
        }
    }

    fn requests(&self) -> Vec<Vec<u8>> {
        self.inner
            .requests()
            .into_iter()
            .filter(|request| !self.hide_connect || !request.starts_with(b"CONNECT "))
            .collect()
    }

    async fn stop(self) {
        self.inner.stop().await;
    }
}

fn options(roots: Vec<RootCertificate>) -> TransportOptions {
    TransportOptions {
        connect_timeout: Duration::from_secs(2),
        read_timeout: Duration::from_secs(2),
        write_timeout: Duration::from_secs(2),
        pool_idle_timeout: Duration::from_secs(2),
        max_idle_per_host: NonZeroUsize::new(4).unwrap(),
        trust: TrustPolicy::Platform {
            additional_roots: roots,
        },
        proxy: ProxyPolicy::None,
    }
}

fn test_tls() -> (TlsAcceptor, RootCertificate) {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Offline Transport Test Root");
    ca_params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let ca = ca_params.self_signed(&key).unwrap();
    let issuer = rcgen::Issuer::from_params(&ca_params, &key);
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let mut leaf_params = rcgen::CertificateParams::new(vec![
        "api.fabric.microsoft.com".to_owned(),
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
    ])
    .unwrap();
    leaf_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Offline Transport Test Leaf");
    leaf_params.use_authority_key_identifier_extension = true;
    leaf_params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
    leaf_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone(), ca.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(leaf_key.serialize_der()).into(),
        )
        .unwrap();
    (
        TlsAcceptor::from(Arc::new(server)),
        RootCertificate::from_der(ca.der()).unwrap(),
    )
}

fn client(server: &Server, roots: Vec<RootCertificate>) -> NativeHttpClient {
    let mut configuration = options(roots);
    configuration.proxy = ProxyPolicy::StaticHttp(
        StaticHttpProxy::new(&format!("http://{}", server.address)).unwrap(),
    );
    NativeHttpClient::new(configuration).unwrap()
}

fn request(method: Method, path: &str) -> Request {
    let mut request = Request::new(
        Url::parse(&format!("https://api.fabric.microsoft.com{path}")).unwrap(),
        method,
    );
    request.insert_header("authorization", "Bearer expired-synthetic-only");
    request.insert_header("content-type", "application/json");
    request
}

async fn bytes(response: azure_core::http::AsyncRawResponse) -> Vec<u8> {
    let mut stream = response.into_body();
    let mut result = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        assert!(chunk.len() <= 8 * 1024);
        result.extend_from_slice(&chunk);
        assert!(result.len() <= 5 * 1024 * 1024);
    }
    result
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

#[derive(Debug)]
struct SyntheticCredential;

#[async_trait]
impl TokenCredential for SyntheticCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        Ok(AccessToken::new(
            "expired-synthetic-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

fn sdk() -> Client {
    Client::new(
        Arc::new(SyntheticCredential),
        CredentialSelection::new("offline", "synthetic", fabric_core::Audience::Fabric).unwrap(),
    )
}

fn executor(client: NativeHttpClient, timeout: Duration) -> HttpExecutor {
    HttpExecutor::new(
        Arc::new(client),
        Arc::new(TokioTimer),
        ExecutionOptions {
            timeout,
            max_delay: Duration::from_secs(5),
        },
    )
    .unwrap()
}

#[tokio::test]
async fn existing_executor_is_the_only_retry_policy_and_never_replays_posts() {
    let (tls, root) = test_tls();
    let server = Server::start(
        vec![
            Reply::body(503, Vec::new()),
            Reply::body(200, br#"{"value":[]}"#.to_vec()),
        ],
        Some(tls),
    )
    .await;
    let exec = executor(client(&server, vec![root]), Duration::from_secs(3));
    assert_eq!(
        sdk()
            .list_workspaces(&exec, WorkspaceListLimits::default(), pending())
            .await
            .unwrap()
            .pages_received,
        1
    );
    assert_eq!(server.requests().len(), 2);
    server.stop().await;

    for reply in [
        Reply::body(429, Vec::new()),
        Reply::body(503, Vec::new()),
        Reply::Close,
    ] {
        let (tls, root) = test_tls();
        let server = Server::start(vec![reply], Some(tls)).await;
        let exec = executor(client(&server, vec![root]), Duration::from_secs(3));
        let definition = NotebookDefinition {
            format: None,
            parts: vec![DefinitionPart::inline("notebook-content.py", b"pass").unwrap()],
        };
        assert!(
            sdk()
                .notebook_session(&exec, NotebookLimits::default(), pending())
                .create(WORKSPACE, "Synthetic", "", &definition)
                .await
                .is_err()
        );
        assert_eq!(server.requests().len(), 1);
        server.stop().await;
    }
}

#[tokio::test]
async fn real_sdk_workspace_pages_and_notebook_202_polling_use_native_tls() {
    let (tls, root) = test_tls();
    let operation = format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}");
    let result = format!("{operation}/result");
    let server = Server::start(
        vec![
            Reply::body(200, br#"{"value":[],"continuationToken":"a+/="}"#.to_vec()),
            Reply::body(200, br#"{"value":[]}"#.to_vec()),
            Reply::body(202, Vec::new()).headers(&[
                ("Location", &operation),
                ("Retry-After", "0"),
                ("x-ms-operation-id", OPERATION),
            ]),
            Reply::body(200, br#"{"status":"Running"}"#.to_vec()).headers(&[("Retry-After", "0")]),
            Reply::body(200, br#"{"status":"Succeeded"}"#.to_vec())
                .headers(&[("Location", &result)]),
            Reply::body(
                200,
                format!(r#"{{"id":"{ITEM}","displayName":"Synthetic","type":"Notebook"}}"#),
            ),
        ],
        Some(tls),
    )
    .await;
    let exec = executor(client(&server, vec![root]), Duration::from_secs(3));
    assert_eq!(
        sdk()
            .list_workspaces(&exec, WorkspaceListLimits::default(), pending())
            .await
            .unwrap()
            .pages_received,
        2
    );
    let definition = NotebookDefinition {
        format: None,
        parts: vec![DefinitionPart::inline("notebook-content.py", b"pass").unwrap()],
    };
    assert_eq!(
        sdk()
            .notebook_session(&exec, NotebookLimits::default(), pending())
            .create(WORKSPACE, "Synthetic", "", &definition)
            .await
            .unwrap()
            .id,
        ITEM
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 6);
    assert!(
        requests[1].starts_with(b"GET /v1/workspaces?continuationToken=a%2B%2F%3D HTTP/1.1\r\n")
    );
    assert!(requests[2].starts_with(b"POST "));
    assert!(requests[5].starts_with(format!("GET /v1/operations/{OPERATION}/result ").as_bytes()));
    for request in requests {
        let wire = String::from_utf8(request).unwrap();
        assert!(wire.contains("host: api.fabric.microsoft.com\r\n"));
        assert!(wire.contains("authorization: Bearer expired-synthetic-only\r\n"));
    }
    assert_eq!(server.state.connections.load(Ordering::SeqCst), 1);
    server.stop().await;
}

#[tokio::test]
async fn decoded_chunks_preserve_compression_headers_and_exact_or_plus_one_limit() {
    const LIMIT: usize = 4 * 1024 * 1024;
    for (length, success) in [(LIMIT, true), (LIMIT + 1, false)] {
        let prefix = br#"{"value":[],"padding":""#;
        let mut body = prefix.to_vec();
        body.resize(length - 2, b'x');
        body.extend_from_slice(br#""}"#);
        let encoded = gzip(&body);
        assert!(
            encoded.len() < 8192,
            "fixture must exercise a compression bomb"
        );
        let (tls, root) = test_tls();
        let server = Server::start(
            vec![Reply::body(200, encoded).headers(&[("Content-Encoding", "gzip")])],
            Some(tls),
        )
        .await;
        let exec = executor(client(&server, vec![root]), Duration::from_secs(3));
        let result = sdk()
            .list_workspaces(&exec, WorkspaceListLimits::default(), pending())
            .await;
        if success {
            assert_eq!(result.unwrap().pages_received, 1);
        } else {
            assert_eq!(
                result.unwrap_err().kind,
                WorkspaceListErrorKind::ResponseLimit
            );
        }
        server.stop().await;
    }

    let body = vec![b'x'; 64 * 1024];
    let encoded = gzip(&body);
    let length = encoded.len().to_string();
    let (tls, root) = test_tls();
    let server = Server::start(
        vec![Reply::body(200, encoded).headers(&[("Content-Encoding", "gzip")])],
        Some(tls),
    )
    .await;
    let response = client(&server, vec![root])
        .execute_request(&request(Method::Get, "/gzip"))
        .await
        .unwrap();
    assert_eq!(
        response
            .headers()
            .get_str(&"content-encoding".into())
            .unwrap(),
        "gzip"
    );
    assert_eq!(
        response
            .headers()
            .get_str(&"content-length".into())
            .unwrap(),
        length
    );
    assert_eq!(bytes(response).await, body);
    server.stop().await;
}

#[tokio::test]
async fn streaming_deadline_cancellation_and_body_drop_release_connections() {
    for (reply, timeout, expected) in [
        (
            Reply::Stall,
            Duration::from_millis(100),
            WorkspaceListErrorKind::Execution(WorkspaceExecutionError::TimedOut),
        ),
        (
            Reply::Endless,
            Duration::from_secs(3),
            WorkspaceListErrorKind::ResponseLimit,
        ),
    ] {
        let (tls, root) = test_tls();
        let server = Server::start(vec![reply], Some(tls)).await;
        let exec = executor(client(&server, vec![root]), timeout);
        let result = sdk()
            .list_workspaces(&exec, WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(result.kind, expected);
        server.wait_closed().await;
        server.stop().await;
    }

    let (tls, root) = test_tls();
    let server = Server::start(vec![Reply::Stall], Some(tls)).await;
    let exec = executor(client(&server, vec![root]), Duration::from_secs(3));
    let result = sdk()
        .list_workspaces(
            &exec,
            WorkspaceListLimits::default(),
            tokio::time::sleep(Duration::from_millis(100)),
        )
        .await
        .unwrap_err();
    assert_eq!(result.kind, WorkspaceListErrorKind::Cancelled);
    server.wait_closed().await;
    server.stop().await;

    let (tls, root) = test_tls();
    let server = Server::start(vec![Reply::Stall], Some(tls)).await;
    let response = client(&server, vec![root])
        .execute_request(&request(Method::Get, "/drop"))
        .await
        .unwrap();
    drop(response);
    server.wait_closed().await;
    server.stop().await;
}

#[tokio::test]
async fn public_sdk_transport_empty_post_preserves_zero_length_over_connect_without_replay() {
    for explicit_length in [false, true] {
        let (tls, root) = test_tls();
        let server = Server::proxy(vec![Reply::RequireContentLength], tls).await;
        let client = client(&server, vec![root]);
        let mut pending = request(Method::Post, "/v1/sdk-empty-post");
        pending.set_body(Vec::new());
        if explicit_length {
            pending.insert_header("content-length", "0");
        }
        let response = client.execute_request(&pending).await.unwrap();
        assert_eq!(u16::from(response.status()), 202);
        assert!(bytes(response).await.is_empty());
        let requests = server.requests();
        assert_eq!(requests.len(), 2, "one CONNECT and one POST, without replay");
        let connect = std::str::from_utf8(&requests[0])
            .unwrap()
            .to_ascii_lowercase();
        assert!(connect.starts_with("connect api.fabric.microsoft.com:443 http/1.1\r\n"));
        assert!(!connect.contains("authorization"));
        let origin = std::str::from_utf8(&requests[1]).unwrap();
        let (head, body) = origin.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("POST /v1/sdk-empty-post HTTP/1.1\r\n"));
        let head = head.to_ascii_lowercase();
        let lengths = head
            .lines()
            .filter(|line| line.starts_with("content-length:"))
            .collect::<Vec<_>>();
        assert_eq!(lengths, ["content-length: 0"]);
        assert!(!head.contains("\r\ntransfer-encoding:"));
        assert!(body.is_empty());
        assert_eq!(server.state.connections.load(Ordering::SeqCst), 1);
        drop(client);
        server.wait_closed().await;
        assert_eq!(server.state.closed.load(Ordering::SeqCst), 1);
        server.stop().await;
    }
}

#[tokio::test]
async fn public_sdk_transport_rejected_connect_releases_socket_without_origin_bearer_or_request() {
    const TOKEN: &str = "sdk-proxy-denial-synthetic-only";
    let server = test_server::Server::start(vec![Reply::body(407, Vec::new())], None).await;
    let mut configuration = options(Vec::new());
    configuration.proxy = ProxyPolicy::StaticHttp(
        StaticHttpProxy::new(&format!("http://{}", server.address)).unwrap(),
    );
    let client = NativeHttpClient::new(configuration).unwrap();
    let mut pending = request(Method::Post, "/v1/sdk-proxy-denied");
    pending.insert_header("authorization", format!("Bearer {TOKEN}"));
    pending.set_body(b"origin-only".to_vec());
    let result = tokio::time::timeout(Duration::from_secs(3), client.execute_request(&pending))
        .await
        .expect("proxy rejection must finish within its deadline");
    assert!(result.is_err(), "a denied CONNECT must not become a response");
    server.wait_closed().await;
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "only CONNECT, no origin request or replay");
    let connect = std::str::from_utf8(&requests[0])
        .unwrap()
        .to_ascii_lowercase();
    assert!(connect.starts_with("connect api.fabric.microsoft.com:443 http/1.1\r\n"));
    assert!(!connect.contains("authorization"));
    assert!(!connect.contains(TOKEN));
    assert!(!connect.contains("/v1/sdk-proxy-denied"));
    assert!(!connect.contains("origin-only"));
    assert_eq!(server.state.connections.load(Ordering::SeqCst), 1);
    assert_eq!(server.state.closed.load(Ordering::SeqCst), 1);
    drop(client);
    server.stop().await;
}

#[tokio::test]
async fn configured_static_http_proxy_tunnels_tls_without_forwarding_bearer_to_connect() {
    let (tls, root) = test_tls();
    let server = Server::proxy(vec![Reply::body(200, br#"{"value":[]}"#.to_vec())], tls).await;
    let mut configuration = options(vec![root]);
    configuration.proxy = ProxyPolicy::StaticHttp(
        StaticHttpProxy::new(&format!("http://{}", server.address)).unwrap(),
    );
    // Public construction: only the explicitly selected proxy routes this to loopback.
    let exec = executor(
        NativeHttpClient::new(configuration).unwrap(),
        Duration::from_secs(3),
    );
    assert_eq!(
        sdk()
            .list_workspaces(&exec, WorkspaceListLimits::default(), pending())
            .await
            .unwrap()
            .pages_received,
        1
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let connect = String::from_utf8(requests[0].clone())
        .unwrap()
        .to_lowercase();
    assert!(connect.starts_with("connect api.fabric.microsoft.com:443 http/1.1\r\n"));
    assert!(!connect.contains("authorization"));
    assert!(!connect.contains("expired-synthetic"));
    let origin = String::from_utf8(requests[1].clone()).unwrap();
    assert!(origin.contains("authorization: Bearer expired-synthetic-only\r\n"));
    server.stop().await;
}

#[tokio::test]
async fn actual_native_compression_failure_is_not_recoverable_stream_transport() {
    let (tls, root) = test_tls();
    let server = Server::start(
        vec![Reply::body(200, b"not-gzip".to_vec()).headers(&[("Content-Encoding", "gzip")])],
        Some(tls),
    )
    .await;
    let http = executor(client(&server, vec![root]), Duration::from_secs(3));
    let context = RequestContext::new(
        Arc::new(SyntheticCredential),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    );
    let executor = NativeFabricStreamExecutor::new(context, http).unwrap();
    let request = NativeEndpoint::new(
        azure_core::http::Url::parse("https://api.fabric.microsoft.com/v1/livyApi/versions/v")
            .unwrap(),
    )
    .unwrap()
    .request(
        NativeMethod::Get,
        &["sessions"],
        &[],
        None,
        NativeEffect::Read,
    )
    .unwrap();
    let deadline = NativeDeadline::new(Duration::from_secs(3)).unwrap();
    let mut response = executor
        .open(
            &request,
            NativeStreamLimits {
                max_fragment_bytes: 64,
                max_body_bytes: 1024,
            },
            &deadline,
        )
        .await
        .unwrap();
    assert_eq!(
        response.body.read(64, &deadline).await,
        Err(NativeError::InvalidResponse)
    );
    assert_eq!(server.requests().len(), 1);
    server.stop().await;
}

#[tokio::test]
async fn actual_native_mid_body_disconnect_is_transport_not_protocol_or_mutation_replay() {
    for method in [NativeMethod::Get, NativeMethod::Post] {
        let (tls, root) = test_tls();
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nab".to_vec();
        let server = Server::start(vec![Reply::Raw(raw)], Some(tls)).await;
        let http = executor(client(&server, vec![root]), Duration::from_secs(3));
        let context = RequestContext::new(
            Arc::new(SyntheticCredential),
            CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        );
        let executor = NativeFabricStreamExecutor::new(context, http).unwrap();
        let request = NativeEndpoint::new(
            azure_core::http::Url::parse("https://api.fabric.microsoft.com/v1/livyApi/versions/v")
                .unwrap(),
        )
        .unwrap()
        .request(
            method,
            &["sessions"],
            &[],
            None,
            if method == NativeMethod::Post {
                NativeEffect::Execute
            } else {
                NativeEffect::Read
            },
        )
        .unwrap();
        let deadline = NativeDeadline::new(Duration::from_secs(3)).unwrap();
        let mut response = executor
            .open(
                &request,
                NativeStreamLimits {
                    max_fragment_bytes: 64,
                    max_body_bytes: 1024,
                },
                &deadline,
            )
            .await
            .unwrap();
        assert_eq!(
            response.body.read(64, &deadline).await.unwrap(),
            Some(b"ab".to_vec())
        );
        assert_eq!(server.requests().len(), 1);
        server.stop().await;
        assert_eq!(
            response.body.read(64, &deadline).await,
            Err(if method == NativeMethod::Post {
                NativeError::OutcomeUnknown
            } else {
                NativeError::Transport
            })
        );
    }
}
