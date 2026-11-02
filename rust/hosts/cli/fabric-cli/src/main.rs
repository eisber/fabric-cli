// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Experimental CLI entry point, not a replacement for the Python `fab`.

#[cfg(feature = "windows-broker")]
mod auth;
mod local_host;
#[cfg(feature = "native-read")]
mod native_read;

#[cfg(not(feature = "native-read"))]
use fabric_cli::HostError;
use fabric_cli::parser::{
    Invocation,
    batch::{self, BatchHost, BatchInvocation, CommandOutcome, CommandParseError},
    invalid_arguments_message, parse_args,
};
use fabric_cli_local::NavigationState;
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

fn run() -> io::Result<ExitCode> {
    dispatch(parse_args(std::env::args_os()), &mut None)
}

fn dispatch(
    invocation: Result<Invocation, clap::Error>,
    local_state: &mut Option<NavigationState>,
) -> io::Result<ExitCode> {
    let (error, exit) = match invocation {
        Ok(Invocation::Batch(invocation)) => {
            return batch::run(&invocation, &mut BatchExecution { local_state: None })
                .map(ExitCode::from);
        }
        Ok(Invocation::Display(text)) => {
            write!(io::stdout().lock(), "{text}")?;
            return Ok(ExitCode::SUCCESS);
        }
        #[cfg(feature = "native-read")]
        Ok(invocation @ Invocation::Local(_)) if matches!(&invocation, Invocation::Local(command) if local_host::needs_remote(command)) =>
        {
            return native_read::run_family(&invocation);
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
        #[cfg(feature = "native-read")]
        Ok(
            invocation @ (Invocation::Access(_)
            | Invocation::Resource(_)
            | Invocation::Transfer(_)
            | Invocation::Deploy(_)
            | Invocation::Table(_)),
        ) => {
            return native_read::run_family(&invocation);
        }
        #[cfg(not(feature = "native-read"))]
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
        #[cfg(feature = "native-read")]
        Ok(Invocation::Read(command)) => return native_read::run(&command),
        #[cfg(feature = "native-read")]
        Ok(Invocation::JobRead(command)) => return native_read::run_job(&command),
        #[cfg(feature = "native-read")]
        Ok(Invocation::JobRun(command)) => return native_read::run_job_execution(&command),
        #[cfg(feature = "native-read")]
        Ok(Invocation::JobMutation(command)) => return native_read::run_job_mutation(&command),
        #[cfg(not(feature = "native-read"))]
        Ok(
            Invocation::Read(_)
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
    write_error(error, exit)
}

fn write_error(error: CommandError, exit: u8) -> io::Result<ExitCode> {
    let envelope = ErrorEnvelope { error };
    let mut stderr = io::stderr().lock();
    serde_json::to_writer(&mut stderr, &envelope).map_err(io::Error::other)?;
    writeln!(stderr)?;
    Ok(ExitCode::from(exit))
}

struct BatchExecution {
    local_state: Option<NavigationState>,
}

impl BatchHost for BatchExecution {
    type Error = io::Error;

    fn execute(&mut self, arguments: &[&str]) -> io::Result<CommandOutcome> {
        let (invocation, exits) = match batch::parse_command(arguments) {
            Ok(parsed) => parsed,
            Err(CommandParseError::Arguments(error)) => {
                dispatch(Err(error), &mut self.local_state)?;
                return Ok(CommandOutcome::Stop(2));
            }
            Err(CommandParseError::UnknownCommand) => {
                write_error(
                    CommandError {
                        code: "UnexpectedError",
                        message: "Unknown batch command. Run fab-rs --help for available commands and usage.".into(),
                    },
                    1,
                )?;
                return Ok(CommandOutcome::Stop(1));
            }
        };
        #[cfg(feature = "windows-broker")]
        if matches!(&invocation, Invocation::Auth(_)) {
            write_error(
                CommandError {
                    code: "NotSupported",
                    message: "Batch authentication requires a host that distinguishes cancellation from handled failure. Use a separate explicit auth invocation.".into(),
                },
                1,
            )?;
            return Ok(CommandOutcome::Stop(1));
        }
        #[cfg(feature = "native-read")]
        if matches!(&invocation, Invocation::Local(command)
            if matches!(command.command, fabric_cli_local::LocalCommand::ChangeDirectory { .. })
                && local_host::needs_remote(command))
        {
            write_error(
                CommandError {
                    code: "NotSupported",
                    message: "Remote batch navigation requires a shared native context host; this command was not executed.".into(),
                },
                1,
            )?;
            return Ok(CommandOutcome::Stop(1));
        }
        let code = dispatch(Ok(invocation), &mut self.local_state)?;
        // Existing executable dispatchers return only ExitCode. Two is terminal
        // misuse/cancellation, whereas handled command failures (1/4) continue.
        let exit = if code == ExitCode::SUCCESS {
            0
        } else if code == ExitCode::from(2) {
            2
        } else if code == ExitCode::from(4) {
            4
        } else {
            1
        };
        Ok(if exits || exit == 2 {
            CommandOutcome::Stop(exit)
        } else {
            CommandOutcome::Returned(exit)
        })
    }

    fn separator(&mut self) -> io::Result<()> {
        writeln!(io::stderr().lock(), "------------------------------")
    }

    fn summary(&mut self, invocation: &BatchInvocation) -> io::Result<()> {
        let configured = if invocation.output_format.is_none()
            && std::env::var_os("FAB_RUST_CONFIG_ROOT").is_some()
        {
            Some(
                local_host::store()
                    .map_err(io::Error::other)?
                    .read()
                    .map_err(io::Error::other)?,
            )
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
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("fab-rs: output failed: {error}");
            ExitCode::FAILURE
        }
    }
}
