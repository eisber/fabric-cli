// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline contracts exercise the actual official Azure credentials.
#![cfg(feature = "headless")]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{Secret, TokenCredential},
    error::{Error, ErrorKind},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
};
use fabric_identity::headless::{
    AssertionFile, HeadlessCredential, HeadlessInput, ManagedHost, ServiceIdentity,
};
use std::{
    collections::VecDeque,
    fmt,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const CLIENT: &str = "22222222-2222-2222-2222-222222222222";
const FABRIC: &str = "https://api.fabric.microsoft.com/.default";
const STORAGE: &str = "https://storage.azure.com/.default";
const ARM: &str = "https://management.azure.com/.default";

#[path = "headless/pipelines.rs"]
mod pipelines;

struct FakeHttp {
    requests: Mutex<Vec<Request>>,
    responses: Mutex<VecDeque<azure_core::Result<AsyncRawResponse>>>,
}

impl fmt::Debug for FakeHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FakeHttp")
    }
}

impl FakeHttp {
    fn new(responses: impl IntoIterator<Item = azure_core::Result<AsyncRawResponse>>) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into_iter().collect()),
        })
    }
}

#[async_trait]
impl HttpClient for FakeHttp {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected network request")
    }
}

fn response(status: u16, body: impl Into<Bytes>) -> azure_core::Result<AsyncRawResponse> {
    Ok(AsyncRawResponse::from_bytes(
        StatusCode::from(status),
        Headers::new(),
        body,
    ))
}

fn token(value: &str, expires_in: u64) -> azure_core::Result<AsyncRawResponse> {
    response(
        200,
        format!(r#"{{"access_token":"{value}","token_type":"Bearer","expires_in":{expires_in}}}"#),
    )
}

fn identity() -> ServiceIdentity {
    ServiceIdentity::new(TENANT, CLIENT).unwrap()
}

fn secret(http: Arc<FakeHttp>) -> Arc<HeadlessCredential> {
    HeadlessCredential::new(
        HeadlessInput::ClientSecret {
            identity: identity(),
            secret: Secret::new("synthetic-secret&=+"),
        },
        http,
    )
    .unwrap()
}

fn body(request: &Request) -> String {
    String::from_utf8(Bytes::from(request.body()).to_vec()).unwrap()
}

#[tokio::test]
async fn official_secret_provider_uses_exact_scopes_public_authority_and_memory_cache() {
    let http = FakeHttp::new([
        token("fabric-token", 3600),
        token("storage-token", 3600),
        token("arm-token", 3600),
    ]);
    let credential = secret(http.clone());
    assert!(
        http.requests.lock().unwrap().is_empty(),
        "construction must be inert"
    );
    for (scope, value) in [
        (FABRIC, "fabric-token"),
        (STORAGE, "storage-token"),
        (ARM, "arm-token"),
    ] {
        let token = credential.get_token(&[scope], None).await.unwrap();
        assert_eq!(token.token.secret(), value);
        assert_eq!(
            credential
                .get_token(&[scope], None)
                .await
                .unwrap()
                .token
                .secret(),
            value
        );
    }
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for (request, resource) in requests.iter().zip([
        "api.fabric.microsoft.com",
        "storage.azure.com",
        "management.azure.com",
    ]) {
        assert_eq!(
            request.url().as_str(),
            format!("https://login.microsoftonline.com/{TENANT}/oauth2/v2.0/token")
        );
        assert_eq!(request.method(), Method::Post);
        let form = body(request);
        assert!(form.contains("client_secret=synthetic-secret%26%3D%2B"));
        assert!(form.contains(&format!("client_id={CLIENT}")));
        assert!(form.contains("grant_type=client_credentials"));
        assert!(form.contains(&format!("scope=https%3A%2F%2F{resource}%2F.default")));
    }
    assert_eq!(format!("{credential:?}"), "HeadlessCredential { .. }");
}

#[tokio::test]
async fn official_cache_renews_near_expiry_and_never_uses_an_expired_result() {
    let http = FakeHttp::new([token("near-expiry", 120), token("renewed", 3600)]);
    let credential = secret(http.clone());
    assert_eq!(
        credential
            .get_token(&[FABRIC], None)
            .await
            .unwrap()
            .token
            .secret(),
        "near-expiry"
    );
    assert_eq!(
        credential
            .get_token(&[FABRIC], None)
            .await
            .unwrap()
            .token
            .secret(),
        "renewed"
    );
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    let credential = secret(FakeHttp::new([token("expired", 0)]));
    assert!(credential.get_token(&[FABRIC], None).await.is_err());
    let credential = secret(FakeHttp::new([
        token("near-expiry", 120),
        response(
            400,
            r#"{"error":"invalid_client","error_description":"synthetic-refresh-failure"}"#,
        ),
    ]));
    credential.get_token(&[FABRIC], None).await.unwrap();
    assert!(
        credential.get_token(&[FABRIC], None).await.is_err(),
        "failed refresh must not return a stale cached token"
    );
}

#[tokio::test]
async fn dropping_an_official_acquisition_releases_its_cache_lock_for_retry() {
    use std::{
        future::Future,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Waker},
    };

    #[derive(Debug, Default)]
    struct CancelWire(AtomicUsize);
    #[async_trait]
    impl HttpClient for CancelWire {
        async fn execute_request(&self, _: &Request) -> azure_core::Result<AsyncRawResponse> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                std::future::pending::<()>().await;
            }
            token("after-cancel", 3600)
        }
    }

    let http = Arc::new(CancelWire::default());
    let credential = HeadlessCredential::new(
        HeadlessInput::ClientSecret {
            identity: identity(),
            secret: Secret::new("synthetic-secret"),
        },
        http.clone(),
    )
    .unwrap();
    let scopes = [FABRIC];
    let mut pending = credential.get_token(&scopes, None);
    assert!(Future::poll(pending.as_mut(), &mut Context::from_waker(Waker::noop())).is_pending());
    drop(pending);
    assert_eq!(
        credential
            .get_token(&scopes, None)
            .await
            .unwrap()
            .token
            .secret(),
        "after-cancel"
    );
    assert_eq!(http.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn scope_rejection_precedes_all_provider_work() {
    let http = FakeHttp::new([]);
    let credential = secret(http.clone());
    for scopes in [
        vec![],
        vec!["https://evil.example/.default"],
        vec![FABRIC, ARM],
        vec!["https://api.fabric.microsoft.com/"],
    ] {
        let error = credential.get_token(&scopes, None).await.unwrap_err();
        assert_eq!(error.to_string(), "unsupported headless credential scope");
    }
    assert!(http.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn oauth_http_and_transport_failures_are_redacted_through_debug_and_sources() {
    let marker = "sensitive-provider-body-token";
    let http = FakeHttp::new([
        response(
            400,
            format!(r#"{{"error":"invalid_client","error_description":"{marker}"}}"#),
        ),
        Err(Error::with_message(ErrorKind::Io, marker)),
        response(200, marker),
    ]);
    let credential = secret(http.clone());
    for _ in 0..3 {
        let error = credential.get_token(&[FABRIC], None).await.unwrap_err();
        assert!(!format!("{error:?} {error}").contains(marker));
        let mut source = std::error::Error::source(&error);
        while let Some(error) = source {
            assert!(!format!("{error:?} {error}").contains(marker));
            source = error.source();
        }
    }
    assert_eq!(
        http.requests.lock().unwrap().len(),
        3,
        "no provider fallback or retry"
    );
}

#[tokio::test]
async fn malformed_and_oversized_provider_metadata_is_bounded_before_official_parsing() {
    let http = FakeHttp::new([
        response(
            200,
            r#"{"access_token":"synthetic","expires_in":9223372036854775807}"#,
        ),
        response(200, r#"{"access_token":"synthetic","expires_in":-1}"#),
        response(200, r#"{"access_token":"","expires_in":3600}"#),
        response(200, vec![b'x'; 256 * 1024 + 1]),
        response(302, "synthetic-redirect-body"),
    ]);
    let credential = secret(http.clone());
    for _ in 0..5 {
        let error = credential.get_token(&[FABRIC], None).await.unwrap_err();
        assert!(!format!("{error:?} {error}").contains("synthetic"));
    }
    assert_eq!(http.requests.lock().unwrap().len(), 5);
}

struct TestFile(PathBuf);

impl TestFile {
    fn new() -> Self {
        let root = std::env::var_os("FAB_IDENTITY_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        #[cfg(unix)]
        let root = root.canonicalize().unwrap();
        assert!(root.is_absolute() && root.is_dir());
        let directory = root.join(format!(
            ".headless-test-{}-{}",
            std::process::id(),
            azure_core::time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        Self(directory.join("assertion"))
    }
}

impl Drop for TestFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

#[tokio::test]
async fn official_assertion_provider_rereads_rotated_file_only_on_token_refresh() {
    let file = TestFile::new();
    let http = FakeHttp::new([
        token("first-access-token", 120),
        token("renewed-access-token", 3600),
    ]);
    let credential = HeadlessCredential::new(
        HeadlessInput::AssertionFile {
            identity: identity(),
            assertion: AssertionFile::new(file.0.clone()).unwrap(),
        },
        http.clone(),
    )
    .unwrap();
    assert!(http.requests.lock().unwrap().is_empty());
    std::fs::write(&file.0, "synthetic.assertion.one\n").unwrap();
    credential.get_token(&[FABRIC], None).await.unwrap();
    std::fs::write(&file.0, "synthetic.assertion.two").unwrap();
    credential.get_token(&[FABRIC], None).await.unwrap();
    std::fs::remove_file(&file.0).unwrap();
    credential.get_token(&[FABRIC], None).await.unwrap();
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(body(&requests[0]).contains("client_assertion=synthetic.assertion.one"));
    assert!(body(&requests[1]).contains("client_assertion=synthetic.assertion.two"));
    for request in &*requests {
        assert!(body(request).contains(
            "client_assertion_type=urn%3Aietf%3Aparams%3Aoauth%3Aclient-assertion-type%3Ajwt-bearer"
        ));
        assert!(!body(request).contains("client_secret="));
    }
}

#[tokio::test]
async fn unsafe_missing_oversized_and_invalid_utf8_assertions_never_reach_http() {
    for path in ["relative-token", "../token", "//server/share/token"] {
        assert!(AssertionFile::new(path.into()).is_err());
    }
    let file = TestFile::new();
    let http = FakeHttp::new([]);
    let credential = HeadlessCredential::new(
        HeadlessInput::AssertionFile {
            identity: identity(),
            assertion: AssertionFile::new(file.0.clone()).unwrap(),
        },
        http.clone(),
    )
    .unwrap();
    assert!(credential.get_token(&[FABRIC], None).await.is_err());
    for bytes in [
        vec![b'a'; 65_537],
        vec![255],
        b"contains whitespace".to_vec(),
        Vec::new(),
    ] {
        std::fs::write(&file.0, bytes).unwrap();
        let error = credential.get_token(&[FABRIC], None).await.unwrap_err();
        assert!(!format!("{error:?}").contains(&file.0.to_string_lossy().to_string()));
    }
    std::fs::remove_file(&file.0).unwrap();
    std::fs::create_dir(&file.0).unwrap();
    assert!(credential.get_token(&[FABRIC], None).await.is_err());
    assert!(http.requests.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn projected_assertions_may_rotate_inside_parent_but_cannot_escape_it() {
    let file = TestFile::new();
    let parent = file.0.parent().unwrap();
    std::fs::create_dir(parent.join("generation")).unwrap();
    std::fs::write(
        parent.join("generation").join("token"),
        "synthetic.projected.assertion",
    )
    .unwrap();
    std::os::unix::fs::symlink("generation/token", &file.0).unwrap();
    let http = FakeHttp::new([token("projected", 120)]);
    let credential = HeadlessCredential::new(
        HeadlessInput::AssertionFile {
            identity: identity(),
            assertion: AssertionFile::new(file.0.clone()).unwrap(),
        },
        http.clone(),
    )
    .unwrap();
    credential.get_token(&[FABRIC], None).await.unwrap();
    std::fs::remove_file(&file.0).unwrap();
    let outside = TestFile::new();
    std::fs::write(&outside.0, "synthetic.outside.assertion").unwrap();
    std::os::unix::fs::symlink(&outside.0, &file.0).unwrap();
    assert!(credential.get_token(&[FABRIC], None).await.is_err());
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn aks_leaf_and_data_links_rotate_beneath_the_pinned_root() {
    let file = TestFile::new();
    let root = file.0.parent().unwrap();
    for (generation, value) in [
        ("generation-one", "synthetic.one"),
        ("generation-two", "synthetic.two"),
    ] {
        std::fs::create_dir(root.join(generation)).unwrap();
        std::fs::write(root.join(generation).join("token"), value).unwrap();
    }
    std::os::unix::fs::symlink("generation-one", root.join("..data")).unwrap();
    std::os::unix::fs::symlink("..data/token", &file.0).unwrap();
    let http = FakeHttp::new([token("first", 120), token("second", 3600)]);
    let credential = HeadlessCredential::new(
        HeadlessInput::AssertionFile {
            identity: identity(),
            assertion: AssertionFile::new(file.0.clone()).unwrap(),
        },
        http.clone(),
    )
    .unwrap();
    credential.get_token(&[FABRIC], None).await.unwrap();
    std::os::unix::fs::symlink("generation-two", root.join("..data-new")).unwrap();
    std::fs::rename(root.join("..data-new"), root.join("..data")).unwrap();
    credential.get_token(&[FABRIC], None).await.unwrap();
    let requests = http.requests.lock().unwrap();
    assert!(body(&requests[0]).contains("client_assertion=synthetic.one"));
    assert!(body(&requests[1]).contains("client_assertion=synthetic.two"));
}

#[cfg(unix)]
#[tokio::test]
async fn redirected_parents_are_rejected_and_retargeting_cannot_switch_the_root() {
    let file = TestFile::new();
    let outside = TestFile::new();
    let storage = TestFile::new();
    std::fs::write(&file.0, "synthetic.inside").unwrap();
    std::fs::write(&outside.0, "synthetic.outside").unwrap();
    let redirected = storage.0.parent().unwrap().join("redirected-parent");
    std::os::unix::fs::symlink(outside.0.parent().unwrap(), &redirected).unwrap();
    assert!(AssertionFile::new(redirected.join("assertion")).is_err());
    let source = AssertionFile::new(file.0.clone()).unwrap();
    let moved = storage.0.parent().unwrap().join("original-root");
    std::fs::rename(file.0.parent().unwrap(), &moved).unwrap();
    std::os::unix::fs::symlink(outside.0.parent().unwrap(), file.0.parent().unwrap()).unwrap();
    let http = FakeHttp::new([token("inside-only", 3600)]);
    let credential = HeadlessCredential::new(
        HeadlessInput::AssertionFile {
            identity: identity(),
            assertion: source,
        },
        http.clone(),
    )
    .unwrap();
    credential.get_token(&[FABRIC], None).await.unwrap();
    let requests = http.requests.lock().unwrap();
    assert!(body(&requests[0]).contains("client_assertion=synthetic.inside"));
    assert!(!body(&requests[0]).contains("synthetic.outside"));
}

#[cfg(unix)]
fn wait_bounded(mut child: std::process::Child) -> std::process::Output {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("FIFO fixture process exceeded its bounded lifetime and was terminated");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[cfg(unix)]
#[test]
fn fifo_assertions_fail_in_an_isolated_process_with_bounded_shutdown() {
    use std::process::Stdio;
    let file = TestFile::new();
    #[cfg(not(target_vendor = "apple"))]
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &file.0,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .unwrap();
    #[cfg(target_vendor = "apple")]
    {
        let output = wait_bounded(
            Command::new("/usr/bin/mkfifo")
                .args(["-m", "600"])
                .arg(&file.0)
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = wait_bounded(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "fifo_assertion_process_child", "--nocapture"])
            .env_clear()
            .env("FAB_HEADLESS_FIFO_TEST", &file.0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("fifo-rejected-without-blocking"));
}

#[cfg(unix)]
#[test]
fn fifo_assertion_process_child() {
    use azure_identity::ClientAssertion;
    use std::time::{Duration, Instant};
    let Some(path) = std::env::var_os("FAB_HEADLESS_FIFO_TEST") else {
        return;
    };
    let source = AssertionFile::new(path.into()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let start = Instant::now();
    let result = runtime.block_on(async {
        tokio::time::timeout(Duration::from_millis(250), source.secret(None)).await
    });
    runtime.shutdown_timeout(Duration::from_millis(250));
    assert!(
        matches!(result, Ok(Err(_))),
        "FIFO must fail before reading, not time out"
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    println!("fifo-rejected-without-blocking");
}

#[cfg(windows)]
#[test]
fn windows_parent_junction_is_refused_before_any_assertion_target_open() {
    let file = TestFile::new();
    let outside = TestFile::new();
    std::fs::write(&outside.0, "synthetic.outside").unwrap();
    let junction = file.0.parent().unwrap().join("redirected-parent");
    let output = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(outside.0.parent().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(AssertionFile::new(junction.join("assertion")).is_err());
    assert_eq!(
        std::fs::read_to_string(&outside.0).unwrap(),
        "synthetic.outside"
    );
}

#[test]
fn managed_identity_official_providers_use_isolated_explicit_host_environments() {
    for host in ["vm", "app-service"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", "managed_identity_process_child", "--nocapture"]);
        child.env_clear().env("FAB_HEADLESS_TEST_HOST", host);
        if host == "app-service" {
            child
                .env("IDENTITY_ENDPOINT", "http://127.0.0.1:49152/msi/token")
                .env("IDENTITY_HEADER", "synthetic-platform-header");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("managed-provider-exercised"));
    }
}

#[tokio::test]
async fn managed_identity_process_child() {
    let Ok(host) = std::env::var("FAB_HEADLESS_TEST_HOST") else {
        return;
    };
    let host = match host.as_str() {
        "vm" => ManagedHost::VirtualMachine,
        "app-service" => ManagedHost::app_service("http://127.0.0.1:49152/msi/token").unwrap(),
        _ => panic!("unknown test host"),
    };
    for client_id in [None, Some(CLIENT.to_owned())] {
        let expires = azure_core::time::OffsetDateTime::now_utc().unix_timestamp() + 3600;
        let http = FakeHttp::new([response(
            200,
            format!(
                r#"{{"access_token":"synthetic-managed-token","expires_on":"{expires}","token_type":"Bearer","resource":"https://api.fabric.microsoft.com"}}"#
            ),
        )]);
        let credential = HeadlessCredential::new(
            HeadlessInput::ManagedIdentity {
                host: host.clone(),
                client_id: client_id.clone(),
            },
            http.clone(),
        )
        .unwrap();
        assert!(http.requests.lock().unwrap().is_empty());
        let token = credential.get_token(&[FABRIC], None).await.unwrap();
        assert_eq!(token.token.secret(), "synthetic-managed-token");
        credential.get_token(&[FABRIC], None).await.unwrap();
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.method(), Method::Get);
        let query = request.url().query_pairs().collect::<Vec<_>>();
        assert!(
            query
                .iter()
                .any(|(name, value)| name == "resource"
                    && value == "https://api.fabric.microsoft.com")
        );
        assert_eq!(
            query
                .iter()
                .find(|(name, _)| name == "client_id")
                .map(|(_, value)| value.as_ref()),
            client_id.as_deref()
        );
        let header = request.headers().get_str(&"x-identity-header".into()).ok();
        if matches!(host, ManagedHost::AppService { .. }) {
            assert_eq!(request.url().host_str(), Some("127.0.0.1"));
            assert_eq!(header, Some("synthetic-platform-header"));
        } else {
            assert_eq!(request.url().host_str(), Some("169.254.169.254"));
            assert!(header.is_none());
        }
    }
    println!("managed-provider-exercised");
}

#[test]
fn app_service_endpoint_contract_refuses_remote_origins_and_secret_header_injection() {
    for endpoint in [
        "http://evil.example/token",
        "https://127.0.0.1/token",
        "http://127.0.0.1/token?secret=value",
        "http://user@127.0.0.1/token",
        "http://127.0.0.1/token#fragment",
    ] {
        assert!(ManagedHost::app_service(endpoint).is_err());
    }
    assert!(ServiceIdentity::new("not-a-tenant", CLIENT).is_err());
    for endpoint in [
        "http://127.0.0.1:49152/msi/token",
        "http://169.254.255.2:8081/msi/token",
        "http://[::1]:49152/msi/token",
    ] {
        assert!(ManagedHost::app_service(endpoint).is_ok());
    }
}
