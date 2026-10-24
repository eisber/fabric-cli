// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Temporary offline CLI entry point, not a replacement for the Python `fab`.

use fabric_cli::{
    HostError,
    parser::{INVALID_ARGUMENTS, Invocation, parse_args},
};
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
        Ok(Invocation::Read(_)) => (
            CommandError {
                code: "NotConfigured",
                message: HostError::NotConfigured.to_string(),
            },
            1,
        ),
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
