// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Typed native stop schema over the unchanged library state machine.

use clap::{Arg, ArgAction, Command, error::ErrorKind};
use fabric_cli_mutations::v2_stop;
use fabric_sdk::{
    Client, mirroring, mirroring_stop,
    v2::{ApprovalPolicy, PreparedOperation},
};
use fabric_sdk_operations::{OperationExecutor, OperationLimits};
use serde_json::{Value, json};
use std::{
    ffi::{OsStr, OsString},
    future::Future,
    time::Duration,
};

/// Native-only local deadline and optional library intent.
#[derive(Clone, Debug)]
pub struct StopCommand {
    command: Option<v2_stop::StopCommand>,
    path: Option<String>,
    timeout: Option<Duration>,
}

impl StopCommand {
    /// Explicit execution budget; absent only for local schema discovery.
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// True only for local schema discovery.
    pub fn schema(&self) -> bool {
        self.command.is_none()
    }
}

/// Sanitized local failure, with no supplied values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ArgumentError {
    /// Parser/help classification without raw parser diagnostics.
    #[error("Invalid stop arguments; run stop --help.")]
    Syntax(ErrorKind),
    /// Exact target and finite positive timeout are required for execution.
    #[error(
        "Stop requires an exact MirroredDatabase runtime/mirroring target and a positive --timeout of at most 86400 seconds."
    )]
    Invalid,
}

/// Extend the library schema with native-only local schema and deadline controls.
pub fn command() -> Command {
    v2_stop::command()
        .long_about("Stop one exact MirroredDatabase runtime/mirroring target. Host policy governs observations and optional mutation separately. --yes records intent, not authorization. Stopped and Stopping require no --yes and submit no write. Acceptance is not confirmation of Stopped. One explicit total deadline covers setup, precheck, optional stop and readback. No polling, retry, remote cancellation or START substitution.")
        .mut_arg("path", |arg| arg.required(false).required_unless_present("schema"))
        .arg(Arg::new("schema").long("schema").action(ArgAction::SetTrue)
            .conflicts_with_all(["path", "timeout", "yes", "format"]))
        .arg(Arg::new("timeout").long("timeout").value_name("SECONDS")
            .value_parser(clap::value_parser!(u64)).required_unless_present("schema"))
}

/// Parse once before any identity, runtime or transport setup.
///
/// # Errors
/// Rejects invalid targets, repeated flags, unsupported controls and timeouts.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<StopCommand, ArgumentError> {
    let mut args = args.into_iter();
    if args.next().as_deref() != Some(OsStr::new("stop")) {
        return Err(ArgumentError::Invalid);
    }
    let matches = command()
        .no_binary_name(true)
        .try_get_matches_from(args)
        .map_err(|error| ArgumentError::Syntax(error.kind()))?;
    if matches.get_flag("schema") {
        return Ok(StopCommand {
            command: None,
            path: None,
            timeout: None,
        });
    }
    let path = matches
        .get_one::<String>("path")
        .ok_or(ArgumentError::Invalid)?;
    let seconds = *matches
        .get_one::<u64>("timeout")
        .ok_or(ArgumentError::Invalid)?;
    if !(1..=86_400).contains(&seconds) {
        return Err(ArgumentError::Invalid);
    }
    let command = v2_stop::StopCommand::new(path, matches.get_flag("yes"))
        .map_err(|_| ArgumentError::Invalid)?;
    Ok(StopCommand {
        command: Some(command),
        path: Some(path.clone()),
        timeout: Some(Duration::from_secs(seconds)),
    })
}

/// Invocation-bound policy, never a flag-only authorization fallback.
pub struct Policy<'a>(pub &'a StopCommand);

impl ApprovalPolicy for Policy<'_> {
    type Error = &'static str;

    fn authorize(&mut self, prepared: &PreparedOperation) -> Result<(), Self::Error> {
        let Some(command) = &self.0.command else {
            return Err("Schema is local only.");
        };
        let effects = prepared.effects();
        if self.0.path.as_deref() != Some(prepared.target().canonical_path())
            || prepared.operation_id() != mirroring_stop::OPERATION_ID
            || prepared.input() != &json!({"yes":command.approved()})
            || !(effects == mirroring_stop::READ_EFFECTS
                || (command.approved() && effects == mirroring_stop::WRITE_EFFECTS))
        {
            return Err("Host policy refused the exact stop target, intent or effects.");
        }
        Ok(())
    }
}

/// Native dispatch failure before the SDK stop state machine starts.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// Local/schema invocations cannot execute.
    #[error("Stop execution requires a validated target and deadline.")]
    Invalid,
    /// Exact target/context preparation failed.
    #[error("Stop target or context could not be prepared.")]
    Preparation(#[source] fabric_sdk::v2::ReceiptError),
    /// Host policy refused observation before credentials or service dispatch.
    #[error("Host policy refused stop observation.")]
    Approval(&'static str),
}

/// Preserved library result plus materialized JSON, independently of delivery.
pub struct Delivery {
    /// Original typed SDK and policy evidence, even when both streams fail.
    pub execution: mirroring_stop::StopExecution<&'static str>,
    /// Safe JSON selected by the preserved library adapter.
    pub value: Value,
}

impl Delivery {
    /// Deliver once with the library's evidence-preserving failure behavior.
    pub fn write_to(&self, stdout: &mut dyn std::io::Write, stderr: &mut dyn std::io::Write) -> u8 {
        let exit = u8::from(self.execution.result.is_err() || self.execution.interrupted);
        let written = if exit == 0 {
            write_json(stdout, &self.value)
        } else {
            write_json(&mut *stderr, &self.value)
        };
        if written.is_ok() {
            return exit;
        }
        let _ = write_json(
            stderr,
            &json!({
                "operation":mirroring_stop::OPERATION_ID,
                "error":{"code":"OutputFailed","message":"Stop evidence delivery failed; do not replay the mutation."},
                "originalResult":self.value
            }),
        );
        1
    }
}

fn write_json(writer: &mut dyn std::io::Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value).map_err(std::io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Execute with caller-injected policy, context, transport and one total signal.
///
/// # Errors
/// Local preparation and read-policy failures precede every remote request.
pub async fn dispatch<E: OperationExecutor, P: ApprovalPolicy<Error = &'static str> + Send>(
    command: &StopCommand,
    client: &Client,
    executor: &E,
    policy: &mut P,
    limits: OperationLimits,
    interrupted: impl Future<Output = ()> + Send,
) -> Result<Delivery, CommandError> {
    let library = command.command.as_ref().ok_or(CommandError::Invalid)?;
    let target = mirroring::target(command.path.as_deref().ok_or(CommandError::Invalid)?)
        .map_err(CommandError::Preparation)?;
    let prepared = mirroring_stop::prepare(client, target, library.approved())
        .map_err(CommandError::Preparation)?;
    let approved = prepared.authorize(policy).map_err(CommandError::Approval)?;
    let execution = mirroring_stop::execute(approved, executor, policy, limits, interrupted).await;
    let mut value = v2_stop::result_metadata(library, &execution.result);
    value["remoteCancelled"] = json!(false);
    value["remoteReplayed"] = json!(false);
    value["localObservationInterrupted"] = json!(execution.interrupted);
    if execution.interrupted && execution.result.is_ok() {
        value["error"] = json!({
            "code":"Interrupted",
            "message":"Local observation was interrupted; retain the received result and do not replay."
        });
    }
    if execution.policy_error.is_some() {
        value["hostPolicyDenied"] = json!(true);
    }
    Ok(Delivery { execution, value })
}
