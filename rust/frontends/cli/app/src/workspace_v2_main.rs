// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in workspace v2 executable; existing local and native GET previews are unchanged.
#![cfg_attr(test, deny(unnameable_test_items))]

#[cfg(all(feature = "native-host", feature = "windows-broker"))]
#[allow(
    dead_code,
    reason = "Only silent native credential composition is reused."
)]
mod auth;
#[cfg(all(feature = "native-host", windows, feature = "windows-broker"))]
#[allow(
    dead_code,
    reason = "Only silent native credential composition is reused."
)]
mod auth_binding;
#[cfg(feature = "native-host")]
#[allow(
    dead_code,
    reason = "Native composition does not dispatch legacy commands."
)]
mod batch_state;
#[cfg(feature = "native-host")]
#[allow(
    dead_code,
    reason = "Shared native deletion composition is not mounted here."
)]
mod delete_host;
#[cfg(feature = "headless")]
#[allow(
    dead_code,
    reason = "Only explicit native credential selection is reused."
)]
mod headless;
#[cfg(feature = "native-host")]
#[allow(
    dead_code,
    reason = "Native composition only reuses clock and binding inputs."
)]
mod local_host;
#[cfg(feature = "native-host")]
#[allow(
    dead_code,
    reason = "This binary dispatches only the workspace v2 entry point."
)]
mod native_read;
#[cfg(feature = "native-host")]
#[allow(
    dead_code,
    reason = "Only silent native credential composition is reused."
)]
mod runtime_config;

use fabric_cli::workspace_v2::{
    OutputReservation, ParsedWorkspaceInvocation, WorkspaceCommand, failure_envelope, parse_args,
    write_result,
};
use fabric_sdk_api::workspace_reads::WorkspaceReadError;
use std::{
    io::{self, Write},
    process::ExitCode,
};

fn write_failure(command: Option<&WorkspaceCommand>, error: WorkspaceReadError) -> io::Result<u8> {
    let reservation = OutputReservation::new(None)
        .map_err(|_| io::Error::other("Workspace output could not be initialized."))?;
    write_result(
        reservation,
        &failure_envelope(command, error, None),
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

fn run() -> io::Result<u8> {
    match parse_args(std::env::args_os()) {
        Ok(ParsedWorkspaceInvocation::Display(text)) => {
            write!(io::stdout().lock(), "{text}")?;
            Ok(0)
        }
        Ok(ParsedWorkspaceInvocation::Read(command)) => {
            #[cfg(feature = "native-host")]
            {
                native_read::run_workspace_v2(&command)
            }
            #[cfg(not(feature = "native-host"))]
            {
                let reservation = match OutputReservation::new(command.output()) {
                    Ok(reservation) => reservation,
                    Err(error) => return write_failure(Some(&command), error),
                };
                write_result(
                    reservation,
                    &failure_envelope(Some(&command), WorkspaceReadError::NotConfigured, None),
                    &mut io::stdout().lock(),
                    &mut io::stderr().lock(),
                )
            }
        }
        Err(error) => write_failure(None, error),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(exit) => ExitCode::from(exit),
        Err(_) => {
            let _ = write_failure(None, WorkspaceReadError::OutputFailed);
            ExitCode::from(1)
        }
    }
}
