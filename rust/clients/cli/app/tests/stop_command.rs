// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native stop policy, library state machine and real HTTP-stack coverage.

#![deny(unnameable_test_items)]

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_cli::stop_command::{self, ArgumentError, Policy};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    mirroring, mirroring_stop,
    v2::{ApprovalPolicy, Effect, PreparedOperation, Registry, Verb},
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse,
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    future::{Future, pending, ready},
    io::{self, Write},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::{ScriptedHttp, TestCredential, TestTimer, response};
use tokio::sync::Notify;

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.MirroredDatabase/runtime/mirroring";
const OTHER: &str = "/11111111-1111-1111-1111-111111111111.Workspace/33333333-3333-3333-3333-333333333333.MirroredDatabase/runtime/mirroring";
const HOST: &str = "dailyapi.fabric.microsoft.com";

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn command(yes: bool) -> stop_command::StopCommand {
    let mut values = vec!["stop", TARGET, "--timeout", "10"];
    if yes {
        values.push("--yes");
    }
    stop_command::parse(args(&values)).unwrap()
}

fn client() -> (Client, Arc<TestCredential>) {
    let credential = Arc::new(TestCredential::default());
    (
        Client::with_context(RequestContext::with_endpoint(
            credential.clone(),
            CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
            EndpointBinding::with_host(ServiceOrigin::Fabric, HOST).unwrap(),
        )),
        credential,
    )
}

fn executor(http: Arc<ScriptedHttp>) -> HttpExecutor {
    HttpExecutor::new(
        http,
        Arc::new(TestTimer::default()),
        ExecutionOptions::default(),
    )
    .unwrap()
    .with_endpoint(EndpointBinding::with_host(ServiceOrigin::Fabric, HOST).unwrap())
}

fn state(state: &str) -> azure_core::http::AsyncRawResponse {
    response(200, &json!({"status":state}).to_string(), &[])
}

fn actions(http: &ScriptedHttp) -> Vec<String> {
    http.requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| {
            assert_eq!(request.method(), azure_core::http::Method::Post);
            assert_eq!(request.url().host_str(), Some(HOST));
            assert!(azure_core::Bytes::from(request.body()).is_empty());
            request.url().path().rsplit('/').next().unwrap().to_owned()
        })
        .collect()
}

#[test]
fn native_schema_extends_but_does_not_change_the_library_contract() {
    assert!(fabric_cli_mutations::v2_stop::parse_args(["stop", TARGET]).is_ok());
    assert!(fabric_cli_mutations::v2_stop::parse_args(["stop", TARGET, "--timeout", "1"]).is_err());
    let schema = stop_command::parse(args(&["stop", "--schema"])).unwrap();
    assert!(schema.schema());
    assert_eq!(schema.timeout(), None);
    for timeout in ["1", "86400"] {
        let value = stop_command::parse(args(&[
            "stop",
            TARGET,
            "--timeout",
            timeout,
            "--format",
            "json",
        ]))
        .unwrap();
        assert!(!value.schema());
        assert_eq!(
            value.timeout().unwrap().as_secs(),
            timeout.parse::<u64>().unwrap()
        );
    }
    let help = stop_command::parse(args(&["stop", "--help"])).unwrap_err();
    assert_eq!(
        help,
        ArgumentError::Syntax(clap::error::ErrorKind::DisplayHelp)
    );
    for values in [
        vec!["stop", TARGET],
        vec!["stop", TARGET, "--yes"],
        vec!["stop", TARGET, "--timeout", "0"],
        vec!["stop", TARGET, "--timeout", "86401"],
        vec!["stop", TARGET, "--timeout", "1", "--approve"],
        vec!["stop", TARGET, "--timeout", "1", "--yes", "--yes"],
        vec!["stop", TARGET, "--timeout", "1", "--format", "table"],
        vec!["stop", "--schema", "--yes"],
        vec!["stop", "--schema", "--format", "json"],
    ] {
        assert!(stop_command::parse(args(&values)).is_err(), "{values:?}");
    }
}

#[test]
fn registry_resolves_both_lifecycle_verbs_only_for_the_exact_facet() {
    let mut registry = Registry::default();
    mirroring::register(&mut registry).unwrap();
    mirroring_stop::register(&mut registry).unwrap();
    let target = mirroring::target(TARGET).unwrap();
    for (verb, operation) in [
        (Verb::Start, mirroring::OPERATION_ID),
        (Verb::Stop, mirroring_stop::OPERATION_ID),
    ] {
        assert_eq!(
            registry
                .resolve(verb, target.selection(), None)
                .unwrap()
                .operation
                .id(),
            operation
        );
        for owner in ["Notebook", "SQLDatabase"] {
            let mut wrong = target.selection().clone();
            wrong.owner_type = owner.into();
            assert!(registry.resolve(verb, &wrong, None).is_err());
        }
    }
}

struct CheckedPolicy {
    read: usize,
    write: usize,
    deny_read: bool,
    deny_write: bool,
}

impl ApprovalPolicy for CheckedPolicy {
    type Error = &'static str;
    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
        assert_eq!(operation.target().canonical_path(), TARGET);
        assert_eq!(operation.operation_id(), mirroring_stop::OPERATION_ID);
        if operation.effects().contains(&Effect::RemoteWrite) {
            assert_eq!(operation.effects(), mirroring_stop::WRITE_EFFECTS);
            assert_eq!(operation.input(), &json!({"yes":true}));
            self.write += 1;
            if self.deny_write {
                return Err("synthetic write refusal");
            }
        } else {
            assert_eq!(operation.effects(), mirroring_stop::READ_EFFECTS);
            self.read += 1;
            if self.deny_read {
                return Err("synthetic observation refusal");
            }
        }
        Ok(())
    }
}

fn policy() -> CheckedPolicy {
    CheckedPolicy {
        read: 0,
        write: 0,
        deny_read: false,
        deny_write: false,
    }
}

#[tokio::test]
async fn no_yes_stopped_and_stopping_are_policy_governed_single_read_no_ops() {
    for (observed, expected, confirmed) in [
        ("Stopped", "noOp", true),
        ("Stopping", "alreadyStopping", false),
    ] {
        for yes in [false, true] {
            let command = command(yes);
            let (client, _) = client();
            let http = ScriptedHttp::new([state(observed)]);
            let mut policy = policy();
            let delivery = stop_command::dispatch(
                &command,
                &client,
                &executor(http.clone()),
                &mut policy,
                OperationLimits::default(),
                pending(),
            )
            .await
            .unwrap();
            assert_eq!(delivery.value["outcome"], expected);
            assert_eq!(delivery.value["stoppedConfirmed"], confirmed);
            assert_eq!(delivery.value["mutationMayHaveOccurred"], false);
            assert_eq!(actions(&http), ["getMirroringStatus"]);
            assert_eq!((policy.read, policy.write), (1, 0));
        }
    }
}

#[tokio::test]
async fn running_without_yes_and_other_states_cannot_submit() {
    for (observed, code) in [
        ("Running", "ApprovalRequired"),
        ("Paused", "InvalidState"),
        ("running", "InvalidResponse"),
    ] {
        let command = command(false);
        let (client, _) = client();
        let http = ScriptedHttp::new([state(observed)]);
        let mut policy = policy();
        let delivery = stop_command::dispatch(
            &command,
            &client,
            &executor(http.clone()),
            &mut policy,
            OperationLimits::default(),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(delivery.value["error"]["code"], code);
        assert_eq!(actions(&http), ["getMirroringStatus"]);
        assert_eq!((policy.read, policy.write), (1, 0));
    }
}

#[tokio::test]
async fn host_refusal_is_not_bypassed_by_yes_and_read_refusal_precedes_credentials() {
    for deny_read in [true, false] {
        let command = command(true);
        let (client, credential) = client();
        let http = ScriptedHttp::new([state("Running")]);
        let mut policy = policy();
        policy.deny_read = deny_read;
        policy.deny_write = true;
        let result = stop_command::dispatch(
            &command,
            &client,
            &executor(http.clone()),
            &mut policy,
            OperationLimits::default(),
            pending(),
        )
        .await;
        if deny_read {
            assert!(matches!(
                result,
                Err(stop_command::CommandError::Approval(_))
            ));
            assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
            assert!(actions(&http).is_empty());
        } else {
            let delivery = result.unwrap();
            assert_eq!(
                delivery.execution.policy_error,
                Some("synthetic write refusal")
            );
            assert_eq!(delivery.value["hostPolicyDenied"], true);
            assert_eq!(delivery.value["mutationMayHaveOccurred"], false);
            assert_eq!(actions(&http), ["getMirroringStatus"]);
        }
    }
}

#[test]
fn native_policy_rejects_target_intent_and_effect_substitution() {
    let command = command(true);
    let (client, _) = client();
    let prepared =
        mirroring_stop::prepare(&client, mirroring::target(OTHER).unwrap(), true).unwrap();
    assert!(prepared.authorize(&mut Policy(&command)).is_err());
    let prepared =
        mirroring_stop::prepare(&client, mirroring::target(TARGET).unwrap(), false).unwrap();
    assert!(prepared.authorize(&mut Policy(&command)).is_err());
    let start = mirroring::prepare(&client, mirroring::target(TARGET).unwrap()).unwrap();
    assert!(start.authorize(&mut Policy(&command)).is_err());
}

#[tokio::test]
async fn prepared_stop_keeps_its_original_credential_selection_and_endpoint() {
    let (client, credential) = client();
    let other_credential = Arc::new(TestCredential::default());
    let other = Client::with_context(RequestContext::new(
        other_credential.clone(),
        CredentialSelection::new("other-account", "other-tenant", Audience::Fabric).unwrap(),
    ));
    let prepared =
        mirroring_stop::prepare(&client, mirroring::target(TARGET).unwrap(), false).unwrap();
    assert_eq!(prepared.context().selection(), client.context().selection());
    assert_ne!(prepared.context().selection(), other.context().selection());
    let mut policy = policy();
    let approved = prepared.authorize(&mut policy).unwrap();
    let http = ScriptedHttp::new([state("Stopped")]);
    let execution = mirroring_stop::execute(
        approved,
        &executor(http.clone()),
        &mut policy,
        OperationLimits::default(),
        pending(),
    )
    .await;
    assert!(execution.result.unwrap().stopped_confirmed());
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert_eq!(other_credential.calls.load(Ordering::SeqCst), 0);
    assert_eq!(actions(&http), ["getMirroringStatus"]);
}

#[test]
fn non_fabric_context_fails_preparation_without_token_acquisition() {
    let credential = Arc::new(TestCredential::default());
    let client = Client::with_context(RequestContext::new(
        credential.clone(),
        CredentialSelection::new("offline", "synthetic", Audience::OneLake).unwrap(),
    ));
    assert!(mirroring_stop::prepare(&client, mirroring::target(TARGET).unwrap(), true).is_err());
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn acceptance_request_completion_and_stopped_confirmation_remain_distinct() {
    for (status, readback, expected, confirmed) in [
        (202, None, "accepted", false),
        (200, Some("Running"), "requestCompleted", false),
        (200, Some("Stopped"), "stopped", true),
    ] {
        let command = command(true);
        let (client, _) = client();
        let mut responses = vec![state("Running"), response(status, "", &[])];
        if let Some(readback) = readback {
            responses.push(state(readback));
        }
        let http = ScriptedHttp::new(responses);
        let mut policy = policy();
        let delivery = stop_command::dispatch(
            &command,
            &client,
            &executor(http.clone()),
            &mut policy,
            OperationLimits::default(),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(delivery.value["outcome"], expected);
        assert_eq!(delivery.value["stoppedConfirmed"], confirmed);
        let expected = if readback.is_some() {
            vec!["getMirroringStatus", "stopMirroring", "getMirroringStatus"]
        } else {
            vec!["getMirroringStatus", "stopMirroring"]
        };
        assert_eq!(actions(&http), expected);
        assert_eq!((policy.read, policy.write), (1, 1));
    }
}

struct Gate {
    http: HttpExecutor,
    calls: AtomicUsize,
    signal: Arc<Notify>,
    at: usize,
    lose_response: bool,
}

impl OperationExecutor for Gate {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let result = OperationExecutor::execute(&self.http, request).await;
            if call == self.at {
                self.signal.notify_one();
                if self.lose_response {
                    pending::<()>().await;
                }
            }
            result
        })
    }
    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        OperationExecutor::wait(&self.http, delay)
    }
}

#[tokio::test]
async fn cancellation_before_preflight_is_no_write_and_no_credential_work() {
    let command = command(true);
    let (client, credential) = client();
    let http = ScriptedHttp::new([]);
    let delivery = stop_command::dispatch(
        &command,
        &client,
        &executor(http.clone()),
        &mut Policy(&command),
        OperationLimits::default(),
        ready(()),
    )
    .await
    .unwrap();
    assert_eq!(delivery.value["mutationMayHaveOccurred"], false);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(actions(&http).is_empty());
}

#[tokio::test]
async fn interruption_during_probe_during_write_and_after_write_preserves_native_evidence() {
    for (at, lose_response, expected_requests, uncertain, completed) in [
        (1, true, 1, false, false),
        (1, false, 1, false, false),
        (2, true, 2, true, false),
        (2, false, 2, true, true),
    ] {
        let command = command(true);
        let (client, _) = client();
        let http = ScriptedHttp::new([state("Running"), response(200, "", &[])]);
        let signal = Arc::new(Notify::new());
        let gate = Gate {
            http: executor(http.clone()),
            calls: AtomicUsize::new(0),
            signal: signal.clone(),
            at,
            lose_response,
        };
        let delivery = stop_command::dispatch(
            &command,
            &client,
            &gate,
            &mut Policy(&command),
            OperationLimits::default(),
            signal.notified(),
        )
        .await
        .unwrap();
        let error = delivery.execution.result.as_ref().unwrap_err();
        assert_eq!(actions(&http).len(), expected_requests);
        assert_eq!(error.mutation_may_have_occurred, uncertain);
        assert_eq!(error.receipt().is_some(), completed);
        if completed {
            assert!(error.receipt().unwrap().request_outcome.is_some());
            assert_eq!(
                delivery.value["requestEvidence"]["requestOutcome"]["httpStatus"],
                200
            );
        }
    }
}

#[tokio::test]
async fn total_deadline_signal_is_not_restarted_between_requests() {
    for (at, uncertain, completed) in [
        (0, false, false),
        (1, false, false),
        (2, true, false),
        (3, true, true),
    ] {
        let command = command(true);
        let (client, _) = client();
        let http = ScriptedHttp::new([state("Running"), response(200, "", &[]), state("Stopped")]);
        let gate = Gate {
            http: executor(http.clone()),
            calls: AtomicUsize::new(0),
            signal: Arc::new(Notify::new()),
            at,
            lose_response: true,
        };
        let deadline =
            fabric_sdk::runtime::NativeDeadline::new(Duration::from_millis(if at == 0 {
                1
            } else {
                10
            }))
            .unwrap();
        if at == 0 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let expired = async {
            if let Ok(remaining) = deadline.remaining() {
                tokio::time::sleep(remaining).await;
            }
        };
        let delivery = stop_command::dispatch(
            &command,
            &client,
            &gate,
            &mut Policy(&command),
            OperationLimits::default(),
            expired,
        )
        .await
        .unwrap();
        let error = delivery.execution.result.as_ref().unwrap_err();
        assert_eq!(actions(&http).len(), at);
        assert_eq!(error.mutation_may_have_occurred, uncertain);
        assert_eq!(error.receipt().is_some(), completed);
        assert_eq!(delivery.value["stoppedConfirmed"], false);
        if completed {
            assert_eq!(
                delivery.value["requestEvidence"]["requestOutcome"]["httpStatus"],
                200
            );
        }
    }
}

#[tokio::test]
async fn accepted_receipt_survives_late_interruption_without_a_readback() {
    let command = command(true);
    let (client, _) = client();
    let http = ScriptedHttp::new([state("Running"), response(202, "", &[])]);
    let signal = Arc::new(Notify::new());
    let gate = Gate {
        http: executor(http.clone()),
        calls: AtomicUsize::new(0),
        signal: signal.clone(),
        at: 2,
        lose_response: false,
    };
    let delivery = stop_command::dispatch(
        &command,
        &client,
        &gate,
        &mut Policy(&command),
        OperationLimits::default(),
        signal.notified(),
    )
    .await
    .unwrap();
    assert_eq!(delivery.value["outcome"], "accepted");
    assert!(delivery.execution.interrupted);
    assert_eq!(delivery.value["localObservationInterrupted"], true);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(delivery.write_to(&mut stdout, &mut stderr), 1);
    assert!(stdout.is_empty());
    let delivered: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(delivered["outcome"], "accepted");
    assert_eq!(delivered["error"]["code"], "Interrupted");
    assert_eq!(delivery.value["stoppedConfirmed"], false);
    assert_eq!(actions(&http), ["getMirroringStatus", "stopMirroring"]);
    assert!(delivery.execution.result.unwrap().request_outcome.is_some());
}

#[tokio::test]
async fn shared_request_budget_blocks_readback_without_losing_completed_submission() {
    let command = command(true);
    let (client, _) = client();
    let http = ScriptedHttp::new([state("Running"), response(200, "", &[])]);
    let limits = OperationLimits {
        max_requests: NonZeroUsize::new(2).unwrap(),
        ..OperationLimits::default()
    };
    let delivery = stop_command::dispatch(
        &command,
        &client,
        &executor(http.clone()),
        &mut Policy(&command),
        limits,
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(actions(&http), ["getMirroringStatus", "stopMirroring"]);
    assert!(
        delivery
            .execution
            .result
            .as_ref()
            .unwrap_err()
            .receipt()
            .unwrap()
            .request_outcome
            .is_some()
    );
    assert_eq!(
        delivery.value["requestEvidence"]["requestOutcome"]["httpStatus"],
        200
    );
}

struct Broken;
impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("synthetic output loss"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("synthetic flush loss"))
    }
}

#[tokio::test]
async fn output_loss_retains_completed_receipt_without_reexecution() {
    let command = command(true);
    let (client, _) = client();
    let http = ScriptedHttp::new([state("Running"), response(200, "", &[]), state("Stopped")]);
    let delivery = stop_command::dispatch(
        &command,
        &client,
        &executor(http.clone()),
        &mut Policy(&command),
        OperationLimits::default(),
        pending(),
    )
    .await
    .unwrap();
    let mut stderr = Vec::new();
    assert_eq!(delivery.write_to(&mut Broken, &mut stderr), 1);
    let value: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(value["error"]["code"], "OutputFailed");
    assert_eq!(value["originalResult"]["stoppedConfirmed"], true);
    assert!(
        delivery
            .execution
            .result
            .as_ref()
            .unwrap()
            .stopped_confirmed()
    );
    assert_eq!(delivery.write_to(&mut Broken, &mut Broken), 1);
    assert_eq!(
        actions(&http),
        ["getMirroringStatus", "stopMirroring", "getMirroringStatus"]
    );
}
