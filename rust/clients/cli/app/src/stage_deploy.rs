// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Isolated v2 deploy parser and caller-injected native SDK consumer.
//!
//! No v1 grammar, repository publication or default approval is installed.
//! Opt-in native hosts supply bounded input, exact stage reads and immediate
//! terminal approval; builds without a native host remain unconfigured.

use clap::{Arg, ArgAction, ArgGroup, Command, error::ErrorKind};
use fabric_deploy::stage::{
    DeploymentApproval, DeploymentError, DeploymentErrorKind, DeploymentOutcome, DeploymentReceipt,
    DeploymentRequest, PreparedDeployment, SelectionIntent, StageMetadata,
};
use fabric_sdk::RequestContext;
use fabric_sdk_operations::{OperationExecutor, OperationLimits};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fmt,
    future::Future,
    io::{self, Write},
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

/// Help/version or a parsed native deployment request; parsing never grants approval.
#[derive(Debug)]
pub enum StageDeployInvocation {
    /// Human help/version, before configuration/credential/input-file access.
    Display(String),
    /// Exact pipeline/target path and bounded caller-owned native input.
    Deploy(StageDeployCommand),
}

/// CLI spelling normalized to the same immutable SDK request.
#[derive(Clone)]
pub struct StageDeployCommand {
    pipeline: String,
    target_stage: String,
    input: PathBuf,
    intent: SelectionIntent,
    max_input_bytes: NonZeroUsize,
    wait: bool,
}

impl fmt::Debug for StageDeployCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StageDeployCommand")
            .field("intent", &self.intent)
            .field("wait", &self.wait)
            .finish_non_exhaustive()
    }
}

impl StageDeployCommand {
    /// Explicit local input path for the caller's existing bounded file capability.
    pub fn input(&self) -> &Path {
        &self.input
    }
    /// Required input byte ceiling; input is rechecked by the SDK before dispatch.
    pub const fn max_input_bytes(&self) -> usize {
        self.max_input_bytes.get()
    }
    /// Whether the shared engine should observe native terminal completion.
    pub const fn wait(&self) -> bool {
        self.wait
    }
    /// Normalize already acquired input bytes without reading files or credentials.
    ///
    /// # Errors
    /// SDK errors retain target/selection/unsupported-branch classification.
    pub fn request(&self, bytes: &[u8]) -> Result<DeploymentRequest, DeploymentError> {
        DeploymentRequest::from_json(
            &self.pipeline,
            &self.target_stage,
            bytes,
            self.max_input_bytes.get(),
            self.intent,
        )
    }
}

/// Schema for this isolated deployment head, not a new global v1 command.
pub fn command() -> Command {
    Command::new("fab-rs-v2-deploy").bin_name("fab-rs --v2-deploy")
        .version(env!("CARGO_PKG_VERSION")).subcommand_required(true).arg_required_else_help(true)
        .subcommand(Command::new("deploy").about("Preview native deployment between explicit assigned pipeline stages")
            .long_about("Preview native deployment between explicit assigned pipeline stages. Requires a native-read or headless build, an explicitly selected identity and immediate terminal approval of the exact native body and effects. The existing native 240-second total deadline includes input, stage reads, approval, submission, observation and output. No automatic approval, remote cancellation, rollback or replay.")
            .arg(Arg::new("path").required(true).value_name("PIPELINE_STAGE_PATH"))
            .arg(Arg::new("input").long("input").required(true).value_parser(clap::value_parser!(PathBuf))
                .help("Bounded local native JSON file, not repository config or stdin"))
            .arg(Arg::new("max-input-bytes").long("max-input-bytes").required(true).value_parser(clap::value_parser!(NonZeroUsize)))
            .arg(Arg::new("selected-items").long("selected-items").action(ArgAction::SetTrue))
            .arg(Arg::new("all-items").long("all-items").action(ArgAction::SetTrue)
                .help("Explicitly select all supported source items; host approval is still required"))
            .group(ArgGroup::new("selection").args(["selected-items", "all-items"]).required(true))
            .arg(Arg::new("wait").long("wait").action(ArgAction::SetTrue)
                .help("Observe native completion within the caller's original deadline; no rollback or remote cancellation")))
}

/// Parse exact UUID-only pipeline-stage paths with no implicit stages or workspace.
///
/// # Errors
/// Returns sanitized invalid arguments without echoing input/path/option values.
pub fn parse_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<StageDeployInvocation, DeploymentError> {
    let invalid = || DeploymentError {
        kind: DeploymentErrorKind::InvalidInput,
        operation: None,
        receipt: None,
    };
    let matches = match command().try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(cause)
            if matches!(
                cause.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return Ok(StageDeployInvocation::Display(cause.to_string()));
        }
        Err(_) => return Err(invalid()),
    };
    let args = matches.subcommand_matches("deploy").ok_or_else(invalid)?;
    let path = args.get_one::<String>("path").ok_or_else(invalid)?;
    let parts = path.split('/').collect::<Vec<_>>();
    let is_uuid = |id: &str| {
        id.len() == 36
            && id.bytes().enumerate().all(|(i, c)| {
                if [8, 13, 18, 23].contains(&i) {
                    c == b'-'
                } else {
                    c.is_ascii_hexdigit()
                }
            })
    };
    if parts.len() != 5
        || !parts[0].is_empty()
        || parts[1] != ".deployment-pipelines"
        || parts[3] != "stages"
        || !is_uuid(parts[2])
        || !is_uuid(parts[4])
    {
        return Err(invalid());
    }
    Ok(StageDeployInvocation::Deploy(StageDeployCommand {
        pipeline: parts[2].into(),
        target_stage: parts[4].into(),
        input: args
            .get_one::<PathBuf>("input")
            .cloned()
            .ok_or_else(invalid)?,
        max_input_bytes: *args
            .get_one::<NonZeroUsize>("max-input-bytes")
            .ok_or_else(invalid)?,
        intent: if args.get_flag("all-items") {
            SelectionIntent::AllSupported
        } else {
            SelectionIntent::Selected
        },
        wait: args.get_flag("wait"),
    }))
}

/// Caller-owned preflight and single receipt delivery; no SDK prints or opens files.
pub trait StageDeployOutput {
    /// Check output availability before approval/submission without writing.
    ///
    /// # Errors
    /// An unavailable destination stops dispatch, not an implicit stdout fallback.
    fn preflight(&mut self) -> io::Result<()>;
    /// Deliver a single JSON outcome; no retry after partial local delivery.
    ///
    /// # Errors
    /// Delivery failure must preserve original accepted/completed/failed evidence.
    fn write(&mut self, value: &Value) -> io::Result<()>;
}

/// Root failure, independent from native deployment outcome.
pub enum StageDeployError {
    /// Invalid input, policy refusal or native operation failure.
    Deployment(DeploymentError),
    /// Output unavailable before submission.
    Preflight(io::Error),
    /// Output failed after execution; retain the complete native result.
    Delivery {
        /// Original local writer error.
        cause: io::Error,
        /// Original operation receipt/failure, never replaced by empty output.
        execution: Box<Result<DeploymentReceipt, DeploymentError>>,
    },
}

impl fmt::Debug for StageDeployError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deployment(error) => f.debug_tuple("Deployment").field(error).finish(),
            Self::Preflight(_) => f.write_str("OutputPreflightFailed"),
            Self::Delivery { execution, .. } => {
                f.debug_tuple("DeliveryFailed").field(execution).finish()
            }
        }
    }
}
impl fmt::Display for StageDeployError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Stage deploy head failed; preserve native and delivery evidence")
    }
}
impl std::error::Error for StageDeployError {}

/// Thin CLI consumer over caller-bound SDK execution and immediate approval.
pub struct StageDeployHead<'a, E, P> {
    /// Exact endpoint, credential and selection used to acquire stage metadata.
    pub context: &'a RequestContext,
    /// Existing shared native HTTP execution.
    pub executor: &'a E,
    /// Submission/observation limits, not a new poller or retry policy.
    pub limits: OperationLimits,
    /// Immediate exact-target/input/effect policy; no implicit yes.
    pub policy: &'a mut P,
}

impl<E: OperationExecutor, P: DeploymentApproval> StageDeployHead<'_, E, P> {
    /// Execute the parsed request with actual pre-acquired stage metadata.
    ///
    /// The outer host owns bounded input acquisition and scoped stage reads through
    /// `fabric_deploy::stage::StageReader`, debits preflight requests from its overall
    /// budget, and supplies cancellation including the same original total deadline.
    /// No stage lookup, identity fallback or extra operation is hidden here.
    ///
    /// # Errors
    /// Returns native failure or output failure with original execution evidence.
    pub async fn run(
        &mut self,
        command: &StageDeployCommand,
        input: &[u8],
        stages: (StageMetadata, StageMetadata),
        output: &mut impl StageDeployOutput,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<DeploymentReceipt, StageDeployError> {
        let request = command
            .request(input)
            .map_err(StageDeployError::Deployment)?;
        let prepared = PreparedDeployment::new(request, stages.0, stages.1)
            .map_err(StageDeployError::Deployment)?;
        prepared
            .require_context(self.context)
            .map_err(StageDeployError::Deployment)?;
        output.preflight().map_err(StageDeployError::Preflight)?;
        let approved = prepared
            .authorize(self.policy)
            .map_err(StageDeployError::Deployment)?;
        let result = approved
            .execute(
                self.context,
                self.executor,
                self.limits,
                command.wait,
                cancelled,
            )
            .await;
        let value = match &result {
            Ok(receipt) => receipt.to_value(),
            Err(error) => {
                let mut value = error.receipt.as_ref().map_or_else(
                    || json!({"operation":"deploy","status":"failed"}),
                    |receipt| receipt.to_value(),
                );
                value["error"] = json!({"code":format!("{:?}",error.kind),
                    "message":"Stage deployment failed or is incomplete; retain native evidence and do not replay."});
                value
            }
        };
        if let Err(cause) = output.write(&value) {
            return Err(StageDeployError::Delivery {
                cause,
                execution: Box::new(result),
            });
        }
        result.map_err(StageDeployError::Deployment)
    }
}

/// Exit policy: accepted submission succeeds as submission, not terminal completion.
pub const fn execution_exit_code(receipt: &DeploymentReceipt) -> u8 {
    match receipt.outcome {
        DeploymentOutcome::Accepted | DeploymentOutcome::Succeeded => 0,
        _ => 1,
    }
}

/// Isolated executable entry without ambient identity/files or blanket approval.
///
/// # Errors
/// Preserves stdout/stderr write errors. Help/version succeeds without host access;
/// parsed operations fail explicitly until an authorized pipeline host is injected.
pub fn run_unconfigured(
    args: impl IntoIterator<Item = OsString>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    match parse_args(args) {
        Ok(StageDeployInvocation::Display(text)) => {
            stdout.write_all(text.as_bytes())?;
            Ok(0)
        }
        Ok(StageDeployInvocation::Deploy(_)) => {
            serde_json::to_writer(&mut *stderr, &json!({"error":{"code":"NotConfigured",
                "message":"Native stage deployment requires an injected pipeline-stage resolver, exact-effect approval, original deadline, credential and executor host."}})).map_err(io::Error::other)?;
            writeln!(stderr)?;
            Ok(1)
        }
        Err(_) => {
            serde_json::to_writer(&mut *stderr, &json!({"error":{"code":"invalid_arguments",
                "message":"Invalid native stage deploy arguments. Run fab-rs --v2-deploy --help."}})).map_err(io::Error::other)?;
            writeln!(stderr)?;
            Ok(2)
        }
    }
}
