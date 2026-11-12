// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline authenticated execution and retry bounds.
#![deny(unnameable_test_items)]
#![forbid(unsafe_code)]
use async_trait::async_trait;
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
struct Credential {
    calls: Mutex<usize>,
}
struct PendingCredential;
#[async_trait]
impl TokenCredential for PendingCredential {
    async fn token(&self, _: &str, _: &OperationContext) -> Result<AccessToken, CoreError> {
        std::future::pending().await
    }
}
#[async_trait]
impl TokenCredential for Credential {
    async fn token(&self, scope: &str, _: &OperationContext) -> Result<AccessToken, CoreError> {
        assert_eq!(scope, Audience::Fabric.scope());
        *self.calls.lock().unwrap() += 1;
        Ok(AccessToken {
            token: "synthetic-offline-token".into(),
            expires_on: SystemTime::UNIX_EPOCH + Duration::from_secs(9999999999),
        })
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
        deadline: clock.now() + Duration::from_secs(seconds),
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
        deadline: clock.start + Duration::from_secs(600),
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
