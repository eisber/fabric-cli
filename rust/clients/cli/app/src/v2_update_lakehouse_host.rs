// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit Lakehouse-only native metadata host over the shared Update adapter.

use super::{Failure, NativeSession, failed, headless};
use fabric_cli::update::{
    PreparedUpdateCommand, UpdateCommand, UpdateCommandError, UpdateDelivery,
};
use fabric_sdk::{
    EndpointBinding, ServiceOrigin,
    items::update::UpdateReport,
    runtime::NativeDeadline,
    v2::{ApprovalPolicy, Outcome, PreparedOperation},
};
use fabric_sdk_operations::OperationLimits;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fmt::Write as _,
    future::Future,
    io::{self, IsTerminal, Write as _},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[cfg(test)]
#[path = "v2_update_lakehouse_host_tests.rs"]
mod tests;

struct ExactApproval {
    captured: PreparedOperation,
    approved: bool,
}

impl ApprovalPolicy for ExactApproval {
    type Error = ();

    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), ()> {
        if self.approved
            && operation == &self.captured
            && operation.target().selection().owner_type == "Lakehouse"
            && !operation.input()["context"].is_null()
        {
            self.approved = false;
            Ok(())
        } else {
            Err(())
        }
    }
}

fn require_lakehouse(command: &UpdateCommand) -> Result<(), Failure> {
    if command.plan || command.target.selection().owner_type != "Lakehouse" {
        return Err(failed(
            "UnsupportedCapability",
            "This native host only executes existing Lakehouse metadata patches.",
            2,
        ));
    }
    Ok(())
}

pub(super) fn execution_command(args: &[OsString]) -> Option<UpdateCommand> {
    if !matches!(
        args.first().and_then(|value| value.to_str()),
        Some("update" | "artifact")
    ) {
        return None;
    }
    let command = fabric_cli::update::parse(
        std::iter::once(OsString::from("fab-cli-v2")).chain(args.iter().cloned()),
    )
    .ok()?;
    require_lakehouse(&command).ok()?;
    Some(command)
}

pub(super) fn run(command: UpdateCommand) -> u8 {
    let deadline = match NativeDeadline::new(command.timeout()) {
        Ok(deadline) => deadline,
        Err(_) => {
            return finish_native(
                Err(failed("InvalidArguments", "Invalid update deadline.", 2)),
                Duration::from_millis(100),
            );
        }
    };
    let result = run_until(command, &deadline);
    // At expiry only this bounded diagnostic/cleanup grace remains. Never let
    // an abandoned terminal writer prevent the executable from returning.
    finish_native(
        result,
        deadline.remaining().unwrap_or(Duration::from_millis(100)),
    )
}

fn run_until(command: UpdateCommand, deadline: &NativeDeadline) -> Result<(), Failure> {
    require_lakehouse(&command)?;
    let cwd = std::env::current_dir().map_err(|_| {
        failed(
            "InvalidWorkingDirectory",
            "Update working directory is unavailable.",
            2,
        )
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .map_err(|_| failed("RuntimeFailed", "Update runtime could not start.", 1))?;
    let result = runtime.block_on(async {
        let file_output = command.output.is_some();
        let prepared = before_submission(deadline, async move {
            tokio::task::spawn_blocking(move || {
                PreparedUpdateCommand::preflight(command, &cwd, &mut io::stdin().lock())
            })
            .await
            .map_err(|_| failed("UpdatePreflightFailed", "Update input worker failed.", 1))?
            .map_err(host_failure)
        })
        .await?;
        // Duplicated output handles avoid an abandoned write holding Rust's
        // global stdout lock during process cleanup.
        let stdout = if file_output {
            None
        } else {
            Some(owned_stdout().map_err(|_| {
                failed(
                    "OutputFailed",
                    "Use --output FILE or redirected stdout for native update output.",
                    1,
                )
            })?)
        };
        if !io::stderr().is_terminal() {
            return Err(failed(
                "ApprovalRequired",
                "Lakehouse update requires an interactive approval terminal; no PATCH was sent.",
                1,
            ));
        }
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
        let timeout = remaining(deadline)?;
        let session = before_submission(deadline, async move {
            tokio::task::spawn_blocking(move || NativeSession::new(selection, endpoint, timeout))
                .await
                .map_err(|_| {
                    failed(
                        "UpdatePreflightFailed",
                        "Update session worker failed; no PATCH was sent.",
                        1,
                    )
                })?
        })
        .await?;
        let prepared = prepared
            .with_context(session.context, OperationLimits::default())
            .map_err(host_failure)?;
        let captured = prepared.update().prepared().clone();
        let prompt_request = captured.clone();
        let approved = before_submission(deadline, async move {
            tokio::task::spawn_blocking(move || {
                let question = approval_question(&prompt_request)?;
                read_native_approval(&question).map_err(|_| {
                    failed(
                        "ApprovalRequired",
                        "Update approval input failed; no PATCH was sent.",
                        1,
                    )
                })
            })
            .await
            .map_err(|_| {
                failed(
                    "ApprovalRequired",
                    "Update approval worker failed; no PATCH was sent.",
                    1,
                )
            })?
        })
        .await?;
        if !approved {
            return Err(failed(
                "ApprovalDenied",
                "Lakehouse update was declined; no PATCH was sent.",
                1,
            ));
        }
        remaining(deadline)?;
        let mut policy = ExactApproval { captured, approved };
        let signal_failed = AtomicBool::new(false);
        let interruption = async {
            tokio::select! {
                biased;
                signal = tokio::signal::ctrl_c() => {
                    signal_failed.store(signal.is_err(), Ordering::Release);
                },
                _ = tokio::time::sleep(remaining(deadline).unwrap_or(Duration::ZERO)) => (),
            }
        };
        let delivery = prepared
            .execute_bound(session.executor, &mut policy, interruption)
            .await
            .map_err(host_failure)?;
        if signal_failed.load(Ordering::Acquire) {
            return Err((
                "SignalFailed",
                "Update cancellation handler failed; original native evidence is retained. Do not replay.".into(),
                report_evidence(delivery.report()),
                1,
            ));
        }
        publish(delivery, stdout, deadline).await
    });
    runtime.shutdown_timeout(Duration::from_millis(100));
    result
}

fn approval_question(operation: &PreparedOperation) -> Result<String, Failure> {
    let evidence = ascii_json(operation).map_err(|_| {
        failed(
            "ApprovalRequired",
            "Exact update approval could not be rendered.",
            1,
        )
    })?;
    Ok(format!(
        "Approve ONE existing Lakehouse metadata PATCH (unguarded concurrency)?\n{evidence}\nNo create, retry, rollback, data movement or speculative rename is performed."
    ))
}

fn read_native_approval(question: &str) -> io::Result<bool> {
    // Never consume JSON stdin (or trailing JSON bytes) as approval.
    #[cfg(windows)]
    let input = std::fs::File::open("CONIN$")?;
    #[cfg(not(windows))]
    let input = std::fs::File::open("/dev/tty")?;
    fabric_cli::console::read_explicit_confirmation(
        &mut io::BufReader::new(input),
        &mut owned_stderr()?,
        question,
    )
}

fn ascii_json(value: &impl serde::Serialize) -> serde_json::Result<String> {
    let json = serde_json::to_string(value)?;
    let mut ascii = String::with_capacity(json.len());
    for character in json.chars() {
        if character.is_ascii() {
            ascii.push(character);
        } else {
            for unit in character.encode_utf16(&mut [0; 2]) {
                write!(ascii, "\\u{unit:04x}").expect("writing to a String cannot fail");
            }
        }
    }
    Ok(ascii)
}

fn owned_stderr() -> io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        Ok(std::fs::File::from(
            io::stderr().as_handle().try_clone_to_owned()?,
        ))
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        Ok(std::fs::File::from(
            io::stderr().as_fd().try_clone_to_owned()?,
        ))
    }
    #[cfg(not(any(windows, unix)))]
    {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

fn finish_native(result: Result<(), Failure>, timeout: Duration) -> u8 {
    if result.is_ok() {
        return 0;
    }
    let Ok(stderr) = owned_stderr() else {
        return 1;
    };
    finish_to(result, stderr, timeout)
}

fn finish_to<W: io::Write + Send + 'static>(
    result: Result<(), Failure>,
    mut stderr: W,
    timeout: Duration,
) -> u8 {
    let Err((code, message, evidence, exit)) = result else {
        return 0;
    };
    let (sent, received) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::Builder::new()
        .name("lakehouse-diagnostic".into())
        .spawn(move || {
            let result =
                ascii_json(&json!({"error":{"code":code,"message":message},"evidence":evidence}))
                    .map_err(io::Error::other)
                    .and_then(|document| {
                        stderr.write_all(document.as_bytes())?;
                        stderr.write_all(b"\n")?;
                        stderr.flush()
                    });
            // The receiver may have reached its deadline while OS output stalled.
            let _ = sent.send(result);
        });
    if worker.is_err() {
        return 1;
    }
    match received.recv_timeout(timeout) {
        Ok(Ok(())) => exit,
        Ok(Err(_)) | Err(_) => 1,
    }
}

fn owned_stdout() -> io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        let output = std::fs::File::from(io::stdout().as_handle().try_clone_to_owned()?);
        if output.is_terminal() {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        Ok(output)
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        Ok(std::fs::File::from(
            io::stdout().as_fd().try_clone_to_owned()?,
        ))
    }
    #[cfg(not(any(windows, unix)))]
    {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

fn remaining(deadline: &NativeDeadline) -> Result<Duration, Failure> {
    deadline.remaining().map_err(|_| {
        failed(
            "TimedOut",
            "The whole update deadline elapsed; no replay or rollback was attempted.",
            1,
        )
    })
}

async fn before_submission<T>(
    deadline: &NativeDeadline,
    work: impl Future<Output = Result<T, Failure>>,
) -> Result<T, Failure> {
    let duration = remaining(deadline)?;
    tokio::select! {
        biased;
        signal = tokio::signal::ctrl_c() => Err(failed(
            if signal.is_ok() { "Cancelled" } else { "SignalFailed" },
            "Update interrupted before submission; no PATCH was sent.", 1)),
        _ = tokio::time::sleep(duration) => Err(failed("TimedOut", "Update expired before submission; no PATCH was sent.", 1)),
        result = work => result,
    }
}

fn report_evidence(report: &UpdateReport) -> Value {
    // The protected receipt deliberately withholds raw causes/foreign data.
    json!({"receipt":report.receipt,"remoteCancelled":false,"remoteReplayed":false,"rollback":false})
}

fn host_failure(error: UpdateCommandError) -> Failure {
    match error {
        UpdateCommandError::Delivery { source, report } => (
            "OutputFailed",
            "Update delivery failed; original remote evidence is retained. Do not replay.".into(),
            {
                let mut evidence = report_evidence(&report);
                evidence["localOutput"] = match source.as_ref() {
                    UpdateCommandError::File(error) => json!({
                        "kind":format!("{:?}", error.kind),
                        "stage":format!("{:?}", error.stage),
                        "pathCreated":error.path_created,
                        "bytesWritten":error.bytes_written
                    }),
                    UpdateCommandError::Io(error) => json!({"kind":format!("{:?}", error.kind())}),
                    _ => json!({"kind":"InvalidDelivery"}),
                };
                evidence
            },
            1,
        ),
        UpdateCommandError::Approval => failed("ApprovalDenied", "Exact update approval did not match; no PATCH was sent.", 1),
        UpdateCommandError::Sdk(fabric_sdk::items::update::UpdateError::Receipt(error)) => (
            "ReceiptFailed",
            "Update receipt construction failed; retained native evidence does not authorize replay.".into(),
            json!({"outcome":error.outcome,"evidence":error.evidence,"remoteCancelled":false,"remoteReplayed":false,"rollback":false}),
            1,
        ),
        _ => failed("UpdatePreflightFailed", "Update preparation failed; no PATCH was sent.", 1),
    }
}

async fn publish<W: io::Write + Send + 'static>(
    delivery: UpdateDelivery,
    stdout: Option<W>,
    deadline: &NativeDeadline,
) -> Result<(), Failure> {
    let evidence = report_evidence(delivery.report());
    let succeeded = matches!(
        delivery.report().receipt.outcome(),
        Outcome::Succeeded { .. }
    );
    let duration = deadline.remaining().map_err(|_| {
        (
            "TimedOut",
            "Update deadline elapsed before delivery; do not replay.".into(),
            evidence.clone(),
            1,
        )
    })?;
    let worker = tokio::task::spawn_blocking(move || {
        match stdout {
            Some(mut stdout) => delivery.deliver(&mut stdout),
            None => delivery.deliver(&mut io::sink()),
        }
        .map_err(host_failure)
    });
    let result = tokio::select! {
        biased;
        signal = tokio::signal::ctrl_c() => return Err((
            if signal.is_ok() { "Cancelled" } else { "SignalFailed" },
            "Update delivery interrupted; local output may still finish. Do not replay.".into(),
            evidence, 1
        )),
        _ = tokio::time::sleep(duration) => return Err((
            "TimedOut", "Update delivery timed out; local output may still finish. Do not replay.".into(), evidence, 1
        )),
        result = worker => result,
    };
    match result {
        Ok(result) => {
            result?;
            if deadline.remaining().is_err() {
                return Err((
                    "TimedOut",
                    "Update deadline elapsed during delivery; do not replay.".into(),
                    evidence,
                    1,
                ));
            }
            if succeeded {
                Ok(())
            } else {
                Err((
                    "UpdateIncomplete",
                    "The native update receipt is not a confirmed success; do not replay.".into(),
                    evidence,
                    1,
                ))
            }
        }
        Err(_) => Err((
            "OutputFailed",
            "Update delivery worker failed; do not replay.".into(),
            evidence,
            1,
        )),
    }
}
