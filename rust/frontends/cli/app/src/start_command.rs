// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit MirroredDatabase lifecycle arguments and caller-owned receipt delivery.

use clap::{Arg, ArgAction, Command, error::ErrorKind};
use fabric_sdk::{
    Client, mirroring,
    v2::{ApprovalPolicy, Outcome, PreparedOperation},
};
use fabric_sdk_operations::{OperationExecutor, OperationLimits};
use serde_json::{Value, json};
use std::{
    ffi::{OsStr, OsString},
    future::Future,
    io::{self, Write},
    time::Duration,
};

/// One explicit lifecycle target, local deadline and invocation-bound approval.
#[derive(Clone, Debug)]
pub struct StartCommand {
    /// Canonical ID-qualified runtime/mirroring path; absent only for schema.
    pub path: Option<String>,
    /// Positive total local deadline, never a remote cancellation policy.
    pub timeout: Option<Duration>,
    /// Explicit approval of this invocation's declared effects.
    pub approve: bool,
    /// Local discovery without identity, transport or mutation.
    pub schema: bool,
}

/// Sanitized argument failure, never including a supplied target or secret.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StartArgumentError {
    /// Syntax/help classification supplied without raw parser diagnostics.
    #[error("Invalid start arguments; run start --help.")]
    Syntax(ErrorKind),
    /// The exact path or total budget is outside this milestone's contract.
    #[error(
        "Start requires an ID-qualified MirroredDatabase runtime/mirroring target and a positive --timeout of at most 86400 seconds."
    )]
    Invalid,
}

/// Describe only the positively bound lifecycle capability, not generic execution.
pub fn start_command() -> Command {
    Command::new("start")
        .infer_long_args(false)
        .args_override_self(false)
        .about("Start mirroring for one existing MirroredDatabase")
        .after_help("Target: /<workspace-id>.Workspace/<item-id>.MirroredDatabase/runtime/mirroring. Requires --approve and an explicit total --timeout in seconds. Only Stopped or Initialized can start. Success means the START action completed, not that mirroring is Running or caught up. No workload submission, restart, implicit stop, cancellation, rollback or replay. Other lifecycle backends and name-based targets are not supported in this milestone.")
        .arg(Arg::new("path").num_args(1).required_unless_present("schema"))
        .arg(
            Arg::new("schema")
                .long("schema")
                .action(ArgAction::SetTrue)
                .conflicts_with_all(["path", "timeout", "approve"]),
        )
        .arg(
            Arg::new("timeout")
                .long("timeout")
                .value_name("SECONDS")
                .value_parser(clap::value_parser!(u64))
                .required_unless_present("schema"),
        )
        .arg(Arg::new("approve").long("approve").action(ArgAction::SetTrue))
}

/// Parse `start` and its arguments without accessing credentials or a runtime.
///
/// # Errors
/// Rejects unsupported targets/options, repeated arguments and invalid budgets.
pub fn parse_start(
    args: impl IntoIterator<Item = OsString>,
) -> Result<StartCommand, StartArgumentError> {
    let mut args = args.into_iter();
    if args.next().as_deref() != Some(OsStr::new("start")) {
        return Err(StartArgumentError::Invalid);
    }
    let matches = start_command()
        .no_binary_name(true)
        .try_get_matches_from(args)
        .map_err(|error| StartArgumentError::Syntax(error.kind()))?;
    let path = matches.get_one::<String>("path").cloned();
    if let Some(path) = &path {
        mirroring::target(path).map_err(|_| StartArgumentError::Invalid)?;
    }
    let timeout = matches
        .get_one::<u64>("timeout")
        .copied()
        .map(Duration::from_secs);
    if timeout.is_some_and(|value| value.is_zero() || value > Duration::from_secs(86_400)) {
        return Err(StartArgumentError::Invalid);
    }
    Ok(StartCommand {
        path,
        timeout,
        approve: matches.get_flag("approve"),
        schema: matches.get_flag("schema"),
    })
}

/// Immediate host approval for the exact parsed operation and effects.
pub struct Policy<'a>(pub &'a StartCommand);

impl ApprovalPolicy for Policy<'_> {
    type Error = &'static str;

    fn authorize(&mut self, prepared: &PreparedOperation) -> Result<(), Self::Error> {
        if !self.0.approve
            || self.0.schema
            || self.0.path.as_deref() != Some(prepared.target().canonical_path())
            || prepared.operation_id() != mirroring::OPERATION_ID
            || prepared.effects() != mirroring::EFFECTS
        {
            return Err(
                "Explicit approval for this mirroring START is required; no lifecycle mutation was submitted.",
            );
        }
        Ok(())
    }
}

/// One materialized receipt, retained independently of its output writer.
pub struct Delivery {
    /// Common SDK envelope, including unprojected target and uncertainty evidence.
    pub value: Value,
    /// Completed/accepted action 0; failed/unknown/delivery failure 1.
    pub exit: u8,
}

impl Delivery {
    /// Write one newline-terminated receipt to its designated stream and flush.
    ///
    /// Output failure retains prior evidence on stderr and never replays START.
    pub fn write_to(&self, stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
        let output = if self.exit == 0 { stdout } else { &mut *stderr };
        if write_json(output, &self.value).is_ok() {
            return self.exit;
        }
        let value = json!({
            "error": {
                "code": "OutputFailed",
                "message": "START receipt delivery failed; retain the outcome and do not replay."
            },
            "evidence": self.value,
            "remoteCancelled": false,
            "remoteReplayed": false
        });
        let _ = write_json(stderr, &value);
        1
    }
}

fn write_json(output: &mut dyn Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, value).map_err(io::Error::other)?;
    writeln!(output)?;
    output.flush()
}

/// Local refusal or retained SDK evidence, never a successful-looking fallback.
pub enum StartCommandError {
    /// Invalid invocation supplied by an embedded caller, before execution.
    Invalid(&'static str),
    /// The caller's exact target/context could not be prepared.
    Preparation(fabric_sdk::v2::ReceiptError),
    /// Invocation-bound approval was refused before credential or service work.
    Approval(&'static str),
    /// Original SDK error, including any contradictory executed evidence.
    Sdk(Box<mirroring::StartError>),
    /// Serialization failed after execution; retain the complete original result.
    Serialization {
        /// Original serialization failure.
        source: serde_json::Error,
        /// Original execution evidence, independent of receipt delivery.
        execution: Box<mirroring::StartExecution>,
    },
}

impl std::fmt::Debug for StartCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::Invalid(_) => "Invalid",
            Self::Preparation(_) => "Preparation",
            Self::Approval(_) => "Approval",
            Self::Sdk(_) => "Sdk",
            Self::Serialization { .. } => "Serialization",
        };
        formatter
            .debug_tuple("StartCommandError")
            .field(&kind)
            .finish()
    }
}

impl std::fmt::Display for StartCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Approval(message) => formatter.write_str(message),
            _ => formatter.write_str("START failed; inspect retained SDK and execution evidence."),
        }
    }
}

impl std::error::Error for StartCommandError {}

impl StartCommandError {
    /// Project only safe execution facts for a native host's failure envelope.
    ///
    /// The original typed SDK/serialization failure remains available to callers.
    pub fn evidence(&self) -> Value {
        match self {
            Self::Sdk(error) => error.evidence(),
            Self::Serialization { execution, .. } => json!({
                "target": execution.receipt.target(),
                "preconditions": execution.receipt.preconditions(),
                "evidence": execution.receipt.evidence(),
                "remoteCancelled": false,
                "remoteReplayed": false
            }),
            _ => Value::Null,
        }
    }
}

/// Approve and execute through the SDK; this adapter installs no identity or HTTP.
///
/// # Errors
/// Local preparation/approval errors precede dispatch. Native failures return
/// their actual failed/unknown receipt rather than becoming an empty success.
pub async fn dispatch<E: OperationExecutor>(
    command: &StartCommand,
    client: &Client,
    executor: &E,
    limits: OperationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<Delivery, StartCommandError> {
    if command.schema {
        return Err(StartCommandError::Invalid(
            "Schema discovery does not dispatch START.",
        ));
    }
    let path = command
        .path
        .as_deref()
        .ok_or(StartCommandError::Invalid("A START target is required."))?;
    let timeout = command
        .timeout
        .ok_or(StartCommandError::Invalid("A START timeout is required."))?;
    if timeout.is_zero() || timeout > Duration::from_secs(86_400) {
        return Err(StartCommandError::Invalid("The START timeout is invalid."));
    }
    let target = mirroring::target(path).map_err(StartCommandError::Preparation)?;
    let prepared = mirroring::prepare(client, target).map_err(StartCommandError::Preparation)?;
    let approved = prepared
        .authorize(&mut Policy(command))
        .map_err(StartCommandError::Approval)?;
    let execution = mirroring::execute(client, approved, executor, limits, cancelled)
        .await
        .map_err(|error| StartCommandError::Sdk(Box::new(error)))?;
    let exit = u8::from(!matches!(
        execution.receipt.outcome(),
        Outcome::Succeeded { .. } | Outcome::Accepted
    ));
    let value = serde_json::to_value(&execution.receipt).map_err(|source| {
        StartCommandError::Serialization {
            source,
            execution: Box::new(execution),
        }
    })?;
    Ok(Delivery { value, exit })
}
