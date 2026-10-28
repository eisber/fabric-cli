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
use fabric_cli::parser::{Invocation, invalid_arguments_message, parse_args};
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
    let (error, exit) = match parse_args(std::env::args_os()) {
        Ok(Invocation::Display(text)) => {
            write!(io::stdout().lock(), "{text}")?;
            return Ok(ExitCode::SUCCESS);
        }
        #[cfg(feature = "native-read")]
        Ok(invocation @ Invocation::Local(_)) if matches!(&invocation, Invocation::Local(command) if local_host::needs_remote(command)) =>
        {
            return native_read::run_family(&invocation);
        }
        Ok(Invocation::Local(command)) => return local_host::run(&command),
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
    let envelope = ErrorEnvelope { error };
    let mut stderr = io::stderr().lock();
    serde_json::to_writer(&mut stderr, &envelope).map_err(io::Error::other)?;
    writeln!(stderr)?;
    Ok(ExitCode::from(exit))
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
