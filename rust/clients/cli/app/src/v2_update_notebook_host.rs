// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Notebook-only native composition; all mutation semantics remain in Update.

use super::{Failure, NativeSession, failed, headless};
use fabric_cli::update::{
    PreparedUpdateCommand, UpdateCommand, UpdateCommandError, UpdateDelivery,
};
use fabric_sdk::{
    EndpointBinding, ServiceOrigin,
    items::update::contract::{ApprovalPolicy, Outcome, PreparedOperation},
};
use fabric_sdk_operations::{OperationExecutor, OperationLimits};
use futures_util::FutureExt;
use serde_json::Value;
use std::{
    future::Future,
    io::{self, IsTerminal, Write},
    time::Duration,
};

#[cfg(test)]
#[path = "v2_update_notebook_host_tests.rs"]
mod tests;

struct ExactApproval(Option<PreparedOperation>);

impl ApprovalPolicy for ExactApproval {
    type Error = ();

    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
        match self.0.take() {
            Some(approved) if approved == *operation => Ok(()),
            _ => Err(()),
        }
    }
}

pub(super) fn run(command: UpdateCommand) -> Result<(), Failure> {
    if command.plan || command.target.selection().owner_type != "Notebook" {
        return Err(failed(
            "UnsupportedCapability",
            "The native Notebook execution host requires a non-plan Notebook metadata update.",
            2,
        ));
    }
    let timeout = Duration::from_millis(command.options.timeout_millis);
    let deadline = tokio::time::Instant::now() + timeout;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .map_err(|_| failed("RuntimeFailed", "Update runtime could not start.", 1))?;
    let result = runtime.block_on(async {
        let interrupted = async {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline) => (),
                _ = tokio::signal::ctrl_c() => (),
            }
        }.shared();
        let preparation_interrupted = interrupted.clone();
        tokio::pin!(preparation_interrupted);
        let preparation = async {
            let prepared = tokio::task::spawn_blocking(move || {
                let cwd = std::env::current_dir().map_err(|_| {
                    failed("InvalidWorkingDirectory", "Update working directory is unavailable.", 2)
                })?;
                PreparedUpdateCommand::preflight(command, &cwd, &mut io::stdin().lock())
                    .map_err(command_failure)
            })
            .await
            .map_err(|_| failed("UpdatePreflightFailed", "Update input worker failed; nothing was submitted.", 1))??;
            if !io::stderr().is_terminal() {
                return Err(failed(
                    "ApprovalHostRequired",
                    "Notebook metadata execution requires explicit terminal approval; no automatic approval is installed.",
                    1,
                ));
            }
            let mode = std::env::var("FAB_RUST_AUTH_MODE").map_err(|_| {
                failed("NotConfigured", "Select explicit FAB_RUST_AUTH_MODE; no credential fallback is used.", 1)
            })?;
            let selection = headless::Selection::capture(&mode)
                .map_err(|(code, message, exit)| failed(code, message, exit))?;
            let endpoint = match std::env::var("FAB_RUST_FABRIC_HOST") {
                Ok(host) => EndpointBinding::with_host(ServiceOrigin::Fabric, &host)
                    .map_err(|_| failed("InvalidConfiguration", "Invalid explicit Fabric endpoint.", 2))?,
                Err(std::env::VarError::NotPresent) => EndpointBinding::public(ServiceOrigin::Fabric),
                Err(_) => return Err(failed("InvalidConfiguration", "Invalid explicit Fabric endpoint.", 2)),
            };
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(failed("TimedOut", "Update expired before approval; nothing was submitted.", 1));
            }
            let session = NativeSession::new(selection, endpoint, remaining)?;
            let bound = prepared
                .with_context(session.context, OperationLimits::default())
                .map_err(command_failure)?;
            let snapshot = bound.update().prepared().clone();
            let approval = tokio::task::spawn_blocking(move || {
                let question = approval_question(&snapshot)?;
                // Structured stdin is never reused as approval input.
                #[cfg(windows)]
                let console = std::fs::File::open("CONIN$");
                #[cfg(not(windows))]
                let console = std::fs::File::open("/dev/tty");
                let approved = console.and_then(|console| {
                    fabric_cli::console::read_explicit_confirmation(
                        &mut io::BufReader::new(console),
                        &mut io::stderr(),
                        &question,
                    )
                }).map_err(|_| failed("ApprovalHostRequired", "Explicit Notebook approval is unavailable; nothing was submitted.", 1))?;
                if !approved {
                    return Err(failed("ApprovalDenied", "Notebook metadata update was declined; nothing was submitted.", 1));
                }
                Ok(ExactApproval(Some(snapshot)))
            })
            .await
            .map_err(|_| failed("ApprovalHostRequired", "Notebook approval worker failed; nothing was submitted.", 1))??;
            Ok((bound, (*session.executor).clone(), approval))
        };
        let (bound, executor, mut approval) = tokio::select! {
            biased;
            _ = &mut preparation_interrupted => return Err(failed("Interrupted", "Update stopped before dispatch; nothing was submitted.", 1)),
            result = preparation => result?,
        };
        execute_and_publish(bound, executor, &mut approval, interrupted, deadline, io::stdout()).await
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

async fn execute_and_publish<E: OperationExecutor>(
    bound: PreparedUpdateCommand,
    executor: E,
    approval: &mut ExactApproval,
    interrupted: impl Future<Output = ()> + Send,
    deadline: tokio::time::Instant,
    mut stdout: impl Write + Send + 'static,
) -> Result<(), Failure> {
    // Separate Shared handles retain completion across SDK and publication.
    let interrupted = interrupted.shared();
    let delivery = bound
        .execute_bound(executor, approval, interrupted.clone())
        .await
        .map_err(command_failure)?;
    let evidence = receipt_value(&delivery)?;
    tokio::select! {
        biased;
        _ = interrupted => Err((
            "InterruptedAfterDispatch",
            "Update observation stopped; local delivery may be incomplete. Inspect the retained receipt; do not replay.".into(),
            evidence,
            1,
        )),
        result = tokio::task::spawn_blocking(move || deliver(delivery, deadline, &mut stdout)) => {
            result.map_err(|_| (
                "OutputFailed",
                "Update delivery worker failed; inspect the retained receipt; do not replay.".into(),
                evidence,
                1,
            ))?
        },
    }
}

fn approval_question(operation: &PreparedOperation) -> Result<String, Failure> {
    let request = serde_json::to_string(operation).map_err(|_| {
        failed(
            "ApprovalHostRequired",
            "Exact update approval could not be rendered.",
            1,
        )
    })?;
    Ok(format!(
        "Patch existing Notebook metadata without a concurrency guard? No create, definition update or replay.\nExact request: {request}\nApprove this write?"
    ))
}

fn receipt_value(delivery: &UpdateDelivery) -> Result<Value, Failure> {
    serde_json::to_value(&delivery.report().receipt).map_err(|_| {
        failed(
            "OutputFailed",
            "Update receipt serialization failed; do not replay.",
            1,
        )
    })
}

fn deliver(
    delivery: UpdateDelivery,
    deadline: tokio::time::Instant,
    stdout: &mut dyn Write,
) -> Result<(), Failure> {
    let evidence = receipt_value(&delivery)?;
    if tokio::time::Instant::now() >= deadline {
        return Err((
            "TimedOut",
            "Update delivery deadline elapsed; inspect the retained receipt and do not replay."
                .into(),
            evidence,
            1,
        ));
    }
    let code = match delivery.report().receipt.outcome() {
        Outcome::Succeeded { .. } | Outcome::Accepted => None,
        Outcome::Partial { .. } => Some("UpdatePartial"),
        Outcome::Unknown { .. } => Some("UpdateUnknown"),
        Outcome::Failed { .. } => Some("UpdateFailed"),
    };
    delivery.deliver(stdout).map_err(command_failure)?;
    if let Some(code) = code {
        return Err((code, "Notebook metadata update did not establish complete success; inspect the receipt and do not replay.".into(), evidence, 1));
    }
    Ok(())
}

fn command_failure(error: UpdateCommandError) -> Failure {
    let evidence = match &error {
        UpdateCommandError::Delivery { report, .. } => serde_json::to_value(&report.receipt),
        UpdateCommandError::Sdk(fabric_sdk::items::update::UpdateError::Receipt(error)) => {
            serde_json::to_value(&error.evidence)
        }
        _ => Ok(Value::Null),
    };
    match evidence {
        Ok(evidence) => ("UpdateFailed", error.to_string(), evidence, 1),
        Err(_) => failed(
            "OutputFailed",
            "Update evidence serialization failed; remote effects may have occurred; do not replay.",
            1,
        ),
    }
}
