// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in graph refresh reuses the explicit v2 head's identity/runtime/HTTP.

use super::{Failure, NativeSession, failed, headless};
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

pub(super) fn run(args: Vec<OsString>) -> Result<(), Failure> {
    let command = match refresh_command::parse(args) {
        Ok(command) => command,
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            refresh_command::command()
                .write_long_help(&mut io::stdout().lock())
                .map_err(|_| failed("OutputFailed", "Refresh help delivery failed.", 1))?;
            return Ok(());
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
        .map_err(|_| failed("RuntimeFailed", "Refresh runtime could not start.", 1))?;
    let interruption = AtomicU8::new(0);
    let result = runtime.block_on(async {
        let remaining = deadline
            .remaining()
            .map_err(|_| failed("TimedOut", "Refresh expired before dispatch.", 1))?;
        let session = NativeSession::new(selection, endpoint, remaining)?;
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
    let mut delivery = result?;
    match interruption.load(Ordering::SeqCst) {
        1 => delivery.value["meta"]["interruption"] = serde_json::json!("TimedOut"),
        2 => delivery.value["meta"]["interruption"] = serde_json::json!("Cancelled"),
        _ => (),
    }
    if delivery.exit != 0 {
        return Err(("RefreshIncomplete","Graph refresh did not establish successful completion; no replay or remote cancel was attempted.".into(), delivery.value, delivery.exit));
    }
    if write_value(&delivery.value).is_err() {
        return Err((
            "OutputFailed",
            "Graph refresh receipt delivery failed; do not replay.".into(),
            delivery.value,
            1,
        ));
    }
    Ok(())
}

fn write_value(value: &serde_json::Value) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value).map_err(io::Error::other)?;
    writeln!(stdout)?;
    stdout.flush()
}
