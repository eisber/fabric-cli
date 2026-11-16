// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! SDK-only credential injection and response-head evidence across future drops.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::*;
use fabric_sdk_http::HttpClient;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
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
        SystemTime::UNIX_EPOCH
    }
    async fn sleep(&self, _: Duration, _: &OperationContext) -> Result<(), CoreError> {
        panic!("initial requests must not retry")
    }
}
#[derive(Debug)]
struct SdkOnlyCredential;
#[async_trait]
impl TokenCredential for SdkOnlyCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, &[Audience::Fabric.scope()]);
        assert!(options.is_some());
        Ok(AccessToken::new(
            "offline",
            OffsetDateTime::from_unix_timestamp(1000).unwrap(),
        ))
    }
}
#[derive(Clone, Copy)]
enum AfterHead {
    StreamError,
    Cancel,
    Stall,
    Expire,
}
struct HeadTransport {
    calls: AtomicUsize,
    mode: AfterHead,
    clock: Arc<TestClock>,
}
#[async_trait]
impl HttpTransport for HeadTransport {
    async fn send(&self, _: ApiRequest, _: &OperationContext) -> Result<ApiResponse, CoreError> {
        panic!("streaming transport must publish its head")
    }
    async fn send_with_observer(
        &self,
        request: ApiRequest,
        context: &OperationContext,
        head: &ResponseHeadCapture,
    ) -> Result<ApiResponse, CoreError> {
        assert_eq!(
            request.headers.get("authorization").unwrap(),
            "Bearer offline"
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        let headers = BTreeMap::from([(
            "location".into(),
            "/v1/operations/accepted?secret=synthetic".into(),
        )]);
        head.record(202, &headers)?;
        match self.mode {
            AfterHead::StreamError => {
                Err(CoreError::new(ErrorKind::Transport, "Body stream failed"))
            }
            AfterHead::Cancel => {
                context.cancellation.cancel();
                std::future::pending().await
            }
            AfterHead::Stall => std::future::pending().await,
            AfterHead::Expire => {
                *self.clock.elapsed.lock().unwrap() += Duration::from_secs(1);
                Ok(ApiResponse {
                    status: 202,
                    headers,
                    body: vec![],
                })
            }
        }
    }
}
fn request(method: &str) -> ApiRequest {
    ApiRequest {
        method: method.into(),
        url: "https://api.fabric.microsoft.com/v1/items".parse().unwrap(),
        headers: BTreeMap::new(),
        body: b"mutation".to_vec(),
    }
}
#[tokio::test(start_paused = true)]
async fn executor_retains_accepted_head_after_error_cancel_stall_or_expiry_without_replay() {
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        for (mode, expected) in [
            (AfterHead::StreamError, ErrorKind::Transport),
            (AfterHead::Cancel, ErrorKind::Cancelled),
            (AfterHead::Stall, ErrorKind::Timeout),
            (AfterHead::Expire, ErrorKind::Timeout),
        ] {
            let clock = Arc::new(TestClock {
                start: Instant::now(),
                elapsed: Mutex::new(Duration::ZERO),
            });
            let transport = Arc::new(HeadTransport {
                calls: AtomicUsize::new(0),
                mode,
                clock: clock.clone(),
            });
            let client = HttpClient::new(
                Arc::new(SdkOnlyCredential),
                transport.clone(),
                clock.clone(),
            );
            let context = OperationContext {
                cancellation: CancellationToken::new(),
                deadline: Some(clock.now() + Duration::from_secs(1)),
            };
            let error = client
                .execute(request(method), Audience::Fabric, &context)
                .await
                .unwrap_err();
            assert_eq!(error.kind, expected);
            let recovery = error.recovery.unwrap();
            assert_eq!(recovery.operation_id.as_deref(), Some("accepted"));
            assert_eq!(
                recovery.status_url,
                "https://api.fabric.microsoft.com/v1/operations/accepted"
            );
            assert!(recovery.outcome_uncertain);
            assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        }
    }
}

#[derive(Debug)]
struct DropCredential {
    dropped: Arc<AtomicBool>,
    cancel: Option<CancellationToken>,
}
struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[async_trait]
impl TokenCredential for DropCredential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        let _drop = DropFlag(self.dropped.clone());
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        std::future::pending().await
    }
}
#[derive(Debug)]
struct FailingCredential;
#[async_trait]
impl TokenCredential for FailingCredential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        Err(azure_core::Error::with_message(
            azure_core::error::ErrorKind::Credential,
            "synthetic-secret-provider-detail",
        ))
    }
}

#[tokio::test(start_paused = true)]
async fn sdk_acquisition_future_is_dropped_on_cancellation_and_deadline() {
    for cancel in [false, true] {
        let clock = Arc::new(TestClock {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
        });
        let transport = Arc::new(HeadTransport {
            calls: AtomicUsize::new(0),
            mode: AfterHead::Stall,
            clock: clock.clone(),
        });
        let context = OperationContext {
            cancellation: CancellationToken::new(),
            deadline: Some(clock.now() + Duration::from_secs(1)),
        };
        let dropped = Arc::new(AtomicBool::new(false));
        let credential = Arc::new(DropCredential {
            dropped: dropped.clone(),
            cancel: cancel.then(|| context.cancellation.clone()),
        });
        let client = HttpClient::new(credential, transport.clone(), clock);
        let error = client
            .execute(request("POST"), Audience::Fabric, &context)
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            if cancel {
                ErrorKind::Cancelled
            } else {
                ErrorKind::Timeout
            }
        );
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn sdk_errors_are_sanitized_unauthorized_not_provider_diagnostics() {
    let clock = Arc::new(TestClock {
        start: Instant::now(),
        elapsed: Mutex::new(Duration::ZERO),
    });
    let transport = Arc::new(HeadTransport {
        calls: AtomicUsize::new(0),
        mode: AfterHead::Stall,
        clock: clock.clone(),
    });
    let client = HttpClient::new(
        Arc::new(FailingCredential),
        transport.clone(),
        clock.clone(),
    );
    let context = OperationContext {
        cancellation: CancellationToken::new(),
        deadline: Some(clock.now() + Duration::from_secs(1)),
    };
    let error = client
        .execute(request("POST"), Audience::Fabric, &context)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unauthorized);
    assert!(!format!("{error:?}").contains("synthetic-secret-provider-detail"));
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

struct RawAcceptedTransport {
    headers: BTreeMap<String, String>,
    calls: AtomicUsize,
}
#[async_trait]
impl HttpTransport for RawAcceptedTransport {
    async fn send(&self, _: ApiRequest, _: &OperationContext) -> Result<ApiResponse, CoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ApiResponse {
            status: 202,
            headers: self.headers.clone(),
            body: b"raw initial".to_vec(),
        })
    }
}
#[tokio::test]
async fn raw_initial_execution_does_not_apply_reconciliation_handle_policy() {
    for fields in [
        vec![("location", "http://evil.invalid/status?secret=synthetic")],
        vec![
            ("location", "/v1/operations/location-id"),
            ("x-ms-operation-id", "different-id"),
        ],
    ] {
        let clock = Arc::new(TestClock {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
        });
        let transport = Arc::new(RawAcceptedTransport {
            headers: fields
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
            calls: AtomicUsize::new(0),
        });
        let client = HttpClient::new(
            Arc::new(SdkOnlyCredential),
            transport.clone(),
            clock.clone(),
        );
        let context = OperationContext {
            cancellation: CancellationToken::new(),
            deadline: Some(clock.now() + Duration::from_secs(240)),
        };
        let initial = client
            .execute(request("POST"), Audience::Fabric, &context)
            .await
            .unwrap();
        assert_eq!(initial.status, 202);
        assert_eq!(initial.body, b"raw initial");
        assert_eq!(initial.headers, transport.headers);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(*clock.elapsed.lock().unwrap(), Duration::ZERO);
        assert_eq!(
            operation_handle(Audience::Fabric, initial.status, &initial.headers)
                .unwrap_err()
                .kind,
            ErrorKind::Protocol
        );
    }
}

#[tokio::test]
async fn duplicate_retry_after_preserves_unique_id_and_never_retries_mutation() {
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let clock = Arc::new(TestClock {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
        });
        let transport = Arc::new(RawAcceptedTransport {
            headers: BTreeMap::from([
                ("Retry-After".into(), "1".into()),
                ("retry-after".into(), "2".into()),
                ("X-MS-Operation-ID".into(), "op-1".into()),
                (
                    "location".into(),
                    "https://evil.invalid/status?secret=synthetic".into(),
                ),
            ]),
            calls: AtomicUsize::new(0),
        });
        let client = HttpClient::new(
            Arc::new(SdkOnlyCredential),
            transport.clone(),
            clock.clone(),
        );
        let context = OperationContext {
            cancellation: CancellationToken::new(),
            deadline: Some(clock.now() + Duration::from_secs(240)),
        };
        let error = client
            .execute(request(method), Audience::Fabric, &context)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(!format!("{error:?}").contains("synthetic"));
        assert!(!format!("{error:?}").contains("evil.invalid"));
        let recovery = error.recovery.unwrap();
        assert_eq!(recovery.operation_id.as_deref(), Some("op-1"));
        assert_eq!(
            recovery.status_url,
            "https://api.fabric.microsoft.com/v1/operations/op-1"
        );
        assert!(recovery.outcome_uncertain);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(*clock.elapsed.lock().unwrap(), Duration::ZERO);
    }
}

#[tokio::test]
async fn rejected_metadata_never_chooses_duplicate_ids_or_salvages_location() {
    for ids in [
        vec![("X-MS-Operation-ID", "op-1"), ("x-ms-operation-id", "op-1")],
        vec![
            ("X-MS-Operation-ID", "op-1"),
            ("x-ms-operation-id", "other"),
        ],
        vec![("x-ms-operation-id", "bad/id")],
        vec![],
    ] {
        let clock = Arc::new(TestClock {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
        });
        let mut headers = BTreeMap::from([
            ("Retry-After".into(), "1".into()),
            ("retry-after".into(), "2".into()),
            (
                "location".into(),
                "/v1/operations/location-only?secret=synthetic".into(),
            ),
        ]);
        headers.extend(
            ids.into_iter()
                .map(|(name, value)| (name.to_string(), value.to_string())),
        );
        let transport = Arc::new(RawAcceptedTransport {
            headers,
            calls: AtomicUsize::new(0),
        });
        let client = HttpClient::new(
            Arc::new(SdkOnlyCredential),
            transport.clone(),
            clock.clone(),
        );
        let context = OperationContext {
            cancellation: CancellationToken::new(),
            deadline: Some(clock.now() + Duration::from_secs(240)),
        };
        let error = client
            .execute(request("POST"), Audience::Fabric, &context)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(error.recovery.is_none());
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(*clock.elapsed.lock().unwrap(), Duration::ZERO);
    }
}

// Emulates native ingress rejecting a raw head before any body is materialized;
// native unit tests separately exercise its real HeaderMap ingestion helper.
struct RejectedRawHeadTransport {
    calls: AtomicUsize,
}
#[async_trait]
impl HttpTransport for RejectedRawHeadTransport {
    async fn send(&self, _: ApiRequest, _: &OperationContext) -> Result<ApiResponse, CoreError> {
        panic!("raw head must be published before returning its protocol error")
    }
    async fn send_with_observer(
        &self,
        _: ApiRequest,
        _: &OperationContext,
        head: &ResponseHeadCapture,
    ) -> Result<ApiResponse, CoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(head.reject_headers(
            202,
            [
                ("retry-after", b"1".as_slice()),
                ("retry-after", b"2".as_slice()),
                ("x-ms-operation-id", b"op-1".as_slice()),
                (
                    "location",
                    b"https://evil.invalid/status?secret=synthetic".as_slice(),
                ),
            ],
            CoreError::new(ErrorKind::Protocol, "Duplicate operation control header"),
        ))
    }
}

#[tokio::test]
async fn rejected_raw_head_preserves_recovery_through_executor_without_mutation_replay() {
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let clock = Arc::new(TestClock {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
        });
        let transport = Arc::new(RejectedRawHeadTransport {
            calls: AtomicUsize::new(0),
        });
        let client = HttpClient::new(
            Arc::new(SdkOnlyCredential),
            transport.clone(),
            clock.clone(),
        );
        let context = OperationContext {
            cancellation: CancellationToken::new(),
            deadline: Some(clock.now() + Duration::from_secs(240)),
        };
        let error = client
            .execute(request(method), Audience::Fabric, &context)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(!format!("{error:?}").contains("synthetic"));
        assert!(!format!("{error:?}").contains("evil.invalid"));
        let recovery = error.recovery.unwrap();
        assert_eq!(recovery.operation_id.as_deref(), Some("op-1"));
        assert_eq!(
            recovery.status_url,
            "https://api.fabric.microsoft.com/v1/operations/op-1"
        );
        assert!(recovery.outcome_uncertain);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(*clock.elapsed.lock().unwrap(), Duration::ZERO);
    }
}
