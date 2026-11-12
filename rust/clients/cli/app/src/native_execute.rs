// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root execute on the existing explicitly selected native host.

#[cfg(test)]
#[path = "native_execute_tests.rs"]
mod tests;

use super::{BatchState, CommandOutcome, Failure, NativeCommand, failure, get};
use fabric_cli::execute::{
    ExecuteCommand,
    host::{
        ApprovedExecution, ExecutionApproval, ExecutionEvidence, ExecutionFailure, FileDestination,
        PreparationError, PreparedExecution, read_input,
    },
};
use fabric_cli_output::{OutputStream, RenderPlan, text::TextPlan};
use fabric_sdk::Client;
use fabric_sdk_http::HttpExecutor;
use fabric_sdk_operations::{OperationErrorKind, OperationLimits, OperationSession};
use std::{
    future::Future,
    io::{self, IsTerminal, Write},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Instant,
};

struct NativeApproval;

const WINDOWS_CONSOLE_OUTPUT_MESSAGE: &str = "Native Windows console result output is not supported in this preview. Use --output FILE or redirect stdout to a file or pipe to preserve Unicode.";

fn preflight_native_output(command: &ExecuteCommand, windows_console: bool) -> Result<(), Failure> {
    if windows_console && command.output.is_none() {
        return Err(failure(
            "UnsupportedCapability",
            format!("{WINDOWS_CONSOLE_OUTPUT_MESSAGE} No query was submitted."),
            1,
        ));
    }
    Ok(())
}

struct ApprovalPrompt<W>(Option<W>);

impl<W: Write> Write for ApprovalPrompt<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .as_mut()
            .ok_or_else(|| io::Error::other("The approval prompt was already closed"))?
            .write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        let Some(mut output) = self.0.take() else {
            return Ok(());
        };
        let result = output.flush();
        // The shared console helper flushes the prompt before reading. Release
        // even an injected locked writer before that read can be abandoned.
        drop(output);
        result
    }
}

fn read_approval_confirmation(
    input: &mut impl io::BufRead,
    output: impl Write,
    question: &str,
) -> io::Result<bool> {
    fabric_cli::console::read_confirmation(input, &mut ApprovalPrompt(Some(output)), question)
}

impl ExecutionApproval for NativeApproval {
    fn approve<'a>(
        &'a mut self,
        prepared: &'a PreparedExecution,
    ) -> Pin<Box<dyn Future<Output = io::Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            if !io::stderr().is_terminal() {
                return Err(io::Error::other(
                    "Interactive approval capability is required",
                ));
            }
            let question = prepared.approval_question();
            tokio::task::spawn_blocking(move || {
                if io::stdin().is_terminal() {
                    return read_approval_confirmation(
                        &mut io::stdin().lock(),
                        io::stderr(),
                        &question,
                    );
                }
                // Structured stdin is not reused as a confirmation reader.
                // An attached controlling console is an explicit host capability;
                // a service/redirected process without one fails closed.
                #[cfg(windows)]
                let console = std::fs::File::open("CONIN$")?;
                #[cfg(not(windows))]
                let console = std::fs::File::open("/dev/tty")?;
                read_approval_confirmation(
                    &mut io::BufReader::new(console),
                    io::stderr(),
                    &question,
                )
            })
            .await
            .map_err(io::Error::other)?
        })
    }
}

fn preparation_failure(error: PreparationError) -> Failure {
    let (code, exit) = match error {
        PreparationError::InvalidInput => ("invalid_arguments", 2),
        PreparationError::ApprovalDenied => ("approval_denied", 1),
        PreparationError::ApprovalUnavailable => ("approval_required", 1),
    };
    failure(code, error.to_string(), exit)
}

pub(super) fn run(command: &ExecuteCommand, state: &mut BatchState) -> io::Result<CommandOutcome> {
    let result = (|| {
        preflight_native_output(command, cfg!(windows) && io::stdout().is_terminal())?;
        let deadline = Instant::now()
            .checked_add(command.timeout)
            .ok_or_else(|| failure("invalid_arguments", "Invalid execution deadline.", 2))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| {
                failure(
                    "runtime_failed",
                    "The execution runtime could not start.",
                    1,
                )
            })?;
        let input_command = command.clone();
        let result = runtime.block_on(async {
            let prepared = before_submission(
                async {
                    tokio::task::spawn_blocking(move || prepare_local(&input_command))
                        .await
                        .map_err(|_| {
                            failure(
                                "input_failed",
                                "The local input worker did not complete; no query was submitted.",
                                1,
                            )
                        })?
                },
                deadline,
            )
            .await?;
            let approved = before_submission(
                async {
                    prepared
                        .approve(&mut NativeApproval)
                        .await
                        .map_err(preparation_failure)
                },
                deadline,
            )
            .await?;
            let endpoint = super::selected_endpoint(std::env::var_os("FAB_API_ENDPOINT_FABRIC"))?;
            let credential = super::selected_credential()?;
            credential.bind_get(state).map_err(|_| {
                failure(
                    "invalid_configuration",
                    "Execution identity binding changed.",
                    2,
                )
            })?;
            let settings = super::settings::NativeSettings::select(None).map_err(|_| {
                failure(
                    "invalid_configuration",
                    "Invalid execution host settings.",
                    2,
                )
            })?;
            let delivery = super::execute_delivery(
                NativeCommand::Execution(&approved, deadline),
                credential,
                "/",
                fabric_cli_context::HomeContext::Unspecified,
                endpoint,
                settings,
                state,
                None,
            )
            .await?;
            let get::Delivery::Execution(delivery) = delivery else {
                return Err(failure(
                    "output_failed",
                    "The native query did not retain its execution delivery.",
                    1,
                ));
            };
            Ok(delivery.publish_native(deadline).await)
        });
        super::finish_runtime(runtime, result)
    })();
    match result {
        Ok(Publication::Complete(outcome)) => Ok(outcome),
        Ok(Publication::Diagnostic(delivery)) => {
            delivery.write_to(&mut io::sink(), &mut io::stderr())
        }
        Err(error) => {
            let stop = matches!(error.0, "cancelled" | "signal_failed" | "timed_out");
            let exit = error.2;
            let outcome = super::write_error(error)?;
            // A dropped preparation/approval future can still own an input read.
            Ok(if stop {
                CommandOutcome::Stop(exit)
            } else {
                outcome
            })
        }
    }
}

fn owned_stdout() -> io::Result<std::fs::File> {
    // An abandoned write must not hold Rust's global stdout lock: process
    // cleanup flushes that stream even after a terminal command outcome.
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        let output = io::stdout()
            .as_handle()
            .try_clone_to_owned()
            .map(std::fs::File::from)?;
        // A duplicated console handle still needs WriteConsoleW for Unicode.
        // Never feed UTF-8 bytes to WriteFile if the handle changed since preflight.
        if output.is_terminal() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                WINDOWS_CONSOLE_OUTPUT_MESSAGE,
            ));
        }
        Ok(output)
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        io::stdout()
            .as_fd()
            .try_clone_to_owned()
            .map(std::fs::File::from)
    }
    #[cfg(not(any(windows, unix)))]
    {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

fn prepare_local(command: &ExecuteCommand) -> Result<PreparedExecution, Failure> {
    let bytes = match command.input.as_deref() {
        None => None,
        Some("-") => Some(read_input(&mut io::stdin().lock()).map_err(|_| {
            failure(
                "invalid_arguments",
                "Unable to read bounded execution input.",
                2,
            )
        })?),
        Some(path) => {
            let mut file = std::fs::File::open(path)
                .map_err(|_| failure("invalid_arguments", "Unable to open execution input.", 2))?;
            Some(read_input(&mut file).map_err(|_| {
                failure(
                    "invalid_arguments",
                    "Unable to read bounded execution input.",
                    2,
                )
            })?)
        }
    };
    let prepared =
        PreparedExecution::new(command, bytes.as_deref()).map_err(preparation_failure)?;
    if let Some(path) = &command.output {
        FileDestination::preflight(path).map_err(|_| {
            failure(
                "output_failed",
                "Output destination is not available; no query was submitted.",
                1,
            )
        })?;
    }
    Ok(prepared)
}

pub(super) async fn before_submission<T>(
    work: impl Future<Output = Result<T, Failure>>,
    deadline: Instant,
) -> Result<T, Failure> {
    if Instant::now() >= deadline {
        return Err(failure(
            "timed_out",
            "The total execution deadline elapsed before query submission.",
            1,
        ));
    }
    tokio::select! {
        biased;
        signal = tokio::signal::ctrl_c() => match signal {
            Ok(()) => Err(failure("cancelled", "Execution cancelled before query submission.", 2)),
            Err(_) => Err(failure("signal_failed", "The cancellation handler could not start; no query was submitted.", 1)),
        },
        _ = tokio::time::sleep_until(deadline.into()) => Err(failure(
            "timed_out", "The total execution deadline elapsed before query submission.", 1,
        )),
        result = work => result,
    }
}

fn diagnostic(
    code: &str,
    message: &str,
    evidence: Option<&ExecutionEvidence>,
    exit: u8,
) -> TextPlan {
    let mut payload = serde_json::to_vec(&serde_json::json!({
        "error": { "code": code, "message": message },
        "operation": {
            "id": evidence.and_then(|value| value.operation_id.as_deref()),
            "mutationMayHaveOccurred": evidence.is_some_and(|value| value.mutation_may_have_occurred),
            "httpStatus": evidence.and_then(|value| value.http_status),
            "remoteCancellationRequested": false,
            "serviceError": evidence.and_then(|value| value.raw_service_error.as_deref()),
        }
    })).expect("bounded diagnostic values serialize");
    payload.push(b'\n');
    TextPlan {
        records: vec![RenderPlan {
            stream: OutputStream::Stderr,
            exit_code: exit,
            payload,
        }],
        exit_code: exit,
    }
}

/// One owned delivery retains operation evidence even if stdout fails later.
pub(super) struct Delivery {
    plan: TextPlan,
    outcome: CommandOutcome,
    evidence: Option<ExecutionEvidence>,
}

enum Publication {
    Complete(CommandOutcome),
    Diagnostic(Delivery),
}

#[derive(Debug)]
struct WriteFailure {
    error: io::Error,
    evidence: Option<ExecutionEvidence>,
}

impl std::fmt::Display for WriteFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let id = self
            .evidence
            .as_ref()
            .and_then(|value| value.operation_id.as_deref());
        write!(
            formatter,
            "Execution output failed; data may be incomplete. Operation ID: {id:?}. Query will not be replayed."
        )
    }
}

impl std::error::Error for WriteFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl Delivery {
    fn failed(code: &str, message: &str, evidence: Option<ExecutionEvidence>, exit: u8) -> Self {
        Self {
            plan: diagnostic(code, message, evidence.as_ref(), exit),
            outcome: super::failure_outcome(code, exit),
            evidence,
        }
    }

    fn retain_tabular_evidence(mut self, error: &fabric_sdk_tabular::TabularError) -> Self {
        if let fabric_sdk_tabular::TabularError::Metadata { evidence } = error {
            let record = &mut self.plan.records[0];
            let mut diagnostic: serde_json::Value =
                serde_json::from_slice(&record.payload).expect("owned execution diagnostic");
            diagnostic["metadataEvidence"] = serde_json::Value::Array(
                evidence
                    .iter()
                    .map(|entry| {
                        serde_json::json!({
                            "location": format!("{:?}", entry.location),
                            "classification": format!("{:?}", entry.classification),
                            "key": entry.key.as_str(),
                            "value": entry.value.as_str(),
                        })
                    })
                    .collect(),
            );
            record.payload = serde_json::to_vec(&diagnostic).expect("bounded metadata evidence");
            record.payload.push(b'\n');
        }
        self
    }

    fn abandoned_output(interruption: Failure, evidence: Option<ExecutionEvidence>) -> Publication {
        let (code, message, exit) = interruption;
        let mut delivery = Self::failed(
            code,
            &format!(
                "{message} Result output is unconfirmed and may be partial; an in-flight write may still complete. The query will not be replayed or remotely cancelled. This host must stop rather than reuse stdout."
            ),
            evidence,
            exit,
        );
        // Cancelled ordinarily lets the REPL continue, but an abandoned stdout
        // worker may still be writing. Stop both the batch and the shell.
        delivery.outcome = CommandOutcome::Stop(exit);
        Publication::Diagnostic(delivery)
    }

    async fn publish_native(self, deadline: Instant) -> Publication {
        if !self
            .plan
            .records
            .iter()
            .any(|record| record.stream == OutputStream::Stdout)
        {
            return if self.plan.records.is_empty() {
                Publication::Complete(self.outcome)
            } else {
                Publication::Diagnostic(self)
            };
        }
        let output = match owned_stdout() {
            Ok(output) => output,
            Err(error) if cfg!(windows) && error.kind() == io::ErrorKind::Unsupported => {
                return Publication::Diagnostic(Self::failed(
                    "UnsupportedCapability",
                    &format!(
                        "{WINDOWS_CONSOLE_OUTPUT_MESSAGE} The query may have completed, but its result was not written. Do not replay automatically."
                    ),
                    self.evidence,
                    1,
                ));
            }
            Err(_) => {
                return Publication::Diagnostic(Self::failed(
                    "output_failed",
                    "The query result stdout handle could not be retained; no query was replayed.",
                    self.evidence,
                    1,
                ));
            }
        };
        self.publish_stdout(output, deadline, async {
            match tokio::signal::ctrl_c().await {
                Ok(()) => failure("cancelled", "Local result output waiting was cancelled.", 2),
                Err(_) => failure(
                    "signal_failed",
                    "The result output cancellation handler failed.",
                    1,
                ),
            }
        })
        .await
    }

    async fn publish_stdout(
        self,
        mut output: impl Write + Send + 'static,
        deadline: Instant,
        cancelled: impl Future<Output = Failure>,
    ) -> Publication {
        let evidence = self.evidence.clone();
        if self
            .plan
            .records
            .iter()
            .any(|record| record.stream != OutputStream::Stdout)
        {
            return Publication::Diagnostic(Self::failed(
                "output_failed",
                "The query delivery mixed data and diagnostic records; no output was published.",
                evidence,
                1,
            ));
        }
        if Instant::now() >= deadline {
            return Self::abandoned_output(
                failure(
                    "timed_out",
                    "The total deadline elapsed before result output.",
                    1,
                ),
                evidence,
            );
        }
        let worker =
            tokio::task::spawn_blocking(move || self.write_to(&mut output, &mut io::sink()));
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline.into()) => Self::abandoned_output(
                failure("timed_out", "The total deadline elapsed during result output.", 1),
                evidence,
            ),
            interruption = cancelled => Self::abandoned_output(interruption, evidence),
            result = worker => match result {
                Ok(Ok(outcome)) => Publication::Complete(outcome),
                Ok(Err(error)) => Publication::Diagnostic(Self::failed(
                    "output_failed",
                    &format!(
                        "Writing or flushing query output failed ({:?}); data may be partial. The query will not be replayed.",
                        error.kind()
                    ),
                    evidence,
                    1,
                )),
                Err(_) => Self::abandoned_output(
                    failure("output_failed", "The result output worker did not complete.", 1),
                    evidence,
                ),
            },
        }
    }

    pub(super) fn write_to(
        self,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> io::Result<CommandOutcome> {
        let written = self.plan.write_to(stdout, stderr).and_then(|()| {
            if self
                .plan
                .records
                .iter()
                .any(|record| record.stream == OutputStream::Stdout)
            {
                stdout.flush()?;
            }
            if self
                .plan
                .records
                .iter()
                .any(|record| record.stream == OutputStream::Stderr)
            {
                stderr.flush()?;
            }
            Ok(())
        });
        written.map_err(|error| {
            io::Error::new(
                error.kind(),
                WriteFailure {
                    error,
                    evidence: self.evidence,
                },
            )
        })?;
        Ok(self.outcome)
    }
}

pub(super) async fn dispatch(
    approved: &ApprovedExecution,
    deadline: Instant,
    sdk: &Client,
    executor: HttpExecutor,
) -> Result<get::Delivery, Failure> {
    if Instant::now() >= deadline {
        return Ok(get::Delivery::execution(Delivery::failed(
            "timed_out",
            "The total execution deadline elapsed before query submission.",
            None,
            1,
        )));
    }
    let destination = approved
        .command()
        .output
        .as_ref()
        .map(FileDestination::preflight)
        .transpose()
        .map_err(|_| {
            failure(
                "output_failed",
                "Output destination became unavailable; no query was submitted.",
                1,
            )
        })?;
    let reason = Arc::new(AtomicU8::new(0));
    let cause = Arc::clone(&reason);
    let cancelled = async move {
        tokio::select! {
            biased;
            signal = tokio::signal::ctrl_c() => {
                cause.store(if signal.is_ok() { 2 } else { 3 }, Ordering::SeqCst);
            },
            _ = tokio::time::sleep_until(deadline.into()) => { cause.store(1, Ordering::SeqCst); },
        }
    };
    let limits = OperationLimits {
        max_response_bytes: std::num::NonZeroUsize::new(approved.command().max_bytes)
            .expect("validated response bound"),
        ..OperationLimits::default()
    };
    let mut session = OperationSession::with_context(sdk.context(), executor, limits, cancelled);
    let result = approved.execute(&mut session).await;
    if result.is_ok() {
        let _ = session.check_cancelled().await;
    }
    let interrupted = reason.load(Ordering::SeqCst);
    if interrupted != 0 {
        let (code, message, exit) = match interrupted {
            1 => (
                "timed_out",
                "The total execution deadline elapsed; remote query execution was not cancelled and may still complete. Do not replay an uncertain submission.",
                1,
            ),
            2 => (
                "cancelled",
                "Local execution observation was cancelled; remote query execution was not cancelled and may still complete. Do not replay an uncertain submission.",
                2,
            ),
            _ => (
                "signal_failed",
                "The cancellation handler failed; inspect operation evidence before any further action.",
                1,
            ),
        };
        return Ok(get::Delivery::execution(Delivery::failed(
            code,
            message,
            Some(ExecutionEvidence::from(
                &session.evidence(OperationErrorKind::Cancelled),
            )),
            exit,
        )));
    }
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let (code, message, evidence, exit) = match &error {
                ExecutionFailure::Query(error) => {
                    let operation = error.operation_error();
                    let status = operation.and_then(|error| match &error.kind {
                        OperationErrorKind::Http { status, .. } => Some(*status),
                        _ => None,
                    });
                    let (code, exit) = match status {
                        Some(401) => ("Unauthorized", 4),
                        Some(403) => ("Forbidden", 1),
                        _ => ("execution_failed", 1),
                    };
                    let message = status.map_or_else(
                        || format!(
                            "Native Dataflow query failed or is incomplete. Classification: {:?}. No query was replayed.",
                            error.kind()
                        ),
                        |status| format!(
                            "Native Dataflow query was rejected with HTTP {status}. No query was replayed."
                        ),
                    );
                    (code, message, operation.map(ExecutionEvidence::from), exit)
                }
                ExecutionFailure::Result {
                    error, evidence, ..
                } => (
                    "execution_failed",
                    format!(
                        "The complete Dataflow result could not be decoded or serialized: {error}. No query was replayed."
                    ),
                    Some(ExecutionEvidence::from(evidence.as_ref())),
                    1,
                ),
            };
            let mut delivery = Delivery::failed(code, &message, evidence, exit);
            if let ExecutionFailure::Result { error, .. } = &error {
                delivery = delivery.retain_tabular_evidence(error);
                delivery.outcome = CommandOutcome::Stop(delivery.plan.exit_code);
            }
            return Ok(get::Delivery::execution(delivery));
        }
    };
    if Instant::now() >= deadline {
        return Ok(get::Delivery::execution(Delivery::failed(
            "timed_out",
            "Execution completed, but the total deadline elapsed before output publication. No query was replayed.",
            Some(result.evidence),
            1,
        )));
    }
    let evidence = Some(result.evidence);
    let plan = if let Some(destination) = destination {
        let work = tokio::task::spawn_blocking(move || destination.publish(&result.bytes));
        let publication = tokio::select! {
            biased;
            signal = tokio::signal::ctrl_c() => {
                let code = if signal.is_ok() { "cancelled" } else { "signal_failed" };
                return Ok(get::Delivery::execution(Delivery::failed(
                    code,
                    "Local output waiting stopped. A partial file may exist and in-flight output may still complete. The completed query will not be replayed or remotely cancelled.",
                    evidence,
                    if signal.is_ok() { 2 } else { 1 },
                )));
            },
            result = tokio::time::timeout_at(deadline.into(), work) => result,
        };
        match publication {
            Ok(Ok(Ok(()))) => TextPlan {
                records: Vec::new(),
                exit_code: 0,
            },
            Ok(Ok(Err(error))) => {
                return Ok(get::Delivery::execution(Delivery::failed(
                    "output_failed",
                    &format!("{error} The query completed and will not be replayed."),
                    evidence,
                    1,
                )));
            }
            _ => {
                return Ok(get::Delivery::execution(Delivery::failed(
                    "output_failed",
                    "Output publication did not complete within the total deadline. A partial file may exist and in-flight local output may still complete. The query will not be replayed.",
                    evidence,
                    1,
                )));
            }
        }
    } else {
        TextPlan {
            records: vec![RenderPlan {
                stream: OutputStream::Stdout,
                exit_code: 0,
                payload: result.bytes,
            }],
            exit_code: 0,
        }
    };
    Ok(get::Delivery::execution(Delivery {
        plan,
        outcome: CommandOutcome::Returned(0),
        evidence,
    }))
}
