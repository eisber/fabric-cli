// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline authenticated execution and retry bounds.
#![deny(unnameable_test_items)]
#![forbid(unsafe_code)]
use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::*;
use fabric_sdk_http::HttpClient;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

struct TestClock {
    start: Instant,
    elapsed: Mutex<Duration>,
}
#[async_trait]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        self.start + *self.elapsed.lock().unwrap()
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + *self.elapsed.lock().unwrap()
    }
    async fn sleep(&self, delay: Duration, context: &OperationContext) -> Result<(), CoreError> {
        let remaining = context.remaining_at(self.now())?;
        *self.elapsed.lock().unwrap() += delay.min(remaining);
        context.remaining_at(self.now())?;
        Ok(())
    }
}
#[derive(Debug)]
struct Credential {
    calls: Mutex<usize>,
}
#[derive(Debug)]
struct PendingCredential;
#[async_trait]
impl TokenCredential for PendingCredential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        std::future::pending().await
    }
}
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, &[Audience::Fabric.scope()]);
        assert!(options.is_some());
        *self.calls.lock().unwrap() += 1;
        Ok(AccessToken::new(
            "synthetic-offline-token",
            OffsetDateTime::from_unix_timestamp(9999999999).unwrap(),
        ))
    }
}
struct Transport {
    calls: Mutex<Vec<ApiRequest>>,
    replies: Mutex<VecDeque<Result<ApiResponse, CoreError>>>,
}
#[async_trait]
impl HttpTransport for Transport {
    async fn send(
        &self,
        request: ApiRequest,
        _: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        assert_eq!(
            request.headers.get("authorization").unwrap(),
            "Bearer synthetic-offline-token"
        );
        self.calls.lock().unwrap().push(request);
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected send")
    }
}
fn reply(status: u16) -> Result<ApiResponse, CoreError> {
    Ok(ApiResponse {
        status,
        headers: BTreeMap::new(),
        body: b"{}".to_vec(),
    })
}
fn context(clock: &dyn Clock, seconds: u64) -> OperationContext {
    OperationContext {
        cancellation: CancellationToken::new(),
        deadline: Some(clock.now() + Duration::from_secs(seconds)),
    }
}
fn request(method: &str) -> ApiRequest {
    ApiRequest {
        method: method.into(),
        url: "https://api.fabric.microsoft.com/v1/items".parse().unwrap(),
        headers: BTreeMap::new(),
        body: vec![],
    }
}
#[tokio::test]
async fn every_mutation_sends_once_even_on_uncertain_send_or_throttling() {
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        for result in [
            reply(429),
            reply(503),
            Err(CoreError::new(ErrorKind::Transport, "Uncertain send")),
        ] {
            let credential = Arc::new(Credential {
                calls: Mutex::new(0),
            });
            let transport = Arc::new(Transport {
                calls: Mutex::new(Vec::new()),
                replies: Mutex::new(VecDeque::from([result])),
            });
            let clock = Arc::new(TestClock {
                start: Instant::now(),
                elapsed: Mutex::new(Duration::ZERO),
            });
            let client = HttpClient::new(credential.clone(), transport.clone(), clock.clone());
            let _result = client
                .execute(
                    request(method),
                    Audience::Fabric,
                    &context(clock.as_ref(), 240),
                )
                .await;
            assert_eq!(transport.calls.lock().unwrap().len(), 1);
            assert_eq!(*credential.calls.lock().unwrap(), 1);
        }
    }
}
#[tokio::test]
async fn safe_get_retries_share_the_operation_budget() {
    let credential = Arc::new(Credential {
        calls: Mutex::new(0),
    });
    let transport = Arc::new(Transport {
        calls: Mutex::new(Vec::new()),
        replies: Mutex::new(VecDeque::from([reply(503), reply(503), reply(200)])),
    });
    let clock = Arc::new(TestClock {
        start: Instant::now(),
        elapsed: Mutex::new(Duration::ZERO),
    });
    let client = HttpClient::new(credential, transport.clone(), clock.clone());
    let error = client
        .execute_poll(
            request("GET"),
            Audience::Fabric,
            &context(clock.as_ref(), 2),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert_eq!(transport.calls.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn input_and_cancellation_fail_before_credentials() {
    let credential = Arc::new(Credential {
        calls: Mutex::new(0),
    });
    let transport = Arc::new(Transport {
        calls: Mutex::new(Vec::new()),
        replies: Mutex::new(VecDeque::new()),
    });
    let clock = Arc::new(TestClock {
        start: Instant::now(),
        elapsed: Mutex::new(Duration::ZERO),
    });
    let client = HttpClient::new(credential.clone(), transport.clone(), clock.clone());
    let mut unsafe_request = request("GET");
    unsafe_request
        .headers
        .insert("Host".into(), "evil.invalid".into());
    assert_eq!(
        client
            .execute(
                unsafe_request,
                Audience::Fabric,
                &context(clock.as_ref(), 240)
            )
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    let ctx = context(clock.as_ref(), 240);
    ctx.cancellation.cancel();
    assert_eq!(
        client
            .execute(request("GET"), Audience::Fabric, &ctx)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    assert_eq!(*credential.calls.lock().unwrap(), 0);
    assert!(transport.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn raw_initial_get_failure_is_inspectable_and_never_retried() {
    for status in [400, 401, 408, 429, 500, 503] {
        let credential = Arc::new(Credential {
            calls: Mutex::new(0),
        });
        let transport = Arc::new(Transport {
            calls: Mutex::new(Vec::new()),
            replies: Mutex::new(VecDeque::from([reply(status)])),
        });
        let clock = Arc::new(TestClock {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
        });
        let client = HttpClient::new(credential, transport.clone(), clock.clone());
        let response = client
            .execute(
                request("GET"),
                Audience::Fabric,
                &context(clock.as_ref(), 240),
            )
            .await
            .unwrap();
        assert_eq!(response.status, status);
        assert_eq!(transport.calls.lock().unwrap().len(), 1);
        assert_eq!(*clock.elapsed.lock().unwrap(), Duration::ZERO);
    }
}

#[tokio::test(start_paused = true)]
async fn token_acquisition_uses_only_remaining_injected_budget() {
    let transport = Arc::new(Transport {
        calls: Mutex::new(Vec::new()),
        replies: Mutex::new(VecDeque::new()),
    });
    let clock = Arc::new(TestClock {
        start: Instant::now(),
        elapsed: Mutex::new(Duration::from_secs(599)),
    });
    let ctx = OperationContext {
        cancellation: CancellationToken::new(),
        deadline: Some(clock.start + Duration::from_secs(600)),
    };
    let client = HttpClient::new(Arc::new(PendingCredential), transport.clone(), clock);
    let before = tokio::time::Instant::now();
    let error = client
        .execute_poll(request("GET"), Audience::Fabric, &ctx)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert!(tokio::time::Instant::now().duration_since(before) < Duration::from_secs(2));
    assert!(transport.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unlimited_wait_is_still_cancellable_without_starting_a_deadline_timer() {
    let cancellation = CancellationToken::new();
    let context = OperationContext {
        cancellation: cancellation.clone(),
        deadline: None,
    };
    let error = fabric_sdk_http::bounded(&context, async {
        cancellation.cancel();
        std::future::pending::<()>().await;
    })
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
}

#[tokio::test]
async fn advertised_hosts_are_only_for_safe_fabric_polling_before_credentials() {
    for url in [
        "https://wabi-us-central-b-primary-redirect.analysis.windows.net/v1/operations/op-1?secret=CANARY",
        "https://msitapi.fabric.microsoft.com/v1/operations/op-1?secret=CANARY",
        "https://msitapi.fabric.microsoft.com/v1/workspaces/workspace/items/item/jobs/instances/job",
        "https://df-msit-scus-redirect.analysis.windows.net/v1/operations/op-1?secret=CANARY",
        "https://df-msit-scus-redirect.analysis.windows.net/v1/operations/op-1/result",
        "https://service-advertised.example.test/v1/operations/op-1",
        "https://result-service.example.test/v1/operations/op-1/result",
    ] {
        assert_safe_polling_permission(url).await;
    }
}

async fn assert_safe_polling_permission(url: &str) {
    let credential = Arc::new(Credential {
        calls: Mutex::new(0),
    });
    let transport = Arc::new(Transport {
        calls: Mutex::new(Vec::new()),
        replies: Mutex::new(VecDeque::from([reply(200)])),
    });
    let clock = Arc::new(TestClock {
        start: Instant::now(),
        elapsed: Mutex::new(Duration::ZERO),
    });
    let client = HttpClient::new(credential.clone(), transport.clone(), clock.clone());
    let ctx = context(clock.as_ref(), 240);
    let mut regional = request("GET");
    regional.url = url.parse().unwrap();
    assert!(
        client
            .execute(regional.clone(), Audience::Fabric, &ctx)
            .await
            .is_err()
    );
    for audience in [Audience::Azure, Audience::Storage, Audience::PowerBi] {
        assert!(
            client
                .execute_poll(regional.clone(), audience, &ctx)
                .await
                .is_err()
        );
    }
    for field in ["method", "body", "headers", "path", "scheme"] {
        let mut invalid = regional.clone();
        match field {
            "method" => invalid.method = "POST".into(),
            "body" => invalid.body = b"{}".to_vec(),
            "headers" => {
                invalid.headers.insert("x-secret".into(), "CANARY".into());
            }
            "path" => invalid.url.set_path("/v1/items"),
            "scheme" => invalid.url.set_scheme("http").unwrap(),
            _ => unreachable!(),
        }
        let error = client
            .execute_poll(invalid, Audience::Fabric, &ctx)
            .await
            .unwrap_err();
        assert!(!error.message.contains("CANARY"));
    }
    assert_eq!(*credential.calls.lock().unwrap(), 0);
    assert!(transport.calls.lock().unwrap().is_empty());
    client
        .execute_poll(regional.clone(), Audience::Fabric, &ctx)
        .await
        .unwrap();
    assert_eq!(*credential.calls.lock().unwrap(), 1);
    let sent = transport.calls.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url, regional.url);
    assert_eq!(
        sent[0].headers.len(),
        1,
        "only newly acquired authorization"
    );
    assert!(sent[0].body.is_empty());
}
