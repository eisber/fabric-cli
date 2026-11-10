// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standalone, ID-only CLI v2 mirroring-stop adapter.
//!
//! This source-only library adapter is unmounted and unvalidated by Cargo checks.
//! It establishes no shared registry, native-host or MCP execution coverage.
//! Hosts must parse before acquiring credentials, render help from [`command`],
//! and inject their existing [`ResourceOperations`] with one shared deadline and
//! cancellation budget. `--yes` records explicit intent only: it is not provider
//! authorization and never bypasses the host's approval policy. Before execution,
//! the host must enforce its full effect and authorization policy; the shared
//! descriptor/prepared/approved-operation and native wiring remain host-owned.
//! The SDK alone owns precondition reads, the explicit-intent gate, submission
//! and readback. There is no runtime, prompt, discovery, retry, polling or remote
//! compensation.

use clap::{Arg, ArgAction, Command, error::ErrorKind};
use fabric_sdk_items::stop::{
    self, MirroringState, MirroringTarget, StopError, StopOptions, StopOutcome, StopReceipt,
};
use fabric_sdk_workspaces::mutations::{ResourceOperations, ResourceOutcome};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fmt,
    io::{self, Write},
};

/// Stable identity of the bounded mirroring-stop operation.
pub const OPERATION_ID: &str = "fabric.stop";

/// Locally validated invocation, not existence, host-policy or service permission.
///
/// Private fields prevent bypassing the exact path/format preflight boundary.
#[derive(Clone, Debug)]
pub struct StopCommand {
    target: MirroringTarget,
    approved: bool,
}

impl StopCommand {
    /// Exact authoritative IDs, preserving the caller's GUID spelling.
    pub fn target(&self) -> &MirroringTarget {
        &self.target
    }

    /// Explicit `--yes` intent, not host-policy approval or provider authorization.
    pub const fn approved(&self) -> bool {
        self.approved
    }
}

/// Sanitized parser classification; supplied values are never retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgumentError {
    /// Clap classification, including help, without raw parser diagnostics.
    Syntax(ErrorKind),
    /// Not the advertised absolute, typed, GUID-addressed mirroring runtime.
    InvalidTarget,
}

impl ArgumentError {
    /// Stable local diagnostic code, never a provider-supplied key.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Syntax(_) => "InvalidArguments",
            Self::InvalidTarget => "InvalidTarget",
        }
    }
}

impl fmt::Display for ArgumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "Invalid stop arguments; use stop --help. An exact GUID-addressed MirroredDatabase runtime/mirroring target and JSON format are required",
        )
    }
}

impl std::error::Error for ArgumentError {}

/// Build the unmounted leaf schema; only explicit approval and JSON are exposed.
///
/// Hosts handle help from this schema before service/credential setup. No wait,
/// timeout, facade, force, projection, file-output or name-resolution controls are
/// advertised in this bounded slice.
pub fn command() -> Command {
    Command::new("stop")
        .about("Stop replication for one exact MirroredDatabase mirroring runtime")
        .long_about(
            "Unmounted, Cargo-unvalidated library adapter, not an installed executable command. \
             Address /<workspace-guid>.Workspace/<item-guid>.MirroredDatabase/runtime/mirroring. \
             Reads status before requiring --yes for a Running mirror. Stopped and \
             Stopping do not submit a write. Accepted or requestCompleted does not \
             confirm Stopped. --yes expresses intent, not host or provider authorization, \
             and never bypasses the host's approval policy. No shared registry, native-host \
             or MCP execution coverage is established. No polling, automatic replay \
             or remote cancellation.",
        )
        .infer_long_args(false)
        .infer_subcommands(false)
        .args_override_self(false)
        .arg(Arg::new("path").required(true).num_args(1))
        .arg(Arg::new("yes").long("yes").action(ArgAction::SetTrue).help(
            "Express stop intent for Running; never bypass host approval or provider authorization",
        ))
        .arg(
            Arg::new("format")
                .long("format")
                .value_name("FORMAT")
                .num_args(1)
                .value_parser(["json"])
                .help("Result format: json (default and only supported format)"),
        )
        .after_help(
            "Effects: stops replication; preserves the item and replicated data according \
             to the provider contract. Does not shut down the source database or guarantee \
             zero cost or draining. For Azure SQL-backed mirrors, stopping and starting \
             fully reinitializes replication; this warning is provider-specific.",
        )
}

/// Parse argv including a leading command token, without I/O or authentication.
///
/// For example, `parse_args(["stop", path, "--yes", "--format", "json"])`.
/// The caller must complete this preflight before setting up credentials or an
/// executor. Help is classified by [`ArgumentError::Syntax`] and rendered by the
/// host from [`command`], never copied from a raw Clap diagnostic.
///
/// # Errors
/// Rejects invalid paths, additional targets, unsupported controls and non-JSON
/// formats without echoing any supplied value.
pub fn parse_args<I, T>(args: I) -> Result<StopCommand, ArgumentError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = command()
        .try_get_matches_from(args)
        .map_err(|error| ArgumentError::Syntax(error.kind()))?;
    let path = matches
        .get_one::<String>("path")
        .ok_or(ArgumentError::InvalidTarget)?;
    Ok(StopCommand {
        target: parse_target(path)?,
        approved: matches.get_flag("yes"),
    })
}

/// Validate only the absolute typed mirroring facet; no discovery or aliasing.
///
/// GUID validation is delegated to the SDK's authoritative target constructor.
///
/// # Errors
/// Rejects names, URLs, relative paths, other types/facets, root aliases,
/// traversal, lists, wildcards, extra separators and non-GUID identities.
pub fn parse_target(path: &str) -> Result<MirroringTarget, ArgumentError> {
    let path = path.strip_prefix('/').ok_or(ArgumentError::InvalidTarget)?;
    let mut parts = path.split('/');
    let workspace = parts
        .next()
        .and_then(|part| part.strip_suffix(".Workspace"))
        .ok_or(ArgumentError::InvalidTarget)?;
    let item = parts
        .next()
        .and_then(|part| part.strip_suffix(".MirroredDatabase"))
        .ok_or(ArgumentError::InvalidTarget)?;
    if parts.next() != Some("runtime")
        || parts.next() != Some("mirroring")
        || parts.next().is_some()
    {
        return Err(ArgumentError::InvalidTarget);
    }
    MirroringTarget::new(workspace, item).map_err(|_| ArgumentError::InvalidTarget)
}

/// Delegate one invocation to the shared SDK using the caller-owned executor.
///
/// The SDK's `approved` option records only validated explicit `--yes` intent.
/// Before calling, the host must independently enforce its full effect and
/// authorization policy. This function does not implement or bypass the host's
/// approval policy, shared approved-operation contract or provider permissions.
/// No credential, transport, timer or operation-monitor implementation is created.
///
/// # Errors
/// Returns the original SDK error, including native uncertainty and any retained
/// completed submission evidence. A failed operation is never replayed.
pub async fn execute(
    command: &StopCommand,
    operations: &mut dyn ResourceOperations,
) -> Result<StopReceipt, StopError> {
    stop::stop(
        operations,
        &command.target,
        StopOptions {
            approved: command.approved,
        },
    )
    .await
}

/// Original SDK evidence and the independent output-delivery result.
///
/// Output failure does not replace the operation result or suggest rollback.
#[derive(Debug)]
pub struct StopRun {
    /// Original successful receipt or structured SDK failure, without projection.
    pub result: Result<StopReceipt, StopError>,
    /// Whether the selected stream failed to write, terminate or flush the JSON.
    pub output_failed: bool,
}

impl StopRun {
    /// Zero for a delivered receipt, one for SDK failure or output failure.
    ///
    /// Acceptance and existing Stopping are valid receipts, not proof of Stopped.
    /// Invalid invocation handling belongs before this boundary and uses exit 2.
    pub fn exit_code(&self) -> u8 {
        u8::from(self.output_failed || self.result.is_err())
    }
}

/// Execute a prevalidated invocation and deliver its sanitized JSON once.
///
/// As with [`execute`], the caller must enforce full host effect/authorization
/// policy first; a parsed `--yes` is intent only, not a policy approval token.
/// The original SDK result is always returned, even if both streams are broken.
/// A failed stdout delivery gets a best-effort stderr diagnostic containing the
/// same safe evidence. Nothing reexecutes or compensates the remote operation.
pub async fn run(
    command: &StopCommand,
    operations: &mut dyn ResourceOperations,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> StopRun {
    let result = execute(command, operations).await;
    let output_failed = write_result(command, &result, stdout, stderr).is_err();
    StopRun {
        result,
        output_failed,
    }
}

/// Render safe outcome/observation evidence, excluding arbitrary service data.
///
/// The SDK receipt/error remains the authoritative native evidence. Completed
/// response bodies, raw provider keys/messages and native error codes are not
/// copied to JSON. Actual accepted operation IDs are retained when supplied.
pub fn result_metadata(command: &StopCommand, result: &Result<StopReceipt, StopError>) -> Value {
    match result {
        Ok(receipt) => receipt_metadata(receipt),
        Err(error) => json!({
            "operation": OPERATION_ID,
            "error": {
                "code": error.code(),
                "message": "Stop failed or is incomplete; inspect retained evidence. No write is retried automatically."
            },
            "target": target_metadata(&command.target),
            "mutationMayHaveOccurred": error.mutation_may_have_occurred,
            "stoppedConfirmed": false,
            "requestEvidence": error.receipt().map(receipt_metadata),
            "responseEvidence": error.response_outcome().map(request_metadata),
            "executionEvidence": error.resource_error().map(|operation| json!({
                "httpStatus": operation.status,
                "operationId": operation.operation_id,
                "mutationMayHaveOccurred": operation.mutation_may_have_occurred
            })),
            "effects": effects(),
            "warnings": warnings()
        }),
    }
}

/// Deliver newline-terminated JSON to stdout for receipts, stderr for failures.
///
/// # Errors
/// Returns the original output error after attempting a sanitized stderr fallback
/// containing the original result evidence. The caller must retain `result`,
/// return nonzero, and never reexecute to repair output. Partial stream writes
/// cannot be undone; even fallback delivery is only best effort.
pub fn write_result(
    command: &StopCommand,
    result: &Result<StopReceipt, StopError>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<()> {
    let document = result_metadata(command, result);
    let written = if result.is_ok() {
        write_json(stdout, &document)
    } else {
        write_json(stderr, &document)
    };
    if let Err(error) = written {
        let fallback = json!({
            "operation": OPERATION_ID,
            "error": {
                "code": "OutputFailed",
                "message": "Could not deliver stop evidence. Output failure does not roll back remote effects; do not retry the stop to repair output."
            },
            "originalResult": document
        });
        let _ = write_json(stderr, &fallback);
        return Err(error);
    }
    Ok(())
}

/// Write a sanitized invalid-invocation diagnostic to stderr; hosts use exit 2.
///
/// Hosts render help directly from [`command`] instead of calling this for
/// Clap's help classification. No rejected path, value or parser text is echoed.
///
/// # Errors
/// Returns output failure without attempting any service work.
pub fn write_argument_error(error: ArgumentError, stderr: &mut dyn Write) -> io::Result<()> {
    write_json(
        stderr,
        &json!({"error": {"code": error.code(), "message": error.to_string()}}),
    )
}

fn receipt_metadata(receipt: &StopReceipt) -> Value {
    let outcome = match receipt.outcome {
        StopOutcome::AlreadyStopped => "noOp",
        StopOutcome::AlreadyStopping => "alreadyStopping",
        StopOutcome::Accepted => "accepted",
        StopOutcome::RequestCompleted => "requestCompleted",
        StopOutcome::Stopped => "stopped",
    };
    json!({
        "operation": OPERATION_ID,
        "outcome": outcome,
        "target": target_metadata(&receipt.target),
        "observedState": receipt.observed_state.map(MirroringState::as_str),
        "requestOutcome": receipt.request_outcome.as_ref().map(request_metadata),
        "mutationMayHaveOccurred": receipt.mutation_may_have_occurred,
        "stoppedConfirmed": receipt.stopped_confirmed(),
        "effects": effects(),
        "warnings": warnings()
    })
}

fn target_metadata(target: &MirroringTarget) -> Value {
    json!({
        "path": format!(
            "/{}.Workspace/{}.MirroredDatabase/runtime/mirroring",
            target.workspace_id(), target.item_id()
        ),
        "workspaceId": target.workspace_id(),
        "itemId": target.item_id(),
        "type": "MirroredDatabase",
        "facet": "runtime/mirroring"
    })
}

fn request_metadata(outcome: &ResourceOutcome) -> Value {
    match outcome {
        ResourceOutcome::Completed { status, .. } => {
            json!({"status": "completed", "httpStatus": status})
        }
        ResourceOutcome::Accepted { operation_id } => {
            json!({"status": "accepted", "operationId": operation_id})
        }
    }
}

fn effects() -> Value {
    json!({
        "intent": "stopReplication",
        "itemAndReplicatedData": "preservedAccordingToProviderContract",
        "sourceDatabaseShutdown": false,
        "zeroCostGuaranteed": false,
        "drainGuaranteed": false
    })
}

fn warnings() -> Value {
    json!([
        {
            "code": "AzureSqlReplicationReinitialization",
            "appliesTo": "AzureSql",
            "message": "For Azure SQL-backed mirrors, stopping and starting fully reinitializes replication. This behavior is provider-specific."
        },
        {
            "code": "NoRemoteRollback",
            "message": "Local interruption, timeout or output failure does not cancel remote work or prove rollback. Effects describe the operation intent, not confirmed completion."
        }
    ])
}

fn write_json(writer: &mut dyn Write, document: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, document).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}
