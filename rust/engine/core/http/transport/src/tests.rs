// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::test_server::{Reply, Server};
use azure_core::{
    Bytes,
    http::{HttpClient, Method, Request, Url},
};
use fabric_http::ExecutionTimer;
use flate2::{Compression, write::GzEncoder};
use futures_util::StreamExt;
use std::{
    io::Write,
    num::NonZeroUsize,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio_rustls::TlsAcceptor;

#[path = "metadata_tests.rs"]
mod metadata_tests;

#[path = "provenance_tests.rs"]
mod provenance_tests;

#[path = "framing_tests.rs"]
mod framing_tests;

const OPERATION: &str = "22222222-2222-2222-2222-222222222222";

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
    NativeHttpClient::for_test(options(roots), server.address).unwrap()
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
        assert!(chunk.len() <= crate::decode::CHUNK_BYTES);
        result.extend_from_slice(&chunk);
        assert!(result.len() <= 5 * 1024 * 1024);
    }
    result
}

#[test]
fn explicit_configuration_rejects_invalid_values_without_disclosing_them() {
    for value in [
        "http://proxy-user:proxy-password@proxy.invalid",
        "https://proxy.invalid",
        "http://proxy.invalid/path",
        "http://proxy.invalid?secret-query",
        "http://proxy.invalid/#fragment",
        "http://proxy.invalid:0",
        "not a URL",
    ] {
        let error = StaticHttpProxy::new(value).unwrap_err();
        assert_eq!(error, TransportError::InvalidProxy);
        assert!(!format!("{error:?} {error}").contains(value));
    }
    let proxy = StaticHttpProxy::new("http://synthetic-proxy.invalid:8080").unwrap();
    assert!(!format!("{proxy:?}").contains("synthetic-proxy"));
    assert_eq!(
        RootCertificate::from_der(b"not-a-certificate").unwrap_err(),
        TransportError::InvalidCertificate
    );
    for duration in [Duration::ZERO, Duration::MAX] {
        let mut configuration = options(Vec::new());
        configuration.connect_timeout = duration;
        assert_eq!(
            configuration.validate(),
            Err(TransportError::InvalidTimeout)
        );
        configuration = options(Vec::new());
        configuration.read_timeout = duration;
        assert_eq!(
            configuration.validate(),
            Err(TransportError::InvalidTimeout)
        );
        configuration = options(Vec::new());
        configuration.write_timeout = duration;
        assert_eq!(
            configuration.validate(),
            Err(TransportError::InvalidTimeout)
        );
        configuration = options(Vec::new());
        configuration.pool_idle_timeout = duration;
        assert_eq!(
            configuration.validate(),
            Err(TransportError::InvalidTimeout)
        );
    }
}

#[tokio::test]
async fn exact_request_status_headers_and_empty_202_are_preserved() {
    let (tls, root) = test_tls();
    let server = Server::start(
        vec![Reply::body(202, Vec::new()).headers(&[
            ("Location", "/v1/operations/synthetic"),
            ("Retry-After", "7"),
            ("x-ms-operation-id", OPERATION),
            ("x-custom-header", "preserved"),
        ])],
        Some(tls),
    )
    .await;
    let client = client(&server, vec![root]);
    let mut request = request(Method::Post, "/v1/workspaces?opaque=a%2B%2F%3D");
    request.set_body(br#"{"exact":true}"#.to_vec());
    let response = client.execute_request(&request).await.unwrap();
    assert_eq!(u16::from(response.status()), 202);
    for (name, value) in [
        ("location", "/v1/operations/synthetic"),
        ("retry-after", "7"),
        ("x-ms-operation-id", OPERATION),
        ("x-custom-header", "preserved"),
        ("content-length", "0"),
    ] {
        assert_eq!(response.headers().get_str(&name.into()).unwrap(), value);
    }
    assert!(bytes(response).await.is_empty());
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let wire = String::from_utf8(requests[0].clone()).unwrap();
    assert!(wire.starts_with("POST /v1/workspaces?opaque=a%2B%2F%3D HTTP/1.1\r\n"));
    assert!(wire.contains("authorization: Bearer expired-synthetic-only\r\n"));
    assert!(wire.contains("content-type: application/json\r\n"));
    assert!(wire.ends_with(r#"{"exact":true}"#));
    assert!(!format!("{client:?}").contains("expired-synthetic-only"));
    server.stop().await;
}

#[tokio::test]
async fn redirects_never_reach_a_second_host_or_leak_authorization() {
    let sink = Server::start(Vec::new(), None).await;
    let destination = format!("http://127.0.0.1:{}/credential-leak", sink.address.port());
    for status in [301, 302, 303, 307, 308] {
        let (tls, root) = test_tls();
        let source = Server::start(
            vec![Reply::body(status, Vec::new()).headers(&[("Location", &destination)])],
            Some(tls),
        )
        .await;
        let response = client(&source, vec![root])
            .execute_request(&request(Method::Post, "/redirect"))
            .await
            .unwrap();
        assert_eq!(u16::from(response.status()), status);
        assert!(bytes(response).await.is_empty());
        assert_eq!(source.requests().len(), 1);
        source.stop().await;
    }
    assert_eq!(sink.state.connections.load(Ordering::SeqCst), 0);
    assert!(sink.requests().is_empty());
    sink.stop().await;
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

#[tokio::test]
async fn untrusted_tls_is_rejected_and_additional_roots_do_not_mutate_global_crypto() {
    let before = rustls::crypto::CryptoProvider::get_default().is_some();
    let (tls, _) = test_tls();
    let server = Server::start(Vec::new(), Some(tls)).await;
    let error = client(&server, Vec::new())
        .execute_request(&request(Method::Get, "/secret-path?private-query"))
        .await
        .unwrap_err();
    let text = format!("{error:?} {error}");
    for secret in [
        "expired-synthetic",
        "private-query",
        "secret-path",
        "127.0.0.1",
        "certificate",
    ] {
        assert!(!text.contains(secret), "{text}");
    }
    assert!(server.requests().is_empty());
    server.stop().await;
    assert_eq!(
        rustls::crypto::CryptoProvider::get_default().is_some(),
        before
    );
}

#[tokio::test]
async fn shared_client_supports_concurrent_requests_and_reuses_its_pool() {
    let (tls, root) = test_tls();
    let server = Server::start(
        (0..4).map(|_| Reply::body(200, b"ok".to_vec())).collect(),
        Some(tls),
    )
    .await;
    let client = client(&server, vec![root]);
    let first = request(Method::Get, "/one");
    let second = request(Method::Get, "/two");
    let (one, two) = tokio::join!(
        client.execute_request(&first),
        client.execute_request(&second)
    );
    assert_eq!(bytes(one.unwrap()).await, b"ok");
    assert_eq!(bytes(two.unwrap()).await, b"ok");
    let connections = server.state.connections.load(Ordering::SeqCst);
    for path in ["/three", "/four"] {
        assert_eq!(
            bytes(
                client
                    .execute_request(&request(Method::Get, path))
                    .await
                    .unwrap()
            )
            .await,
            b"ok"
        );
    }
    assert_eq!(server.requests().len(), 4);
    assert_eq!(server.state.connections.load(Ordering::SeqCst), connections);
    server.stop().await;
}

#[tokio::test]
async fn tokio_timer_uses_async_sleep_and_dropped_waits_are_cancelled() {
    let timer = TokioTimer;
    let start = tokio::time::Instant::now();
    timer.sleep(Duration::from_millis(5)).await;
    assert!(start.elapsed() >= Duration::from_millis(5));
    let mut cancelled = timer.sleep(Duration::from_secs(60));
    assert!(futures_util::poll!(cancelled.as_mut()).is_pending());
    drop(cancelled);
    tokio::task::yield_now().await;
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn no_proxy_policy_ignores_ambient_proxy_environment_in_an_isolated_process() {
    const CHILD: &str = "FABRIC_TRANSPORT_PROXY_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (tls, root) = test_tls();
                let server =
                    Server::start(vec![Reply::body(200, b"direct".to_vec())], Some(tls)).await;
                let client = NativeHttpClient::new(options(vec![root])).unwrap();
                let direct = Request::new(
                    Url::parse(&format!("https://{}/direct", server.address)).unwrap(),
                    Method::Get,
                );
                assert_eq!(
                    bytes(client.execute_request(&direct).await.unwrap()).await,
                    b"direct"
                );
                assert_eq!(server.requests().len(), 1);
                server.stop().await;
            });
        return;
    }
    let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    proxy.set_nonblocking(true).unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "tests::no_proxy_policy_ignores_ambient_proxy_environment_in_an_isolated_process",
        ])
        .env(CHILD, "1")
        .env("NO_PROXY", "")
        .env("no_proxy", "");
    for name in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        command.env(name, &proxy_url);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        proxy.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[tokio::test]
async fn socket_connect_and_read_idle_timeouts_are_finite_and_release_work() {
    // A TCP peer that never starts TLS must be bounded by connect_timeout.
    let server = Server::start(Vec::new(), None).await;
    let mut configuration = options(Vec::new());
    configuration.connect_timeout = Duration::from_millis(50);
    let client = NativeHttpClient::for_test(configuration, server.address).unwrap();
    let start = tokio::time::Instant::now();
    assert!(
        client
            .execute_request(&request(Method::Get, "/tls-stall"))
            .await
            .is_err()
    );
    assert!(start.elapsed() >= Duration::from_millis(50));
    assert!(start.elapsed() < Duration::from_secs(1));
    server.wait_closed().await;
    server.stop().await;

    let (tls, root) = test_tls();
    let server = Server::start(vec![Reply::Stall], Some(tls)).await;
    let mut configuration = options(vec![root]);
    configuration.read_timeout = Duration::from_millis(50);
    let client = NativeHttpClient::for_test(configuration, server.address).unwrap();
    let response = client
        .execute_request(&request(Method::Get, "/read-stall"))
        .await
        .unwrap();
    let start = tokio::time::Instant::now();
    assert!(response.into_body().next().await.unwrap().is_err());
    assert!(start.elapsed() >= Duration::from_millis(40));
    assert!(start.elapsed() < Duration::from_secs(1));
    server.wait_closed().await;
    server.stop().await;
}

#[tokio::test]
async fn direct_client_returns_gateway_status_and_io_failure_without_retrying() {
    for (reply, status) in [
        (Reply::body(503, Vec::new()), Some(503)),
        (Reply::Close, None),
    ] {
        let (tls, root) = test_tls();
        let server = Server::start(vec![reply], Some(tls)).await;
        let result = client(&server, vec![root])
            .execute_request(&request(Method::Get, "/once"))
            .await;
        if let Some(status) = status {
            assert_eq!(u16::from(result.unwrap().status()), status);
        } else {
            assert!(result.is_err());
        }
        assert_eq!(server.requests().len(), 1);
        server.stop().await;
    }
}

#[tokio::test]
async fn decoders_are_pull_driven_and_dropping_them_releases_the_source() {
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    struct Guard(Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let polls = Arc::new(AtomicUsize::new(0));
    let guard = Guard(dropped.clone());
    let count = polls.clone();
    let compressed = Bytes::from(gzip(&vec![b'x'; 4 * 1024 * 1024]));
    let source = futures_util::stream::repeat_with(move || {
        let _keep_alive = &guard;
        count.fetch_add(1, Ordering::SeqCst);
        Ok(compressed.clone())
    });
    let mut headers = azure_core::http::headers::Headers::new();
    headers.insert("content-encoding", "gzip");
    let mut stream = crate::decode::decoded(Box::pin(source), &headers, true).unwrap();
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert_eq!(
        stream.next().await.unwrap().unwrap().len(),
        crate::decode::CHUNK_BYTES
    );
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert!(!dropped.load(Ordering::SeqCst));
    drop(stream);
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn rfc_deflate_decodes_and_malformed_or_unsupported_encodings_fail_safely() {
    let body = b"decoded deflate bytes";
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(body).unwrap();
    let encoded = encoder.finish().unwrap();
    let (tls, root) = test_tls();
    let server = Server::start(
        vec![Reply::body(200, encoded).headers(&[("Content-Encoding", "deflate")])],
        Some(tls),
    )
    .await;
    assert_eq!(
        bytes(
            client(&server, vec![root])
                .execute_request(&request(Method::Get, "/deflate"))
                .await
                .unwrap()
        )
        .await,
        body
    );
    server.stop().await;
    for encoding in ["gzip", "deflate", "br", "gzip, deflate"] {
        let (tls, root) = test_tls();
        let server = Server::start(
            vec![
                Reply::body(200, b"synthetic-invalid-compressed-body".to_vec())
                    .headers(&[("Content-Encoding", encoding)]),
            ],
            Some(tls),
        )
        .await;
        let result = client(&server, vec![root])
            .execute_request(&request(Method::Get, "/encoded?secret-query"))
            .await;
        let error = match result {
            Err(error) => error,
            Ok(response) => response.into_body().next().await.unwrap().unwrap_err(),
        };
        let text = format!("{error:?} {error}");
        assert!(!text.contains("synthetic-invalid"));
        assert!(!text.contains("secret-query"));
        assert!(!text.contains("expired-synthetic"));
        server.stop().await;
    }
}

#[tokio::test]
async fn repeated_and_latin1_headers_are_preserved_and_trailers_fail_explicitly() {
    for (raw, expected) in [
        (
            b"HTTP/1.1 200 Synthetic\r\nx-test: one\r\nx-test: two\r\nContent-Length: 0\r\n\r\n"
                .as_slice(),
            "one, two",
        ),
        (
            b"HTTP/1.1 200 Synthetic\r\nx-test: \xff\r\nContent-Length: 0\r\n\r\n",
            "\u{ff}",
        ),
    ] {
        let (tls, root) = test_tls();
        let server = Server::start(vec![Reply::Raw(raw.to_vec())], Some(tls)).await;
        let response = client(&server, vec![root])
            .execute_request(&request(Method::Get, "/headers"))
            .await
            .unwrap();
        assert_eq!(
            response.headers().get_str(&"x-test".into()).unwrap(),
            expected
        );
        assert!(bytes(response).await.is_empty());
        assert_eq!(server.requests().len(), 1);
        server.stop().await;
    }
    let (tls, root) = test_tls();
    let server = Server::start(
        vec![Reply::Raw(
            b"HTTP/1.1 200 Synthetic\r\nTransfer-Encoding: chunked\r\nTrailer: x-test\r\n\r\n0\r\nx-test: late\r\n\r\n".to_vec()
        )],
        Some(tls),
    ).await;
    let response = client(&server, vec![root])
        .execute_request(&request(Method::Get, "/trailers"))
        .await
        .unwrap();
    assert!(response.into_body().next().await.unwrap().is_err());
    server.stop().await;
}

#[tokio::test]
async fn trusted_root_does_not_bypass_hostname_or_public_https_validation() {
    let (tls, root) = test_tls();
    let server = Server::start(Vec::new(), Some(tls)).await;
    let client = client(&server, vec![root]);
    let wrong_host = Request::new(
        Url::parse("https://wrong-host.invalid/").unwrap(),
        Method::Get,
    );
    assert!(client.execute_request(&wrong_host).await.is_err());
    for url in [
        "http://api.fabric.microsoft.com/",
        "https://user:synthetic-password@api.fabric.microsoft.com/",
        "https://api.fabric.microsoft.com/#fragment",
    ] {
        let request = Request::new(Url::parse(url).unwrap(), Method::Get);
        let error = client.execute_request(&request).await.unwrap_err();
        assert!(!format!("{error:?} {error}").contains("synthetic-password"));
    }
    assert!(server.requests().is_empty());
    server.stop().await;
}

#[test]
fn construction_outside_a_runtime_performs_no_global_initialization() {
    let provider = rustls::crypto::CryptoProvider::get_default().is_some();
    let client = NativeHttpClient::new(options(Vec::new())).unwrap();
    assert_eq!(format!("{client:?}"), "NativeHttpClient(<pooled>)");
    assert_eq!(
        rustls::crypto::CryptoProvider::get_default().is_some(),
        provider
    );
    assert!(tokio::runtime::Handle::try_current().is_err());
    let mut configuration = options(Vec::new());
    configuration.max_idle_per_host = NonZeroUsize::new(1025).unwrap();
    assert_eq!(
        NativeHttpClient::new(configuration).unwrap_err(),
        TransportError::InvalidConfiguration
    );
}
