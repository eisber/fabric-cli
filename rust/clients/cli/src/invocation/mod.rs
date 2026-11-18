// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

mod dispatch;
mod parser;

pub use parser::parser;
pub(crate) use parser::{parse_arguments, welcome_requested};

use crate::{
    Host, console,
    output::{self, write_failure},
};
use clap::error::ErrorKind as ParseErrorKind;
use fabric_cli_command_contracts::{CommandDiagnostics, CommandError};
use fabric_core::{
    CancellationToken,
    diagnostics::{DiagnosticPhase, DiagnosticStage},
};
use parser::{command_name, parse_failure_message, parsed_command_name};
use std::{ffi::OsString, io::Write, time::Duration};

pub(super) fn welcome(stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
    let output = "Welcome to fab-rs.\n\n\n\
                  Get started:\n  fab-rs api workspaces   List workspaces\n  fab-rs auth             Authentication commands\n  fab-rs --help           All commands\n";
    if stdout.write_all(output.as_bytes()).is_err() {
        let _ = stderr.write_all(b"Cannot write welcome output\n");
        return 1;
    }
    0
}

/// Run the real parser/host/output with injected effects and streams.
///
/// Supply the executable name as the first argument, as with `std::env::args_os`.
/// Metadata returns before touching any host hook, selection, credential, or network.
/// No arguments shows a static welcome screen without touching host hooks.
pub async fn run(
    args: impl IntoIterator<Item = OsString>,
    host: &dyn Host,
    cancellation: CancellationToken,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let args: Vec<OsString> = args.into_iter().collect();
    if welcome_requested(&args) {
        return welcome(stdout, stderr);
    }
    let name = command_name(&args);
    let invocation = match parse_arguments(&args) {
        Ok(invocation) => invocation,
        Err(error)
            if matches!(
                error.kind(),
                ParseErrorKind::DisplayHelp | ParseErrorKind::DisplayVersion
            ) =>
        {
            return if stdout.write_all(error.to_string().as_bytes()).is_ok() {
                0
            } else {
                1
            };
        }
        Err(error) => {
            // Clap diagnostics can echo raw input containing header credentials. Only
            // argument definitions from the parser (never user-supplied values) are copied.
            let message = parse_failure_message(name, &error);
            return write_failure(
                host,
                name,
                CommandError::new("InvalidInput", message.as_str()),
                stdout,
                stderr,
            );
        }
    };
    let matches = invocation.matches();
    let name = parsed_command_name(matches);
    let (observer, events) =
        console::observe(host, matches.get_flag("verbose"), cancellation.clone());
    let execution = async {
        observer
            .stage(
                DiagnosticStage::Command,
                DiagnosticPhase::Started,
                Duration::ZERO,
            )
            .await?;
        dispatch::execute(&invocation, host, cancellation, &observer).await
    };
    let (result, stderr_broken) = console::drive(host, stderr, &observer, events, execution).await;
    match result {
        Ok(data) => {
            let output = output::success(&output::timestamp(host), name, data);
            if stdout.write_all(output.as_bytes()).is_ok() {
                0
            } else {
                if !stderr_broken {
                    let _ = stderr.write_all(b"Cannot write JSON output\n");
                }
                1
            }
        }
        Err(error) => {
            if stderr_broken {
                write_failure(host, name, error, stdout, &mut std::io::sink())
            } else {
                write_failure(host, name, error, stdout, stderr)
            }
        }
    }
}
