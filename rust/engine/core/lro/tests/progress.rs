// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline observation, safety and cancellation requirements.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use fabric_core::{
    ApiRequest, ApiResponse, Audience, CancellationToken, Clock, CoreError, ErrorKind,
    HttpExecutor, OperationContext,
};
use fabric_sdk_operations::{LroConfig, PollObserver, PollProgress, complete_observed};
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

const JOB: &str = "/v1/workspaces/workspace/items/item/jobs/instances/job?secret=QUERY-CANARY";

struct TestClock(Mutex<Instant>);
#[async_trait]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
    async fn sleep(&self, delay: Duration, context: &OperationContext) -> Result<(), CoreError> {
        *self.0.lock().unwrap() += delay;
        context.remaining_at(self.now())?;
        Ok(())
    }
}

struct Script {
    replies: Mutex<VecDeque<ApiResponse>>,
    calls: Mutex<usize>,
}
#[async_trait]
impl HttpExecutor for Script {
    async fn execute(
        &self,
        request: ApiRequest,
        _: Audience,
        _: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        assert_eq!(request.method, "GET");
        assert!(request.headers.is_empty());
        assert!(request.body.is_empty());
        *self.calls.lock().unwrap() += 1;
        Ok(self.replies.lock().unwrap().pop_front().unwrap())
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<PollProgress>>);
#[async_trait]
impl PollObserver for Recorder {
    async fn observe(&self, progress: PollProgress) -> Result<(), CoreError> {
        self.0.lock().unwrap().push(progress);
        Ok(())
    }
}

struct CancellingObserver(CancellationToken);
#[async_trait]
impl PollObserver for CancellingObserver {
    async fn observe(&self, _: PollProgress) -> Result<(), CoreError> {
        self.0.cancel();
        std::future::pending().await
    }
}

struct ExpiringObserver<'a>(&'a TestClock);
#[async_trait]
impl PollObserver for ExpiringObserver<'_> {
    async fn observe(&self, _: PollProgress) -> Result<(), CoreError> {
        *self.0.0.lock().unwrap() += Duration::from_secs(2);
        Ok(())
    }
}

fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> ApiResponse {
    ApiResponse {
        status,
        body: body.as_bytes().to_vec(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

async fn observe(
    initial: ApiResponse,
    replies: Vec<ApiResponse>,
    observer: &dyn PollObserver,
    cancellation: CancellationToken,
    config: LroConfig,
) -> (Result<ApiResponse, CoreError>, usize) {
    let script = Script {
        replies: Mutex::new(replies.into()),
        calls: Mutex::new(0),
    };
    let clock = TestClock(Mutex::new(Instant::now()));
    let result = complete_observed(
        &script,
        &clock,
        Audience::Fabric,
        initial,
        cancellation,
        config,
        observer,
    )
    .await;
    let count = *script.calls.lock().unwrap();
    (result, count)
}

#[tokio::test]
async fn jobs_observe_every_pending_and_terminal_status_without_metadata() {
    for terminal in ["Completed", "Deduped", "Failed", "Cancelled"] {
        let recorder = Recorder::default();
        let terminal_body = format!(
            r#"{{"status":"{terminal}","error":"BODY-CANARY","percentageComplete":"PERCENT-CANARY"}}"#
        );
        let (result, count) = observe(
            response(202, "", &[("location", JOB)]),
            vec![
                response(200, r#"{"status":"NotStarted"}"#, &[]),
                response(200, r#"{"status":"InProgress"}"#, &[]),
                response(200, &terminal_body, &[("x-fixture", "HEADER-CANARY")]),
            ],
            &recorder,
            CancellationToken::new(),
            LroConfig::default(),
        )
        .await;
        assert_eq!(count, 3);
        let events = recorder.0.lock().unwrap();
        assert_eq!(
            events.iter().map(|p| p.status()).collect::<Vec<_>>(),
            ["NotStarted", "InProgress", terminal]
        );
        assert!(events.iter().all(|p| p.is_job()));
        assert!(!format!("{events:?}").contains("CANARY"));
        if matches!(terminal, "Failed" | "Cancelled") {
            let error = result.unwrap_err();
            assert_eq!(error.kind, ErrorKind::Service);
            assert!(!format!("{error:?}").contains("CANARY"));
        } else {
            assert_eq!(result.unwrap().body, terminal_body.as_bytes());
        }
    }
}

#[tokio::test]
async fn fabric_observes_status_not_initial_result_or_unknown_body_fields() {
    let recorder = Recorder::default();
    let (result, count) = observe(
        response(
            202,
            r#"{"status":"Accepted"}"#,
            &[("x-ms-operation-id", "op-1")],
        ),
        vec![
            response(200, r#"{"status":"rUnNiNg","percentageComplete":50}"#, &[]),
            response(
                200,
                r#"{"status":"Succeeded"}"#,
                &[("location", "/v1/operations/op-1/result")],
            ),
            response(200, r#"{"status":"RESULT-CANARY"}"#, &[]),
        ],
        &recorder,
        CancellationToken::new(),
        LroConfig::default(),
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(count, 3);
    let events = recorder.0.lock().unwrap();
    assert_eq!(
        events.iter().map(|p| p.status()).collect::<Vec<_>>(),
        ["Running", "Succeeded"]
    );
    assert!(events.iter().all(|p| !p.is_job()));
    assert_eq!(events[0].percentage_complete(), Some(50.0));
    assert_eq!(events[1].percentage_complete(), None);
}

#[tokio::test]
async fn percentages_are_optional_bounded_numbers_not_raw_service_text() {
    let oversized = "1".repeat(400);
    for (value, expected) in [
        ("0", Some(0.0)),
        ("12.5", Some(12.5)),
        ("100", Some(100.0)),
        ("null", None),
        ("-1", None),
        ("100.1", None),
        (r#""PERCENT-CANARY\nINJECTION""#, None),
        (r#""50""#, None),
        ("true", None),
        ("{}", None),
        (r#"{"$serde_json::private::Number":"50"}"#, None),
        (oversized.as_str(), None),
    ] {
        let recorder = Recorder::default();
        let status_body = format!(r#"{{"status":"Completed","percentageComplete":{value}}}"#);
        let (result, count) = observe(
            response(202, "", &[("location", JOB)]),
            vec![response(200, &status_body, &[])],
            &recorder,
            CancellationToken::new(),
            LroConfig::default(),
        )
        .await;
        assert!(result.is_ok(), "{value}");
        assert_eq!(count, 1);
        let events = recorder.0.lock().unwrap();
        assert_eq!(events[0].status(), "Completed");
        assert_eq!(events[0].percentage_complete(), expected, "{value}");
        assert!(!format!("{events:?}").contains("CANARY"));
    }
}

#[tokio::test]
async fn arm_status_and_location_observe_their_existing_completion_protocols() {
    for header in ["azure-asyncoperation", "location"] {
        let recorder = Recorder::default();
        let clock = TestClock(Mutex::new(Instant::now()));
        let replies = if header == "location" {
            vec![response(202, "", &[]), response(200, "", &[])]
        } else {
            vec![
                response(200, r#"{"status":"Running"}"#, &[]),
                response(200, r#"{"status":"Succeeded"}"#, &[]),
            ]
        };
        let script = Script {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(0),
        };
        let result = complete_observed(
            &script,
            &clock,
            Audience::Azure,
            response(202, r#"{"original":true}"#, &[(header, "/status")]),
            CancellationToken::new(),
            LroConfig::default(),
            &recorder,
        )
        .await
        .unwrap();
        assert_eq!(result.status, 200);
        assert_eq!(result.body, br#"{"original":true}"#);
        assert_eq!(*script.calls.lock().unwrap(), 2);
        let events = recorder.0.lock().unwrap();
        assert_eq!(
            events.iter().map(|p| p.status()).collect::<Vec<_>>(),
            ["Running", "Succeeded"]
        );
        assert!(events.iter().all(|p| !p.is_job()));
    }
}

#[tokio::test]
async fn rejected_urls_and_unknown_or_malformed_statuses_never_emit_raw_progress() {
    for location in [
        "https://evil.invalid/v1/operations/op-1?secret=QUERY-CANARY",
        "https://user:PASS-CANARY@api.fabric.microsoft.com/v1/operations/op-1",
        "https://api.fabric.microsoft.com:444/v1/operations/op-1",
        "/v1/operations/op-1#FRAGMENT-CANARY",
        "/v1/operations/%2e%2e/op-1",
        "/v1/operations/op%2f1",
    ] {
        let recorder = Recorder::default();
        let (result, count) = observe(
            response(202, "", &[("location", location)]),
            vec![],
            &recorder,
            CancellationToken::new(),
            LroConfig::default(),
        )
        .await;
        assert_eq!(result.unwrap_err().kind, ErrorKind::Protocol);
        assert_eq!(count, 0);
        assert!(recorder.0.lock().unwrap().is_empty());
    }
    for body in [
        r#"{"status":"BODY-CANARY\nINJECTION"}"#,
        r#"{"status":42}"#,
        "MALFORMED-CANARY",
    ] {
        let recorder = Recorder::default();
        let (result, count) = observe(
            response(202, "", &[("location", JOB)]),
            vec![response(200, body, &[])],
            &recorder,
            CancellationToken::new(),
            LroConfig::default(),
        )
        .await;
        let error = result.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(!format!("{error:?}").contains("CANARY"));
        assert_eq!(count, 1);
        assert!(recorder.0.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn cancellation_bounds_pending_observation_and_timeout_never_reports_unsent_poll() {
    let cancellation = CancellationToken::new();
    let observer = CancellingObserver(cancellation.clone());
    let (result, count) = observe(
        response(202, "", &[("location", JOB)]),
        vec![response(200, r#"{"status":"InProgress"}"#, &[])],
        &observer,
        cancellation,
        LroConfig::default(),
    )
    .await;
    assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(count, 1);

    let recorder = Recorder::default();
    let (result, count) = observe(
        response(202, "", &[("location", JOB)]),
        vec![],
        &recorder,
        CancellationToken::new(),
        LroConfig::new(None, Some(Duration::from_secs(1))).unwrap(),
    )
    .await;
    assert_eq!(result.unwrap_err().kind, ErrorKind::Timeout);
    assert_eq!(count, 0);
    assert!(recorder.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn observation_time_counts_toward_the_same_deadline() {
    let clock = TestClock(Mutex::new(Instant::now()));
    let script = Script {
        replies: Mutex::new(VecDeque::from([response(
            200,
            r#"{"status":"InProgress"}"#,
            &[],
        )])),
        calls: Mutex::new(0),
    };
    let error = complete_observed(
        &script,
        &clock,
        Audience::Fabric,
        response(202, "", &[("location", JOB)]),
        CancellationToken::new(),
        LroConfig::new(Some(Duration::from_secs(1)), Some(Duration::from_secs(2))).unwrap(),
        &ExpiringObserver(&clock),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert_eq!(*script.calls.lock().unwrap(), 1);
    assert!(!format!("{error:?}").contains("CANARY"));
}
