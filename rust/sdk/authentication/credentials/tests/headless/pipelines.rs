// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real official AzurePipelinesCredential with offline caller-injected HTTP.
#![deny(unnameable_test_items)]

use super::*;
use fabric_identity::headless::PipelinesConfig;
use std::ffi::OsString;

const CONNECTION: &str = "33333333-3333-3333-3333-333333333333";
const SYSTEM_TOKEN: &str = "synthetic-pipeline-system-token";

fn endpoint() -> String {
    format!(
        "https://dev.azure.com/organization/{TENANT}/_apis/distributedtask/hubs/build/plans/{TENANT}/jobs/{CLIENT}/oidctoken"
    )
}

fn input(name: &str) -> Option<OsString> {
    Some(match name {
        "SYSTEM_COLLECTIONURI" => "https://dev.azure.com/organization/".into(),
        "SYSTEM_TEAMPROJECTID" | "SYSTEM_PLANID" => TENANT.into(),
        "SYSTEM_HOSTTYPE" => "build".into(),
        "SYSTEM_JOBID" => CLIENT.into(),
        "SYSTEM_OIDCREQUESTURI" => endpoint().into(),
        _ => panic!("unselected input read"),
    })
}

fn config() -> PipelinesConfig {
    PipelinesConfig::from_inputs(CONNECTION, &endpoint(), Secret::new(SYSTEM_TOKEN), input).unwrap()
}

fn credential(http: Arc<dyn HttpClient>) -> Arc<HeadlessCredential> {
    HeadlessCredential::new(
        HeadlessInput::AzurePipelines {
            identity: identity(),
            pipeline: Box::new(config()),
        },
        http,
    )
    .unwrap()
}

fn assertion(value: &str) -> azure_core::Result<AsyncRawResponse> {
    response(200, format!(r#"{{"oidcToken":"{value}"}}"#))
}

fn redacted(error: &Error) {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = source {
        let text = format!("{error:?} {error}");
        for marker in [
            SYSTEM_TOKEN,
            "sensitive-provider",
            "synthetic.assertion",
            "escaped-body",
        ] {
            assert!(!text.contains(marker), "provider failure was not sanitized");
        }
        source = error.source();
    }
}

#[test]
fn official_azure_pipelines_provider_uses_isolated_process_environment_and_fake_http() {
    // Only the child gets SDK-required ambient inputs. The parent test process
    // never changes its environment, including when tests execute concurrently.
    for mode in ["matching", "source-drift", "missing-source"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args([
            "--exact",
            "pipelines::official_provider_process_child",
            "--nocapture",
        ]);
        child.env_clear().env("FAB_HEADLESS_PIPELINE_CHILD", mode);
        for name in [
            "SYSTEM_COLLECTIONURI",
            "SYSTEM_TEAMPROJECTID",
            "SYSTEM_HOSTTYPE",
            "SYSTEM_PLANID",
            "SYSTEM_JOBID",
            "SYSTEM_OIDCREQUESTURI",
        ] {
            child.env(name, input(name).unwrap());
        }
        if mode == "source-drift" {
            child.env(
                "SYSTEM_OIDCREQUESTURI",
                endpoint().replace("organization", "other"),
            );
        } else if mode == "missing-source" {
            child.env_remove("SYSTEM_OIDCREQUESTURI");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "offline Pipelines child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("pipelines-provider-exercised"));
        for bytes in [&output.stdout, &output.stderr] {
            assert!(!String::from_utf8_lossy(bytes).contains(SYSTEM_TOKEN));
        }
    }
}

#[tokio::test]
async fn official_provider_process_child() {
    let Ok(mode) = std::env::var("FAB_HEADLESS_PIPELINE_CHILD") else {
        return;
    };
    if mode != "matching" {
        let http = FakeHttp::new([]);
        assert!(
            HeadlessCredential::new(
                HeadlessInput::AzurePipelines {
                    identity: identity(),
                    pipeline: Box::new(config())
                },
                http.clone(),
            )
            .is_err()
        );
        assert!(http.requests.lock().unwrap().is_empty());
    } else {
        scope_and_renewal().await;
        failures_and_redirects().await;
        drop_cancellation(false).await;
        drop_cancellation(true).await;
    }
    println!("pipelines-provider-exercised");
}

async fn scope_and_renewal() {
    let http = FakeHttp::new([
        assertion("synthetic.assertion.one&=+"),
        token("near-expiry", 120),
        assertion("synthetic.assertion.two"),
        token("renewed", 3600),
        assertion("synthetic.assertion.storage"),
        token("storage", 3600),
        assertion("synthetic.assertion.arm"),
        token("arm", 3600),
    ]);
    let credential = credential(http.clone());
    assert!(
        http.requests.lock().unwrap().is_empty(),
        "construction is network-free"
    );
    for scopes in [
        vec![],
        vec![FABRIC, ARM],
        vec!["https://evil.example/.default"],
    ] {
        redacted(&credential.get_token(&scopes, None).await.unwrap_err());
    }
    assert!(http.requests.lock().unwrap().is_empty());
    for (scope, value) in [
        (FABRIC, "near-expiry"),
        (FABRIC, "renewed"),
        (FABRIC, "renewed"),
        (STORAGE, "storage"),
        (ARM, "arm"),
    ] {
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
    assert!(!format!("{credential:?} {:?}", config()).contains(SYSTEM_TOKEN));
    let requests = http.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        8,
        "one fresh OIDC assertion per access-token refresh; cache hits do not fetch assertions"
    );
    let (pairs, remainder) = requests.as_chunks::<2>();
    assert!(remainder.is_empty());
    for pair in pairs {
        let oidc = &pair[0];
        let token = &pair[1];
        assert_eq!(oidc.method(), Method::Post);
        assert_eq!(
            oidc.url().as_str(),
            format!(
                "{}?api-version=7.1&serviceConnectionId={CONNECTION}",
                endpoint()
            )
        );
        assert!(body(oidc).is_empty());
        assert!(
            oidc.headers()
                .get_str(&"authorization".into())
                .is_ok_and(|value| value == format!("Bearer {SYSTEM_TOKEN}"))
        );
        assert!(
            oidc.headers()
                .get_str(&"x-tfs-fedauthredirect".into())
                .is_ok_and(|value| value == "Suppress")
        );
        assert_eq!(
            token.url().as_str(),
            format!("https://login.microsoftonline.com/{TENANT}/oauth2/v2.0/token")
        );
        assert!(token.headers().get_str(&"authorization".into()).is_err());
        assert!(
            token
                .headers()
                .get_str(&"x-tfs-fedauthredirect".into())
                .is_err()
        );
        assert!(!body(token).contains(SYSTEM_TOKEN));
        assert!(body(token).contains(&format!("client_id={CLIENT}")));
        assert!(body(token).contains("grant_type=client_credentials"));
        assert!(body(token).contains(
            "client_assertion_type=urn%3Aietf%3Aparams%3Aoauth%3Aclient-assertion-type%3Ajwt-bearer"
        ));
        assert!(!body(token).contains("client_secret="));
    }
    assert!(body(&requests[1]).contains("client_assertion=synthetic.assertion.one%26%3D%2B"));
    assert!(body(&requests[3]).contains("client_assertion=synthetic.assertion.two"));
    for (index, resource) in [
        (1, "api.fabric.microsoft.com"),
        (5, "storage.azure.com"),
        (7, "management.azure.com"),
    ] {
        assert!(
            body(&requests[index]).contains(&format!("scope=https%3A%2F%2F{resource}%2F.default"))
        );
    }
}

fn redirect() -> azure_core::Result<AsyncRawResponse> {
    let mut headers = Headers::new();
    headers.insert("location", "https://evil.example/escaped-body");
    headers.insert("x-vss-e2eid", SYSTEM_TOKEN);
    Ok(AsyncRawResponse::from_bytes(
        StatusCode::Found,
        headers,
        SYSTEM_TOKEN.to_owned(),
    ))
}

async fn failures_and_redirects() {
    for failure in [
        response(403, format!("sensitive-provider-{SYSTEM_TOKEN}")),
        response(429, "sensitive-provider-throttle"),
        response(503, "sensitive-provider-unavailable"),
        Err(Error::with_message(ErrorKind::Io, SYSTEM_TOKEN)),
        redirect(),
        response(200, "sensitive-provider-malformed-json"),
        response(200, r#"{"oidcToken":""}"#),
        response(200, r#"{"oidcToken":"invalid assertion"}"#),
        response(200, vec![b'x'; 65_537]),
    ] {
        let http = FakeHttp::new([failure]);
        let credential = credential(http.clone());
        redacted(&credential.get_token(&[FABRIC], None).await.unwrap_err());
        assert_eq!(
            http.requests.lock().unwrap().len(),
            1,
            "no redirect, retry, token exchange or provider fallback"
        );
    }
    for failure in [
        token("expired", 0),
        response(401, format!("sensitive-provider-{SYSTEM_TOKEN}")),
        response(500, "sensitive-provider-token-exchange"),
        redirect(),
    ] {
        let http = FakeHttp::new([assertion("synthetic.assertion"), failure]);
        let credential = credential(http.clone());
        redacted(&credential.get_token(&[FABRIC], None).await.unwrap_err());
        assert_eq!(http.requests.lock().unwrap().len(), 2);
    }
    let http = FakeHttp::new([
        assertion("synthetic.assertion.old"),
        token("near-expiry", 120),
        response(403, "sensitive-provider-refresh"),
    ]);
    let credential = credential(http.clone());
    credential.get_token(&[FABRIC], None).await.unwrap();
    redacted(&credential.get_token(&[FABRIC], None).await.unwrap_err());
    assert_eq!(
        http.requests.lock().unwrap().len(),
        3,
        "a failed refresh never returns the old token"
    );
}

async fn drop_cancellation(during_exchange: bool) {
    use std::{
        future::Future,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Waker},
    };

    #[derive(Debug)]
    struct CancelWire {
        calls: AtomicUsize,
        during_exchange: bool,
    }
    #[async_trait]
    impl HttpClient for CancelWire {
        async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == usize::from(self.during_exchange) {
                std::future::pending::<()>().await;
            }
            if request.url().host_str() == Some("dev.azure.com") {
                assertion("synthetic.assertion.after-cancel")
            } else {
                token("after-cancel", 3600)
            }
        }
    }
    let http = Arc::new(CancelWire {
        calls: AtomicUsize::new(0),
        during_exchange,
    });
    let credential = credential(http.clone());
    let scopes = [FABRIC];
    let mut acquisition = credential.get_token(&scopes, None);
    assert!(
        Future::poll(
            acquisition.as_mut(),
            &mut Context::from_waker(Waker::noop())
        )
        .is_pending()
    );
    drop(acquisition);
    assert_eq!(
        credential
            .get_token(&scopes, None)
            .await
            .unwrap()
            .token
            .secret(),
        "after-cancel"
    );
    assert_eq!(
        http.calls.load(Ordering::SeqCst),
        3 + usize::from(during_exchange)
    );
}
