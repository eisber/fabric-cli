// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline root/facade composition, with no production adapter or live identity.
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli_jobs::{
    JsonInput,
    v2_run::{self, RunAdapter, RunDispatchError},
};
use fabric_cli_output::CommandError;
use fabric_sdk::v2::{Availability, Registry, Target, TargetKind, Verb};
use fabric_sdk_jobs::{
    Audience, CredentialSelection, JobClient, JobExecutionError, JobExecutor, JobFuture, JobLimits,
    JobRequest, JobResponse, TokenCredential,
    run::{
        RunCapabilities, RunClock, RunError, RunFailure, RunFormat, RunHandle, RunInput,
        RunInputRule, RunOutcome, RunOutput, RunOwner, RunOwnerKind, RunSession, RunTarget,
        RunTargetKind, SchedulerObserver,
    },
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{pending, poll_fn},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const RUN: &str = "33333333-3333-3333-3333-333333333333";

fn target(kind: RunTargetKind) -> RunTarget {
    let owner = RunOwner::new(
        match kind {
            RunTargetKind::Udf => RunOwnerKind::UserDataFunction,
            RunTargetKind::LivyStatement | RunTargetKind::LivyOneShot => RunOwnerKind::Lakehouse,
            _ => RunOwnerKind::Notebook,
        },
        WS,
        ITEM,
    )
    .unwrap();
    match kind {
        RunTargetKind::Udf => RunTarget::udf(owner, "calculate").unwrap(),
        RunTargetKind::LivyStatement => RunTarget::livy(owner, 0).unwrap(),
        _ => RunTarget::item(owner, kind).unwrap(),
    }
}

struct Files {
    value: Value,
    reads: AtomicUsize,
}
impl Files {
    fn new(value: Value) -> Self {
        Self {
            value,
            reads: AtomicUsize::new(0),
        }
    }
}
impl JsonInput for Files {
    fn read_json(&self, path: &str) -> Result<Value, CommandError> {
        assert!(matches!(path, "-" | "input.json"));
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.value.clone())
    }
}

#[derive(Default)]
struct Sink {
    exists: AtomicBool,
    bytes: Mutex<Vec<u8>>,
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
            *self.bytes.lock().unwrap() = bytes.to_vec();
            Ok(())
        })
    }
}

#[derive(Default, Debug)]
struct Clock(AtomicU64);
impl RunClock for Clock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
    fn sleep(&self, delay: Duration) -> JobFuture<'_, ()> {
        // Only observation sleeps advance the fake clock, not the guard timer.
        Box::pin(poll_fn(move |_| {
            if delay <= Duration::from_secs(5) {
                self.0
                    .fetch_add(u64::try_from(delay.as_millis()).unwrap(), Ordering::SeqCst);
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }))
    }
}

#[derive(Default, Debug)]
struct Credential(AtomicUsize);
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new("offline-only", OffsetDateTime::UNIX_EPOCH))
    }
}

#[derive(Clone, Default)]
struct Executor {
    responses: Arc<Mutex<VecDeque<Result<JobResponse, JobExecutionError>>>>,
    requests: Arc<Mutex<Vec<Request>>>,
    budgets: Arc<Mutex<Vec<Duration>>>,
}
struct Request {
    method: String,
    url: String,
    body: Vec<u8>,
}
impl Executor {
    fn scripted(responses: Vec<Result<JobResponse, JobExecutionError>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
            ..Self::default()
        }
    }
}
impl JobExecutor for Executor {
    fn with_run_budget(
        &self,
        remaining: Duration,
    ) -> Result<Arc<dyn JobExecutor>, JobExecutionError> {
        self.budgets.lock().unwrap().push(remaining);
        Ok(Arc::new(self.clone()))
    }
    fn execute<'a>(
        &'a self,
        request: JobRequest<'a>,
    ) -> JobFuture<'a, Result<JobResponse, JobExecutionError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(Request {
                method: request.method().to_string(),
                url: request.url().to_string(),
                body: request.body().to_vec(),
            });
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unplanned request/replay")
        })
    }
    fn wait(&self, _: Duration) -> JobFuture<'_, Result<(), JobExecutionError>> {
        panic!("no second poller")
    }
}

fn client(credential: Arc<Credential>, executor: &Executor) -> JobClient {
    JobClient::new(
        credential,
        CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
        Arc::new(executor.clone()),
        JobLimits::default(),
    )
}
fn location() -> String {
    format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/items/{ITEM}/jobs/instances/{RUN}")
}
fn accepted(location: &str, delay: &str) -> JobResponse {
    JobResponse {
        status: 202,
        body: Vec::new(),
        headers: BTreeMap::from([
            ("Location".into(), location.into()),
            ("Retry-After".into(), delay.into()),
        ]),
    }
}
fn status(state: &str) -> JobResponse {
    JobResponse {
        status: 200,
        body: serde_json::to_vec(&json!({"id":RUN,"itemId":ITEM,"status":state})).unwrap(),
        headers: BTreeMap::new(),
    }
}
fn failure(session: &RunSession<'_>, source: RunError) -> Box<RunFailure> {
    Box::new(RunFailure {
        source,
        evidence: session.evidence().clone(),
    })
}

/// Test-only executable Notebook binding; not exported/installed in production.
struct Notebook {
    kind: RunTargetKind,
}
impl Notebook {
    fn new() -> Self {
        Self {
            kind: RunTargetKind::Notebook,
        }
    }
}
impl RunAdapter for Notebook {
    fn capabilities(&self) -> RunCapabilities {
        RunCapabilities {
            target: self.kind,
            input: if self.kind == RunTargetKind::Notebook {
                RunInputRule::OptionalObject
            } else {
                RunInputRule::RequiredObject
            },
            wait: true,
            formats: vec![RunFormat::Json],
        }
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"parameters":{"type":"array"}}})
    }
    fn validate(&self, _: &RunTarget, _: &RunInput) -> Result<(), RunError> {
        Ok(())
    }
    fn execute<'a>(
        &'a self,
        session: &'a mut RunSession<'_>,
    ) -> JobFuture<'a, Result<(), Box<RunFailure>>> {
        Box::pin(async move {
            let body = match session.plan().input() {
                RunInput::Omitted => Vec::new(),
                RunInput::Json(value) => serde_json::to_vec(value).unwrap(),
            };
            let url = session
                .evidence()
                .endpoint
                .url(&format!(
                    "/v1/workspaces/{WS}/notebooks/{ITEM}/jobs/execute/instances?beta=false",
                ))
                .unwrap();
            let response = session.submit(url, body).await?;
            if response.status != 202 {
                return Err(failure(session, RunError::Protocol));
            }
            let handle = RunHandle::job(
                session.plan().target(),
                &session.evidence().endpoint,
                response
                    .headers
                    .get("Location")
                    .ok_or_else(|| failure(session, RunError::Protocol))?,
            )
            .map_err(|error| failure(session, error))?;
            session.bind_handle(&handle)?;
            if session.plan().wait() {
                session.observe(&handle, &SchedulerObserver).await?;
            }
            Ok(())
        })
    }
}

#[test]
fn common_and_facade_normalize_identically_without_echoing_arguments() {
    for (facade, kind) in [
        ("notebook", RunTargetKind::Notebook),
        ("user-data-function", RunTargetKind::Udf),
        ("lakehouse", RunTargetKind::LivyStatement),
    ] {
        let root = v2_run::parse_args([
            "fab-cli",
            "run",
            "secret-path",
            "--input",
            "-",
            "--wait",
            "--timeout",
            "10m",
        ])
        .unwrap();
        let alias = v2_run::parse_args([
            "fab-cli",
            "artifact",
            facade,
            "run",
            "secret-path",
            "--input",
            "-",
            "--wait",
            "--timeout",
            "10m",
        ])
        .unwrap();
        assert_eq!(root.path, alias.path);
        assert_eq!(root.input, alias.input);
        assert_eq!(root.wait, alias.wait);
        assert_eq!(root.timeout, alias.timeout);
        assert!(!format!("{root:?}").contains("secret-path"));
        let input = match kind {
            RunTargetKind::LivyStatement => json!({"code":"print(1)","language":"pyspark"}),
            _ => json!({}),
        };
        let files = Files::new(input);
        let output = Sink::default();
        let adapter = Notebook { kind };
        let plan = v2_run::prepare(&alias, target(kind), Some(&adapter), &files, &output).unwrap();
        assert_eq!(plan.plan().timeout().duration(), Duration::from_secs(600));
        assert_eq!(files.reads.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn durations_and_unsupported_grammar_fail_before_io() {
    for duration in ["0s", "-1s", "1.5s", "30", "25h", "18446744073709551615h"] {
        assert!(v2_run::parse_args(["fab-cli", "run", "N", "--timeout", duration]).is_err());
    }
    for duration in ["1ms", "1s", "1m", "1h", "24h"] {
        assert!(v2_run::parse_args(["fab-cli", "run", "N", "--timeout", duration]).is_ok());
    }
    for args in [
        vec!["fab-cli", "invoke", "N"],
        vec!["fab-cli", "wait", "N"],
        vec!["fab-cli", "run", "N", "--format", "arrow"],
        vec!["fab-cli", "run", "N", "--check-exit"],
        vec!["fab-cli", "run", "N", "--url", "https://invalid.example"],
        vec!["fab-cli", "run", ""],
    ] {
        assert!(v2_run::parse_args(args).is_err());
    }
}

#[test]
fn unavailable_cell_oneshot_facade_and_beta_fail_without_input_reads() {
    let files = Files::new(json!({}));
    let output = Sink::default();
    let adapter = Notebook::new();
    let root = v2_run::parse_args(["fab-cli", "run", "N", "--input", "input.json"]).unwrap();
    for kind in [RunTargetKind::NotebookCell, RunTargetKind::LivyOneShot] {
        assert!(matches!(
            v2_run::prepare(&root, target(kind), Some(&adapter), &files, &output),
            Err(RunError::UnsupportedCapability)
        ));
    }
    assert!(matches!(
        v2_run::prepare(
            &root,
            target(RunTargetKind::Notebook),
            None,
            &files,
            &output
        ),
        Err(RunError::UnsupportedCapability)
    ));
    let wrong = v2_run::parse_args([
        "fab-cli",
        "artifact",
        "user-data-function",
        "run",
        "N",
        "--input",
        "input.json",
    ])
    .unwrap();
    assert!(
        v2_run::prepare(
            &wrong,
            target(RunTargetKind::Notebook),
            Some(&adapter),
            &files,
            &output
        )
        .is_err()
    );
    let beta = v2_run::parse_args([
        "fab-cli",
        "run",
        "N",
        "--wait",
        "--check-exit",
        "--input",
        "input.json",
    ])
    .unwrap();
    assert!(matches!(
        v2_run::prepare(
            &beta,
            target(RunTargetKind::Notebook),
            Some(&adapter),
            &files,
            &output
        ),
        Err(RunError::UnsupportedCapability)
    ));
    assert_eq!(files.reads.load(Ordering::SeqCst), 0);
}

#[test]
fn native_validation_defaults_and_output_preflight_are_shared() {
    for (kind, wait, seconds) in [
        (RunTargetKind::Notebook, false, 30),
        (RunTargetKind::Notebook, true, 300),
        (RunTargetKind::Udf, false, 120),
        (RunTargetKind::Udf, true, 120),
        (RunTargetKind::LivyStatement, false, 30),
        (RunTargetKind::LivyStatement, true, 300),
    ] {
        let mut args = vec!["fab-cli", "run", "target", "--input", "-"];
        if wait {
            args.push("--wait");
        }
        let command = v2_run::parse_args(args).unwrap();
        let input = if kind == RunTargetKind::LivyStatement {
            json!({"code":"print(1)","language":"pyspark"})
        } else {
            json!({})
        };
        let files = Files::new(input);
        let sink = Sink::default();
        let adapter = Notebook { kind };
        let prepared =
            v2_run::prepare(&command, target(kind), Some(&adapter), &files, &sink).unwrap();
        assert_eq!(
            prepared.plan().timeout().duration(),
            Duration::from_secs(seconds)
        );
    }
    let command = v2_run::parse_args(["fab-cli", "run", "N", "--input", "-"]).unwrap();
    let adapter = Notebook::new();
    let output = Sink::default();
    for input in [
        Value::Null,
        json!([]),
        json!({"parameters":{"x":{"type":"int","value":2}}}),
    ] {
        assert!(matches!(
            v2_run::prepare(
                &command,
                target(RunTargetKind::Notebook),
                Some(&adapter),
                &Files::new(input),
                &output
            ),
            Err(RunError::InvalidInput)
        ));
    }
    output.exists.store(true, Ordering::SeqCst);
    assert!(matches!(
        v2_run::prepare(
            &command,
            target(RunTargetKind::Notebook),
            Some(&adapter),
            &Files::new(json!({})),
            &output
        ),
        Err(RunError::Output)
    ));
}

#[test]
fn registry_availability_alias_identity_schema_and_atomic_conflicts() {
    let mut registry = Registry::default();
    v2_run::register(&mut registry, &[]).unwrap();
    let selected = Target {
        kind: TargetKind::Item,
        owner_type: "Notebook".into(),
        facet: "item".into(),
        representation: "json".into(),
        generation: None,
    };
    let root = registry.resolve(Verb::Run, &selected, None).unwrap();
    let alias = registry
        .resolve(Verb::Run, &selected, Some("notebook"))
        .unwrap();
    assert_eq!(root.operation.id(), alias.operation.id());
    assert!(matches!(
        root.binding.availability(),
        Availability::Unsupported(_)
    ));
    assert!(
        registry
            .resolve(Verb::Run, &selected, Some("lakehouse"))
            .is_err()
    );
    let before = serde_json::to_value(registry.bindings()).unwrap();
    assert!(v2_run::register(&mut registry, &[]).is_err());
    assert_eq!(serde_json::to_value(registry.bindings()).unwrap(), before);
    let adapter = Notebook::new();
    let mut configured = Registry::default();
    v2_run::register(&mut configured, &[&adapter]).unwrap();
    let resolution = configured.resolve(Verb::Run, &selected, None).unwrap();
    assert!(matches!(
        resolution.binding.availability(),
        Availability::Conditional(_)
    ));
    assert_eq!(resolution.operation.input_schema(), &adapter.input_schema());
    let mut ambiguous = Registry::default();
    assert!(v2_run::register(&mut ambiguous, &[&adapter, &adapter]).is_err());
    assert_eq!(ambiguous.operations().count(), 0);
}

#[tokio::test]
async fn accepted_root_and_facade_issue_one_native_post_and_no_status_get() {
    for args in [
        vec!["fab-cli", "run", "N"],
        vec!["fab-cli", "artifact", "notebook", "run", "N"],
        vec!["fab-cli", "run", "N", "--input", "-"],
    ] {
        let executor = Executor::scripted(vec![Ok(accepted(&location(), "60"))]);
        let credential = Arc::new(Credential::default());
        let sdk = client(credential.clone(), &executor);
        let files = Files::new(json!({}));
        let output = Sink::default();
        let adapter = Notebook::new();
        let command = v2_run::parse_args(args).unwrap();
        let prepared = v2_run::prepare(
            &command,
            target(RunTargetKind::Notebook),
            Some(&adapter),
            &files,
            &output,
        )
        .unwrap();
        let evidence = v2_run::dispatch(prepared, &sdk, Arc::new(Clock::default()), pending())
            .await
            .unwrap();
        assert_eq!(evidence.outcome, RunOutcome::Accepted);
        assert!(evidence.output_committed);
        let receipt: Value = serde_json::from_slice(&output.bytes.lock().unwrap()).unwrap();
        assert_eq!(receipt["status"], "accepted");
        assert_eq!(receipt["meta"]["handle"], location());
        let requests = executor.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert!(
            requests[0]
                .url
                .ends_with("/jobs/execute/instances?beta=false")
        );
        assert_eq!(
            requests[0].body,
            if command.input.is_some() {
                b"{}".to_vec()
            } else {
                Vec::new()
            }
        );
        assert_eq!(credential.0.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn waiting_uses_shared_observer_and_workload_failures_are_non_success() {
    for state in ["Completed", "Failed", "Cancelled", "Deduped", "FutureState"] {
        let executor = Executor::scripted(vec![Ok(accepted(&location(), "0")), Ok(status(state))]);
        let sdk = client(Arc::new(Credential::default()), &executor);
        let command = v2_run::parse_args(["fab-cli", "run", "N", "--wait"]).unwrap();
        let files = Files::new(json!({}));
        let output = Sink::default();
        let adapter = Notebook::new();
        let prepared = v2_run::prepare(
            &command,
            target(RunTargetKind::Notebook),
            Some(&adapter),
            &files,
            &output,
        )
        .unwrap();
        let result = v2_run::dispatch(prepared, &sdk, Arc::new(Clock::default()), pending()).await;
        if state == "Completed" {
            assert_eq!(result.unwrap().outcome, RunOutcome::Completed);
        } else {
            let Err(RunDispatchError::Unsuccessful { evidence }) = result else {
                panic!("expected unsuccessful workload")
            };
            assert!(evidence.handle.is_some());
            assert!(output.bytes.lock().unwrap().is_empty());
        }
        let requests = executor.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].method, "GET");
        assert_eq!(requests[1].url, location());
    }
}

#[tokio::test]
async fn observation_denial_response_loss_and_unsafe_location_never_replay() {
    for responses in [
        vec![Err(JobExecutionError::Failed)],
        vec![Ok(accepted("https://untrusted.example/run", "0"))],
        vec![
            Ok(accepted(&location(), "0")),
            Ok(JobResponse {
                status: 403,
                body: Vec::new(),
                headers: BTreeMap::new(),
            }),
        ],
    ] {
        let executor = Executor::scripted(responses);
        let sdk = client(Arc::new(Credential::default()), &executor);
        let command = v2_run::parse_args(["fab-cli", "run", "N", "--wait"]).unwrap();
        let files = Files::new(json!({}));
        let output = Sink::default();
        let adapter = Notebook::new();
        let prepared = v2_run::prepare(
            &command,
            target(RunTargetKind::Notebook),
            Some(&adapter),
            &files,
            &output,
        )
        .unwrap();
        let Err(RunDispatchError::Execution(error)) =
            v2_run::dispatch(prepared, &sdk, Arc::new(Clock::default()), pending()).await
        else {
            panic!("expected retained failure")
        };
        assert!(error.evidence.mutation_may_have_occurred);
        let requests = executor.requests.lock().unwrap();
        assert_eq!(requests.iter().filter(|r| r.method == "POST").count(), 1);
        assert!(
            requests
                .iter()
                .all(|r| !r.url.contains("/cancel") && r.method != "DELETE")
        );
    }
}

#[tokio::test]
async fn service_delay_beyond_deadline_retains_acceptance_without_early_poll() {
    let executor = Executor::scripted(vec![Ok(accepted(&location(), "301"))]);
    let sdk = client(Arc::new(Credential::default()), &executor);
    let command = v2_run::parse_args(["fab-cli", "run", "N", "--wait"]).unwrap();
    let files = Files::new(json!({}));
    let output = Sink::default();
    let adapter = Notebook::new();
    let prepared = v2_run::prepare(
        &command,
        target(RunTargetKind::Notebook),
        Some(&adapter),
        &files,
        &output,
    )
    .unwrap();
    let Err(RunDispatchError::Execution(error)) =
        v2_run::dispatch(prepared, &sdk, Arc::new(Clock::default()), pending()).await
    else {
        panic!("expected timeout")
    };
    assert_eq!(error.source, RunError::Deadline);
    assert_eq!(error.evidence.outcome, RunOutcome::Accepted);
    assert!(error.evidence.handle.is_some());
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancellation_before_send_has_zero_credentials_and_requests() {
    let executor = Executor::default();
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone(), &executor);
    let command = v2_run::parse_args(["fab-cli", "run", "N"]).unwrap();
    let files = Files::new(json!({}));
    let output = Sink::default();
    let adapter = Notebook::new();
    let prepared = v2_run::prepare(
        &command,
        target(RunTargetKind::Notebook),
        Some(&adapter),
        &files,
        &output,
    )
    .unwrap();
    assert!(
        v2_run::dispatch(prepared, &sdk, Arc::new(Clock::default()), async {})
            .await
            .is_err()
    );
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn output_race_preserves_accepted_handle_and_never_resubmits() {
    let executor = Executor::scripted(vec![Ok(accepted(&location(), "60"))]);
    let sdk = client(Arc::new(Credential::default()), &executor);
    let command = v2_run::parse_args(["fab-cli", "run", "N"]).unwrap();
    let files = Files::new(json!({}));
    let output = Sink::default();
    let adapter = Notebook::new();
    let prepared = v2_run::prepare(
        &command,
        target(RunTargetKind::Notebook),
        Some(&adapter),
        &files,
        &output,
    )
    .unwrap();
    output.exists.store(true, Ordering::SeqCst);
    let Err(RunDispatchError::Execution(error)) =
        v2_run::dispatch(prepared, &sdk, Arc::new(Clock::default()), pending()).await
    else {
        panic!("expected output collision")
    };
    assert_eq!(error.source, RunError::Output);
    assert_eq!(error.evidence.outcome, RunOutcome::Accepted);
    assert!(error.evidence.handle.is_some());
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

struct OverflowClock;
impl RunClock for OverflowClock {
    fn now(&self) -> Duration {
        Duration::MAX
    }
    fn sleep(&self, _: Duration) -> JobFuture<'_, ()> {
        Box::pin(pending())
    }
}

#[tokio::test]
async fn clock_overflow_returns_startup_error_without_panicking_or_sending() {
    let executor = Executor::default();
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone(), &executor);
    let command = v2_run::parse_args(["fab-cli", "run", "N"]).unwrap();
    let files = Files::new(json!({}));
    let output = Sink::default();
    let adapter = Notebook::new();
    let prepared = v2_run::prepare(
        &command,
        target(RunTargetKind::Notebook),
        Some(&adapter),
        &files,
        &output,
    )
    .unwrap();
    assert!(matches!(
        v2_run::dispatch(prepared, &sdk, Arc::new(OverflowClock), pending()).await,
        Err(RunDispatchError::Start(RunError::InvalidTimeout))
    ));
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(executor.requests.lock().unwrap().is_empty());
}

struct NoExecution;
impl RunAdapter for NoExecution {
    fn capabilities(&self) -> RunCapabilities {
        Notebook::new().capabilities()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object"})
    }
    fn validate(&self, _: &RunTarget, _: &RunInput) -> Result<(), RunError> {
        Ok(())
    }
    fn execute<'a>(
        &'a self,
        session: &'a mut RunSession<'_>,
    ) -> JobFuture<'a, Result<(), Box<RunFailure>>> {
        Box::pin(async move {
            session.record(RunOutcome::Completed, Some(json!({"not":"executed"})));
            Ok(())
        })
    }
}

#[tokio::test]
async fn an_adapter_cannot_claim_success_without_actual_submission_evidence() {
    let executor = Executor::default();
    let sdk = client(Arc::new(Credential::default()), &executor);
    let command = v2_run::parse_args(["fab-cli", "run", "N"]).unwrap();
    let files = Files::new(json!({}));
    let output = Sink::default();
    let prepared = v2_run::prepare(
        &command,
        target(RunTargetKind::Notebook),
        Some(&NoExecution),
        &files,
        &output,
    )
    .unwrap();
    let result = v2_run::dispatch(prepared, &sdk, Arc::new(Clock::default()), pending()).await;
    assert!(matches!(
        result,
        Err(RunDispatchError::Execution(error)) if error.source == RunError::Protocol
    ));
    assert!(executor.requests.lock().unwrap().is_empty());
    assert!(output.bytes.lock().unwrap().is_empty());
}

struct FalseCompletion;
impl RunAdapter for FalseCompletion {
    fn capabilities(&self) -> RunCapabilities {
        Notebook::new().capabilities()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object"})
    }
    fn validate(&self, _: &RunTarget, _: &RunInput) -> Result<(), RunError> {
        Ok(())
    }
    fn execute<'a>(
        &'a self,
        session: &'a mut RunSession<'_>,
    ) -> JobFuture<'a, Result<(), Box<RunFailure>>> {
        Box::pin(async move {
            let url = session
                .evidence()
                .endpoint
                .url(&format!(
                    "/v1/workspaces/{WS}/notebooks/{ITEM}/jobs/execute/instances?beta=false",
                ))
                .unwrap();
            session.submit(url, Vec::new()).await?;
            session.record(RunOutcome::Completed, Some(json!({"not":"observed"})));
            Ok(())
        })
    }
}

#[tokio::test]
async fn accepted_submission_cannot_be_promoted_to_unobserved_completion() {
    let executor = Executor::scripted(vec![Ok(accepted(&location(), "60"))]);
    let sdk = client(Arc::new(Credential::default()), &executor);
    let command = v2_run::parse_args(["fab-cli", "run", "N"]).unwrap();
    let files = Files::new(json!({}));
    let output = Sink::default();
    let prepared = v2_run::prepare(
        &command,
        target(RunTargetKind::Notebook),
        Some(&FalseCompletion),
        &files,
        &output,
    )
    .unwrap();
    assert!(matches!(
        v2_run::dispatch(prepared, &sdk, Arc::new(Clock::default()), pending()).await,
        Err(RunDispatchError::Execution(error)) if error.source == RunError::Protocol
    ));
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
    assert!(output.bytes.lock().unwrap().is_empty());
}

/// Intentionally permissive test adapter: the dispatcher must corroborate its claim.
struct DirectCompletion(RunTargetKind);
impl RunAdapter for DirectCompletion {
    fn capabilities(&self) -> RunCapabilities {
        Notebook { kind: self.0 }.capabilities()
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object"})
    }
    fn validate(&self, _: &RunTarget, _: &RunInput) -> Result<(), RunError> {
        Ok(())
    }
    fn execute<'a>(
        &'a self,
        session: &'a mut RunSession<'_>,
    ) -> JobFuture<'a, Result<(), Box<RunFailure>>> {
        Box::pin(async move {
            let path = if self.0 == RunTargetKind::Udf {
                "/v1/offline-fixture-function".to_owned()
            } else {
                format!(
                    "/v1/workspaces/{WS}/lakehouses/{ITEM}/livyapi/versions/2023-12-01/sessions/0/statements"
                )
            };
            let url = session.evidence().endpoint.url(&path).unwrap();
            let response = session.submit(url, b"{}".to_vec()).await?;
            let value: Value = serde_json::from_slice(&response.body).unwrap();
            if self.0 == RunTargetKind::Udf {
                session.correlate(value["invocationId"].as_str().unwrap())?;
                session.record(RunOutcome::Completed, Some(value["output"].clone()));
            } else {
                let monitor = session
                    .evidence()
                    .endpoint
                    .url(&format!("{path}/0"))
                    .unwrap();
                let handle = RunHandle::livy(
                    session.plan().target(),
                    &session.evidence().endpoint,
                    monitor.as_str(),
                    0,
                )
                .map_err(|error| failure(session, error))?;
                session.bind_handle(&handle)?;
                session.record(RunOutcome::Completed, Some(value));
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn synchronous_udf_wait_is_redundant_and_null_false_zero_are_results() {
    for (wait, output_value) in [(false, Value::Null), (true, json!(false)), (true, json!(0))] {
        let response = JobResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: serde_json::to_vec(&json!({
                "functionName":"calculate","invocationId":"correlation-only",
                "status":"Succeeded","output":output_value,"errors":[]
            }))
            .unwrap(),
        };
        let executor = Executor::scripted(vec![Ok(response)]);
        let sdk = client(Arc::new(Credential::default()), &executor);
        let mut args = vec!["fab-cli", "run", "F", "--input", "-"];
        if wait {
            args.push("--wait");
        }
        let command = v2_run::parse_args(args).unwrap();
        let files = Files::new(json!({}));
        let sink = Sink::default();
        let adapter = DirectCompletion(RunTargetKind::Udf);
        let prepared = v2_run::prepare(
            &command,
            target(RunTargetKind::Udf),
            Some(&adapter),
            &files,
            &sink,
        )
        .unwrap();
        let result = v2_run::dispatch(prepared, &sdk, Arc::new(Clock::default()), pending())
            .await
            .unwrap();
        assert_eq!(result.result, Some(output_value));
        assert!(result.handle.is_none());
        assert_eq!(result.correlation_id.as_deref(), Some("correlation-only"));
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
        assert!(
            executor
                .budgets
                .lock()
                .unwrap()
                .iter()
                .all(|budget| *budget == Duration::from_secs(120))
        );
    }
}

#[tokio::test]
async fn udf_completed_claim_must_match_the_actual_function_response() {
    let response = JobResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: serde_json::to_vec(&json!({
            "functionName":"different","invocationId":"correlation-only",
            "status":"Succeeded","output":false,"errors":[]
        }))
        .unwrap(),
    };
    let executor = Executor::scripted(vec![Ok(response)]);
    let sdk = client(Arc::new(Credential::default()), &executor);
    let command = v2_run::parse_args(["fab-cli", "run", "F", "--input", "-"]).unwrap();
    let files = Files::new(json!({}));
    let sink = Sink::default();
    let adapter = DirectCompletion(RunTargetKind::Udf);
    let prepared = v2_run::prepare(
        &command,
        target(RunTargetKind::Udf),
        Some(&adapter),
        &files,
        &sink,
    )
    .unwrap();
    assert!(matches!(
        v2_run::dispatch(prepared,&sdk,Arc::new(Clock::default()),pending()).await,
        Err(RunDispatchError::Execution(error)) if error.source==RunError::Protocol
    ));
    assert!(sink.bytes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn livy_completion_requires_real_statement_output_and_complete_coverage() {
    for (state, output_status, partial, success) in [
        ("available", "ok", false, true),
        ("waiting", "ok", false, false),
        ("available", "error", false, false),
        ("available", "ok", true, false),
    ] {
        let response=JobResponse {
            status:200, headers:BTreeMap::new(),
            body:serde_json::to_vec(&json!({
                "id":0,"state":state,"output":{"status":output_status,"data":{"text/plain":"1"},"truncated":partial}
            })).unwrap(),
        };
        let executor = Executor::scripted(vec![Ok(response)]);
        let sdk = client(Arc::new(Credential::default()), &executor);
        let command = v2_run::parse_args([
            "fab-cli",
            "artifact",
            "lakehouse",
            "run",
            "L",
            "--input",
            "-",
        ])
        .unwrap();
        let files = Files::new(json!({"code":"print(1)","language":"pyspark"}));
        let sink = Sink::default();
        let adapter = DirectCompletion(RunTargetKind::LivyStatement);
        let prepared = v2_run::prepare(
            &command,
            target(RunTargetKind::LivyStatement),
            Some(&adapter),
            &files,
            &sink,
        )
        .unwrap();
        assert_eq!(
            v2_run::dispatch(prepared, &sdk, Arc::new(Clock::default()), pending())
                .await
                .is_ok(),
            success
        );
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}
