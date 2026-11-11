// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Run-only native composition; legacy jobs and unrelated family gates are unchanged.

use super::{CommandOutcome, Failure, failure};
use fabric_cli::{
    native_tabular::{NativeCommand, Operation},
    run_notebook::{Dispatch, MAX_INPUT_BYTES, PreparedNotebook},
};
use fabric_sdk_jobs::{
    JobClient, JobFuture, JobLimits,
    run::{RunClock, RunDeadline, RunError, RunFormat, RunOutput, RunTimeout},
};
use std::{
    future::Future,
    io::{self, IsTerminal, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(test)]
#[path = "native_run_notebook_tests.rs"]
mod tests;

struct Clock(Instant);
impl RunClock for Clock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
    fn sleep(&self, delay: Duration) -> JobFuture<'_, ()> {
        Box::pin(tokio::time::sleep(delay))
    }
}

fn read_prepared(command: &NativeCommand) -> Result<PreparedNotebook, Failure> {
    let Operation::Run(options) = &command.operation else {
        return Err(failure(
            "UnsupportedCapability",
            "This host supports Notebook Run only.",
            1,
        ));
    };
    let bytes = options
        .input
        .as_deref()
        .map(|path| {
            let mut input: Box<dyn Read> = if path == "-" {
                Box::new(io::stdin())
            } else {
                Box::new(std::fs::File::open(path).map_err(|_| {
                    failure(
                        "InputFailure",
                        "Notebook input could not be opened; no submission occurred.",
                        1,
                    )
                })?)
            };
            let mut bytes = Vec::new();
            input
                .by_ref()
                .take((MAX_INPUT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| {
                    failure(
                        "InputFailure",
                        "Notebook input could not be read; no submission occurred.",
                        1,
                    )
                })?;
            Ok(bytes)
        })
        .transpose()?;
    PreparedNotebook::new(command, bytes.as_deref())
        .map_err(|error| failure("InvalidInput", error.to_string(), 2))
}

pub(super) fn run(command: &NativeCommand) -> io::Result<CommandOutcome> {
    let timeout = match fabric_cli::run_notebook::preflight(command) {
        Ok((_, timeout)) => timeout,
        Err(error) => return publish(&Dispatch::unavailable(error)),
    };
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::other("Invalid Notebook Run deadline"))?;
    let runtime = match super::native_runtime() {
        Ok(runtime) => runtime,
        Err(error) => return super::write_error(error),
    };
    let command = command.clone();
    let result = runtime.block_on(async {
        let prepared = super::execute::before_submission(async {
            tokio::task::spawn_blocking(move || read_prepared(&command)).await
                .map_err(|_| failure("InputFailure", "Notebook input worker failed; no submission occurred.", 1))?
        }, deadline).await?;
        let output_path = prepared.output().to_owned();
        let output = super::execute::before_submission(async {
            tokio::task::spawn_blocking(move || NativeOutput::prepare(&output_path)).await
                .map_err(|_| failure("OutputFailure", "Notebook output preflight worker failed; no submission occurred.", 1))?
                .map_err(|_| failure("OutputFailure", "Notebook output destination is unavailable or already exists; no submission occurred.", 1))
        }, deadline).await?;
        let question = prepared.approval_question();
        let approved = super::execute::before_submission(async {
            if !io::stderr().is_terminal() {
                return Err(failure("ApprovalRequired", "An interactive terminal is required to approve Notebook execution; no submission occurred.", 1));
            }
            tokio::task::spawn_blocking(move || {
                // Structured stdin cannot double as execution approval.
                #[cfg(windows)]
                let console = std::fs::File::open("CONIN$")?;
                #[cfg(not(windows))]
                let console = std::fs::File::open("/dev/tty")?;
                fabric_cli::console::read_explicit_confirmation(
                    &mut io::BufReader::new(console), &mut io::stderr(), &question,
                )
            }).await
                .map_err(|_| failure("ApprovalRequired", "Notebook approval worker failed; no submission occurred.", 1))?
                .map_err(|_| failure("ApprovalRequired", "Explicit Notebook execution approval is unavailable; no submission occurred.", 1))
        }, deadline).await?;
        if !approved {
            return Err(failure("ApprovalDenied", "Notebook Run was not approved; no submission occurred.", 1));
        }
        let endpoint = super::selected_endpoint(std::env::var_os("FAB_API_ENDPOINT_FABRIC"))?;
        let credential = super::selected_credential()?;
        let (client, identity_client) = super::native_clients(&credential, true)?;
        let (sdk, executor) = super::connect_native(
            credential, client, identity_client, endpoint,
            fabric_sdk_http::ExecutionOptions::default(), Some(deadline),
        ).await?;
        let signal_failed = AtomicBool::new(false);
        let receipt = dispatch_prepared(
            &prepared, &sdk, executor, deadline, &output, async {
                if tokio::signal::ctrl_c().await.is_err() {
                    signal_failed.store(true, Ordering::SeqCst);
                }
            },
        ).await;
        let receipt = if signal_failed.load(Ordering::SeqCst) {
            receipt.signal_registration_failed()
        } else {
            receipt
        };
        let receipt = receipt.with_output_attempt(output.attempted.load(Ordering::SeqCst));
        Ok((receipt, output))
    });
    match super::finish_runtime(runtime, result) {
        Ok((receipt, output)) => {
            if receipt.exit_code() == 0 {
                return Ok(CommandOutcome::Returned(0));
            }
            if receipt.value()["error"]["code"] == "OutputFailure" {
                return publish_output_failure(&receipt, output.partial.load(Ordering::SeqCst));
            }
            publish(&receipt)
        }
        Err(error) => super::write_error(error),
    }
}

async fn dispatch_prepared(
    prepared: &PreparedNotebook,
    sdk: &fabric_sdk::Client,
    executor: fabric_sdk_http::HttpExecutor,
    deadline: Instant,
    output: &dyn RunOutput,
    cancelled: impl Future<Output = ()> + Send,
) -> Dispatch {
    let jobs = JobClient::with_context(
        sdk.context().clone(),
        Arc::new(fabric_cli_jobs::HttpJobExecutor::new(executor)),
        JobLimits::default(),
    );
    let clock = Arc::new(Clock(Instant::now()));
    let remaining = deadline.saturating_duration_since(clock.0);
    let timeout = match RunTimeout::parse(&format!("{}ms", remaining.as_millis())) {
        Ok(timeout) => timeout,
        Err(_) => return Dispatch::unavailable(RunError::Deadline),
    };
    let deadline = match RunDeadline::from_start(Duration::ZERO, timeout) {
        Ok(deadline) => deadline,
        Err(error) => return Dispatch::unavailable(error),
    };
    prepared
        .dispatch_before(&jobs, clock, cancelled, deadline, output)
        .await
}

struct NativeOutput {
    file: Option<Arc<fabric_cli::execute::host::FileDestination>>,
    attempted: AtomicBool,
    partial: Arc<AtomicBool>,
}

impl NativeOutput {
    fn prepare(path: &str) -> io::Result<Self> {
        Ok(Self {
            file: if path == "-" {
                None
            } else {
                Some(Arc::new(
                    fabric_cli::execute::host::FileDestination::preflight(path)?,
                ))
            },
            attempted: AtomicBool::new(false),
            partial: Arc::new(AtomicBool::new(false)),
        })
    }
}

impl RunOutput for NativeOutput {
    fn preflight(&self, format: RunFormat) -> Result<(), RunError> {
        if format != RunFormat::Json || self.attempted.load(Ordering::SeqCst) {
            return Err(RunError::Output);
        }
        Ok(())
    }
    fn commit<'a>(&'a self, bytes: &'a [u8]) -> JobFuture<'a, Result<(), RunError>> {
        Box::pin(async move {
            if self.attempted.swap(true, Ordering::SeqCst) {
                return Err(RunError::Output);
            }
            let bytes = bytes.to_vec();
            if let Some(file) = &self.file {
                let file = file.clone();
                let written = bytes.clone();
                let partial = self.partial.clone();
                tokio::task::spawn_blocking(move || {
                    file.publish(&written).map_err(|error| {
                        partial.store(error.partial, Ordering::SeqCst);
                        RunError::Output
                    })
                })
                .await
                .map_err(|_| RunError::Output)??;
            } else {
                let written = bytes.clone();
                tokio::task::spawn_blocking(move || {
                    let mut stdout = io::stdout().lock();
                    stdout
                        .write_all(&written)
                        .and_then(|()| stdout.flush())
                        .map_err(|_| RunError::Output)
                })
                .await
                .map_err(|_| RunError::Output)??;
            }
            Ok(())
        })
    }
}

fn publish_output_failure(receipt: &Dispatch, partial: bool) -> io::Result<CommandOutcome> {
    let mut retained = receipt.value().clone();
    retained["data"] = serde_json::Value::Null;
    retained["error"] = serde_json::json!({
        "code":"OutputFailure",
        "message":"Run receipt delivery failed. Retain the handle; do not replay submission.",
        "partialFileMayExist":partial
    });
    serde_json::to_writer(&mut io::stderr().lock(), &retained)?;
    writeln!(io::stderr().lock())?;
    Ok(CommandOutcome::Returned(1))
}

fn publish(receipt: &Dispatch) -> io::Result<CommandOutcome> {
    let result = if receipt.exit_code() == 0 {
        serde_json::to_writer(&mut io::stdout().lock(), receipt.value())
            .map_err(io::Error::other)
            .and_then(|()| writeln!(io::stdout().lock()))
    } else {
        serde_json::to_writer(&mut io::stderr().lock(), receipt.value())
            .map_err(io::Error::other)
            .and_then(|()| writeln!(io::stderr().lock()))
    };
    if result.is_err() {
        let mut retained = receipt.value().clone();
        retained["error"] = serde_json::json!({
            "code":"OutputFailure",
            "message":"Run receipt delivery failed. Retain the handle; do not replay submission."
        });
        serde_json::to_writer(&mut io::stderr().lock(), &retained)?;
        writeln!(io::stderr().lock())?;
        return Ok(CommandOutcome::Returned(1));
    }
    let stopped = receipt.value()["error"]["code"]
        .as_str()
        .is_some_and(|code| matches!(code, "Cancelled" | "TimedOut" | "SignalFailed"));
    Ok(if stopped {
        CommandOutcome::Stop(receipt.exit_code())
    } else {
        CommandOutcome::Returned(receipt.exit_code())
    })
}
