// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline diagnostic delivery, privacy, and recovery across failed observers.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::{
    ApiRequest, ApiResponse, Audience, CancellationToken, Clock, CoreError, ErrorKind,
    HttpExecutor, HttpTransport, OperationContext, ResponseHeadCapture,
    diagnostics::{DiagnosticEvent, DiagnosticObserver, DiagnosticPhase, Metadata},
};
use fabric_sdk_http::HttpClient;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

struct TestClock {
    now: Mutex<Instant>,
    sleeps: AtomicUsize,
}
#[async_trait]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self.now.lock().unwrap()
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
    async fn wait_until(&self, deadline: Instant, _: &OperationContext) -> Result<(), CoreError> {
        tokio::time::sleep(deadline.saturating_duration_since(self.now())).await;
        Ok(())
    }
    async fn sleep(&self, duration: Duration, context: &OperationContext) -> Result<(), CoreError> {
        self.sleeps.fetch_add(1, Ordering::SeqCst);
        *self.now.lock().unwrap() += duration;
        context.remaining_at(self.now())?;
        Ok(())
    }
}
fn clock() -> Arc<TestClock> {
    Arc::new(TestClock {
        now: Mutex::new(Instant::now()),
        sleeps: AtomicUsize::new(0),
    })
}

#[derive(Debug)]
struct Credential {
    fail: bool,
}
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        if self.fail {
            return Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Credential,
                "PROVIDER-CANARY",
            ));
        }
        Ok(AccessToken::new(
            "TOKEN-CANARY",
            OffsetDateTime::from_unix_timestamp(1000).unwrap(),
        ))
    }
}
struct Reply {
    status: Option<u16>,
    fail_body: bool,
}
struct Transport {
    replies: Mutex<VecDeque<Reply>>,
    calls: AtomicUsize,
}
#[async_trait]
impl HttpTransport for Transport {
    async fn send(
        &self,
        _: ApiRequest,
        _: &OperationContext,
        _: &dyn Clock,
    ) -> Result<ApiResponse, CoreError> {
        panic!("tests require response-head observation")
    }
    async fn send_with_observer(
        &self,
        request: ApiRequest,
        _: &OperationContext,
        _: &dyn Clock,
        head: &ResponseHeadCapture,
    ) -> Result<ApiResponse, CoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.headers["authorization"], "Bearer TOKEN-CANARY");
        let reply = self.replies.lock().unwrap().pop_front().unwrap();
        let headers = BTreeMap::from([
            ("Authorization".into(), "HEADER-CANARY".into()),
            ("Proxy-Authorization".into(), "PROXY-CANARY".into()),
            (
                "Content-Type".into(),
                "application/json; secret=PARAMETER-CANARY".into(),
            ),
            (
                "requestId".into(),
                "00000000-0000-4000-8000-000000000001".into(),
            ),
            (
                "Location".into(),
                "/v1/operations/OPERATION-CANARY?sig=QUERY-CANARY".into(),
            ),
            (
                "x-ms-public-api-error-code".into(),
                "SERVICE-CODE-CANARY".into(),
            ),
        ]);
        if let Some(status) = reply.status {
            head.record(status, &headers)?;
        }
        if reply.fail_body {
            return Err(CoreError::new(ErrorKind::Transport, "TRANSPORT-CANARY"));
        }
        Ok(ApiResponse {
            status: reply.status.unwrap(),
            headers,
            body: b"BODY-CANARY".to_vec(),
        })
    }
}
fn transport(replies: impl IntoIterator<Item = Reply>) -> Arc<Transport> {
    Arc::new(Transport {
        replies: Mutex::new(replies.into_iter().collect()),
        calls: AtomicUsize::new(0),
    })
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Marker {
    CredentialStart,
    CredentialEnd,
    Send,
    Response,
    Failure,
    Retry,
}
fn marker(event: &DiagnosticEvent) -> Marker {
    match event {
        DiagnosticEvent::Credential {
            phase: DiagnosticPhase::Started,
            ..
        } => Marker::CredentialStart,
        DiagnosticEvent::Credential { .. } => Marker::CredentialEnd,
        DiagnosticEvent::HttpSend { .. } => Marker::Send,
        DiagnosticEvent::HttpResponse { .. } => Marker::Response,
        DiagnosticEvent::HttpFailure { .. } => Marker::Failure,
        DiagnosticEvent::Retry { .. } => Marker::Retry,
        _ => panic!("unexpected HTTP diagnostic"),
    }
}
#[derive(Clone, Copy)]
enum Action {
    Record,
    Fail,
    Cancel,
    Stall,
    Expire,
}
struct Observer {
    action: Action,
    target: Marker,
    target_hit: usize,
    hits: AtomicUsize,
    events: Mutex<Vec<DiagnosticEvent>>,
    cancellation: CancellationToken,
    clock: Arc<TestClock>,
}
#[async_trait]
impl DiagnosticObserver for Observer {
    async fn observe(&self, event: DiagnosticEvent) -> Result<(), CoreError> {
        let matches = marker(&event) == self.target;
        self.events.lock().unwrap().push(event);
        if !matches || self.hits.fetch_add(1, Ordering::SeqCst) + 1 != self.target_hit {
            return Ok(());
        }
        match self.action {
            Action::Record => Ok(()),
            Action::Fail => Err(CoreError::new(ErrorKind::Transport, "SINK-CANARY")),
            Action::Cancel => {
                self.cancellation.cancel();
                std::future::pending().await
            }
            Action::Stall => std::future::pending().await,
            Action::Expire => {
                *self.clock.now.lock().unwrap() += Duration::from_secs(30);
                Ok(())
            }
        }
    }
}
fn observer(
    clock: &Arc<TestClock>,
    context: &OperationContext,
    action: Action,
    target: Marker,
    hit: usize,
) -> Arc<Observer> {
    Arc::new(Observer {
        action,
        target,
        target_hit: hit,
        hits: AtomicUsize::new(0),
        events: Mutex::new(Vec::new()),
        cancellation: context.cancellation.clone(),
        clock: clock.clone(),
    })
}
fn context(clock: &TestClock) -> OperationContext {
    OperationContext {
        deadline: Some(clock.now() + Duration::from_secs(20)),
        cancellation: CancellationToken::new(),
    }
}
fn request(method: &str) -> ApiRequest {
    ApiRequest {
        method: method.into(),
        url: "https://api.fabric.microsoft.com/v1/workspaces/WORKSPACE-CANARY/items/ITEM-CANARY?key=QUERY-CANARY".parse().unwrap(),
        headers: BTreeMap::new(),
        body: Vec::new(),
    }
}
fn client(clock: &Arc<TestClock>, transport: &Arc<Transport>) -> HttpClient {
    HttpClient::new(
        Arc::new(Credential { fail: false }),
        transport.clone(),
        clock.clone(),
    )
}
fn assert_safe_events(observer: &Observer) {
    let text = format!("{:?}", observer.events.lock().unwrap());
    for canary in [
        "TOKEN-CANARY",
        "HEADER-CANARY",
        "PROXY-CANARY",
        "PARAMETER-CANARY",
        "OPERATION-CANARY",
        "QUERY-CANARY",
        "SERVICE-CODE-CANARY",
        "BODY-CANARY",
        "TRANSPORT-CANARY",
        "PROVIDER-CANARY",
        "SINK-CANARY",
        "WORKSPACE-CANARY",
        "ITEM-CANARY",
    ] {
        assert!(!text.contains(canary));
    }
    assert!(!text.to_ascii_lowercase().contains("authorization"));
}

#[tokio::test]
async fn request_ids_and_location_are_never_copied_from_caller_headers_into_send_events() {
    let clock = clock();
    let context = context(&clock);
    let transport = transport([Reply {
        status: Some(200),
        fail_body: false,
    }]);
    let observer = observer(&clock, &context, Action::Record, Marker::Response, 1);
    let mut request = request("POST");
    let request_id = "00000000-0000-4000-8000-000000000002";
    let x_ms_request_id = "00000000-0000-4000-8000-000000000003";
    request.headers = BTreeMap::from([
        (
            "Content-Type".into(),
            "application/json; secret=PARAMETER-CANARY".into(),
        ),
        ("requestId".into(), request_id.into()),
        ("X-MS-REQUEST-ID".into(), x_ms_request_id.into()),
        (
            "Location".into(),
            "/v1/operations/CALLER-LOCATION-CANARY".into(),
        ),
        ("Retry-After".into(), "321".into()),
    ]);
    client(&clock, &transport)
        .with_diagnostics(observer.clone())
        .execute(request, Audience::Fabric, &context)
        .await
        .unwrap();
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    let events = observer.events.lock().unwrap();
    let outgoing = events
        .iter()
        .find_map(|event| match event {
            DiagnosticEvent::HttpSend { headers, .. } => Some(headers),
            _ => None,
        })
        .unwrap();
    assert_eq!(outgoing.request_id(), Metadata::Absent);
    assert_eq!(outgoing.x_ms_request_id(), Metadata::Absent);
    assert_eq!(outgoing.location(), Metadata::Absent);
    assert_eq!(outgoing.retry_after(), Metadata::Absent);
    assert_eq!(outgoing.content_type().to_string(), "application/json");
    let text = format!("{events:?}");
    for canary in [
        request_id,
        x_ms_request_id,
        "CALLER-LOCATION-CANARY",
        "PARAMETER-CANARY",
    ] {
        assert!(!text.contains(canary));
    }
    assert!(text.contains("00000000-0000-4000-8000-000000000001"));
}

#[tokio::test]
async fn default_disabled_and_enabled_keep_the_same_requests_and_safe_get_retries() {
    for verbose in [false, true] {
        let clock = clock();
        let context = context(&clock);
        let transport = transport([
            Reply {
                status: Some(503),
                fail_body: false,
            },
            Reply {
                status: Some(200),
                fail_body: false,
            },
        ]);
        let observer = observer(&clock, &context, Action::Record, Marker::Response, 1);
        let mut client = client(&clock, &transport);
        if verbose {
            client = client.with_diagnostics(observer.clone());
        }
        let response = client
            .execute_poll(request("GET"), Audience::Fabric, &context)
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"BODY-CANARY");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 2);
        assert_eq!(clock.sleeps.load(Ordering::SeqCst), 1);
        assert_safe_events(&observer);
        let events = observer.events.lock().unwrap();
        if verbose {
            let attempts: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    DiagnosticEvent::HttpSend { id, attempt, .. } => Some((*id, *attempt)),
                    _ => None,
                })
                .collect();
            assert_eq!(attempts, [(1, 1), (2, 2)]);
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, DiagnosticEvent::Retry { .. }))
            );
        } else {
            assert!(events.is_empty());
        }
    }
}

#[tokio::test]
async fn reporting_failure_before_the_first_send_never_starts_http() {
    for target in [Marker::CredentialStart, Marker::CredentialEnd, Marker::Send] {
        let clock = clock();
        let context = context(&clock);
        let transport = transport([]);
        let observer = observer(&clock, &context, Action::Fail, target, 1);
        let error = client(&clock, &transport)
            .with_diagnostics(observer.clone())
            .execute(request("POST"), Audience::Fabric, &context)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Diagnostic);
        assert!(error.message.contains("no request was sent"));
        assert!(error.recovery.is_none());
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        assert_safe_events(&observer);
    }
}

#[tokio::test]
async fn reporting_failure_after_received_status_keeps_status_and_accepted_recovery() {
    for status in [200, 202, 503] {
        let clock = clock();
        let context = context(&clock);
        let transport = transport([Reply {
            status: Some(status),
            fail_body: false,
        }]);
        let observer = observer(&clock, &context, Action::Fail, Marker::Response, 1);
        let error = client(&clock, &transport)
            .with_diagnostics(observer.clone())
            .execute_poll(request("GET"), Audience::Fabric, &context)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Diagnostic);
        assert!(error.message.contains(&format!("HTTP status {status}")));
        assert!(!error.message.contains("no request was sent"));
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(clock.sleeps.load(Ordering::SeqCst), 0);
        if status == 202 {
            let recovery = error.recovery.unwrap();
            assert_eq!(recovery.operation_id.as_deref(), Some("OPERATION-CANARY"));
            assert!(!recovery.status_url.contains("QUERY-CANARY"));
        }
        assert_safe_events(&observer);
    }
}

#[tokio::test]
async fn failed_response_body_keeps_head_evidence_before_a_reporting_failure() {
    for status in [None, Some(202)] {
        let clock = clock();
        let context = context(&clock);
        let transport = transport([Reply {
            status,
            fail_body: true,
        }]);
        let observer = observer(&clock, &context, Action::Fail, Marker::Failure, 1);
        let error = client(&clock, &transport)
            .with_diagnostics(observer.clone())
            .execute_poll(request("GET"), Audience::Fabric, &context)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Diagnostic);
        assert!(error.message.contains(if status.is_some() {
            "HTTP status 202"
        } else {
            "outcome is uncertain"
        }));
        assert_eq!(error.recovery.is_some(), status.is_some());
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(clock.sleeps.load(Ordering::SeqCst), 0);
        assert_safe_events(&observer);
    }
}

#[tokio::test]
async fn successful_failure_event_retains_status_from_headers_before_a_body_error() {
    let clock = clock();
    let context = context(&clock);
    let transport = transport([Reply {
        status: Some(202),
        fail_body: true,
    }]);
    let observer = observer(&clock, &context, Action::Record, Marker::Failure, 1);
    let error = client(&clock, &transport)
        .with_diagnostics(observer.clone())
        .execute(request("POST"), Audience::Fabric, &context)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Transport);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(observer.events.lock().unwrap().last(),
        Some(DiagnosticEvent::HttpFailure { status: Some(status), kind: ErrorKind::Transport, .. })
            if status.code() == 202));
    assert_safe_events(&observer);
}

#[tokio::test]
async fn retry_diagnostic_or_next_acquisition_failure_does_not_claim_nothing_was_sent() {
    for (target, hit, sleeps) in [(Marker::Retry, 1, 0), (Marker::CredentialStart, 2, 1)] {
        let clock = clock();
        let context = context(&clock);
        let transport = transport([Reply {
            status: Some(503),
            fail_body: false,
        }]);
        let observer = observer(&clock, &context, Action::Fail, target, hit);
        let error = client(&clock, &transport)
            .with_diagnostics(observer)
            .execute_poll(request("GET"), Audience::Fabric, &context)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Diagnostic);
        assert!(error.message.contains("HTTP status 503"));
        assert!(!error.message.contains("no request was sent"));
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(clock.sleeps.load(Ordering::SeqCst), sleeps);
    }
}

#[tokio::test(start_paused = true)]
async fn observer_waits_are_bounded_and_preserve_previously_accepted_headers() {
    for (action, expected) in [
        (Action::Cancel, ErrorKind::Cancelled),
        (Action::Stall, ErrorKind::Timeout),
        (Action::Expire, ErrorKind::Timeout),
    ] {
        for target in [Marker::Send, Marker::Response] {
            let clock = clock();
            let context = context(&clock);
            let transport = transport([Reply {
                status: Some(202),
                fail_body: false,
            }]);
            let observer = observer(&clock, &context, action, target, 1);
            let error = client(&clock, &transport)
                .with_diagnostics(observer)
                .execute(request("POST"), Audience::Fabric, &context)
                .await
                .unwrap_err();
            assert_eq!(error.kind, expected);
            let sent = target == Marker::Response;
            assert_eq!(transport.calls.load(Ordering::SeqCst), usize::from(sent));
            assert_eq!(error.recovery.is_some(), sent);
        }
    }
}

#[tokio::test]
async fn credential_failure_event_contains_only_the_fixed_failure_category() {
    let clock = clock();
    let context = context(&clock);
    let transport = transport([]);
    let observer = observer(&clock, &context, Action::Record, Marker::CredentialEnd, 1);
    let client = HttpClient::new(
        Arc::new(Credential { fail: true }),
        transport.clone(),
        clock,
    )
    .with_diagnostics(observer.clone());
    let error = client
        .execute(request("POST"), Audience::Fabric, &context)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unauthorized);
    assert!(matches!(
        observer.events.lock().unwrap().last(),
        Some(DiagnosticEvent::Credential {
            phase: DiagnosticPhase::Failed(ErrorKind::Unauthorized),
            ..
        })
    ));
    assert_safe_events(&observer);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}
