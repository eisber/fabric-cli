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
    async fn wait_until(&self, deadline: Instant, _: &OperationContext) -> Result<(), CoreError> {
        tokio::time::sleep(deadline.saturating_duration_since(self.now())).await;
        Ok(())
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
        _: &dyn Clock,
    ) -> Result<ApiResponse, CoreError> {
        assert!(request.headers.contains_key(USER_AGENT_HEADER));
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
async fn managed_context_survives_retries_polls_and_results_without_custom_headers() {
    let credential = Arc::new(Credential {
        calls: Mutex::new(0),
    });
    let transport = Arc::new(Transport {
        calls: Mutex::new(Vec::new()),
        replies: Mutex::new(VecDeque::from([
            reply(200),
            reply(503),
            reply(200),
            reply(200),
        ])),
    });
    let clock = Arc::new(TestClock {
        start: Instant::now(),
        elapsed: Mutex::new(Duration::ZERO),
    });
    let id = "00000000-0000-4000-8000-000000000001";
    let metadata = RequestMetadata::new(
        RequestCommand::Api,
        Some(AgentKind::Copilot),
        diagnostics::RequestId::parse(id),
        Some("SKILL-CANARY"),
    )
    .unwrap();
    let client = HttpClient::new(credential.clone(), transport.clone(), clock.clone())
        .with_request_metadata(metadata);
    let ctx = context(clock.as_ref(), 240);
    let mut initial = request("POST");
    initial
        .headers
        .insert("if-match".into(), "INITIAL-CANARY".into());
    client
        .execute(initial, Audience::Fabric, &ctx)
        .await
        .unwrap();
    let mut poll = request("GET");
    poll.url = "https://service-advertised.example.test/v1/operations/op-1"
        .parse()
        .unwrap();
    client
        .execute_poll(poll.clone(), Audience::Fabric, &ctx)
        .await
        .unwrap();
    poll.url.set_path("/v1/operations/op-1/result");
    client
        .execute_poll(poll, Audience::Fabric, &ctx)
        .await
        .unwrap();
    let sent = transport.calls.lock().unwrap();
    assert_eq!(sent.len(), 4);
    assert_eq!(*credential.calls.lock().unwrap(), 4);
    for (index, request) in sent.iter().enumerate() {
        assert_eq!(request.headers[CLIENT_REQUEST_ID_HEADER], id);
        assert_eq!(request.headers[FABRIC_SKILL_HEADER], "SKILL-CANARY");
        assert!(request.headers[USER_AGENT_HEADER].ends_with("; copilot)"));
        assert_eq!(request.headers.contains_key("if-match"), index == 0);
    }
}

#[tokio::test]
async fn managed_header_spoofing_is_rejected_before_credentials() {
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
    for name in [
        USER_AGENT_HEADER,
        CLIENT_REQUEST_ID_HEADER,
        FABRIC_SKILL_HEADER,
    ] {
        let mut request = request("GET");
        request
            .headers
            .insert(name.to_uppercase(), "SECRET-CANARY".into());
        let error = client
            .execute(request, Audience::Fabric, &context(clock.as_ref(), 240))
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert!(!error.message.contains("SECRET-CANARY"));
    }
    assert_eq!(*credential.calls.lock().unwrap(), 0);
    assert!(transport.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn configured_origin_is_checked_before_token_acquisition() {
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
    let endpoint =
        Endpoint::from_hostname(Audience::Fabric, "msitapi.fabric.microsoft.com").unwrap();
    let client = HttpClient::new(credential.clone(), transport.clone(), clock.clone())
        .with_endpoint(endpoint);
    let ctx = context(clock.as_ref(), 240);
    assert!(
        client
            .execute(request("GET"), Audience::Fabric, &ctx)
            .await
            .is_err()
    );
    let mut other_audience = request("GET");
    other_audience.url = "https://management.azure.com/subscriptions"
        .parse()
        .unwrap();
    let error = client
        .execute(other_audience, Audience::Azure, &ctx)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert_eq!(*credential.calls.lock().unwrap(), 0);
    assert!(transport.calls.lock().unwrap().is_empty());

    let mut configured = request("GET");
    configured.url = "https://msitapi.fabric.microsoft.com/v1/items"
        .parse()
        .unwrap();
    client
        .execute(configured, Audience::Fabric, &ctx)
        .await
        .unwrap();
    assert_eq!(*credential.calls.lock().unwrap(), 1);
    assert_eq!(transport.calls.lock().unwrap().len(), 1);
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
    let client = HttpClient::new(
        Arc::new(PendingCredential),
        transport.clone(),
        clock.clone(),
    );
    let before = tokio::time::Instant::now();
    let error = client
        .execute_poll(request("GET"), Audience::Fabric, &ctx)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert_eq!(*clock.elapsed.lock().unwrap(), Duration::from_secs(599));
    assert_eq!(
        tokio::time::Instant::now().duration_since(before),
        Duration::from_secs(1)
    );
    assert!(transport.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unlimited_wait_is_still_cancellable_without_starting_a_deadline_timer() {
    let clock = TestClock {
        start: Instant::now(),
        elapsed: Mutex::new(Duration::ZERO),
    };
    let cancellation = CancellationToken::new();
    let context = OperationContext::with_deadline(None, cancellation.clone());
    let error = fabric_sdk_http::bounded_with_clock(&clock, &context, async {
        cancellation.cancel();
        std::future::pending::<()>().await
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
        2,
        "only newly acquired authorization and managed User-Agent"
    );
    assert!(sent[0].body.is_empty());
}
