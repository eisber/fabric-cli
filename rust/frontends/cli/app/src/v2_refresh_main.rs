// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in graph refresh reuses the explicit v2 head's identity/runtime/HTTP.

use super::{Failure, NativeSession, failed, v2_credentials};
use fabric_cli::refresh_command::{self, Policy};
use fabric_cli_jobs::HttpJobExecutor;
use fabric_sdk::{Client, EndpointBinding, ServiceOrigin, runtime::NativeDeadline};
use fabric_sdk_graph as refresh;
use fabric_sdk_jobs::{JobClient, JobLimits};
use std::{
    ffi::OsString,
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

#[cfg(test)]
#[path = "v2_refresh_main_tests.rs"]
mod tests;

pub(super) fn run(args: Vec<OsString>) -> Result<u8, Failure> {
    let command = match refresh_command::parse(args) {
        Ok(command) => command,
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            refresh_command::command()
                .write_long_help(&mut io::stdout().lock())
                .map_err(|_| failed("OutputFailed", "Refresh help delivery failed.", 1))?;
            return Ok(0);
        }
        Err(_) => {
            return Err(failed(
                "InvalidArguments",
                "Invalid refresh arguments; run refresh --help.",
                2,
            ));
        }
    };
    if command.schema {
        let schema = refresh::schemas()
            .map_err(|_| failed("SchemaFailed", "Refresh schema registration failed.", 1))?;
        return write_value(&schema)
            .map(|()| 0)
            .map_err(|_| failed("OutputFailed", "Refresh schema delivery failed.", 1));
    }
    let prepared = refresh::prepare(
        command
            .path
            .as_deref()
            .ok_or_else(|| failed("InvalidArguments", "Refresh target is required.", 2))?,
    )
    .map_err(|_| failed("InvalidArguments", "Invalid refresh target.", 2))?;
    prepared
        .authorize(&mut Policy(&command))
        .map_err(|message| failed("ApprovalRequired", message, 1))?;
    let deadline = NativeDeadline::new(
        command
            .timeout
            .ok_or_else(|| failed("InvalidArguments", "Refresh timeout is required.", 2))?,
    )
    .map_err(|_| failed("InvalidArguments", "Invalid refresh deadline.", 2))?;
    let selection = v2_credentials::Selection::capture()?;
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
        .map_err(|_| failed("RuntimeFailed", "Refresh runtime could not start.", 1))?;
    let interruption = AtomicU8::new(0);
    let result = runtime.block_on(async {
        let remaining = deadline
            .remaining()
            .map_err(|_| failed("TimedOut", "Refresh expired before dispatch.", 1))?;
        let session = NativeSession::new(selection, endpoint, remaining).await?;
        let executor = Arc::new(HttpJobExecutor::new((*session.executor).clone()));
        let jobs = JobClient::with_context(
            session.context.clone(),
            executor.clone(),
            JobLimits::default(),
        );
        let client = Client::with_context(session.context);
        let remaining = deadline
            .remaining()
            .map_err(|_| failed("TimedOut", "Refresh expired before dispatch.", 1))?;
        let interrupted = async {
            tokio::select! {
                _ = tokio::time::sleep(remaining) => {interruption.store(1,Ordering::SeqCst);},
                _ = tokio::signal::ctrl_c() => {interruption.store(2,Ordering::SeqCst);},
            }
        };
        refresh_command::dispatch(&command, &client, &jobs, executor.as_ref(), interrupted)
            .await
            .map_err(|message| failed("RefreshPreflightFailed", message, 1))
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    Ok(finish_delivery(
        result?,
        interruption.load(Ordering::SeqCst),
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    ))
}

fn finish_delivery(
    mut delivery: refresh_command::Delivery,
    interruption: u8,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    match interruption {
        1 => delivery.value["meta"]["interruption"] = serde_json::json!("TimedOut"),
        2 => delivery.value["meta"]["interruption"] = serde_json::json!("Cancelled"),
        _ => (),
    }
    delivery.write_to(stdout, stderr)
}

fn write_value(value: &serde_json::Value) -> io::Result<()> {
    write_value_to(value, &mut io::stdout().lock())
}

fn write_value_to(value: &serde_json::Value, stdout: &mut dyn Write) -> io::Result<()> {
    serde_json::to_writer(&mut *stdout, value).map_err(io::Error::other)?;
    writeln!(stdout)?;
    stdout.flush()
}
