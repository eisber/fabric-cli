// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Credential-free Notebook Run preparation and single-submission dispatch.

use crate::native_tabular::{NativeCommand, Operation};
use fabric_cli_jobs::{
    JsonInput,
    v2_run::{self, NotebookAdapter, RunCommand, RunDispatchError},
};
use fabric_sdk_jobs::{
    JobClient, JobFuture,
    run::{
        RunClock, RunDeadline, RunError, RunEvidence, RunHandleKind, RunOutput, RunOwner,
        RunOwnerKind, RunTarget, RunTargetKind, RunTimeout,
    },
};
use serde_json::{Value, json};
use std::{
    future::Future,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

/// One local input byte limit, independent of bounded remote response evidence.
pub const MAX_INPUT_BYTES: usize = 1_048_576;

/// Immutable target/input selection before approval or credential access.
pub struct PreparedNotebook {
    command: RunCommand,
    target: RunTarget,
    input: Option<Value>,
    timeout: Duration,
}

/// Validate the exact native target and options without input or credential I/O.
///
/// Only ID-qualified whole Notebooks are bound. Unsupported verdicts and
/// non-JSON representations fail before any input, token or submission.
///
/// # Errors
/// Rejects unsupported targets/exports and malformed finite durations.
pub fn preflight(command: &NativeCommand) -> Result<(RunTarget, Duration), RunError> {
    let Operation::Run(options) = &command.operation else {
        return Err(RunError::UnsupportedCapability);
    };
    if command.export.is_some() || options.check_exit {
        return Err(RunError::UnsupportedCapability);
    }
    if options.output.trim().is_empty()
        || options
            .input
            .as_ref()
            .is_some_and(|input| input.trim().is_empty())
    {
        return Err(RunError::InvalidInput);
    }
    let parts: Vec<_> = command.path.split('/').collect();
    let ["", workspace, item] = parts.as_slice() else {
        return Err(RunError::InvalidTarget);
    };
    let workspace = workspace
        .strip_suffix(".Workspace")
        .ok_or(RunError::InvalidTarget)?;
    let item = item
        .strip_suffix(".Notebook")
        .ok_or(RunError::UnsupportedCapability)?;
    if !guid(workspace) || !guid(item) {
        return Err(RunError::InvalidTarget);
    }
    let target = RunTarget::item(
        RunOwner::new(RunOwnerKind::Notebook, workspace, item)?,
        RunTargetKind::Notebook,
    )?;
    let timeout = options
        .timeout
        .as_deref()
        .map(RunTimeout::parse)
        .transpose()?
        .unwrap_or_else(|| RunTimeout::for_target(RunTargetKind::Notebook, options.wait));
    Ok((target, timeout.duration()))
}

fn guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

impl PreparedNotebook {
    /// Validate bounded strict JSON and the released native Notebook schema.
    ///
    /// # Errors
    /// Rejects omitted/supplied input mismatches, duplicates, unknown fields and
    /// native parameter/compute errors without reflecting supplied values.
    pub fn new(command: &NativeCommand, bytes: Option<&[u8]>) -> Result<Self, RunError> {
        let (target, timeout) = preflight(command)?;
        let Operation::Run(options) = &command.operation else {
            return Err(RunError::UnsupportedCapability);
        };
        if options.input.is_some() != bytes.is_some() {
            return Err(RunError::InvalidInput);
        }
        let input = bytes
            .map(|bytes| {
                if bytes.len() > MAX_INPUT_BYTES {
                    return Err(RunError::InvalidInput);
                }
                fabric_cli_output::parse_data(
                    std::str::from_utf8(bytes).map_err(|_| RunError::InvalidInput)?,
                )
                .map_err(|_| RunError::InvalidInput)?;
                fabric_cli_output::input::decode_json_input(
                    Some(bytes),
                    "application/json",
                    NonZeroUsize::new(MAX_INPUT_BYTES).expect("positive limit"),
                )
                .map_err(|_| RunError::InvalidInput)
            })
            .transpose()?;
        fabric_sdk_jobs::notebook::NotebookRunRequest::from_input(input.clone())
            .map_err(|_| RunError::InvalidInput)?
            .validate_standard_execution()
            .map_err(|_| RunError::UnsupportedCapability)?;
        Ok(Self {
            command: RunCommand {
                path: command.path.clone(),
                input: options.input.clone(),
                facade: None,
                wait: options.wait,
                timeout: options.timeout.clone(),
                output: options.output.clone(),
                check_exit: options.check_exit,
            },
            target,
            input,
            timeout,
        })
    }

    /// One immutable target/input description for the caller's approval UI.
    pub fn approval_question(&self) -> String {
        format!(
            "Execute whole Notebook {} in workspace {} with input {}? This can start compute, consume capacity and change data. Local timeout/interruption does not cancel remote work.",
            self.target.owner().item(),
            self.target.owner().workspace(),
            self.input
                .as_ref()
                .map_or_else(|| "<omitted>".into(), Value::to_string),
        )
    }

    /// Exact finite total budget selected during local validation.
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Exact no-clobber receipt file selection, or `-` for stdout.
    pub fn output(&self) -> &str {
        &self.command.output
    }

    /// Execute after the caller has approved this exact target and input.
    ///
    /// No approval decision is inferred from parameters or a bypass flag.
    /// Embedding callers are responsible for providing their execution policy.
    pub async fn dispatch(
        &self,
        client: &JobClient,
        clock: Arc<dyn RunClock>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Dispatch {
        self.dispatch_with_budget(client, clock, cancelled, self.timeout)
            .await
    }

    /// Dispatch using only the original host deadline's remaining budget.
    ///
    /// Local input, approval and identity setup cannot restart the run timeout.
    pub async fn dispatch_with_budget(
        &self,
        client: &JobClient,
        clock: Arc<dyn RunClock>,
        cancelled: impl Future<Output = ()> + Send,
        remaining: Duration,
    ) -> Dispatch {
        self.dispatch_to(
            client,
            clock,
            cancelled,
            remaining,
            &CapturedOutput::default(),
        )
        .await
    }

    /// Dispatch into the caller's exact preflighted no-clobber output capability.
    pub async fn dispatch_to(
        &self,
        client: &JobClient,
        clock: Arc<dyn RunClock>,
        cancelled: impl Future<Output = ()> + Send,
        remaining: Duration,
        output: &dyn RunOutput,
    ) -> Dispatch {
        if remaining.is_zero() || remaining > self.timeout || remaining.as_millis() == 0 {
            return Dispatch::unavailable(RunError::Deadline);
        }
        let timeout = match RunTimeout::parse(&format!("{}ms", remaining.as_millis())) {
            Ok(timeout) => timeout,
            Err(error) => return Dispatch::unavailable(error),
        };
        let deadline = match RunDeadline::start(clock.as_ref(), timeout) {
            Ok(deadline) => deadline,
            Err(error) => return Dispatch::unavailable(error),
        };
        self.dispatch_before(client, clock, cancelled, deadline, output)
            .await
    }

    /// Preserve an absolute deadline in the supplied clock's domain, including preflight.
    pub async fn dispatch_before(
        &self,
        client: &JobClient,
        clock: Arc<dyn RunClock>,
        cancelled: impl Future<Output = ()> + Send,
        deadline: RunDeadline,
        output: &dyn RunOutput,
    ) -> Dispatch {
        let files = Input(self.input.clone());
        let adapter = NotebookAdapter;
        let prepared = match v2_run::prepare(
            &self.command,
            self.target.clone(),
            Some(&adapter),
            &files,
            output,
        ) {
            Ok(prepared) => prepared,
            Err(error) => return Dispatch::unavailable(error),
        };
        match v2_run::dispatch_with_receipt(
            prepared,
            client,
            clock,
            cancelled,
            Some(deadline),
            |evidence| {
                let receipt = Dispatch::from_evidence(evidence, None, 0);
                serde_json::to_vec(receipt.value()).map_err(|_| RunError::Output)
            },
        )
        .await
        {
            Ok(evidence) => Dispatch::from_evidence(&evidence, None, 0),
            Err(RunDispatchError::Start(error)) => Dispatch::unavailable(error),
            Err(RunDispatchError::Execution(error)) => {
                Dispatch::from_evidence(&error.evidence, Some(error.source), 1)
            }
            Err(RunDispatchError::Unsuccessful { evidence }) => {
                Dispatch::from_evidence(&evidence, None, 1)
            }
        }
    }
}

struct Input(Option<Value>);
impl JsonInput for Input {
    fn read_json(&self, _: &str) -> Result<Value, fabric_cli_output::CommandError> {
        self.0.clone().ok_or_else(|| {
            fabric_cli_output::CommandError::new(
                Some("Notebook input is unavailable.".into()),
                Some("InvalidInput".into()),
            )
        })
    }
}

#[derive(Default)]
// Library callers without file I/O retain the exact committed receipt in memory.
struct CapturedOutput(Mutex<Option<Vec<u8>>>);
impl RunOutput for CapturedOutput {
    fn preflight(&self, _: fabric_sdk_jobs::run::RunFormat) -> Result<(), RunError> {
        if self.0.lock().map_err(|_| RunError::Output)?.is_some() {
            return Err(RunError::Output);
        }
        Ok(())
    }
    fn commit<'a>(&'a self, bytes: &'a [u8]) -> JobFuture<'a, Result<(), RunError>> {
        Box::pin(async move {
            let mut slot = self.0.lock().map_err(|_| RunError::Output)?;
            if slot.is_some() {
                return Err(RunError::Output);
            }
            *slot = Some(bytes.to_vec());
            Ok(())
        })
    }
}

/// Structured receipt whose local failure never authorizes replay.
pub struct Dispatch {
    value: Value,
    exit: u8,
}

impl Dispatch {
    /// Safe JSON projection, without raw response headers or provider errors.
    pub fn value(&self) -> &Value {
        &self.value
    }
    /// Accepted/completed 0; failed/unknown/host errors 1; invalid local input 2.
    pub const fn exit_code(&self) -> u8 {
        self.exit
    }

    /// Retain possible local output effects if interruption drops a blocking write.
    pub fn with_output_attempt(mut self, attempted: bool) -> Self {
        self.value["meta"]["localOutputMayHaveOccurred"] = json!(attempted);
        self
    }

    /// Retain the native signal-handler error separately from user interruption.
    pub fn signal_registration_failed(mut self) -> Self {
        self.value["error"] = json!({
            "code":"SignalFailed",
            "message":"The local cancellation handler could not start; observation stopped without remote cancellation or replay."
        });
        self.exit = 1;
        self
    }
    /// Local pre-dispatch failure; no remote side effects are claimed.
    pub fn unavailable(error: RunError) -> Self {
        Self {
            value: json!({
                "status":"error", "error":{"code":error_code(error),"message":error.to_string()},
                "meta":{"mutationMayHaveOccurred":false,"applicationSuccessVerified":false}
            }),
            exit: if matches!(
                error,
                RunError::InvalidInput | RunError::InvalidTarget | RunError::InvalidTimeout
            ) {
                2
            } else {
                1
            },
        }
    }
    fn from_evidence(evidence: &RunEvidence, error: Option<RunError>, exit: u8) -> Self {
        // Exclude post-commit state so committed and returned receipts stay identical.
        use fabric_sdk_jobs::run::RunOutcome;
        let handle = evidence.handle.as_ref().map(|handle| match handle.kind() {
            RunHandleKind::Job(id) => {
                json!({"kind":"itemJobRun","id":id,"url":handle.url().as_str()})
            }
            RunHandleKind::Operation(id) => {
                json!({"kind":"operation","id":id,"url":handle.url().as_str()})
            }
            RunHandleKind::Livy { .. } => Value::Null,
        });
        Self {
            value: json!({
                "status":match evidence.outcome {
                    RunOutcome::NotSubmitted => "notSubmitted", RunOutcome::Accepted => "accepted",
                    RunOutcome::Completed => "completed", RunOutcome::Failed => "failed",
                    RunOutcome::Partial => "partial", RunOutcome::Unknown => "unknown",
                },
                "data":evidence.result,
                "error":error.map(|error| json!({"code":error_code(error),"message":error.to_string()})),
                "meta":{
                    "workspaceId":evidence.target.owner().workspace(),
                    "itemId":evidence.target.owner().item(), "handle":handle,
                    "mutationMayHaveOccurred":evidence.mutation_may_have_occurred,
                    "initialResponseComplete":evidence.initial_response_complete,
                    "lastResponseComplete":evidence.last_response_complete,
                    "responseBodyOmitted":evidence.response_body_omitted,
                    "httpStatus":evidence.initial_response.as_ref().map(|response| response.status),
                    "completionDomain":"scheduler", "applicationSuccessVerified":false,
                    "remoteCancellationRequested":false,
                }
            }),
            exit,
        }
    }
}

fn error_code(error: RunError) -> &'static str {
    match error {
        RunError::InvalidInput | RunError::InvalidTarget | RunError::InvalidTimeout => {
            "InvalidInput"
        }
        RunError::UnsupportedCapability => "UnsupportedCapability",
        RunError::Deadline => "TimedOut",
        RunError::Interrupted => "Cancelled",
        RunError::Credential => "CredentialFailed",
        RunError::Context => "InvalidContext",
        RunError::Http(_) => "HttpError",
        RunError::Replay => "ReplayForbidden",
        RunError::Protocol => "InvalidResponse",
        RunError::ResponseLimit => "ResponseLimit",
        RunError::Output => "OutputFailure",
        RunError::WrongOwner | RunError::UnsupportedExecutor | RunError::Transport => "RunFailed",
    }
}
