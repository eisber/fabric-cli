// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive Run composition, not a replacement for the Python-compatible job host.
//!
//! The caller resolves the exact target and supplies an approved executable
//! adapter, input reader, no-clobber output, credentials and clock. The default
//! registry explicitly reports unavailable bindings; pure request validators
//! alone do not install production Notebook, UDF or Livy execution.

use crate::JsonInput;
use clap::{Arg, ArgAction, Command};
use fabric_sdk::v2::{
    Availability, Binding, ContractError, Effect, Generation, OperationDescriptor, Prerequisite,
    Registry, Verb,
};
use fabric_sdk_jobs::{
    JobClient, JobFuture,
    run::{
        RunCapabilities, RunClock, RunDeadline, RunError, RunEvidence, RunFailure, RunFormat,
        RunHandleKind, RunInput, RunInputRule, RunObserver, RunOptions, RunOutcome, RunOutput,
        RunOwnerKind, RunPreflight, RunSession, RunTarget, RunTargetKind, RunTimeout,
        SchedulerObserver,
    },
};
use serde_json::{Value, json};
use std::{ffi::OsString, fmt, future::Future, sync::Arc};

/// One artifact constraint on the common Run operation, never execution authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunFacade {
    /// Whole Notebook execution.
    Notebook,
    /// One published function.
    UserDataFunction,
    /// A statement in an explicitly borrowed Lakehouse session.
    Lakehouse,
}

impl RunFacade {
    /// Exact selected facade spelling.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Notebook => "notebook",
            Self::UserDataFunction => "user-data-function",
            Self::Lakehouse => "lakehouse",
        }
    }

    fn owner(self) -> RunOwnerKind {
        match self {
            Self::Notebook => RunOwnerKind::Notebook,
            Self::UserDataFunction => RunOwnerKind::UserDataFunction,
            Self::Lakehouse => RunOwnerKind::Lakehouse,
        }
    }
}

const FACADES: [RunFacade; 3] = [
    RunFacade::Notebook,
    RunFacade::UserDataFunction,
    RunFacade::Lakehouse,
];

/// Parsed arguments; path and input selection are left to the caller's host.
#[derive(Clone, Eq, PartialEq)]
pub struct RunCommand {
    /// One explicit path, passed unchanged to the typed resolver.
    pub path: String,
    /// UTF-8 JSON input file, or `-` for an explicitly supplied stdin reader.
    pub input: Option<String>,
    /// Type constraint supplied by the facade, absent on common Run.
    pub facade: Option<RunFacade>,
    /// Observe this new submission; redundant for synchronous UDF execution.
    pub wait: bool,
    /// An explicit validated duration, replacing the adapter mode's default.
    pub timeout: Option<String>,
    /// Selected output destination; the caller must supply its no-clobber sink.
    pub output: String,
    /// Beta verdict checking, unavailable until a separate protocol is released.
    pub check_exit: bool,
}

impl fmt::Debug for RunCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunCommand")
            .field("facade", &self.facade)
            .field("wait", &self.wait)
            .field("check_exit", &self.check_exit)
            .finish_non_exhaustive()
    }
}

/// The same leaf can be mounted at the common root and each artifact facade.
pub fn run_command() -> Command {
    Command::new("run")
        .about("Execute one explicitly supported target; timeout never cancels remote work")
        .arg(Arg::new("path").required(true).value_name("PATH"))
        .arg(Arg::new("input").long("input").value_name("FILE|-"))
        .arg(
            Arg::new("wait")
                .long("wait")
                .action(ArgAction::SetTrue)
                .help("Observe this submission; does not select or resubmit an existing run"),
        )
        .arg(Arg::new("timeout").long("timeout").value_name("DURATION"))
        .arg(
            Arg::new("format")
                .long("format")
                .default_value("json")
                .value_parser(["json"]),
        )
        .arg(
            Arg::new("output")
                .long("output")
                .short('o')
                .default_value("-")
                .value_name("FILE|-"),
        )
        .arg(
            Arg::new("check-exit")
                .long("check-exit")
                .requires("wait")
                .action(ArgAction::SetTrue)
                .help("Requires a separately advertised Notebook Beta verdict adapter"),
        )
}

/// Standalone additive grammar; construction performs no configuration or I/O.
///
/// The central executable must explicitly mount this surface and supply a host.
/// This library does not change the existing v1 parser or advertise a live host.
pub fn command() -> Command {
    let mut artifacts = Command::new("artifact").subcommand_required(true);
    for facade in FACADES {
        artifacts = artifacts.subcommand(
            Command::new(facade.name())
                .subcommand_required(true)
                .subcommand(run_command()),
        );
    }
    Command::new("fab-cli")
        .subcommand_required(true)
        .subcommand(run_command())
        .subcommand(artifacts)
}

/// Parse common/facade arguments without resolving paths, files or credentials.
///
/// # Errors
/// Rejects invalid grammar, blank values and invalid explicit durations without
/// echoing supplied arguments. Hosts render help directly from [`command`].
pub fn parse_args<I, T>(args: I) -> Result<RunCommand, RunError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = command()
        .try_get_matches_from(args)
        .map_err(|_| RunError::InvalidInput)?;
    let (facade, leaf) = match matches.subcommand() {
        Some(("run", leaf)) => (None, leaf),
        Some(("artifact", artifacts)) => {
            let (name, owner) = artifacts.subcommand().ok_or(RunError::InvalidInput)?;
            let facade = FACADES
                .into_iter()
                .find(|candidate| candidate.name() == name)
                .ok_or(RunError::UnsupportedCapability)?;
            (
                Some(facade),
                owner
                    .subcommand_matches("run")
                    .ok_or(RunError::InvalidInput)?,
            )
        }
        _ => return Err(RunError::InvalidInput),
    };
    let text = |name| -> Result<Option<String>, RunError> {
        leaf.get_one::<String>(name)
            .map(|value| {
                if value.trim().is_empty() {
                    Err(RunError::InvalidInput)
                } else {
                    Ok(value.clone())
                }
            })
            .transpose()
    };
    let timeout = text("timeout")?;
    if let Some(timeout) = &timeout {
        RunTimeout::parse(timeout)?;
    }
    Ok(RunCommand {
        path: text("path")?.ok_or(RunError::InvalidInput)?,
        input: text("input")?,
        facade,
        wait: leaf.get_flag("wait"),
        timeout,
        output: text("output")?.ok_or(RunError::InvalidInput)?,
        check_exit: leaf.get_flag("check-exit"),
    })
}

/// An executable, positively bound adapter supplied by the approved outer host.
///
/// Pure payload validators are not implementations of this interface. The host
/// verifies owner/context/endpoint/schema/effect approval before dispatch.
pub trait RunAdapter: Send + Sync {
    /// Exactly supported target, omission, wait and representation capabilities.
    fn capabilities(&self) -> RunCapabilities;
    /// Actual native input schema exposed by the configured binding.
    fn input_schema(&self) -> Value;
    /// Pure native validation, including the actual published UDF input schema.
    fn validate(&self, target: &RunTarget, input: &RunInput) -> Result<(), RunError>;
    /// Execute using only the supplied session's shared budget and evidence.
    ///
    /// Adapters must use `submit`, `observe`, `interpret`, `record` and
    /// `correlate` as appropriate; no second HTTP/runtime/poller or replay.
    /// Return only after establishing the binding's declared terminal condition
    /// (or acceptance for asynchronous no-wait submission).
    fn execute<'a>(
        &'a self,
        session: &'a mut RunSession<'_>,
    ) -> JobFuture<'a, Result<(), Box<RunFailure>>>;
}

/// Released whole-Notebook adapter over the SDK's shared execution lifecycle.
///
/// Construction grants no provider permission or credentials. The outer host
/// must bind the exact target and execution policy before dispatch.
pub struct NotebookAdapter;

impl RunAdapter for NotebookAdapter {
    fn capabilities(&self) -> RunCapabilities {
        RunCapabilities {
            target: RunTargetKind::Notebook,
            input: RunInputRule::OptionalObject,
            wait: true,
            formats: vec![RunFormat::Json],
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "parameters": {"type": "array"},
                "executionData": {"type": "object"}
            },
            "description": "Native Notebook parameters and standard compute configuration; high-concurrency options are unavailable."
        })
    }

    fn validate(&self, target: &RunTarget, input: &RunInput) -> Result<(), RunError> {
        if target.kind() != RunTargetKind::Notebook {
            return Err(RunError::WrongOwner);
        }
        fabric_sdk_jobs::notebook::NotebookRunRequest::from_input(match input {
            RunInput::Omitted => None,
            RunInput::Json(value) => Some(value.clone()),
        })
        .map_err(|_| RunError::InvalidInput)?
        .validate_standard_execution()
        .map_err(|_| RunError::UnsupportedCapability)
    }

    fn execute<'a>(
        &'a self,
        session: &'a mut RunSession<'_>,
    ) -> JobFuture<'a, Result<(), Box<RunFailure>>> {
        Box::pin(fabric_sdk_jobs::notebook::execute(session))
    }
}

/// A preflight plan retaining the exact validated adapter and output sink.
///
/// It does not own credentials, acquire a token, or establish remote permission.
pub struct PreparedRun<'a> {
    plan: RunPreflight,
    adapter: &'a dyn RunAdapter,
    output: &'a dyn RunOutput,
}

/// Local startup failure or a phase failure retaining native execution evidence.
#[derive(Debug, thiserror::Error)]
pub enum RunDispatchError {
    /// A valid timeout could not start on the caller's monotonic clock.
    #[error(transparent)]
    Start(RunError),
    /// Original execution or output failure with its protected evidence.
    #[error(transparent)]
    Execution(#[from] Box<RunFailure>),
    /// Observed failed, partial or unknown workload, not a local protocol failure.
    #[error("run did not establish its declared successful outcome")]
    Unsuccessful {
        /// Original protected execution and result evidence.
        evidence: Box<RunEvidence>,
    },
}

impl PreparedRun<'_> {
    /// Inspect the common normalized SDK plan, without recreating it per facade.
    pub fn plan(&self) -> &RunPreflight {
        &self.plan
    }
}

fn supported(kind: RunTargetKind) -> bool {
    matches!(
        kind,
        RunTargetKind::Notebook | RunTargetKind::Udf | RunTargetKind::LivyStatement
    )
}

fn native_input(target: &RunTarget, input: &RunInput) -> Result<(), RunError> {
    match (target.kind(), input) {
        (RunTargetKind::Notebook, input) => {
            fabric_sdk_jobs::notebook::NotebookRunRequest::from_input(match input {
                RunInput::Omitted => None,
                RunInput::Json(value) => Some(value.clone()),
            })
            .map(|_| ())
            .map_err(|_| RunError::InvalidInput)
        }
        (RunTargetKind::LivyStatement, RunInput::Json(value)) => {
            fabric_sdk_jobs::livy::StatementInput::from_json(value)
                .map(|_| ())
                .map_err(|_| RunError::InvalidInput)
        }
        (RunTargetKind::Udf, RunInput::Json(value)) if value.is_object() => Ok(()),
        _ => Err(RunError::InvalidInput),
    }
}

/// Prepare a command using the caller's already resolved, approved exact target.
///
/// `files` supplies bounded, syntax-validated JSON (including duplicate-key and
/// integer fidelity policy). `output` must be the sink for `command.output`;
/// it is retained so execution cannot substitute a different destination.
/// Remote name resolution, if needed, belongs in the host's original budget,
/// not in this pure resolved-target entry point.
///
/// # Errors
/// Missing executable adapters, cells, one-shot, wrong facades and unavailable
/// Beta verdicts fail before reading input, output work, credentials or HTTP.
pub fn prepare<'a>(
    command: &RunCommand,
    target: RunTarget,
    adapter: Option<&'a dyn RunAdapter>,
    files: &dyn JsonInput,
    output: &'a dyn RunOutput,
) -> Result<PreparedRun<'a>, RunError> {
    if !supported(target.kind())
        || command.check_exit
        || command
            .facade
            .is_some_and(|facade| facade.owner() != target.owner().kind())
    {
        return Err(RunError::UnsupportedCapability);
    }
    let adapter = adapter.ok_or(RunError::UnsupportedCapability)?;
    let capabilities = adapter.capabilities();
    let expected_input = if target.kind() == RunTargetKind::Notebook {
        RunInputRule::OptionalObject
    } else {
        RunInputRule::RequiredObject
    };
    if capabilities.target != target.kind() || capabilities.input != expected_input {
        return Err(RunError::UnsupportedCapability);
    }
    let input = match command.input.as_deref() {
        Some(path) => RunInput::Json(files.read_json(path).map_err(|_| RunError::InvalidInput)?),
        None => RunInput::Omitted,
    };
    let plan = RunPreflight::validate(
        target,
        &capabilities,
        RunOptions {
            wait: command.wait,
            timeout: command.timeout.as_deref(),
            format: RunFormat::Json,
        },
        input,
        output,
        |target, input| {
            native_input(target, input)?;
            adapter.validate(target, input)
        },
    )?;
    Ok(PreparedRun {
        plan,
        adapter,
        output,
    })
}

/// Dispatch the retained adapter and commit a structured receipt exactly once.
///
/// There is no default adapter or production authentication discovery. The
/// caller injects the original context and runtime; [`RunSession`] owns all
/// deadline, observation, uncertainty and no-replay behavior.
///
/// # Errors
/// Returns original bounded execution evidence on adapter/timeout/output failure.
/// A failed/partial/unknown workload is non-success even if HTTP succeeded.
pub async fn dispatch(
    prepared: PreparedRun<'_>,
    client: &JobClient,
    clock: Arc<dyn RunClock>,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<RunEvidence, RunDispatchError> {
    dispatch_with_receipt(prepared, client, clock, cancelled, None, |evidence| {
        serde_json::to_vec(&receipt(evidence)).map_err(|_| RunError::Output)
    })
    .await
}

/// Dispatch with a pure bounded receipt projection into the retained output sink.
///
/// The formatter does not execute work or substitute an output destination.
/// Existing [`dispatch`] keeps its original receipt contract. Hosts can project
/// richer scoped evidence without a second publication outside the run budget.
/// Return an encoded receipt without its trailing newline; this function adds
/// the shared framing. Formatter errors are always classified as local Output.
/// An explicit deadline preserves the original host budget through preparation.
///
/// # Errors
/// Preserves execution, projection and no-clobber output evidence; never replays.
pub async fn dispatch_with_receipt(
    prepared: PreparedRun<'_>,
    client: &JobClient,
    clock: Arc<dyn RunClock>,
    cancelled: impl Future<Output = ()> + Send,
    deadline: Option<RunDeadline>,
    render: impl FnOnce(&RunEvidence) -> Result<Vec<u8>, RunError> + Send,
) -> Result<RunEvidence, RunDispatchError> {
    let mut session = match deadline {
        Some(deadline) => {
            RunSession::with_deadline(client, prepared.plan, clock, cancelled, deadline)
        }
        None => RunSession::new(client, prepared.plan, clock, cancelled),
    }
    .map_err(RunDispatchError::Start)?;
    prepared.adapter.execute(&mut session).await?;
    let evidence = session.evidence();
    let accepted = evidence.outcome == RunOutcome::Accepted
        && !session.plan().wait()
        && session.plan().target().kind() != RunTargetKind::Udf
        && evidence.handle.is_some();
    let completed = evidence.outcome == RunOutcome::Completed;
    if !accepted && !completed {
        return Err(RunDispatchError::Unsuccessful {
            evidence: Box::new(evidence.clone()),
        });
    }
    if !evidence.mutation_may_have_occurred
        || !evidence.initial_response_complete
        || evidence.initial_response.is_none()
        || (session.plan().target().kind() == RunTargetKind::Udf
            && evidence.correlation_id.is_none())
        || (completed && !completion_observed(evidence))
    {
        return Err(Box::new(RunFailure {
            source: RunError::Protocol,
            evidence: evidence.clone(),
        })
        .into());
    }
    let mut bytes = render(evidence).map_err(|_| {
        Box::new(RunFailure {
            source: RunError::Output,
            evidence: evidence.clone(),
        })
    })?;
    bytes.push(b'\n');
    session.commit(prepared.output, &bytes).await?;
    Ok(session.evidence().clone())
}

fn completion_observed(evidence: &RunEvidence) -> bool {
    match evidence.target.kind() {
        RunTargetKind::Notebook => match (&evidence.handle, &evidence.last_response) {
            (Some(handle), Some(response)) if evidence.last_response_complete => SchedulerObserver
                .decode(handle, response)
                .is_ok_and(|value| value.outcome == RunOutcome::Completed),
            _ => false,
        },
        RunTargetKind::LivyStatement => {
            let Some(handle) = &evidence.handle else {
                return false;
            };
            let RunHandleKind::Livy { statement, .. } = handle.kind() else {
                return false;
            };
            let response = if evidence.last_response.is_some() {
                if !evidence.last_response_complete {
                    return false;
                }
                evidence.last_response.as_ref()
            } else {
                evidence.initial_response.as_ref()
            };
            response
                .filter(|response| response.status == 200)
                .is_some_and(|response| {
                    let Ok(value) = serde_json::from_slice::<Value>(&response.body) else {
                        return false;
                    };
                    let Ok(value) = fabric_sdk_jobs::livy::StatementObservation::from_json(value)
                    else {
                        return false;
                    };
                    value.verdict() == fabric_sdk_jobs::livy::StatementVerdict::Succeeded
                        && !value.has_partial_output()
                        && (value.id().native().as_u64() == Some(*statement)
                            || value
                                .id()
                                .native()
                                .as_str()
                                .is_some_and(|id| id.parse::<u64>() == Ok(*statement)))
                })
        }
        RunTargetKind::Udf => evidence
            .initial_response
            .as_ref()
            .filter(|response| response.status == 200)
            .is_some_and(|response| {
                let Ok(value) = serde_json::from_slice::<Value>(&response.body) else {
                    return false;
                };
                value.get("functionName").and_then(Value::as_str) == evidence.target.function()
                    && value.get("invocationId").and_then(Value::as_str)
                        == evidence.correlation_id.as_deref()
                    && value.get("status").and_then(Value::as_str) == Some("Succeeded")
                    && value
                        .get("errors")
                        .and_then(Value::as_array)
                        .is_some_and(Vec::is_empty)
                    && value.get("output").is_some()
                    && evidence.handle.is_none()
            }),
        _ => false,
    }
}

fn receipt(evidence: &RunEvidence) -> Value {
    json!({
        "status": match evidence.outcome {
            RunOutcome::Accepted => "accepted",
            RunOutcome::Completed => "succeeded",
            RunOutcome::Failed => "failed",
            RunOutcome::Partial => "partial",
            _ => "unknown",
        },
        "data": evidence.result,
        "meta": {
            "workspaceId": evidence.target.owner().workspace(),
            "itemId": evidence.target.owner().item(),
            "handle": evidence.handle.as_ref().map(|handle| handle.url().as_str()),
            "correlationId": evidence.correlation_id,
            "mutationMayHaveOccurred": evidence.mutation_may_have_occurred,
        }
    })
}

/// Register one operation per exact execution binding, shared by root and facade.
///
/// `adapters` is the outer host's configured executable set. Absent adapters
/// register as unsupported, not available merely because a validator exists.
/// Registration is atomic and performs no I/O, authorization or execution.
///
/// # Errors
/// Preserves duplicate/conflicting registry errors without partially registering.
pub fn register(
    registry: &mut Registry,
    adapters: &[&dyn RunAdapter],
) -> Result<(), ContractError> {
    let mut candidate = registry.clone();
    for (facade, owner, facet, kind, id) in [
        (
            RunFacade::Notebook,
            "Notebook",
            "item",
            RunTargetKind::Notebook,
            "fabric.run.notebook",
        ),
        (
            RunFacade::UserDataFunction,
            "UserDataFunction",
            "function",
            RunTargetKind::Udf,
            "fabric.run.udf",
        ),
        (
            RunFacade::Lakehouse,
            "Lakehouse",
            "livy-statements",
            RunTargetKind::LivyStatement,
            "fabric.run.livy",
        ),
    ] {
        candidate.alias(facade.name(), owner)?;
        let configured: Vec<_> = adapters
            .iter()
            .filter(|adapter| adapter.capabilities().target == kind)
            .collect();
        if configured.len() > 1 {
            return Err(ContractError::AmbiguousBinding);
        }
        candidate.register(OperationDescriptor::new(
            id,
            Verb::Run,
            configured.first().map_or_else(
                || json!({"type":"object","description":"No executable adapter or native input schema configured."}),
                |adapter| adapter.input_schema(),
            ),
            json!({"type":"object","description":"Accepted or completed execution receipt; raw provider diagnostics are not logs."}),
            vec![Effect::RemoteWrite, Effect::MeteredConsumption],
        )?.with_optional_effects(vec![Effect::LocalRead, Effect::RemoteRead, Effect::LocalWrite])?)?;
        candidate.bind(Binding::new(
            owner,
            facet,
            "json",
            Generation::Any,
            id,
            if !configured.is_empty() {
                Availability::Conditional(vec![Prerequisite {
                    id: "approved-native-run-binding".into(),
                    description: "Exact owner, endpoint, audience, schema, execution approval and bounded output must be verified by the host; registration grants none.".into(),
                }])
            } else {
                Availability::Unsupported("No configured executable native Run adapter; pure request validation is not execution support.".into())
            },
        )?)?;
    }
    *registry = candidate;
    Ok(())
}
