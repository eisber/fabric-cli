// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Assertion-construction unit fakes, not cryptographic or native-store evidence.
#![deny(unnameable_test_items)]

use super::*;
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const CLIENT: &str = "22222222-2222-2222-2222-222222222222";
const NOW: i64 = 1_800_000_000;

struct Clock(Mutex<VecDeque<i64>>);

impl AssertionClock for Clock {
    fn now(&self) -> i64 {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected clock read")
    }
}

struct Signer {
    inputs: Mutex<Vec<Vec<u8>>>,
    nonces: AtomicUsize,
    changed: AtomicBool,
    change_on_sign: bool,
    signature_length: usize,
    error: Option<HeadlessError>,
}

impl Signer {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inputs: Mutex::new(Vec::new()),
            nonces: AtomicUsize::new(0),
            changed: AtomicBool::new(false),
            change_on_sign: false,
            signature_length: 256,
            error: None,
        })
    }
}

impl fmt::Debug for Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("synthetic-sensitive-signer-debug")
    }
}

#[async_trait]
impl CertificateSigner for Signer {
    fn certificate_sha256(&self) -> [u8; 32] {
        if self.changed.load(Ordering::Acquire) {
            [0xff; 32]
        } else {
            [0xfb; 32]
        }
    }

    fn assertion_nonce(&self) -> Result<[u8; 16], HeadlessError> {
        Ok((self.nonces.fetch_add(1, Ordering::AcqRel) as u128 + 1).to_be_bytes())
    }

    async fn sign_ps256(&self, signing_input: &[u8]) -> Result<Vec<u8>, HeadlessError> {
        self.inputs.lock().unwrap().push(signing_input.to_vec());
        if self.change_on_sign {
            self.changed.store(true, Ordering::Release);
        }
        if let Some(error) = self.error {
            return Err(error);
        }
        // Deliberately not a valid signature: these tests prove serialization only.
        Ok(vec![0x5a; self.signature_length])
    }
}

fn make_assertion(signer: Arc<Signer>, times: &[i64]) -> CertificateAssertion {
    CertificateAssertion {
        identity: ServiceIdentity::new(TENANT, CLIENT).unwrap(),
        config: CertificateConfig::new(signer),
        clock: Arc::new(Clock(Mutex::new(times.iter().copied().collect()))),
    }
}

fn decoded(part: &str) -> serde_json::Value {
    serde_json::from_slice(&base64::decode_url_safe(part).unwrap()).unwrap()
}

#[tokio::test]
async fn unit_fake_assertion_has_exact_ps256_der_fingerprint_claims_and_lifetime() {
    let signer = Signer::new();
    let assertion = make_assertion(Arc::clone(&signer), &[NOW, NOW + 1]);
    let value = assertion.assertion().await.unwrap();
    let parts: Vec<_> = value.split('.').collect();
    assert_eq!(parts.len(), 3);
    for part in &parts {
        assert!(!part.contains(['+', '/', '=']));
    }
    let header = decoded(parts[0]);
    assert_eq!(header.as_object().unwrap().len(), 3);
    assert_eq!(header["alg"], "PS256");
    assert_eq!(header["typ"], "JWT");
    assert!(header.get("x5t").is_none());
    assert!(header.get("x5c").is_none());
    assert_eq!(
        base64::decode_url_safe(header["x5t#S256"].as_str().unwrap()).unwrap(),
        [0xfb; 32]
    );
    let payload = decoded(parts[1]);
    assert_eq!(payload.as_object().unwrap().len(), 7);
    assert_eq!(
        payload["aud"],
        format!("https://login.microsoftonline.com/{TENANT}/oauth2/v2.0/token")
    );
    assert_eq!(payload["iss"], CLIENT);
    assert_eq!(payload["sub"], CLIENT);
    assert_eq!(payload["iat"], NOW);
    assert_eq!(payload["nbf"], NOW);
    assert_eq!(payload["exp"], NOW + 300);
    assert_eq!(
        base64::decode_url_safe(payload["jti"].as_str().unwrap()).unwrap(),
        1_u128.to_be_bytes()
    );
    assert_eq!(
        signer.inputs.lock().unwrap().as_slice(),
        [format!("{}.{}", parts[0], parts[1]).into_bytes()]
    );
    assert_eq!(signer.nonces.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn every_assertion_construction_requests_a_new_nonce() {
    let signer = Signer::new();
    let assertion = make_assertion(Arc::clone(&signer), &[NOW, NOW, NOW, NOW]);
    let first = assertion.assertion().await.unwrap();
    let second = assertion.assertion().await.unwrap();
    let first = decoded(first.split('.').nth(1).unwrap());
    let second = decoded(second.split('.').nth(1).unwrap());
    assert_ne!(first["jti"], second["jti"]);
    assert_eq!(signer.inputs.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn invalid_clock_precedes_nonce_and_signing_and_expired_or_rewound_work_is_rejected() {
    for now in [-1, i64::MAX, 253_402_300_799] {
        let signer = Signer::new();
        let assertion = make_assertion(Arc::clone(&signer), &[now]);
        assert_eq!(
            assertion.assertion().await.unwrap_err(),
            failure(CertificateError::Clock)
        );
        assert!(signer.inputs.lock().unwrap().is_empty());
        assert_eq!(signer.nonces.load(Ordering::Acquire), 0);
    }
    for finished in [NOW - 1, NOW + 300, NOW + 301] {
        let signer = Signer::new();
        let assertion = make_assertion(Arc::clone(&signer), &[NOW, finished]);
        assert_eq!(
            assertion.assertion().await.unwrap_err(),
            failure(CertificateError::Clock)
        );
        assert_eq!(signer.inputs.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn selection_drift_is_rejected_before_work_and_after_signing() {
    let signer = Signer::new();
    let assertion = make_assertion(Arc::clone(&signer), &[]);
    signer.changed.store(true, Ordering::Release);
    assert_eq!(
        assertion.assertion().await.unwrap_err(),
        failure(CertificateError::Selection)
    );
    assert!(signer.inputs.lock().unwrap().is_empty());
    assert_eq!(signer.nonces.load(Ordering::Acquire), 0);

    let signer = Arc::new(Signer {
        change_on_sign: true,
        ..Arc::try_unwrap(Signer::new()).unwrap()
    });
    let assertion = make_assertion(Arc::clone(&signer), &[NOW]);
    assert_eq!(
        assertion.assertion().await.unwrap_err(),
        failure(CertificateError::Selection)
    );
    assert_eq!(signer.inputs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_signature_size_and_typed_signer_failures_never_publish_an_assertion() {
    for signature_length in [0, 255, 513] {
        let signer = Arc::new(Signer {
            signature_length,
            ..Arc::try_unwrap(Signer::new()).unwrap()
        });
        let assertion = make_assertion(signer, &[NOW, NOW]);
        assert_eq!(
            assertion.assertion().await.unwrap_err(),
            failure(CertificateError::Signature)
        );
    }
    for error in [
        CertificateError::Capacity,
        CertificateError::Cancelled,
        CertificateError::Policy,
        CertificateError::InteractionRequired,
    ] {
        let signer = Arc::new(Signer {
            error: Some(failure(error)),
            ..Arc::try_unwrap(Signer::new()).unwrap()
        });
        let assertion = make_assertion(signer, &[NOW]);
        assert_eq!(assertion.assertion().await.unwrap_err(), failure(error));
    }
}

#[test]
fn construction_and_debug_never_read_nonce_sign_or_format_injected_signer() {
    let signer = Signer::new();
    let assertion = make_assertion(Arc::clone(&signer), &[]);
    assert_eq!(
        format!("{:?}", assertion.config),
        "CertificateConfig { .. }"
    );
    assert_eq!(format!("{assertion:?}"), "CertificateAssertion { .. }");
    assert_eq!(assertion.config.certificate_sha256(), [0xfb; 32]);
    assert_eq!(signer.nonces.load(Ordering::Acquire), 0);
    assert!(signer.inputs.lock().unwrap().is_empty());
}

#[test]
fn certificate_errors_have_no_source_or_sensitive_debug_fields() {
    use std::error::Error as _;

    for error in [
        CertificateError::Policy,
        CertificateError::Clock,
        CertificateError::Native { code: -1 },
    ] {
        assert!(error.source().is_none());
        assert!(!format!("{error} {error:?}").contains("synthetic-sensitive"));
    }
}

#[derive(Debug)]
struct NeverHttp;

#[async_trait]
impl azure_core::http::HttpClient for NeverHttp {
    async fn execute_request(
        &self,
        _: &azure_core::http::Request,
    ) -> azure_core::Result<azure_core::http::AsyncRawResponse> {
        panic!("no HTTP is allowed when certificate assertion construction fails")
    }
}

fn provider_options() -> ClientOptions {
    ClientOptions {
        cloud: Some(Arc::new(azure_core::cloud::CloudConfiguration::AzurePublic)),
        transport: Some(azure_core::http::Transport::new(Arc::new(NeverHttp))),
        retry: azure_core::http::RetryOptions::none(),
        ..Default::default()
    }
}

#[tokio::test]
async fn official_provider_construction_is_inert_and_preserves_sanitized_certificate_failures() {
    let signer = Arc::new(Signer {
        error: Some(failure(CertificateError::Policy)),
        ..Arc::try_unwrap(Signer::new()).unwrap()
    });
    let config = CertificateConfig::new(signer.clone());
    let provider = config
        .provider(
            ServiceIdentity::new(TENANT, CLIENT).unwrap(),
            provider_options(),
        )
        .unwrap();
    assert_eq!(signer.nonces.load(Ordering::Acquire), 0);
    assert!(signer.inputs.lock().unwrap().is_empty());
    let error = provider
        .get_token(&["https://api.fabric.microsoft.com/.default"], None)
        .await
        .unwrap_err();
    let sanitized = super::super::sanitize_provider(error);
    assert_eq!(
        sanitized.to_string(),
        credential_error(failure(CertificateError::Policy)).to_string()
    );
    assert_eq!(signer.nonces.load(Ordering::Acquire), 1);
    assert_eq!(signer.inputs.lock().unwrap().len(), 1);
}

#[derive(Debug, Default)]
struct PendingSigner(AtomicUsize);

#[async_trait]
impl CertificateSigner for PendingSigner {
    fn certificate_sha256(&self) -> [u8; 32] {
        [0xfb; 32]
    }

    fn assertion_nonce(&self) -> Result<[u8; 16], HeadlessError> {
        Ok([1; 16])
    }

    async fn sign_ps256(&self, _: &[u8]) -> Result<Vec<u8>, HeadlessError> {
        if self.0.fetch_add(1, Ordering::AcqRel) == 0 {
            std::future::pending::<()>().await;
        }
        Err(failure(CertificateError::Cancelled))
    }
}

#[tokio::test]
async fn dropping_a_fake_signature_wait_releases_the_official_token_cache_lock() {
    use std::task::{Context, Waker};

    let signer = Arc::new(PendingSigner::default());
    let provider = CertificateConfig::new(signer.clone())
        .provider(
            ServiceIdentity::new(TENANT, CLIENT).unwrap(),
            provider_options(),
        )
        .unwrap();
    let scopes = ["https://api.fabric.microsoft.com/.default"];
    let mut pending = provider.get_token(&scopes, None);
    assert!(
        pending
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    drop(pending);
    let error = provider.get_token(&scopes, None).await.unwrap_err();
    assert_eq!(
        super::super::sanitize_provider(error).to_string(),
        credential_error(failure(CertificateError::Cancelled)).to_string()
    );
    assert_eq!(signer.0.load(Ordering::Acquire), 2);
}

#[derive(Default)]
struct RequestHttp {
    identifiers: Mutex<Vec<String>>,
}

impl fmt::Debug for RequestHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RequestHttp")
    }
}

#[async_trait]
impl azure_core::http::HttpClient for RequestHttp {
    async fn execute_request(
        &self,
        request: &azure_core::http::Request,
    ) -> azure_core::Result<azure_core::http::AsyncRawResponse> {
        use azure_core::{
            Bytes,
            http::{AsyncRawResponse, Method, StatusCode, Url, headers::Headers},
        };

        assert_eq!(request.method(), Method::Post);
        assert_eq!(
            request.url().as_str(),
            "https://login.microsoftonline.com/11111111-1111-1111-1111-111111111111/oauth2/v2.0/token"
        );
        let body = Bytes::from(request.body());
        let form = std::str::from_utf8(body.as_ref()).unwrap();
        let parsed = Url::parse(&format!("https://test.invalid/?{form}")).unwrap();
        let pairs = parsed.query_pairs().collect::<Vec<_>>();
        let value = |name| {
            let matches = pairs
                .iter()
                .filter(|(actual, _)| actual == name)
                .collect::<Vec<_>>();
            assert_eq!(matches.len(), 1);
            matches[0].1.as_ref().to_owned()
        };
        assert_eq!(pairs.len(), 5);
        assert_eq!(value("client_id"), CLIENT);
        assert_eq!(value("grant_type"), "client_credentials");
        assert_eq!(
            value("client_assertion_type"),
            "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"
        );
        assert!(matches!(
            value("scope").as_str(),
            "https://api.fabric.microsoft.com/.default"
                | "https://storage.azure.com/.default"
                | "https://management.azure.com/.default"
        ));
        let assertion = value("client_assertion");
        let parts = assertion.split('.').collect::<Vec<_>>();
        assert_eq!(parts.len(), 3);
        let header = decoded(parts[0]);
        assert_eq!(
            header,
            serde_json::json!({
                "alg": "PS256",
                "typ": "JWT",
                "x5t#S256": "-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_s"
            })
        );
        let payload = decoded(parts[1]);
        assert_eq!(
            payload["aud"].as_str().unwrap().as_bytes(),
            request.url().as_str().as_bytes(),
            "actual official OAuth request and signed JWT audience must not drift"
        );
        assert_eq!(payload["iss"], "22222222-2222-2222-2222-222222222222");
        assert_eq!(payload["sub"], "22222222-2222-2222-2222-222222222222");
        assert_eq!(payload["iat"], payload["nbf"]);
        let issued = payload["iat"].as_i64().unwrap();
        assert_eq!(payload["exp"].as_i64().unwrap() - issued, 300);
        let now = OffsetDateTime::now_utc().unix_timestamp();
        assert!(issued <= now && now < issued + 300);
        let identifier = payload["jti"].as_str().unwrap();
        assert_eq!(base64::decode_url_safe(identifier).unwrap().len(), 16);
        let mut identifiers = self.identifiers.lock().unwrap();
        assert!(!identifiers.iter().any(|existing| existing == identifier));
        identifiers.push(identifier.into());
        // This transport checks the official request/claims contract with a
        // deliberately fake signer. Cryptographic acceptance is independently
        // covered by the published salt32 vector, not claimed by this fake.
        Ok(AsyncRawResponse::from_bytes(
            StatusCode::Ok,
            Headers::new(),
            r#"{"access_token":"synthetic-offline-token","token_type":"Bearer","expires_in":3600}"#,
        ))
    }
}

#[tokio::test]
async fn official_public_cloud_requests_match_protected_claims_and_keep_three_scope_caches() {
    let signer = Signer::new();
    let http = Arc::new(RequestHttp::default());
    let credential = super::super::HeadlessCredential::new(
        super::super::HeadlessInput::Certificate {
            identity: ServiceIdentity::new(TENANT, CLIENT).unwrap(),
            certificate: Box::new(CertificateConfig::new(signer.clone())),
        },
        http.clone(),
    )
    .unwrap();
    assert_eq!(signer.nonces.load(Ordering::Acquire), 0);
    for scope in [
        "https://api.fabric.microsoft.com/.default",
        "https://storage.azure.com/.default",
        "https://management.azure.com/.default",
    ] {
        for _ in 0..2 {
            credential.get_token(&[scope], None).await.unwrap();
        }
    }
    assert_eq!(http.identifiers.lock().unwrap().len(), 3);
    assert_eq!(signer.nonces.load(Ordering::Acquire), 3);
    for scopes in [
        Vec::new(),
        vec!["https://example.invalid/.default"],
        vec![
            "https://api.fabric.microsoft.com/.default",
            "https://storage.azure.com/.default",
        ],
    ] {
        assert!(credential.get_token(&scopes, None).await.is_err());
    }
    assert_eq!(signer.nonces.load(Ordering::Acquire), 3);
}

#[tokio::test]
async fn public_cloud_override_child() {
    if std::env::var_os("FABRIC_CERTIFICATE_TEST_CHILD").is_none() {
        return;
    }
    let signer = Signer::new();
    let http = Arc::new(RequestHttp::default());
    let guard = super::super::CredentialHttp {
        inner: http.clone(),
        endpoint: azure_core::http::Url::parse(&format!(
            "https://login.microsoftonline.com/{TENANT}/oauth2/v2.0/token"
        ))
        .unwrap(),
        host: None,
        managed_client: None,
        pbi_bootstrap: false,
    };
    let mut options = provider_options();
    options.cloud = Some(Arc::new(
        azure_core::cloud::CloudConfiguration::AzureGovernment,
    ));
    options.transport = Some(azure_core::http::Transport::new(Arc::new(guard)));
    let provider = CertificateConfig::new(signer)
        .provider(ServiceIdentity::new(TENANT, CLIENT).unwrap(), options)
        .unwrap();
    provider
        .get_token(&["https://api.fabric.microsoft.com/.default"], None)
        .await
        .unwrap();
    assert_eq!(http.identifiers.lock().unwrap().len(), 1);
}

#[test]
fn ambient_authority_and_internal_cloud_override_cannot_drift_request_or_assertion() {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.env_clear();
    for name in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let output = command
        .env("FABRIC_CERTIFICATE_TEST_CHILD", "1")
        .env("AZURE_AUTHORITY_HOST", "https://authority.invalid/")
        .env("AZURE_CLIENT_SEND_CERTIFICATE_CHAIN", "true")
        .args([
            "--exact",
            "headless::certificate::tests::public_cloud_override_child",
            "--test-threads=1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated fake-HTTP authority test failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "the exact child test must execute rather than match zero tests"
    );
}
