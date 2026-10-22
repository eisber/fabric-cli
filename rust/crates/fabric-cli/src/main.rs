// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Temporary offline CLI entry point, not a replacement for the Python `fab`.

use clap::{Command, error::ErrorKind};
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
    message: &'static str,
}

fn run() -> io::Result<ExitCode> {
    let result = Command::new("fab-rs")
        .bin_name("fab-rs")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Experimental offline scaffold for Microsoft Fabric")
        .after_help("Only help and version are implemented. No authentication, service, or deployment commands are available.")
        .arg_required_else_help(true)
        .try_get_matches();
    match result {
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            write!(io::stdout().lock(), "{error}")?;
            Ok(ExitCode::SUCCESS)
        }
        _ => {
            let envelope = ErrorEnvelope {
                error: CommandError {
                    code: "invalid_arguments",
                    message: "Only --help and --version are available in this scaffold.",
                },
            };
            let mut stderr = io::stderr().lock();
            serde_json::to_writer(&mut stderr, &envelope).map_err(io::Error::other)?;
            writeln!(stderr)?;
            Ok(ExitCode::from(2))
        }
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
