// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{Failure, NativeSession, failed, headless};
use fabric_cli::exists_command::{ExistsArgumentError, exists_command, parse_exists, write_exists};
use fabric_sdk::{
    Client, EndpointBinding, ServiceOrigin,
    exists::{ExistsHost, ExistsInterruption, prepare_exists},
    v2::{ApprovalPolicy, Effect, PreparedOperation},
};
use std::{
    ffi::OsString,
    io,
    time::{Duration, Instant},
};

pub(super) fn run(args: Vec<OsString>) -> Result<u8, Failure> {
    let command = match parse_exists(args) {
        Ok(command) => command,
        Err(ExistsArgumentError::Syntax(clap::error::ErrorKind::DisplayHelp)) => {
            exists_command()
                .write_long_help(&mut io::stdout().lock())
                .map_err(|_| failed("OutputFailed", "Exists help output failed.", 1))?;
            return Ok(0);
        }
        Err(error) => return Err(failed("InvalidArguments", error.to_string(), 2)),
    };
    let limits = command
        .limits()
        .map_err(|_| failed("InvalidArguments", "Invalid Exists bounds.", 2))?;
    let started = Instant::now();
    let mode = std::env::var("FAB_RUST_AUTH_MODE").map_err(|_| {
        failed(
            "NotConfigured",
            "Select an explicit FAB_RUST_AUTH_MODE; no credential chain is used.",
            1,
        )
    })?;
    let selection = headless::Selection::capture(&mode)
        .map_err(|(code, message, exit)| failed(code, message, exit))?;
    let request = command
        .request(&selection.tenant)
        .map_err(|_| failed("InvalidArguments", "Invalid Exists target or tenant.", 2))?;
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
    let prepared = prepare_exists(&request)
        .map_err(|_| failed("InvalidArguments", "Invalid Exists preparation.", 2))?;
    let approved = prepared.authorize(&mut ReadPolicy).map_err(|()| {
        failed(
            "ApprovalDenied",
            "Exists requires exactly read-only metadata intent.",
            1,
        )
    })?;
    let timeout = command.timeout;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .map_err(|_| failed("RuntimeFailed", "Exists runtime could not be created.", 1))?;
    let result = runtime.block_on(async {
        let remaining = timeout
            .checked_sub(started.elapsed())
            .filter(|value| !value.is_zero())
            .ok_or_else(|| failed("TimedOut", "Exists deadline elapsed before dispatch.", 1))?;
        let session = NativeSession::new(selection, endpoint, remaining)?;
        let client = Client::with_context(session.context);
        let host = ExistsHost {
            client: &client,
            items: session.executor.as_ref(),
            workspaces: session.executor.as_ref(),
        };
        let deadline = timeout.saturating_sub(started.elapsed());
        let interrupted = async {
            tokio::select! {
                _ = tokio::time::sleep(deadline) => ExistsInterruption::DeadlineExceeded,
                _ = tokio::signal::ctrl_c() => ExistsInterruption::Cancelled,
            }
        };
        host.execute_until(&approved, &request, limits, interrupted)
            .await
            .map_err(|_| {
                failed(
                    "ExistsFailed",
                    "Exists could not produce a bounded report; no mutation was attempted.",
                    1,
                )
            })
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    let report = result?;
    if started.elapsed() >= timeout {
        return Err((
            "TimedOut",
            "Exists total deadline elapsed before output delivery.".into(),
            serde_json::json!({
                "exists": null,
                "nativeStatuses": report.native().responses().iter().map(|response| response.status()).collect::<Vec<_>>(),
                "mutationMayHaveOccurred": false
            }),
            1,
        ));
    }
    write_exists(&report, &mut io::stdout().lock()).map_err(|_| {
        failed(
            "OutputFailed",
            "Exists report output failed; do not replay.",
            1,
        )
    })
}

struct ReadPolicy;
impl ApprovalPolicy for ReadPolicy {
    type Error = ();
    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
        if operation.effects() == [Effect::RemoteRead] && operation.preconditions().is_empty() {
            Ok(())
        } else {
            Err(())
        }
    }
}
