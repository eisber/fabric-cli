// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native stop composition using the released synchronous explicit session.

use super::{Failure, NativeSession, failed, headless};
use fabric_cli::stop_command::{self, ArgumentError};
use fabric_sdk::{Client, EndpointBinding, ServiceOrigin, mirroring_stop, runtime::NativeDeadline};
use fabric_sdk_operations::OperationLimits;
use std::{
    ffi::OsString,
    io::{self, Write},
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};

pub(super) fn run(args: Vec<OsString>) -> Result<u8, Failure> {
    let command = match stop_command::parse(args) {
        Ok(command) => command,
        Err(ArgumentError::Syntax(clap::error::ErrorKind::DisplayHelp)) => {
            stop_command::command()
                .write_long_help(&mut io::stdout().lock())
                .map_err(|_| failed("OutputFailed", "Stop help delivery failed.", 1))?;
            return Ok(0);
        }
        Err(error) => return Err(failed("InvalidArguments", error.to_string(), 2)),
    };
    if command.schema() {
        let schema = mirroring_stop::schemas()
            .map_err(|_| failed("SchemaFailed", "Stop schema registration failed.", 1))?;
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(&mut stdout, &schema)
            .map_err(|_| failed("OutputFailed", "Stop schema delivery failed.", 1))?;
        writeln!(stdout)
            .and_then(|()| stdout.flush())
            .map_err(|_| failed("OutputFailed", "Stop schema delivery failed.", 1))?;
        return Ok(0);
    }
    let timeout = command
        .timeout()
        .ok_or_else(|| failed("InvalidArguments", "Stop timeout is required.", 2))?;
    let deadline = NativeDeadline::new(timeout)
        .map_err(|_| failed("InvalidArguments", "Invalid stop deadline.", 2))?;
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
        .map_err(|_| failed("RuntimeFailed", "Stop runtime could not start.", 1))?;
    let interruption = AtomicU8::new(0);
    let result = runtime.block_on(async {
        let remaining = deadline
            .remaining()
            .map_err(|_| failed("TimedOut", "Stop expired before dispatch.", 1))?;
        let session = NativeSession::new(selection, endpoint, remaining)?;
        deadline
            .remaining()
            .map_err(|_| failed("TimedOut", "Stop expired before dispatch.", 1))?;
        let interrupted = async {
            let Ok(remaining) = deadline.remaining() else {
                interruption.store(1, Ordering::SeqCst);
                return;
            };
            tokio::select! {
                _ = tokio::time::sleep(remaining) => { interruption.store(1, Ordering::SeqCst); }
                _ = tokio::signal::ctrl_c() => { interruption.store(2, Ordering::SeqCst); }
            }
        };
        execute(&command, session, interrupted).await
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    let mut delivery = result?;
    match interruption.load(Ordering::SeqCst) {
        1 => {
            delivery.value["interruption"] = serde_json::json!("TimedOut");
            if delivery.execution.result.is_ok() {
                delivery.value["error"]["code"] = serde_json::json!("TimedOut");
            }
        }
        2 => {
            delivery.value["interruption"] = serde_json::json!("Cancelled");
            if delivery.execution.result.is_ok() {
                delivery.value["error"]["code"] = serde_json::json!("Cancelled");
            }
        }
        _ => (),
    }

    Ok(delivery.write_to(&mut io::stdout().lock(), &mut io::stderr().lock()))
}

pub(super) async fn execute(
    command: &stop_command::StopCommand,
    session: NativeSession,
    interrupted: impl std::future::Future<Output = ()> + Send,
) -> Result<stop_command::Delivery, Failure> {
    let client = Client::with_context(session.context);
    stop_command::dispatch(
        command,
        &client,
        session.executor.as_ref(),
        &mut stop_command::Policy(command),
        OperationLimits::default(),
        interrupted,
    )
    .await
    .map_err(|error| failed("StopFailed", error.to_string(), 1))
}
