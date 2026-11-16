// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline observation, safety and cancellation requirements.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use fabric_core::{
    ApiRequest, ApiResponse, Audience, CancellationToken, Clock, CoreError, ErrorKind,
    HttpExecutor, OperationContext, OperationKind,
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
struct Recorder(
    Mutex<Vec<PollProgress>>,
    Mutex<Vec<OperationKind>>,
    Mutex<usize>,
);
#[async_trait]
impl PollObserver for Recorder {
    async fn started(&self, kind: OperationKind) -> Result<(), CoreError> {
        self.1.lock().unwrap().push(kind);
        Ok(())
    }

    async fn observe(&self, progress: PollProgress) -> Result<(), CoreError> {
        self.0.lock().unwrap().push(progress);
        Ok(())
    }

    async fn polling(&self, _: OperationKind) -> Result<(), CoreError> {
        let mut count = self.2.lock().unwrap();
        assert_eq!(*count, self.0.lock().unwrap().len());
        *count += 1;
        Ok(())
    }
}

struct StartClock<'a> {
    clock: TestClock,
    observer: &'a Recorder,
    expected: OperationKind,
}
#[async_trait]
impl Clock for StartClock<'_> {
    fn now(&self) -> Instant {
        self.clock.now()
    }
    fn wall_now(&self) -> SystemTime {
        self.clock.wall_now()
    }
    async fn sleep(&self, delay: Duration, context: &OperationContext) -> Result<(), CoreError> {
        assert_eq!(*self.observer.1.lock().unwrap(), [self.expected]);
        self.clock.sleep(delay, context).await
    }
}

enum StartAction {
    Fail,
    Cancel(CancellationToken),
    Expire,
}
struct StoppingStart<'a> {
    action: StartAction,
    clock: &'a TestClock,
    during_poll: bool,
    during_wait: bool,
}
impl StoppingStart<'_> {
    async fn stop(&self) -> Result<(), CoreError> {
        match &self.action {
            StartAction::Fail => Err(CoreError::new(
                ErrorKind::Transport,
                "Cannot write polling progress",
            )),
            StartAction::Cancel(cancellation) => {
                cancellation.cancel();
                std::future::pending().await
            }
            StartAction::Expire => {
                *self.clock.0.lock().unwrap() += Duration::from_secs(2);
                Ok(())
            }
        }
    }
}
#[async_trait]
impl PollObserver for StoppingStart<'_> {
    async fn started(&self, _: OperationKind) -> Result<(), CoreError> {
        if self.during_poll || self.during_wait {
            Ok(())
        } else {
            self.stop().await
        }
    }
    async fn polling(&self, _: OperationKind) -> Result<(), CoreError> {
        if self.during_poll {
            self.stop().await
        } else {
            Ok(())
        }
    }
    async fn waiting(&self, _: OperationKind, _: Duration) -> Result<(), CoreError> {
        if self.during_wait {
            self.stop().await
        } else {
            Ok(())
        }
    }
    async fn observe(&self, _: PollProgress) -> Result<(), CoreError> {
        panic!("notification must stop reconciliation before a status GET")
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
async fn start_is_once_before_waiting_and_does_not_invent_a_service_status() {
    for (location, kind, terminal) in [
        (JOB, OperationKind::FabricJob, "Completed"),
        ("/v1/operations/op-1", OperationKind::Fabric, "Succeeded"),
    ] {
        let observer = Recorder::default();
        let clock = StartClock {
            clock: TestClock(Mutex::new(Instant::now())),
            observer: &observer,
            expected: kind,
        };
        let start = clock.now();
        let script = Script {
            replies: Mutex::new(VecDeque::from([
                response(200, r#"{"status":"InProgress"}"#, &[]),
                response(200, &format!(r#"{{"status":"{terminal}"}}"#), &[]),
            ])),
            calls: Mutex::new(0),
        };
        complete_observed(
            &script,
            &clock,
            Audience::Fabric,
            response(202, "", &[("location", location), ("retry-after", "600")]),
            CancellationToken::new(),
            LroConfig::default(),
            &observer,
        )
        .await
        .unwrap();
        assert!(clock.now().duration_since(start) >= Duration::from_secs(600));
        assert_eq!(*observer.1.lock().unwrap(), [kind]);
        assert_eq!(observer.0.lock().unwrap().len(), 2);
        assert_eq!(*observer.2.lock().unwrap(), 2);
        assert_eq!(*script.calls.lock().unwrap(), 2);
    }
}

#[tokio::test]
async fn notification_failures_cancellation_and_time_share_reconciliation_bounds_and_recovery() {
    for (during_poll, during_wait) in [(false, false), (true, false), (false, true)] {
        let cancellation = CancellationToken::new();
        for (action, expected) in [
            (StartAction::Fail, ErrorKind::Transport),
            (
                StartAction::Cancel(cancellation.clone()),
                ErrorKind::Cancelled,
            ),
            (StartAction::Expire, ErrorKind::Timeout),
        ] {
            let clock = TestClock(Mutex::new(Instant::now()));
            let observer = StoppingStart {
                action,
                clock: &clock,
                during_poll,
                during_wait,
            };
            let script = Script {
                replies: Mutex::new(VecDeque::new()),
                calls: Mutex::new(0),
            };
            let token = if matches!(observer.action, StartAction::Cancel(_)) {
                cancellation.clone()
            } else {
                CancellationToken::new()
            };
            let error = complete_observed(
                &script,
                &clock,
                Audience::Fabric,
                response(202, "", &[("location", JOB)]),
                token,
                LroConfig::new(
                    Some(Duration::from_millis(100)),
                    Some(Duration::from_secs(1)),
                )
                .unwrap(),
                &observer,
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind, expected);
            assert!(error.recovery.unwrap().outcome_uncertain);
            assert_eq!(*script.calls.lock().unwrap(), 0);
        }
    }
}

struct Schedule<'a> {
    clock: &'a TestClock,
    accepted: Mutex<Vec<(OperationKind, u16)>>,
    waits: Mutex<Vec<Duration>>,
    callback_delay: Duration,
}
#[async_trait]
impl PollObserver for Schedule<'_> {
    async fn accepted(&self, kind: OperationKind, status: u16) -> Result<(), CoreError> {
        self.accepted.lock().unwrap().push((kind, status));
        Ok(())
    }
    async fn waiting(&self, _: OperationKind, delay: Duration) -> Result<(), CoreError> {
        self.waits.lock().unwrap().push(delay);
        *self.clock.0.lock().unwrap() += self.callback_delay;
        Ok(())
    }
    async fn observe(&self, _: PollProgress) -> Result<(), CoreError> {
        Ok(())
    }
}

#[tokio::test]
async fn schedule_reports_server_and_configured_delays_without_adding_callback_time() {
    for (kind, audience, status, headers, configured, expected) in [
        (
            OperationKind::FabricJob,
            Audience::Fabric,
            202,
            vec![("location", JOB), ("retry-after", "25")],
            None,
            vec![25, 7],
        ),
        (
            OperationKind::Fabric,
            Audience::Fabric,
            202,
            vec![("location", "/v1/operations/op-1")],
            None,
            vec![2, 7],
        ),
        (
            OperationKind::Fabric,
            Audience::Fabric,
            202,
            vec![("location", "/v1/operations/op-1"), ("retry-after", "30")],
            Some(20),
            vec![30, 20],
        ),
        (
            OperationKind::ArmStatus,
            Audience::Azure,
            201,
            vec![
                (
                    "azure-asyncoperation",
                    "https://management.azure.com/status",
                ),
                ("retry-after", "25"),
            ],
            None,
            vec![25, 10],
        ),
        (
            OperationKind::ArmStatus,
            Audience::Storage,
            201,
            vec![("azure-asyncoperation", "/status"), ("retry-after", "25")],
            None,
            vec![25, 10],
        ),
    ] {
        for callback_delay in [
            Duration::ZERO,
            Duration::from_secs(1),
            Duration::from_secs(40),
        ] {
            let clock = TestClock(Mutex::new(Instant::now()));
            let start = clock.now();
            let observer = Schedule {
                clock: &clock,
                accepted: Mutex::new(Vec::new()),
                waits: Mutex::new(Vec::new()),
                callback_delay,
            };
            let terminal = if kind == OperationKind::FabricJob {
                "Completed"
            } else {
                "Succeeded"
            };
            let script = Script {
                replies: Mutex::new(VecDeque::from([
                    response(200, r#"{"status":"InProgress"}"#, &[]),
                    response(200, &format!(r#"{{"status":"{terminal}"}}"#), &[]),
                ])),
                calls: Mutex::new(0),
            };
            complete_observed(
                &script,
                &clock,
                audience,
                response(status, "", &headers),
                CancellationToken::new(),
                LroConfig::new(configured.map(Duration::from_secs), None).unwrap(),
                &observer,
            )
            .await
            .unwrap();
            assert_eq!(*observer.accepted.lock().unwrap(), [(kind, status)]);
            let expected: Vec<_> = expected.iter().copied().map(Duration::from_secs).collect();
            assert_eq!(*observer.waits.lock().unwrap(), expected);
            assert_eq!(
                clock.now().duration_since(start),
                expected
                    .iter()
                    .map(|delay| (*delay).max(callback_delay))
                    .sum::<Duration>(),
                "callback latency counts toward each selected delay"
            );
            assert_eq!(*script.calls.lock().unwrap(), 2);
        }
    }
}

#[tokio::test]
async fn raw_and_pre_cancelled_operations_do_not_notify_start() {
    for (initial, cancelled) in [
        (response(200, "{}", &[]), false),
        (response(202, "", &[]), false),
        (response(202, "", &[("location", JOB)]), true),
    ] {
        let observer = Recorder::default();
        let cancellation = CancellationToken::new();
        if cancelled {
            cancellation.cancel();
        }
        let (result, count) = observe(
            initial,
            vec![],
            &observer,
            cancellation,
            LroConfig::default(),
        )
        .await;
        assert_eq!(result.is_err(), cancelled);
        assert_eq!(count, 0);
        assert!(observer.0.lock().unwrap().is_empty());
        assert!(observer.1.lock().unwrap().is_empty());
        assert_eq!(*observer.2.lock().unwrap(), 0);
    }
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
        "http://evil.invalid/v1/operations/op-1?secret=QUERY-CANARY",
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
        assert!(recorder.1.lock().unwrap().is_empty());
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
