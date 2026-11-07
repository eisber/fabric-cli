// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Experimental CLI entry point, not a replacement for the Python `fab`.
#![cfg_attr(test, deny(unnameable_test_items))]

#[cfg(feature = "windows-broker")]
mod auth;
#[cfg(all(windows, feature = "windows-broker"))]
mod auth_binding;
mod batch_state;
mod delete_host;
#[cfg(feature = "headless")]
mod headless;
mod local_host;
#[cfg(feature = "native-host")]
mod native_read;
#[cfg(feature = "repl")]
mod repl_host;
mod runtime_config;
#[cfg(feature = "semantic-model")]
mod semantic_model;
#[cfg(feature = "self-update")]
mod update_host;

use batch_state::{BatchState, CommandOutcome};
#[cfg(not(feature = "native-host"))]
use fabric_cli::HostError;
use fabric_cli::parser::{
    Invocation,
    batch::{self, BatchHost, BatchInvocation, CommandOutcome as BatchOutcome, CommandParseError},
    invalid_arguments_message, parse_args,
};
use fabric_cli_output::{json_dumps, text::OutputFormat};
use serde::Serialize;
use std::{
    io::{self, Write},
    process::ExitCode,
};
#[derive(Serialize)]
struct ErrorEnvelope {
    error: CommandError,
}

#[derive(Serialize)]
struct CommandError {
    code: &'static str,
    message: String,
}

#[cfg(feature = "self-update")]
type UpdateLifetime = fabric_cli_update::ParentLifetime;
#[cfg(not(feature = "self-update"))]
type UpdateLifetime = ();

fn run(_update_lifetime: &mut UpdateLifetime) -> io::Result<ExitCode> {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    #[cfg(feature = "v2-preview")]
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "--v2-preview")
    {
        let arguments = arguments
            .iter()
            .take(1)
            .chain(arguments.iter().skip(2))
            .cloned();
        return fabric_cli::v2_preview::run_unconfigured(
            arguments,
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        )
        .map(ExitCode::from);
    }
    #[cfg(feature = "repl")]
    if arguments.len() == 1 {
        return repl_host::run();
    }
    let invocation = parse_args(arguments);
    #[cfg(feature = "semantic-model")]
    if let Ok(Invocation::SemanticModel(command)) = &invocation {
        // Offline analysis validates before update policy or navigation setup.
        return semantic_model::run(command).map(exit_status);
    }
    #[cfg(feature = "v2-cancel")]
    if let Ok(Invocation::Cancel(command)) = &invocation {
        // ID-only cancellation never restores navigation or runs startup updates.
        return dispatch_cancel(command, None).map(exit_status);
    }
    #[cfg(feature = "v2-cancel")]
    if let Err(error) = &invocation {
        return write_error(
            CommandError {
                code: "invalid_arguments",
                message: invalid_arguments_message(error),
            },
            2,
        );
    }
    if let Ok(Invocation::NativeTabular(command)) = &invocation {
        return write_export(fabric_cli::native_tabular::not_configured(command)).map(exit_status);
    }
    #[cfg(feature = "self-update")]
    if let Ok(invocation) = &invocation {
        if let Invocation::Update(command) = invocation {
            return update_host::run(command, _update_lifetime).map(exit_status);
        }
        if let Some(outcome) = update_host::startup(invocation, _update_lifetime)? {
            return Ok(exit_status(outcome));
        }
    }
    let mut state = BatchState::capture();
    dispatch(invocation, &mut state).map(exit_status)
}

fn exit_status(outcome: CommandOutcome) -> ExitCode {
    match outcome {
        CommandOutcome::Returned(exit)
        | CommandOutcome::Stop(exit)
        | CommandOutcome::Cancelled(exit) => ExitCode::from(exit),
    }
}

fn dispatch(
    invocation: Result<Invocation, clap::Error>,
    local_state: &mut BatchState,
) -> io::Result<CommandOutcome> {
    let (error, exit) = match invocation {
        Ok(Invocation::NativeTabular(command)) => {
            return write_export(fabric_cli::native_tabular::not_configured(&command));
        }
        Ok(invocation @ Invocation::Tabular(_)) => {
            let Invocation::Tabular(export) = &invocation else {
                unreachable!("matched tabular export");
            };
            let source =
                fabric_cli::tabular_export::source(&export.command).unwrap_or("unsupported");
            if fabric_cli::tabular_export::preflight(&export.request).is_err() {
                let delivery = fabric_cli::tabular_export::ExportDelivery::preflight_failed(
                    source,
                    &export.request,
                );
                return write_export(delivery);
            }
            if matches!(source, "config ls" | "label list-local") {
                let store = local_host::store().map_err(io::Error::other)?;
                let delivery = fabric_cli::tabular_export::dispatch_local(
                    export,
                    &store,
                    &mut fabric_cli::tabular_export::FileSink::default(),
                )
                .expect("local export selected");
                return write_export(delivery);
            }
            #[cfg(feature = "native-host")]
            {
                return native_read::run_family(&invocation, local_state);
            }
            #[cfg(not(feature = "native-host"))]
            {
                let delivery = fabric_cli::tabular_export::ExportDelivery::host_failed(
                    source,
                    &export.request,
                    "NotConfigured",
                    &HostError::NotConfigured.to_string(),
                    1,
                );
                return write_export(delivery);
            }
        }
        Ok(Invocation::Batch(invocation)) => {
            return batch::run(&invocation, &mut BatchExecution { local_state })
                .map(CommandOutcome::Returned);
        }
        Ok(Invocation::Display(text)) => {
            write!(io::stdout().lock(), "{text}")?;
            return Ok(CommandOutcome::Returned(0));
        }
        #[cfg(feature = "semantic-model")]
        Ok(Invocation::SemanticModel(command)) => return semantic_model::run(&command),
        #[cfg(feature = "v2-cancel")]
        Ok(Invocation::Cancel(command)) => {
            return dispatch_cancel(&command, Some(local_state));
        }
        Ok(Invocation::Delete(command)) => return delete_host::run(&command),
        #[cfg(feature = "self-update")]
        Ok(Invocation::Update(_)) => {
            write_error(
                CommandError {
                    code: "update_context",
                    message: "Run fab-rs update as a separate invocation, outside batches and the interactive shell.".into(),
                },
                2,
            )?;
            return Ok(CommandOutcome::Returned(2));
        }
        #[cfg(feature = "native-host")]
        Ok(invocation @ Invocation::Local(_)) if matches!(&invocation, Invocation::Local(command) if local_host::needs_remote(command, local_state)) =>
        {
            return native_read::run_family(&invocation, local_state);
        }
        Ok(Invocation::Local(command)) => return local_host::run(&command, local_state),
        Ok(Invocation::Access(command))
            if matches!(
                command.kind,
                fabric_cli_resources::frontdoor::AccessKind::LabelListLocal
            ) =>
        {
            return local_host::local_access(&command);
        }
        #[cfg(feature = "native-host")]
        Ok(
            invocation @ (Invocation::Access(_)
            | Invocation::Resource(_)
            | Invocation::Transfer(_)
            | Invocation::Deploy(_)
            | Invocation::Table(_)),
        ) => {
            return native_read::run_family(&invocation, local_state);
        }
        #[cfg(not(feature = "native-host"))]
        Ok(
            Invocation::Access(_)
            | Invocation::Resource(_)
            | Invocation::Transfer(_)
            | Invocation::Deploy(_)
            | Invocation::Table(_),
        ) => (
            CommandError {
                code: "NotConfigured",
                message: fabric_cli::HostError::NotConfigured.to_string(),
            },
            1,
        ),
        #[cfg(feature = "native-host")]
        Ok(Invocation::Read(command)) => return native_read::run(&command, local_state),
        #[cfg(feature = "native-host")]
        Ok(Invocation::Execute(command)) => {
            return native_read::run_execute(&command, local_state);
        }
        #[cfg(feature = "native-host")]
        Ok(Invocation::JobRead(command)) => return native_read::run_job(&command, local_state),
        #[cfg(feature = "native-host")]
        Ok(Invocation::JobRun(command)) => {
            return native_read::run_job_execution(&command, local_state);
        }
        #[cfg(feature = "native-host")]
        Ok(Invocation::JobMutation(command)) => {
            return native_read::run_job_mutation(&command, local_state);
        }
        #[cfg(not(feature = "native-host"))]
        Ok(
            Invocation::Read(_)
            | Invocation::Execute(_)
            | Invocation::JobRead(_)
            | Invocation::JobRun(_)
            | Invocation::JobMutation(_),
        ) => (
            CommandError {
                code: "NotConfigured",
                message: HostError::NotConfigured.to_string(),
            },
            1,
        ),
        #[cfg(feature = "windows-broker")]
        Ok(Invocation::Auth(command)) => return auth::run(&command),
        Err(error) => (
            CommandError {
                code: "invalid_arguments",
                message: invalid_arguments_message(&error),
            },
            2,
        ),
    };
    write_error(error, exit)?;
    Ok(if exit == 2 {
        CommandOutcome::Stop(exit)
    } else {
        CommandOutcome::Returned(exit)
    })
}

#[cfg(feature = "v2-cancel")]
fn dispatch_cancel(
    command: &fabric_cli::parser::CancelCommand,
    state: Option<&mut BatchState>,
) -> io::Result<CommandOutcome> {
    #[cfg(feature = "native-host")]
    {
        native_read::run_cancel(command, state)
    }
    #[cfg(not(feature = "native-host"))]
    {
        let _ = state;
        let delivery = match fabric_cli::cancel::preflight(command) {
            Ok(target) => fabric_cli::cancel::Dispatch::unavailable(
                Some(&target),
                None,
                "NotConfigured",
                "Cancellation requires an explicitly configured credential and HTTP host.",
                1,
            ),
            Err(error) => fabric_cli::cancel::Dispatch::unavailable(
                None,
                None,
                error.code(),
                &error.to_string(),
                2,
            ),
        };
        let exit = delivery.write_to(&mut io::stdout().lock(), &mut io::stderr().lock());
        Ok(if exit != delivery.exit_code() || exit == 2 {
            CommandOutcome::Stop(exit)
        } else {
            CommandOutcome::Returned(exit)
        })
    }
}

fn write_export(
    delivery: fabric_cli::tabular_export::ExportDelivery,
) -> io::Result<CommandOutcome> {
    delivery
        .write_to_streams(&mut io::stdout().lock(), &mut io::stderr().lock())
        .map(CommandOutcome::Returned)
        .map_err(fabric_cli::tabular_export::ReceiptWriteFailure::into_io_error)
}

fn write_error(error: CommandError, exit: u8) -> io::Result<ExitCode> {
    let envelope = ErrorEnvelope { error };
    let mut stderr = io::stderr().lock();
    serde_json::to_writer(&mut stderr, &envelope).map_err(io::Error::other)?;
    writeln!(stderr)?;
    Ok(ExitCode::from(exit))
}

struct BatchExecution<'a> {
    local_state: &'a mut BatchState,
}

impl BatchHost for BatchExecution<'_> {
    type Error = io::Error;

    fn execute(&mut self, arguments: &[&str]) -> io::Result<BatchOutcome> {
        let (invocation, exits) = match batch::parse_command(arguments) {
            Ok(parsed) => parsed,
            Err(CommandParseError::Arguments(error)) => {
                dispatch(Err(error), self.local_state)?;
                return Ok(BatchOutcome::Stop(2));
            }
            Err(CommandParseError::UnknownCommand) => {
                write_error(
                    CommandError {
                        code: "UnexpectedError",
                        message: "Unknown batch command. Run fab-rs --help for available commands and usage.".into(),
                    },
                    1,
                )?;
                return Ok(BatchOutcome::Stop(1));
            }
        };
        #[cfg(feature = "self-update")]
        if matches!(&invocation, Invocation::Update(_)) {
            dispatch(Ok(invocation), self.local_state)?;
            return Ok(BatchOutcome::Stop(2));
        }
        let outcome = dispatch(Ok(invocation), self.local_state)?;
        Ok(match outcome {
            CommandOutcome::Returned(exit) if exits => BatchOutcome::Stop(exit),
            outcome => outcome.into(),
        })
    }

    fn separator(&mut self) -> io::Result<()> {
        writeln!(io::stderr().lock(), "------------------------------")
    }

    fn summary(&mut self, invocation: &BatchInvocation) -> io::Result<()> {
        let configured = if invocation.output_format.is_none() {
            local_host::available_store()
                .map_err(io::Error::other)?
                .map(|store| store.read())
                .transpose()
                .map_err(io::Error::other)?
        } else {
            None
        };
        let format = OutputFormat::resolve(
            invocation.output_format.as_deref(),
            configured
                .as_ref()
                .and_then(|settings| settings.get("output_format"))
                .and_then(serde_json::Value::as_str)
                .or(Some("text")),
        )
        .map_err(io::Error::other)?;
        let message = format!("{} commands executed.", invocation.commands.len());
        let mut stdout = io::stdout().lock();
        write!(stdout, "\n\n")?;
        if format == OutputFormat::Text {
            writeln!(stdout, "\n* {message}\n")?;
        } else {
            // The source summary has a list-valued command, unlike per-command
            // envelopes. Reuse the shared Python string encoder for every value.
            let quoted = |value: &str| {
                json_dumps(&serde_json::Value::String(value.into())).map_err(io::Error::other)
            };
            writeln!(
                stdout,
                "{{\n    \"timestamp\": {},\n    \"status\": \"Success\",\n    \"command\": [",
                quoted(local_host::timestamp().map_err(io::Error::other)?.as_str())?
            )?;
            for (index, command) in invocation.commands.iter().enumerate() {
                writeln!(
                    stdout,
                    "        {}{}",
                    quoted(command)?,
                    if index + 1 == invocation.commands.len() {
                        ""
                    } else {
                        ","
                    }
                )?;
            }
            writeln!(
                stdout,
                "    ],\n    \"result\": {{\n        \"message\": {}\n    }}\n}}",
                quoted(&message)?
            )?;
        }
        Ok(())
    }
}

fn main() -> ExitCode {
    #[cfg(feature = "self-update")]
    let mut update_lifetime = fabric_cli_update::ParentLifetime::new();
    #[cfg(not(feature = "self-update"))]
    let mut update_lifetime = ();
    match run(&mut update_lifetime) {
        Ok(code) => code,
        Err(error) => {
            if error
                .get_ref()
                .and_then(|error| {
                    error.downcast_ref::<fabric_cli::tabular_export::ReceiptWriteFailure>()
                })
                .is_none()
            {
                eprintln!("fab-rs: output failed: {error}");
            }
            ExitCode::FAILURE
        }
    }
}
