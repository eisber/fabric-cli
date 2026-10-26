// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Experimental CLI entry point, not a replacement for the Python `fab`.

#[cfg(feature = "windows-broker")]
mod auth;
#[cfg(feature = "native-read")]
mod native_read;

#[cfg(not(feature = "native-read"))]
use fabric_cli::HostError;
use fabric_cli::parser::{INVALID_ARGUMENTS, Invocation, parse_args};
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
        Ok(Invocation::Read(command)) => return native_read::run(&command),
        #[cfg(feature = "native-read")]
        Ok(Invocation::JobRead(command)) => return native_read::run_job(&command),
        #[cfg(not(feature = "native-read"))]
        Ok(Invocation::Read(_) | Invocation::JobRead(_)) => (
            CommandError {
                code: "NotConfigured",
                message: HostError::NotConfigured.to_string(),
            },
            1,
        ),
        #[cfg(feature = "windows-broker")]
        Ok(Invocation::Auth(command)) => return auth::run(&command),
        Err(_) => (
            CommandError {
                code: "invalid_arguments",
                message: INVALID_ARGUMENTS.into(),
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
