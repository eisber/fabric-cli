// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Independent offline v2 acceptance; frozen v1 fixtures are unchanged.
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    http::{Method, Url},
    time::OffsetDateTime,
};
use fabric_sdk_jobs::{
    Audience, CredentialSelection, JobClient, JobExecutionError, JobExecutor, JobFuture, JobLimits,
    JobRequest, JobResponse, TokenCredential,
    run::{
        RunCapabilities, RunClock, RunDeadline, RunError, RunExchange, RunFormat, RunHandle,
        RunHandleKind, RunInput, RunInputRule, RunOptions, RunOutcome, RunOutput, RunOwner,
        RunOwnerKind, RunPreflight, RunSession, RunTarget, RunTargetKind, RunTimeout,
        SchedulerObserver,
    },
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const ID: &str = "33333333-3333-3333-3333-333333333333";

#[derive(Debug, Default)]
struct Clock {
    milliseconds: AtomicU64,
    advance_on_sleep: Mutex<VecDeque<Duration>>,
    observed_sleeps: Mutex<Vec<Duration>>,
}
impl Clock {
    fn advance(&self, milliseconds: u64) {
        self.milliseconds.fetch_add(milliseconds, Ordering::SeqCst);
    }
}
impl RunClock for Clock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.milliseconds.load(Ordering::SeqCst))
    }
    fn sleep(&self, delay: Duration) -> JobFuture<'_, ()> {
        let end = self.now() + delay;
        Box::pin(poll_fn(move |_| {
            let mut scripted = self.advance_on_sleep.lock().unwrap();
            if scripted.front() == Some(&delay) {
                scripted.pop_front();
                self.observed_sleeps.lock().unwrap().push(delay);
                self.advance(delay.as_millis().try_into().unwrap());
            }
            if self.now() >= end {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }))
    }
}

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
    clock: Option<Arc<Clock>>,
    delay: u64,
    interrupt: Option<Arc<AtomicBool>>,
    pending: bool,
}
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.pending {
            return pending().await;
        }
        if let Some(clock) = &self.clock {
            clock.advance(self.delay);
        }
        if let Some(interrupt) = &self.interrupt {
            interrupt.store(true, Ordering::SeqCst);
        }
        Ok(AccessToken::new(
            "offline-run-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Step {
    response: Result<JobResponse, JobExecutionError>,
    elapsed: u64,
}

struct RecordedRequest {
    method: Method,
    url: String,
    body: Vec<u8>,
}

#[derive(Clone)]
struct Executor {
    steps: Arc<Mutex<VecDeque<Step>>>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    budgets: Arc<Mutex<Vec<Duration>>>,
    clock: Arc<Clock>,
    partial: Arc<Mutex<Option<JobResponse>>>,
    request_times: Arc<Mutex<Vec<Duration>>>,
}
impl Executor {
    fn new(clock: Arc<Clock>, steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            steps: Arc::new(Mutex::new(steps.into_iter().collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
            budgets: Arc::new(Mutex::new(Vec::new())),
            clock,
            partial: Arc::new(Mutex::new(None)),
            request_times: Arc::new(Mutex::new(Vec::new())),
        }
    }
}
impl JobExecutor for Executor {
    fn with_run_budget(
        &self,
        remaining: Duration,
    ) -> Result<Arc<dyn JobExecutor>, JobExecutionError> {
        assert!(!remaining.is_zero());
        self.budgets.lock().unwrap().push(remaining);
        Ok(Arc::new(self.clone()))
    }
    fn execute<'a>(
        &'a self,
        request: JobRequest<'a>,
    ) -> JobFuture<'a, Result<JobResponse, JobExecutionError>> {
        Box::pin(async move {
            self.request_times.lock().unwrap().push(self.clock.now());
            self.requests.lock().unwrap().push(RecordedRequest {
                method: request.method(),
                url: request.url().to_string(),
                body: request.body().to_vec(),
            });
            let step = self
                .steps
                .lock()
                .unwrap()
                .pop_front()
                .expect("no replay/unplanned request");
            self.clock.advance(step.elapsed);
            step.response
        })
    }
    fn execute_run<'a>(
        &'a self,
        request: JobRequest<'a>,
        _: &'a fabric_sdk_jobs::run::RunResponseRecorder,
    ) -> JobFuture<'a, RunExchange> {
        Box::pin(async move {
            match self.execute(request).await {
                Ok(response) => RunExchange::Complete(response),
                Err(error) => RunExchange::Failed {
                    error,
                    observed: self.partial.lock().unwrap().take(),
                },
            }
        })
    }
    fn wait(&self, _: Duration) -> JobFuture<'_, Result<(), JobExecutionError>> {
        panic!("run uses injected total-budget timer, not a second poller")
    }
}

#[derive(Default)]
struct Sink {
    exists: AtomicBool,
    bytes: Mutex<Option<Vec<u8>>>,
}
impl RunOutput for Sink {
    fn preflight(&self, _: RunFormat) -> Result<(), RunError> {
        if self.exists.load(Ordering::SeqCst) {
            Err(RunError::Output)
        } else {
            Ok(())
        }
    }
    fn commit<'a>(&'a self, bytes: &'a [u8]) -> JobFuture<'a, Result<(), RunError>> {
        Box::pin(async move {
            if self.exists.swap(true, Ordering::SeqCst) {
                return Err(RunError::Output);
            }
            *self.bytes.lock().unwrap() = Some(bytes.to_vec());
            Ok(())
        })
    }
}
fn target() -> RunTarget {
    RunTarget::item(
        RunOwner::new(RunOwnerKind::Notebook, WS, ITEM).unwrap(),
        RunTargetKind::Notebook,
    )
    .unwrap()
}
fn caps(kind: RunTargetKind) -> RunCapabilities {
    RunCapabilities {
        target: kind,
        input: RunInputRule::OptionalObject,
        wait: true,
        formats: vec![RunFormat::Json],
    }
}
fn plan(timeout: &str) -> RunPreflight {
    RunPreflight::validate(
        target(),
        &caps(RunTargetKind::Notebook),
        RunOptions {
            wait: true,
            timeout: Some(timeout),
            format: RunFormat::Json,
        },
        RunInput::Omitted,
        &Sink::default(),
        |_, _| Ok(()),
    )
    .unwrap()
}
fn client(credential: Arc<Credential>, executor: &Executor) -> JobClient {
    JobClient::new(
        credential,
        CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
        Arc::new(executor.clone()),
        JobLimits::default(),
    )
}
fn submit_url() -> Url {
    Url::parse(&format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/notebooks/{ITEM}/jobs/execute/instances?beta=false")).unwrap()
}
fn monitor() -> String {
    format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/items/{ITEM}/jobs/instances/{ID}")
}
fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> Step {
    Step {
        elapsed: 0,
        response: Ok(JobResponse {
            status,
            body: body.as_bytes().to_vec(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).into(), (*value).into()))
                .collect(),
        }),
    }
}

#[test]
fn duration_grammar_checked_arithmetic_and_defaults() {
    for (text, milliseconds) in [
        ("1ms", 1),
        ("001s", 1_000),
        ("5m", 300_000),
        ("24h", 86_400_000),
        ("86400000ms", 86_400_000),
    ] {
        assert_eq!(
            RunTimeout::parse(text).unwrap().duration(),
            Duration::from_millis(milliseconds)
        );
    }
    for text in [
        "",
        "0ms",
        "0s",
        "-1s",
        "+1s",
        "1",
        "1.0s",
        "0.5m",
        "25h",
        "86400001ms",
        "1MS",
        "1 s",
        " 1s",
        "1s ",
        "１s",
        "18446744073709551615h",
        "18446744073709551616ms",
    ] {
        assert_eq!(RunTimeout::parse(text), Err(RunError::InvalidTimeout));
    }
    for (kind, wait, seconds) in [
        (RunTargetKind::Notebook, false, 30),
        (RunTargetKind::Notebook, true, 300),
        (RunTargetKind::LivyStatement, false, 30),
        (RunTargetKind::LivyStatement, true, 300),
        (RunTargetKind::Udf, false, 120),
        (RunTargetKind::Udf, true, 120),
        (RunTargetKind::LivyOneShot, true, 600),
    ] {
        assert_eq!(
            RunTimeout::for_target(kind, wait).duration(),
            Duration::from_secs(seconds)
        );
    }
}

#[test]
fn preflight_owner_capability_schema_null_empty_format_and_output_are_local() {
    let credential = Credential::default();
    let options = RunOptions {
        wait: false,
        timeout: None,
        format: RunFormat::Json,
    };
    let wrong = RunOwner::new(RunOwnerKind::UserDataFunction, WS, ITEM).unwrap();
    assert_eq!(
        RunTarget::item(wrong, RunTargetKind::Notebook).unwrap_err(),
        RunError::WrongOwner
    );
    for invalid in ["", "../ws", "x/y", "x%2fy", "x\\y"] {
        assert_eq!(
            RunOwner::new(RunOwnerKind::Notebook, invalid, ITEM).unwrap_err(),
            RunError::InvalidTarget
        );
    }
    let cell = RunTarget::item(target().owner().clone(), RunTargetKind::NotebookCell).unwrap();
    assert_eq!(
        RunPreflight::validate(
            cell,
            &caps(RunTargetKind::NotebookCell),
            options,
            RunInput::Omitted,
            &Sink::default(),
            |_, _| Ok(())
        )
        .unwrap_err(),
        RunError::UnsupportedCapability,
    );
    for input in [
        RunInput::Json(Value::Null),
        RunInput::Json(json!([])),
        RunInput::Json(json!("code")),
    ] {
        assert_eq!(
            RunPreflight::validate(
                target(),
                &caps(RunTargetKind::Notebook),
                options,
                input,
                &Sink::default(),
                |_, _| Ok(())
            )
            .unwrap_err(),
            RunError::InvalidInput
        );
    }
    for input in [RunInput::Omitted, RunInput::Json(json!({}))] {
        assert!(
            RunPreflight::validate(
                target(),
                &caps(RunTargetKind::Notebook),
                options,
                input,
                &Sink::default(),
                |_, _| Ok(())
            )
            .is_ok()
        );
    }
    let sink = Sink::default();
    sink.exists.store(true, Ordering::SeqCst);
    assert_eq!(
        RunPreflight::validate(
            target(),
            &caps(RunTargetKind::Notebook),
            options,
            RunInput::Omitted,
            &sink,
            |_, _| Ok(())
        )
        .unwrap_err(),
        RunError::Output
    );
    assert_eq!(
        RunPreflight::validate(
            target(),
            &caps(RunTargetKind::Notebook),
            RunOptions {
                format: RunFormat::Binary,
                ..options
            },
            RunInput::Omitted,
            &Sink::default(),
            |_, _| Ok(())
        )
        .unwrap_err(),
        RunError::UnsupportedCapability
    );
    assert_eq!(
        RunPreflight::validate(
            target(),
            &caps(RunTargetKind::Notebook),
            options,
            RunInput::Json(json!({"unknown":1})),
            &Sink::default(),
            |_, _| Err(RunError::InvalidInput)
        )
        .unwrap_err(),
        RunError::InvalidInput
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn deadline_n_minus_one_n_n_plus_one_and_cancellation_win_ties() {
    for elapsed in [999, 1_000, 1_001] {
        let clock = Clock::default();
        let deadline = RunDeadline::start(&clock, RunTimeout::parse("1s").unwrap()).unwrap();
        clock.advance(elapsed);
        let polled = AtomicBool::new(false);
        let mut never = pin!(pending());
        let result = deadline
            .guard(&clock, never.as_mut(), async {
                polled.store(true, Ordering::SeqCst);
                42
            })
            .await;
        assert_eq!(polled.load(Ordering::SeqCst), elapsed == 999);
        assert_eq!(
            result,
            if elapsed == 999 {
                Ok(42)
            } else {
                Err(RunError::Deadline)
            }
        );
    }
    let clock = Clock::default();
    let deadline = RunDeadline::start(&clock, RunTimeout::parse("1s").unwrap()).unwrap();
    let polled = AtomicBool::new(false);
    let mut stop = pin!(ready(()));
    assert_eq!(
        deadline
            .guard(&clock, stop.as_mut(), async {
                polled.store(true, Ordering::SeqCst);
            })
            .await,
        Err(RunError::Interrupted)
    );
    assert!(!polled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn one_mutation_on_auth_http_drop_stream_error_and_replay_guard() {
    for status in [401, 429, 500, 503] {
        let clock = Arc::new(Clock::default());
        let executor = Executor::new(
            clock.clone(),
            [response(status, r#"{"error":"native"}"#, &[])],
        );
        let credential = Arc::new(Credential::default());
        let client = client(credential.clone(), &executor);
        let mut session = RunSession::new(&client, plan("10m"), clock, pending()).unwrap();
        let failure = session.submit(submit_url(), vec![]).await.unwrap_err();
        assert_eq!(failure.source, RunError::Http(status));
        assert!(failure.evidence.mutation_may_have_occurred);
        assert_eq!(failure.evidence.initial_response.unwrap().status, status);
        assert_eq!(
            session
                .submit(submit_url(), vec![])
                .await
                .unwrap_err()
                .source,
            RunError::Replay
        );
        let requests = executor.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, Method::Post);
        assert_eq!(requests[0].url, submit_url().as_str());
        assert!(requests[0].body.is_empty());
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            executor.budgets.lock().unwrap()[0],
            Duration::from_secs(600)
        );
    }
    for error in [
        JobExecutionError::Failed,
        JobExecutionError::ResponseLimit,
        JobExecutionError::TimedOut,
    ] {
        let clock = Arc::new(Clock::default());
        let executor = Executor::new(
            clock.clone(),
            [Step {
                response: Err(error),
                elapsed: 0,
            }],
        );
        let client = client(Arc::new(Credential::default()), &executor);
        let mut session = RunSession::new(&client, plan("5m"), clock, pending()).unwrap();
        let failure = session.submit(submit_url(), vec![]).await.unwrap_err();
        assert!(failure.evidence.mutation_may_have_occurred);
        assert!(failure.evidence.initial_response.is_none());
        assert_eq!(failure.evidence.outcome, RunOutcome::Unknown);
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn credential_time_and_interruption_prevent_post_at_deadline() {
    for elapsed in [999, 1_000, 1_001] {
        let clock = Arc::new(Clock::default());
        let executor = Executor::new(
            clock.clone(),
            [response(202, "", &[("Location", &monitor())])],
        );
        let credential = Arc::new(Credential {
            clock: Some(clock.clone()),
            delay: elapsed,
            ..Credential::default()
        });
        let client = client(credential, &executor);
        let mut session = RunSession::new(&client, plan("1s"), clock, pending()).unwrap();
        let result = session.submit(submit_url(), vec![]).await;
        assert_eq!(
            executor.requests.lock().unwrap().len(),
            usize::from(elapsed == 999)
        );
        if elapsed == 999 {
            assert!(result.is_ok());
        } else {
            let failure = result.unwrap_err();
            assert_eq!(failure.source, RunError::Deadline);
            assert!(!failure.evidence.mutation_may_have_occurred);
        }
    }
    let clock = Arc::new(Clock::default());
    let interrupt = Arc::new(AtomicBool::new(false));
    let executor = Executor::new(clock.clone(), []);
    let credential = Arc::new(Credential {
        interrupt: Some(interrupt.clone()),
        ..Credential::default()
    });
    let client = client(credential, &executor);
    let cancelled = poll_fn(move |_| {
        if interrupt.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let mut session = RunSession::new(&client, plan("5m"), clock, cancelled).unwrap();
    let failure = session.submit(submit_url(), vec![]).await.unwrap_err();
    assert_eq!(failure.source, RunError::Interrupted);
    assert!(!failure.evidence.mutation_may_have_occurred);
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn received_acceptance_survives_deadline_on_response_and_byte_bound() {
    let clock = Arc::new(Clock::default());
    let mut step = response(202, "actual-accepted-body", &[("Location", &monitor())]);
    step.elapsed = 1_000;
    let executor = Executor::new(clock.clone(), [step]);
    let client = client(Arc::new(Credential::default()), &executor);
    let mut session = RunSession::new(&client, plan("1s"), clock, pending()).unwrap();
    let failure = session.submit(submit_url(), vec![]).await.unwrap_err();
    assert_eq!(failure.source, RunError::Deadline);
    assert_eq!(failure.evidence.outcome, RunOutcome::Accepted);
    assert_eq!(
        failure.evidence.initial_response.unwrap().body,
        b"actual-accepted-body"
    );

    let clock = Arc::new(Clock::default());
    let executor = Executor::new(
        clock.clone(),
        [response(202, "too-large", &[("Location", &monitor())])],
    );
    let client = JobClient::new(
        Arc::new(Credential::default()),
        CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
        Arc::new(executor),
        JobLimits {
            max_response_bytes: NonZeroUsize::new(2).unwrap(),
            ..JobLimits::default()
        },
    );
    let mut session = RunSession::new(&client, plan("5m"), clock, pending()).unwrap();
    let failure = session.submit(submit_url(), vec![]).await.unwrap_err();
    assert_eq!(failure.source, RunError::ResponseLimit);
    assert_eq!(failure.evidence.outcome, RunOutcome::Accepted);
    assert!(failure.evidence.response_body_omitted);
    assert!(failure.evidence.initial_response.unwrap().body.is_empty());
}

#[test]
fn native_handle_scope_origin_collection_and_udf_correlation_are_distinct() {
    let endpoint = fabric_sdk_jobs::EndpointBinding::public(fabric_sdk_jobs::ServiceOrigin::Fabric);
    let handle = RunHandle::job(&target(), &endpoint, &monitor()).unwrap();
    assert_eq!(handle.kind(), &RunHandleKind::Job(ID.into()));
    for location in [
        monitor().replace(WS, "other"),
        monitor().replace(ITEM, "other"),
        monitor().replace("api.fabric.microsoft.com", "evil.example"),
        monitor().replace(ID, ""),
        format!("{}?id=other", monitor()),
        monitor().replace("/jobs/", "/%6aobs/"),
        format!("{}#fragment", monitor()),
    ] {
        assert!(RunHandle::job(&target(), &endpoint, &location).is_err());
    }
    let udf = RunTarget::udf(
        RunOwner::new(RunOwnerKind::UserDataFunction, WS, ITEM).unwrap(),
        "function",
    )
    .unwrap();
    assert!(
        RunHandle::operation(
            &udf,
            &endpoint,
            &format!("https://api.fabric.microsoft.com/v1/operations/{ID}")
        )
        .is_err()
    );
    let livy =
        RunTarget::livy(RunOwner::new(RunOwnerKind::Lakehouse, WS, ITEM).unwrap(), 0).unwrap();
    let route = format!(
        "https://api.fabric.microsoft.com/v1/workspaces/{WS}/lakehouses/{ITEM}/livyapi/versions/2023-12-01/sessions/0/statements/0"
    );
    let handle = RunHandle::livy(&livy, &endpoint, &route, 0).unwrap();
    assert_eq!(
        handle.kind(),
        &RunHandleKind::Livy {
            session: 0,
            statement: 0
        }
    );
    assert!(RunHandle::livy(&livy, &endpoint, &route.replace(ITEM, "other"), 0).is_err());
}

#[tokio::test]
async fn retry_after_insufficient_budget_never_polls_or_cancels() {
    for seconds in ["1", "2", "999999"] {
        let clock = Arc::new(Clock::default());
        let executor = Executor::new(
            clock.clone(),
            [response(
                202,
                "",
                &[("Location", &monitor()), ("Retry-After", seconds)],
            )],
        );
        let client = client(Arc::new(Credential::default()), &executor);
        let mut session = RunSession::new(&client, plan("1s"), clock, pending()).unwrap();
        session.submit(submit_url(), vec![]).await.unwrap();
        let handle = RunHandle::job(&target(), client.context().endpoint(), &monitor()).unwrap();
        let failure = session
            .observe(&handle, &SchedulerObserver)
            .await
            .unwrap_err();
        assert_eq!(failure.source, RunError::Deadline);
        assert_eq!(failure.evidence.handle.unwrap(), handle);
        assert_eq!(failure.evidence.outcome, RunOutcome::Accepted);
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn existing_run_observation_terminal_unknown_and_wait_denial_never_submit() {
    for (state, outcome) in [
        ("Completed", RunOutcome::Completed),
        ("Failed", RunOutcome::Failed),
        ("Cancelled", RunOutcome::Failed),
        ("Deduped", RunOutcome::Failed),
        ("NewUnknownStatus", RunOutcome::Unknown),
    ] {
        let clock = Arc::new(Clock::default());
        let executor = Executor::new(
            clock.clone(),
            [response(
                200,
                &json!({"status":state,"id":ID,"itemId":ITEM}).to_string(),
                &[],
            )],
        );
        let client = client(Arc::new(Credential::default()), &executor);
        let mut session = RunSession::new(&client, plan("5m"), clock, pending()).unwrap();
        let handle = RunHandle::job(&target(), client.context().endpoint(), &monitor()).unwrap();
        assert_eq!(
            session.observe(&handle, &SchedulerObserver).await.unwrap(),
            outcome
        );
        assert!(!session.evidence().mutation_may_have_occurred);
        let requests = executor.requests.lock().unwrap();
        assert_eq!(requests[0].method, Method::Get);
        assert_eq!(requests[0].url, monitor());
        assert!(requests[0].body.is_empty());
    }
    let clock = Arc::new(Clock::default());
    let executor = Executor::new(
        clock.clone(),
        [
            response(
                202,
                "acceptance",
                &[("Location", &monitor()), ("Retry-After", "0")],
            ),
            response(403, r#"{"error":"read denied"}"#, &[]),
        ],
    );
    let client = client(Arc::new(Credential::default()), &executor);
    let mut session = RunSession::new(&client, plan("5m"), clock, pending()).unwrap();
    session.submit(submit_url(), vec![]).await.unwrap();
    let handle = RunHandle::job(&target(), client.context().endpoint(), &monitor()).unwrap();
    let failure = session
        .observe(&handle, &SchedulerObserver)
        .await
        .unwrap_err();
    assert_eq!(failure.source, RunError::Http(403));
    assert_eq!(failure.evidence.outcome, RunOutcome::Accepted);
    assert_eq!(
        failure.evidence.initial_response.unwrap().body,
        b"acceptance"
    );
    assert_eq!(failure.evidence.last_response.unwrap().status, 403);
    assert_eq!(failure.evidence.handle.unwrap(), handle);
    assert_eq!(executor.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn synchronous_udf_null_false_zero_and_output_race_retain_native_result() {
    for output in [Value::Null, json!(false), json!(0), json!({})] {
        let clock = Arc::new(Clock::default());
        let executor = Executor::new(clock.clone(), []);
        let client = client(Arc::new(Credential::default()), &executor);
        let target = RunTarget::udf(
            RunOwner::new(RunOwnerKind::UserDataFunction, WS, ITEM).unwrap(),
            "exact",
        )
        .unwrap();
        let plan = RunPreflight::validate(
            target,
            &caps(RunTargetKind::Udf),
            RunOptions {
                wait: true,
                timeout: None,
                format: RunFormat::Json,
            },
            RunInput::Json(json!({})),
            &Sink::default(),
            |_, _| Ok(()),
        )
        .unwrap();
        let mut session = RunSession::new(&client, plan, clock, pending()).unwrap();
        session.record(RunOutcome::Completed, Some(output.clone()));
        session.correlate("correlation-not-a-job").unwrap();
        let sink = Sink::default();
        sink.exists.store(true, Ordering::SeqCst);
        let failure = session.commit(&sink, b"native-result").await.unwrap_err();
        assert_eq!(failure.source, RunError::Output);
        assert_eq!(failure.evidence.outcome, RunOutcome::Completed);
        assert_eq!(failure.evidence.result, Some(output));
        assert_eq!(
            failure.evidence.correlation_id.as_deref(),
            Some("correlation-not-a-job")
        );
        assert!(failure.evidence.handle.is_none());
        assert!(executor.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn stream_failure_preserves_actual_acceptance_headers_and_incomplete_prefix() {
    let clock = Arc::new(Clock::default());
    let executor = Executor::new(
        clock.clone(),
        [Step {
            response: Err(JobExecutionError::Failed),
            elapsed: 0,
        }],
    );
    *executor.partial.lock().unwrap() = Some(JobResponse {
        status: 202,
        headers: [("Location".into(), monitor())].into_iter().collect(),
        body: b"partial-native-prefix".to_vec(),
    });
    let client = client(Arc::new(Credential::default()), &executor);
    let mut session = RunSession::new(&client, plan("5m"), clock, pending()).unwrap();
    let failure = session.submit(submit_url(), vec![]).await.unwrap_err();
    assert_eq!(failure.source, RunError::Transport);
    assert_eq!(failure.evidence.outcome, RunOutcome::Accepted);
    assert_eq!(
        failure.evidence.handle.unwrap().kind(),
        &RunHandleKind::Job(ID.into())
    );
    assert_eq!(
        failure.evidence.initial_response.unwrap().body,
        b"partial-native-prefix"
    );
    assert!(!failure.evidence.initial_response_complete);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn oversized_response_omission_survives_winning_deadline() {
    let clock = Arc::new(Clock::default());
    let mut step = response(202, "too-large", &[("Location", &monitor())]);
    step.elapsed = 1_000;
    let executor = Executor::new(clock.clone(), [step]);
    let client = JobClient::new(
        Arc::new(Credential::default()),
        CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
        Arc::new(executor),
        JobLimits {
            max_response_bytes: NonZeroUsize::new(2).unwrap(),
            ..JobLimits::default()
        },
    );
    let mut session = RunSession::new(&client, plan("1s"), clock, pending()).unwrap();
    let failure = session.submit(submit_url(), vec![]).await.unwrap_err();
    assert_eq!(failure.source, RunError::Deadline);
    assert!(failure.evidence.response_body_omitted);
    assert!(!failure.evidence.initial_response_complete);
    assert!(failure.evidence.initial_response.unwrap().body.is_empty());
    assert!(failure.evidence.handle.is_some());
}

#[tokio::test]
async fn interruption_is_latched_for_repeated_phases_without_repoll_panic() {
    let clock = Arc::new(Clock::default());
    let executor = Executor::new(clock.clone(), []);
    let client = client(Arc::new(Credential::default()), &executor);
    let mut session = RunSession::new(&client, plan("5m"), clock, ready(())).unwrap();
    for _ in 0..3 {
        assert_eq!(
            session.phase(async { Ok(()) }).await.unwrap_err().source,
            RunError::Interrupted
        );
    }
    assert!(executor.requests.lock().unwrap().is_empty());
}

struct LateSink {
    sink: Sink,
    clock: Arc<Clock>,
}
impl RunOutput for LateSink {
    fn preflight(&self, format: RunFormat) -> Result<(), RunError> {
        self.sink.preflight(format)
    }
    fn commit<'a>(&'a self, bytes: &'a [u8]) -> JobFuture<'a, Result<(), RunError>> {
        Box::pin(async move {
            self.sink.commit(bytes).await?;
            self.clock.advance(1_000);
            Ok(())
        })
    }
}

#[tokio::test]
async fn output_committed_fact_survives_deadline_verdict_and_no_clobber() {
    let clock = Arc::new(Clock::default());
    let executor = Executor::new(clock.clone(), []);
    let client = client(Arc::new(Credential::default()), &executor);
    let sink = LateSink {
        sink: Sink::default(),
        clock: clock.clone(),
    };
    let mut session = RunSession::new(&client, plan("1s"), clock, pending()).unwrap();
    session.record(RunOutcome::Completed, Some(json!(false)));
    let failure = session.commit(&sink, b"original-result").await.unwrap_err();
    assert_eq!(failure.source, RunError::Deadline);
    assert!(failure.evidence.output_committed);
    assert_eq!(failure.evidence.result, Some(json!(false)));
    assert_eq!(
        sink.sink.bytes.lock().unwrap().as_deref(),
        Some(b"original-result".as_slice())
    );
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn provider_minimum_above_legacy_delay_cap_is_honored_in_shared_driver() {
    let clock = Arc::new(Clock::default());
    clock
        .advance_on_sleep
        .lock()
        .unwrap()
        .extend([Duration::from_secs(120), Duration::from_secs(61)]);
    let executor = Executor::new(
        clock.clone(),
        [
            response(202, "", &[("Location", &monitor()), ("Retry-After", "120")]),
            response(200, r#"{"status":"InProgress"}"#, &[("Retry-After", "61")]),
            response(200, r#"{"status":"Completed"}"#, &[]),
        ],
    );
    let client = client(Arc::new(Credential::default()), &executor);
    let mut session = RunSession::new(&client, plan("5m"), clock.clone(), pending()).unwrap();
    session.submit(submit_url(), vec![]).await.unwrap();
    let handle = RunHandle::job(&target(), client.context().endpoint(), &monitor()).unwrap();
    assert_eq!(
        session.observe(&handle, &SchedulerObserver).await.unwrap(),
        RunOutcome::Completed
    );
    assert_eq!(
        *clock.observed_sleeps.lock().unwrap(),
        [Duration::from_secs(120), Duration::from_secs(61)]
    );
    assert_eq!(
        *executor.request_times.lock().unwrap(),
        [
            Duration::ZERO,
            Duration::from_secs(120),
            Duration::from_secs(181)
        ]
    );
    assert_eq!(
        executor
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.method)
            .collect::<Vec<_>>(),
        [Method::Post, Method::Get, Method::Get]
    );
}

#[tokio::test]
async fn remote_resolution_and_exchange_over_240_seconds_share_explicit_10m_budget() {
    let clock = Arc::new(Clock::default());
    let mut step = response(202, "", &[("Location", &monitor())]);
    step.elapsed = 241_000;
    let executor = Executor::new(clock.clone(), [step]);
    let credential = Arc::new(Credential {
        clock: Some(clock.clone()),
        delay: 59_000,
        ..Credential::default()
    });
    let client = client(credential, &executor);
    let mut session = RunSession::new(&client, plan("10m"), clock.clone(), pending()).unwrap();
    session
        .phase(async {
            clock.advance(60_000);
            Ok(())
        })
        .await
        .unwrap();
    session.submit(submit_url(), vec![]).await.unwrap();
    assert_eq!(session.remaining().unwrap(), Duration::from_secs(240));
    assert_eq!(
        *executor.budgets.lock().unwrap(),
        [Duration::from_secs(540), Duration::from_secs(481)]
    );
    assert_eq!(session.evidence().outcome, RunOutcome::Accepted);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn verdict_interpretation_consumes_budget_and_retains_already_decoded_result() {
    let clock = Arc::new(Clock::default());
    let executor = Executor::new(clock.clone(), []);
    let client = client(Arc::new(Credential::default()), &executor);
    let mut session = RunSession::new(&client, plan("1s"), clock.clone(), pending()).unwrap();
    let response = JobResponse {
        status: 200,
        body: vec![],
        headers: Default::default(),
    };
    let result = session
        .interpret(&response, |_| {
            clock.advance(1_000);
            Ok(fabric_sdk_jobs::run::RunObservation {
                outcome: RunOutcome::Completed,
                terminal: true,
                result: Some(json!(0)),
            })
        })
        .await;
    let failure = result.err().unwrap();
    assert_eq!(failure.source, RunError::Deadline);
    assert_eq!(failure.evidence.outcome, RunOutcome::Completed);
    assert_eq!(failure.evidence.result, Some(json!(0)));
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[derive(Clone)]
struct PendingStreamExecutor {
    clock: Arc<Clock>,
    interrupt: Arc<AtomicBool>,
    deadline_case: bool,
    requests: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
}
struct DropProbe(Arc<AtomicBool>);
impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl JobExecutor for PendingStreamExecutor {
    fn with_run_budget(&self, _: Duration) -> Result<Arc<dyn JobExecutor>, JobExecutionError> {
        Ok(Arc::new(self.clone()))
    }
    fn execute<'a>(
        &'a self,
        _: JobRequest<'a>,
    ) -> JobFuture<'a, Result<JobResponse, JobExecutionError>> {
        Box::pin(pending())
    }
    fn execute_run<'a>(
        &'a self,
        _: JobRequest<'a>,
        recorder: &'a fabric_sdk_jobs::run::RunResponseRecorder,
    ) -> JobFuture<'a, RunExchange> {
        Box::pin(async move {
            let _drop = DropProbe(self.dropped.clone());
            self.requests.fetch_add(1, Ordering::SeqCst);
            recorder
                .record(
                    &JobResponse {
                        status: 202,
                        body: b"observed-prefix".to_vec(),
                        headers: [("Location".into(), monitor())].into_iter().collect(),
                    },
                    false,
                )
                .unwrap();
            if self.deadline_case {
                self.clock.advance(1_000);
            } else {
                self.interrupt.store(true, Ordering::SeqCst);
            }
            poll_fn(|cx| {
                cx.waker().wake_by_ref();
                Poll::<RunExchange>::Pending
            })
            .await
        })
    }
    fn wait(&self, _: Duration) -> JobFuture<'_, Result<(), JobExecutionError>> {
        Box::pin(pending())
    }
}

#[tokio::test]
async fn losing_pending_stream_is_dropped_without_discarding_observed_acceptance() {
    for deadline_case in [false, true] {
        let clock = Arc::new(Clock::default());
        let interrupt = Arc::new(AtomicBool::new(false));
        let executor = PendingStreamExecutor {
            clock: clock.clone(),
            interrupt: interrupt.clone(),
            deadline_case,
            requests: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicBool::new(false)),
        };
        let client = JobClient::new(
            Arc::new(Credential::default()),
            CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
            Arc::new(executor.clone()),
            JobLimits::default(),
        );
        let cancelled = poll_fn(move |_| {
            if interrupt.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let mut session = RunSession::new(&client, plan("1s"), clock, cancelled).unwrap();
        let failure = session.submit(submit_url(), vec![]).await.unwrap_err();
        assert_eq!(
            failure.source,
            if deadline_case {
                RunError::Deadline
            } else {
                RunError::Interrupted
            }
        );
        assert_eq!(failure.evidence.outcome, RunOutcome::Accepted);
        assert_eq!(
            failure.evidence.initial_response.unwrap().body,
            b"observed-prefix"
        );
        assert!(!failure.evidence.initial_response_complete);
        assert_eq!(
            failure.evidence.handle.unwrap().kind(),
            &RunHandleKind::Job(ID.into())
        );
        assert_eq!(executor.requests.load(Ordering::SeqCst), 1);
        assert!(executor.dropped.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn dropping_submit_future_preserves_dispatch_and_recorded_acceptance() {
    let clock = Arc::new(Clock::default());
    let executor = PendingStreamExecutor {
        clock: clock.clone(),
        interrupt: Arc::new(AtomicBool::new(false)),
        deadline_case: false,
        requests: Arc::new(AtomicUsize::new(0)),
        dropped: Arc::new(AtomicBool::new(false)),
    };
    let client = JobClient::new(
        Arc::new(Credential::default()),
        CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
        Arc::new(executor.clone()),
        JobLimits::default(),
    );
    let mut session = RunSession::new(&client, plan("5m"), clock, pending()).unwrap();
    {
        let mut submitted = Box::pin(session.submit(submit_url(), vec![]));
        poll_fn(|cx| {
            assert!(submitted.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    assert!(executor.dropped.load(Ordering::SeqCst));
    assert_eq!(executor.requests.load(Ordering::SeqCst), 1);
    assert!(session.evidence().mutation_may_have_occurred);
    assert_eq!(session.evidence().outcome, RunOutcome::Accepted);
    assert_eq!(
        session.evidence().initial_response.as_ref().unwrap().body,
        b"observed-prefix"
    );
    assert!(!session.evidence().initial_response_complete);
    assert_eq!(
        session.evidence().handle.as_ref().unwrap().kind(),
        &RunHandleKind::Job(ID.into())
    );
    let failure = session.submit(submit_url(), vec![]).await.unwrap_err();
    assert_eq!(failure.source, RunError::Replay);
    assert!(failure.evidence.mutation_may_have_occurred);
    assert_eq!(failure.evidence.outcome, RunOutcome::Accepted);
    assert_eq!(executor.requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn established_handle_cannot_be_replaced_by_another_run_on_the_same_owner() {
    let clock = Arc::new(Clock::default());
    let executor = Executor::new(
        clock.clone(),
        [
            response(
                202,
                "original-acceptance",
                &[("Location", &monitor()), ("Retry-After", "0")],
            ),
            response(200, r#"{"status":"Completed"}"#, &[]),
        ],
    );
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone(), &executor);
    let mut session = RunSession::new(&client, plan("5m"), clock, pending()).unwrap();
    session.submit(submit_url(), vec![]).await.unwrap();
    let original = RunHandle::job(&target(), client.context().endpoint(), &monitor()).unwrap();
    let other = RunHandle::job(
        &target(),
        client.context().endpoint(),
        &monitor().replace(ID, "44444444-4444-4444-4444-444444444444"),
    )
    .unwrap();
    session.bind_handle(&original).unwrap();
    session.bind_handle(&original).unwrap();
    let failure = session
        .observe(&other, &SchedulerObserver)
        .await
        .unwrap_err();
    assert_eq!(failure.source, RunError::Context);
    assert_eq!(failure.evidence.handle.as_ref(), Some(&original));
    assert_eq!(failure.evidence.outcome, RunOutcome::Accepted);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
    assert_eq!(
        session
            .observe(&original, &SchedulerObserver)
            .await
            .unwrap(),
        RunOutcome::Completed
    );
    assert_eq!(executor.requests.lock().unwrap().len(), 2);

    let clock = Arc::new(Clock::default());
    let mut imported = RunSession::new(&client, plan("5m"), clock, pending()).unwrap();
    imported.bind_handle(&other).unwrap();
    imported.bind_handle(&other).unwrap();
    assert_eq!(imported.evidence().handle.as_ref(), Some(&other));
    assert_eq!(
        imported.bind_handle(&original).unwrap_err().source,
        RunError::Context
    );
    assert_eq!(
        imported
            .submit(submit_url(), vec![])
            .await
            .unwrap_err()
            .source,
        RunError::Replay
    );
    assert!(!imported.evidence().mutation_may_have_occurred);
}

#[tokio::test]
async fn dropping_submit_during_credentials_retains_zero_dispatch_evidence() {
    let clock = Arc::new(Clock::default());
    let executor = Executor::new(clock.clone(), []);
    let credential = Arc::new(Credential {
        pending: true,
        ..Credential::default()
    });
    let client = client(credential.clone(), &executor);
    let mut session = RunSession::new(&client, plan("5m"), clock, pending()).unwrap();
    {
        let mut submitted = Box::pin(session.submit(submit_url(), vec![]));
        poll_fn(|cx| {
            assert!(submitted.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    assert!(!session.evidence().mutation_may_have_occurred);
    assert_eq!(session.evidence().outcome, RunOutcome::NotSubmitted);
    assert!(session.evidence().initial_response.is_none());
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert!(executor.requests.lock().unwrap().is_empty());
    assert_eq!(
        session
            .submit(submit_url(), vec![])
            .await
            .unwrap_err()
            .source,
        RunError::Replay
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
}
