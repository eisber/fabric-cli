// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native list binding reuses the explicit v2 session, not a credential chain.

use super::{Failure, NativeSession, failed, headless};
use fabric_cli::v2_dataagent_list_host::{
    ListArgumentError, ListDelivery, ListError, ListHost, ListInterruption, list_command,
    parse_list,
};
use fabric_sdk::{Client, EndpointBinding, ServiceOrigin};
use std::{ffi::OsString, io, pin::pin, time::Instant};

pub(super) fn run(args: Vec<OsString>) -> Result<u8, Failure> {
    let command = match parse_list(args) {
        Ok(command) => command,
        Err(ListArgumentError::Syntax(clap::error::ErrorKind::DisplayHelp)) => {
            list_command()
                .write_long_help(&mut io::stdout().lock())
                .map_err(|_| failed("OutputFailed", "Datasource list help output failed.", 1))?;
            return Ok(0);
        }
        Err(error) => return Err(failed("InvalidArguments", error.to_string(), 2)),
    };
    let started = Instant::now();
    let timeout = command.timeout();
    let cwd = std::env::current_dir().map_err(|_| {
        failed(
            "InvalidWorkingDirectory",
            "Output working directory is unavailable.",
            2,
        )
    })?;
    let delivery = ListDelivery::preflight(&command, &cwd).map_err(list_failure)?;
    let mode = std::env::var("FAB_RUST_AUTH_MODE").map_err(|_| {
        failed(
            "NotConfigured",
            "Select an explicit FAB_RUST_AUTH_MODE; no credential chain is used.",
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
        .max_blocking_threads(1)
        .build()
        .map_err(|_| {
            failed(
                "RuntimeFailed",
                "Datasource list runtime could not be created.",
                1,
            )
        })?;
    let result = runtime.block_on(async {
        let mut interrupted = pin!(async {
            tokio::select! {
                biased;
                _ = tokio::time::sleep(timeout.saturating_sub(started.elapsed())) => ListInterruption::TimedOut,
                _ = tokio::signal::ctrl_c() => ListInterruption::Cancelled,
            }
        });
        let session = NativeSession::new(selection, endpoint, timeout)?;
        let client = Client::with_context(session.context);
        let host = ListHost {
            client: &client,
            items: session.executor.as_ref(),
            workspaces: session.executor.as_ref(),
            operations: session.executor.as_ref(),
        };
        let result = host.execute(&command, &chrono::Utc::now().to_rfc3339(), interrupted.as_mut())
            .await.map_err(list_failure)?;
        delivery.deliver_supervised(result, io::stdout(), interrupted.as_mut()).await.map_err(list_failure)
    });
    runtime.shutdown_background();
    result
}

fn list_failure(error: ListError) -> Failure {
    let evidence = match &error {
        ListError::File(error) => serde_json::json!({
            "pathCreated":error.path_created,"bytesWritten":error.bytes_written,"rollback":false
        }),
        ListError::DeliveryInterrupted(_) => serde_json::json!({
            "deliveryMayHaveStarted":true,"deliveryState":"unknown","rollback":false,
            "remoteCancelled":false,"remoteReplayed":false
        }),
        _ => serde_json::Value::Null,
    };
    (error.code(), error.to_string(), evidence, 1)
}
