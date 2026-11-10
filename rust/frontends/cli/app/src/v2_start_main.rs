// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Mirroring lifecycle composition reusing the v2 head's explicit native session.

use super::{Failure, NativeSession, failed, headless};
use fabric_cli::start_command::{self, StartArgumentError};
use fabric_sdk::{Client, EndpointBinding, ServiceOrigin, mirroring, runtime::NativeDeadline};
use fabric_sdk_operations::OperationLimits;
use std::{
    ffi::OsString,
    io::{self, Write},
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};

pub(super) fn run(args: Vec<OsString>) -> Result<u8, Failure> {
    let command = match start_command::parse_start(args) {
        Ok(command) => command,
        Err(StartArgumentError::Syntax(clap::error::ErrorKind::DisplayHelp)) => {
            start_command::start_command()
                .write_long_help(&mut io::stdout().lock())
                .map_err(|_| failed("OutputFailed", "START help delivery failed.", 1))?;
            return Ok(0);
        }
        Err(error) => return Err(failed("InvalidArguments", error.to_string(), 2)),
    };
    if command.schema {
        let schema = mirroring::schemas()
            .map_err(|_| failed("SchemaFailed", "START schema registration failed.", 1))?;
        serde_json::to_writer(io::stdout().lock(), &schema)
            .map_err(|_| failed("OutputFailed", "START schema delivery failed.", 1))?;
        let mut stdout = io::stdout().lock();
        writeln!(stdout)
            .and_then(|()| stdout.flush())
            .map_err(|_| failed("OutputFailed", "START schema delivery failed.", 1))?;
        return Ok(0);
    }
    if !command.approve {
        return Err(failed(
            "ApprovalRequired",
            "Explicit --approve is required for this mirroring START; no mutation was submitted.",
            1,
        ));
    }
    let timeout = command
        .timeout
        .ok_or_else(|| failed("InvalidArguments", "START timeout is required.", 2))?;
    let deadline = NativeDeadline::new(timeout)
        .map_err(|_| failed("InvalidArguments", "Invalid START deadline.", 2))?;
    let mode = std::env::var("FAB_RUST_AUTH_MODE").map_err(|_| {
        failed(
            "NotConfigured",
            "Select explicit FAB_RUST_AUTH_MODE; no credential fallback is used.",
            1,
        )
    })?;
    let selection = headless::Selection::capture(&mode)
        .map_err(|(code, message, exit)| failed(code, message, exit))?;
    let endpoint = match std::env::var("FAB_RUST_FABRIC_HOST") {
        Ok(host) => EndpointBinding::with_host(ServiceOrigin::Fabric, &host).map_err(|_| {
            failed(
                "InvalidConfiguration",
                "Invalid explicit Fabric endpoint.",
                2,
            )
        })?,
        Err(std::env::VarError::NotPresent) => EndpointBinding::public(ServiceOrigin::Fabric),
        Err(_) => {
            return Err(failed(
                "InvalidConfiguration",
                "Invalid explicit Fabric endpoint.",
                2,
            ));
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .map_err(|_| failed("RuntimeFailed", "START runtime could not start.", 1))?;
    let interruption = AtomicU8::new(0);
    let result = runtime.block_on(async {
        let remaining = deadline
            .remaining()
            .map_err(|_| failed("TimedOut", "START expired before dispatch.", 1))?;
        let session = NativeSession::new(selection, endpoint, remaining)?;
        let client = Client::with_context(session.context);
        let remaining = deadline
            .remaining()
            .map_err(|_| failed("TimedOut", "START expired before dispatch.", 1))?;
        let interrupted = async {
            tokio::select! {
                _ = tokio::time::sleep(remaining) => { interruption.store(1, Ordering::SeqCst); }
                _ = tokio::signal::ctrl_c() => { interruption.store(2, Ordering::SeqCst); }
            }
        };
        start_command::dispatch(
            &command,
            &client,
            session.executor.as_ref(),
            OperationLimits::default(),
            interrupted,
        )
        .await
        .map_err(|error| ("StartFailed", error.to_string(), error.evidence(), 1))
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    let mut delivery = result?;
    match interruption.load(Ordering::SeqCst) {
        1 => delivery.value["meta"]["interruption"] = serde_json::json!("TimedOut"),
        2 => delivery.value["meta"]["interruption"] = serde_json::json!("Cancelled"),
        _ => (),
    }
    Ok(delivery.write_to(&mut io::stdout().lock(), &mut io::stderr().lock()))
}
