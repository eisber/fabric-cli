// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Typed TABLE command registration and SDK/output composition on the caller's runtime.
//!
//! The front door supplies resolved OneLake context, service execution, generic
//! jobs, configuration, clock and streams. No ambient authentication or global
//! CLI state is accessed.

mod decimal;
mod operations;
mod options;
mod parser;
mod shared;

pub use operations::{FabricLoadCompletion, TableOperationObserver};
pub use options::{load_options, optimize, vacuum, vacuum_with_digit_limit};
pub use parser::{TableCommand, TableOperation, command};
pub use shared::{LoadCompletion, SharedTableExecutor, StorageReads};

use fabric_cli_context::ItemType;
use fabric_cli_output::{
    CommandError, Indent, OutputEnvelope, OutputError, OutputInput, OutputStream, RenderPlan,
    Timestamp,
};
use fabric_sdk_tables::{
    LoadOutcome, MaintenanceExecutor, MaintenanceJobType, MaintenanceOutcome, TableClient,
    TableError, TableErrorKind, TableExecutor, TableFuture, TableLimits, TablePath,
};
use serde_json::{Value, json};
use std::{
    fmt,
    future::{Future, poll_fn},
    io::{self, Write},
    pin::pin,
    task::Poll,
};

/// Result of shared context resolution, with explicit resource classification.
///
/// This must be an actual resolved OneLake child, not an owning Item or an OS
/// path. The context host owns traversal/FS_LS eligibility and final existence.
#[derive(Clone, Debug)]
pub struct ResolvedTablePath {
    /// Resolved workspace/item/local address.
    pub path: TablePath,
    /// Canonical item type; schema accepts more types than mutations.
    pub item_type: ItemType,
}

/// Async context boundary owned by the front door/OneLake lane, not another cache.
pub trait TableResolver: Send + Sync {
    /// Resolve raw Python `nargs='+'` tokens relative to the host's current/home.
    ///
    /// `must_exist=false` suppresses only final-path not-found for table load.
    /// All other failures, OneLake HEADs and shortcut resolution remain visible.
    fn resolve<'a>(
        &'a self,
        path: &'a [String],
        must_exist: bool,
        cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<ResolvedTablePath, CommandError>>;
}

/// Source output intent, available to the front door's shared text renderer.
#[derive(Clone, Debug)]
pub struct TableOutput {
    /// JSON envelope input or no source success output for Deduped/Cancelled jobs.
    pub input: Option<OutputInput>,
    /// Source asks for schema column headers.
    pub show_headers: bool,
    /// Source asks for job IDs to be displayed as key/value pairs.
    pub show_key_value_list: bool,
}

/// Text rendering is shared CLI policy, not a second table-specific formatter.
pub trait TableTextOutput: Send + Sync {
    /// Render source output intent; no-output terminal jobs must remain empty.
    fn render(&self, output: &TableOutput) -> Result<RenderPlan, OutputError>;
    /// Render Python command errors using the shared text/error stream policy.
    fn error(&self, error: &CommandError) -> Result<RenderPlan, OutputError>;
    /// Render an already-normalized shared error without constructing a second error.
    fn normalized_error(&self, input: &OutputInput) -> Result<RenderPlan, OutputError> {
        self.render(&TableOutput {
            input: Some(input.clone()),
            show_headers: false,
            show_key_value_list: false,
        })
    }
}

/// Command failure retaining the original SDK evidence until explicit presentation.
#[derive(Clone, Debug)]
pub enum TableFailure {
    /// Source parser/context/option failure.
    Command(CommandError),
    /// Option conversion failure; unexpected Python conversions escape to main.
    Options(CommandError),
    /// Original table/operation/storage failure, never flattened before projection.
    Sdk(Box<TableError>),
}
impl fmt::Display for TableFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .write_str("TABLE command failed; inspect normalized output and retained SDK evidence")
    }
}
impl std::error::Error for TableFailure {}
impl From<CommandError> for TableFailure {
    fn from(error: CommandError) -> Self {
        Self::Command(error)
    }
}
impl From<TableError> for TableFailure {
    fn from(error: TableError) -> Self {
        Self::Sdk(Box::new(error))
    }
}
impl TableFailure {
    /// Project once through the existing shared error policy, preserving normalized text.
    pub fn envelope(&self, timestamp: Timestamp, command: Option<String>) -> OutputEnvelope {
        match self {
            Self::Command(error) => error.envelope(timestamp, command),
            Self::Options(error) => {
                let envelope = error.envelope(timestamp.clone(), command);
                if envelope.result().error_code() == Some("UnexpectedError") {
                    error.envelope(timestamp, None)
                } else {
                    envelope
                }
            }
            Self::Sdk(error) => {
                if let TableErrorKind::SharedOperation(operation) = &error.kind {
                    OutputEnvelope::new(
                        timestamp,
                        OutputInput {
                            command,
                            ..fabric_cli_mutations::operation_error_output(operation)
                        },
                    )
                } else if let TableErrorKind::OneLake(storage) = &error.kind
                    && matches!(
                        storage.as_ref(),
                        fabric_sdk_onelake::OneLakeError::Http { .. }
                    )
                {
                    OutputEnvelope::new(
                        timestamp,
                        OutputInput {
                            command,
                            ..fabric_cli_mutations::OneLakeReadError::Sdk((**storage).clone())
                                .output()
                        },
                    )
                } else {
                    table_error((**error).clone()).envelope(timestamp, command)
                }
            }
        }
    }
}

/// Immutable command-local host. All side effects are caller-owned boundaries.
pub struct TableHost<'a> {
    /// Explicitly configured TABLE SDK.
    pub sdk: &'a TableClient,
    /// Shared HTTP/Fabric operation adapter.
    pub executor: &'a dyn TableExecutor,
    /// Shared generic-job adapter.
    pub jobs: &'a dyn MaintenanceExecutor,
    /// Shared remote context resolution.
    pub resolver: &'a dyn TableResolver,
    /// Explicit current configuration default (`json` or `text`).
    pub output_format: &'a str,
    /// Optional shared text formatter; missing text support fails explicitly.
    pub text: Option<&'a dyn TableTextOutput>,
    /// Caller-injected timestamp.
    pub timestamp: Timestamp,
    /// Finite SDK read/source bounds.
    pub limits: TableLimits,
}

/// Output failures are propagated, never translated to successful empty output.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// Invalid output data.
    #[error(transparent)]
    Output(#[from] OutputError),
    /// Caller stream failed, potentially after a partial write.
    #[error(transparent)]
    Io(#[from] io::Error),
}

enum ExecutionFailure {
    Command(TableFailure),
    Io(io::Error),
}
impl From<CommandError> for ExecutionFailure {
    fn from(error: CommandError) -> Self {
        Self::Command(error.into())
    }
}
impl From<io::Error> for ExecutionFailure {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<TableError> for ExecutionFailure {
    fn from(error: TableError) -> Self {
        Self::Command(error.into())
    }
}
impl From<TableFailure> for ExecutionFailure {
    fn from(error: TableFailure) -> Self {
        Self::Command(error)
    }
}

impl TableHost<'_> {
    /// Execute the SDK and return source output intent without terminal progress.
    ///
    /// Use [`Self::run`] for source-ordered progress on the caller's stderr.
    pub async fn execute(
        &self,
        command: &TableCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TableOutput, CommandError> {
        self.execute_detailed(command, cancelled)
            .await
            .map_err(|error| match error {
                TableFailure::Command(error) | TableFailure::Options(error) => error,
                TableFailure::Sdk(error) => table_error(*error),
            })
    }

    /// Execute without progress I/O, retaining SDK failures and normalized output semantics.
    ///
    /// This is the source-fidelity boundary used by dispatch; the legacy
    /// [`Self::execute`] returns only the older `CommandError` view.
    pub async fn execute_detailed(
        &self,
        command: &TableCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TableOutput, TableFailure> {
        match self
            .execute_with_diagnostics(command, cancelled, &mut io::sink())
            .await
        {
            Ok(output) => Ok(output),
            Err(ExecutionFailure::Command(error)) => Err(error),
            Err(ExecutionFailure::Io(_)) => unreachable!("std::io::Sink never fails"),
        }
    }

    /// Execute with source-ordered diagnostics while retaining original SDK evidence.
    ///
    /// The caller renders the returned result using its shared text/JSON policy.
    /// A diagnostic write failure stops execution; it never replays a mutation.
    pub async fn execute_observed(
        &self,
        command: &TableCommand,
        cancelled: impl Future<Output = ()> + Send,
        diagnostics: &mut (impl Write + Send),
    ) -> io::Result<Result<TableOutput, TableFailure>> {
        match self
            .execute_with_diagnostics(command, cancelled, diagnostics)
            .await
        {
            Ok(output) => Ok(Ok(output)),
            Err(ExecutionFailure::Command(error)) => Ok(Err(error)),
            Err(ExecutionFailure::Io(error)) => Err(error),
        }
    }

    async fn execute_with_diagnostics(
        &self,
        command: &TableCommand,
        cancelled: impl Future<Output = ()> + Send,
        diagnostics: &mut (impl Write + Send),
    ) -> Result<TableOutput, ExecutionFailure> {
        let mut cancelled = pin!(cancelled);
        if poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await {
            return Err(
                options::error("LongRunningOperationCancelled", "Operation cancelled").into(),
            );
        }
        let target = self
            .resolver
            .resolve(
                &command.path,
                !matches!(command.operation, TableOperation::Load { .. }),
                Box::pin(cancelled.as_mut()),
            )
            .await?;
        let schema = matches!(command.operation, TableOperation::Schema);
        let mut output = TableOutput {
            input: None,
            show_headers: schema,
            show_key_value_list: false,
        };
        let input = match &command.operation {
            TableOperation::Schema => {
                writeln!(
                    diagnostics,
                    "Getting schema for '{}' table...",
                    target.path.table_name()
                )?;
                let schema = self
                    .sdk
                    .schema_string(self.executor, &target.path, self.limits, cancelled.as_mut())
                    .await?;
                writeln!(diagnostics, "Schema extracted successfully")?;
                // Preserve malformed-source classification separately from the
                // shared output decoder's finite-integer safety boundary.
                serde_json::from_str::<Value>(&schema)
                    .map_err(|_| TableError::new(TableErrorKind::InvalidResponse))?;
                let decoded = fabric_cli_output::parse_data(&schema)
                    .map_err(|_| TableFailure::Command(CommandError::invalid_json()))?;
                let data = fabric_sdk_tables::schema_fields_value(&decoded)?;
                OutputInput {
                    data: Some(data),
                    ..OutputInput::default()
                }
            }
            TableOperation::Load {
                file,
                mode,
                format,
                extension,
            } => {
                writeln!(
                    diagnostics,
                    "Loading '{}' table. It may take some time...",
                    target.path.table_name()
                )?;
                let source = self
                    .resolver
                    .resolve(file, false, Box::pin(cancelled.as_mut()))
                    .await?;
                let kind = self
                    .sdk
                    .source_kind(self.executor, &source.path, self.limits, cancelled.as_mut())
                    .await?;
                let options = load_options(mode, format.as_deref(), extension.as_deref())
                    .map_err(TableFailure::Options)?;
                if self.limits.max_requests.get() < 2 {
                    return Err(TableError::new(TableErrorKind::Limit).into());
                }
                let mut limits = self.limits;
                limits.max_requests =
                    std::num::NonZeroUsize::new(limits.max_requests.get() - 1).unwrap();
                let result = self
                    .sdk
                    .load_with_kind(
                        self.executor,
                        &target.path,
                        &source.path,
                        kind,
                        &options,
                        limits,
                        cancelled.as_mut(),
                    )
                    .await?;
                OutputInput {
                    message: Some(match result {
                        LoadOutcome::Completed => {
                            format!("Table '{}' loaded successfully", target.path.table_name())
                        }
                        LoadOutcome::Accepted { .. } => "Load table operation started".into(),
                        LoadOutcome::NoOutput => return Ok(output),
                    }),
                    ..OutputInput::default()
                }
            }
            TableOperation::Vacuum { retain_n_hours } => {
                let maintenance = vacuum(retain_n_hours).map_err(TableFailure::Options)?;
                writeln!(
                    diagnostics,
                    "Vacuuming the '{}' table. It may take some time...",
                    target.path.table_name()
                )?;
                return self
                    .maintenance(
                        command.leaf_name(),
                        &target.path,
                        maintenance.execution_data(&target.path),
                        target.item_type,
                        Box::pin(cancelled.as_mut()),
                        diagnostics,
                    )
                    .await;
            }
            TableOperation::Optimize { vorder, zorder } => {
                let maintenance = optimize(*vorder, zorder.as_deref());
                writeln!(
                    diagnostics,
                    "Optimizing the '{}' table. It may take some time...",
                    target.path.table_name()
                )?;
                return self
                    .maintenance(
                        command.leaf_name(),
                        &target.path,
                        maintenance.execution_data(&target.path),
                        target.item_type,
                        Box::pin(cancelled.as_mut()),
                        diagnostics,
                    )
                    .await;
            }
        };
        output.input = Some(OutputInput {
            command: Some("table".into()),
            ..input
        });
        Ok(output)
    }

    async fn maintenance(
        &self,
        command: &str,
        target: &TablePath,
        config: Value,
        item_type: ItemType,
        cancelled: TableFuture<'_, ()>,
        diagnostics: &mut (impl Write + Send),
    ) -> Result<TableOutput, ExecutionFailure> {
        let job_type = MaintenanceJobType::from_item_type(item_type.as_str()).ok_or_else(|| {
            options::error("UnsupportedCommand", "not supported for command 'job run'")
        })?;
        let mut io_error = None;
        let mut observe = |event| {
            if let Err(error) = maintenance_diagnostic(diagnostics, event) {
                io_error = Some(error);
                return Err(TableError::new(TableErrorKind::Execution));
            }
            Ok(())
        };
        let result = self
            .jobs
            .run_observed_for_job(target, config, job_type, &mut observe, cancelled)
            .await;
        if let Some(error) = io_error {
            return Err(ExecutionFailure::Io(error));
        }
        let result = result?;
        let input = match result {
            MaintenanceOutcome::Completed { instance_id } => Some(OutputInput {
                command: Some(command.into()),
                message: Some(format!("Job instance '{instance_id}' completed")),
                data: Some(json!({"id": instance_id})),
                ..OutputInput::default()
            }),
            MaintenanceOutcome::CancellationAccepted { instance_id } => Some(OutputInput {
                command: Some("table".into()),
                message: Some(format!("Job instance '{instance_id}' cancelled (async)")),
                data: Some(json!({"id": instance_id})),
                ..OutputInput::default()
            }),
            MaintenanceOutcome::TerminalWithoutOutput { .. }
            | MaintenanceOutcome::TimedOutWithoutCancellation { .. } => None,
        };
        Ok(TableOutput {
            input,
            show_headers: false,
            show_key_value_list: true,
        })
    }

    /// Execute and render final output without terminal progress.
    ///
    /// Use [`Self::run`] for the full caller-owned stdout/stderr behavior.
    pub async fn dispatch(
        &self,
        command: &TableCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<RenderPlan, OutputError> {
        let format = command
            .output_format
            .as_deref()
            .unwrap_or(self.output_format);
        // Fail before service work if the host cannot render the requested format.
        let result =
            if !matches!(format, "json" | "text") || (format == "text" && self.text.is_none()) {
                Err(options::error(
                    "NotSupported",
                    "The host has not configured the requested TABLE output format",
                )
                .into())
            } else {
                self.execute_detailed(command, cancelled).await
            };
        self.render_result(command, result)
    }

    fn render_result(
        &self,
        command: &TableCommand,
        result: Result<TableOutput, TableFailure>,
    ) -> Result<RenderPlan, OutputError> {
        let format = command
            .output_format
            .as_deref()
            .unwrap_or(self.output_format);
        if format == "text"
            && let Some(text) = self.text
        {
            return match result {
                Ok(output) => text.render(&output),
                Err(TableFailure::Command(error) | TableFailure::Options(error)) => {
                    text.error(&error)
                }
                Err(TableFailure::Sdk(error)) => {
                    if let TableErrorKind::SharedOperation(operation) = &error.kind {
                        text.normalized_error(&fabric_cli_mutations::operation_error_output(
                            operation,
                        ))
                    } else if let TableErrorKind::OneLake(storage) = &error.kind
                        && matches!(
                            storage.as_ref(),
                            fabric_sdk_onelake::OneLakeError::Http { .. }
                        )
                    {
                        text.normalized_error(
                            &fabric_cli_mutations::OneLakeReadError::Sdk((**storage).clone())
                                .output(),
                        )
                    } else {
                        text.error(&table_error(*error))
                    }
                }
            };
        }
        match result {
            Ok(TableOutput {
                input: Some(input), ..
            }) => OutputEnvelope::new(self.timestamp.clone(), input).render(Indent::Four),
            Ok(_) => Ok(RenderPlan {
                stream: OutputStream::Stdout,
                exit_code: 0,
                payload: Vec::new(),
            }),
            Err(error) => error
                .envelope(self.timestamp.clone(), Some(command.leaf_name().into()))
                .render(Indent::Four),
        }
    }

    /// Write to caller-owned streams; no flush, retry or fallback hides a write error.
    pub async fn run(
        &self,
        command: &TableCommand,
        cancelled: impl Future<Output = ()> + Send,
        stdout: &mut impl Write,
        stderr: &mut (impl Write + Send),
    ) -> Result<u8, WriteError> {
        let format = command
            .output_format
            .as_deref()
            .unwrap_or(self.output_format);
        let result =
            if !matches!(format, "json" | "text") || (format == "text" && self.text.is_none()) {
                Err(options::error(
                    "NotSupported",
                    "The host has not configured the requested TABLE output format",
                )
                .into())
            } else {
                match self
                    .execute_with_diagnostics(command, cancelled, stderr)
                    .await
                {
                    Ok(output) => Ok(output),
                    Err(ExecutionFailure::Command(error)) => Err(error),
                    Err(ExecutionFailure::Io(error)) => return Err(WriteError::Io(error)),
                }
            };
        let plan = self.render_result(command, result)?;
        plan.write_to(stdout, stderr)?;
        Ok(plan.exit_code)
    }
}

fn maintenance_diagnostic(
    writer: &mut impl Write,
    event: fabric_sdk_tables::MaintenanceEvent,
) -> io::Result<()> {
    use fabric_sdk_tables::MaintenanceEvent;
    match event {
        MaintenanceEvent::Created {
            instance_id,
            timeout,
        } => {
            writeln!(writer, "\u{221f} Job instance '{instance_id}' created")?;
            match timeout {
                Some(seconds) => writeln!(writer, "\u{221f} Timeout: {seconds} seconds"),
                None => writeln!(writer, "\u{221f} Timeout: no timeout specified"),
            }
        }
        MaintenanceEvent::Status { status, .. } => {
            match status.as_str() {
                "NotStarted" | "InProgress" | "Completed" | "Cancelled" | "Deduped" => {
                    writeln!(writer, "\u{221f} Job instance status: {status}")?
                }
                _ => {}
            }
            if matches!(status.as_str(), "Completed" | "Cancelled" | "Deduped") {
                writeln!(writer)?;
            }
            Ok(())
        }
        MaintenanceEvent::TimedOut {
            instance_id,
            elapsed_seconds,
            cancelling,
        } => {
            writeln!(
                writer,
                "Job instance '{instance_id}' timed out after {elapsed_seconds} seconds"
            )?;
            if cancelling {
                writeln!(
                    writer,
                    "Cancelling job instance '{instance_id}' (timeout). To change this behaviour and continue running on timeout, set job_cancel_ontimeout config property to 'false'"
                )
            } else {
                writeln!(
                    writer,
                    "Job still running. To change this behaviour and cancel on timeout, set job_cancel_ontimeout config property to 'true'"
                )
            }
        }
    }
}

fn table_error(error: TableError) -> CommandError {
    match error.kind {
        TableErrorKind::InvalidInput => options::error(
            "InvalidPath",
            "Invalid path. Please provide a valid file or directory",
        ),
        TableErrorKind::InvalidDeltaTable => options::error(
            "InvalidDeltaTable",
            "Failed to extract the table schema. Please ensure the path points to a valid Delta table",
        ),
        TableErrorKind::InvalidResponse => {
            CommandError::unexpected(Some("Invalid table service response".into()))
        }
        TableErrorKind::Credential => options::error("Unauthorized", "Access is unauthorized"),
        TableErrorKind::Cancelled | TableErrorKind::OperationCancelled => {
            options::error("LongRunningOperationCancelled", "Operation cancelled")
        }
        TableErrorKind::OperationFailed => {
            options::error("LongRunningOperationFailed", "Operation failed")
        }
        TableErrorKind::TimedOut => options::error(
            "OperationTimedOut",
            "Table operation did not complete before its deadline",
        ),
        TableErrorKind::Limit => options::error(
            "OperationLimit",
            "Table operation exceeded its configured bound",
        ),
        TableErrorKind::Execution => options::error(
            "RequestFailed",
            "Table request failed or its completion is unconfirmed",
        ),
        TableErrorKind::SharedOperation(error) => operations::command_error(*error),
        TableErrorKind::OneLake(error) => {
            use fabric_sdk_onelake::{OneLakeError, OneLakeExecutionError};
            let kind = match *error {
                OneLakeError::InvalidInput | OneLakeError::InvalidAudience => {
                    TableErrorKind::InvalidInput
                }
                OneLakeError::Credential => TableErrorKind::Credential,
                OneLakeError::Execution(OneLakeExecutionError::TimedOut) => {
                    TableErrorKind::TimedOut
                }
                OneLakeError::Execution(OneLakeExecutionError::Cancelled)
                | OneLakeError::Cancelled => TableErrorKind::Cancelled,
                OneLakeError::Limit
                | OneLakeError::Execution(OneLakeExecutionError::ResponseLimit) => {
                    TableErrorKind::Limit
                }
                OneLakeError::Execution(_) => TableErrorKind::Execution,
                OneLakeError::Http { status, code, .. } => TableErrorKind::Http {
                    status,
                    code,
                    message: None,
                },
                OneLakeError::InvalidResponse | OneLakeError::RepeatedContinuation => {
                    TableErrorKind::InvalidResponse
                }
            };
            table_error(TableError::new(kind))
        }
        TableErrorKind::Maintenance { code, message, .. } => options::error(&code, message),
        TableErrorKind::Http {
            status,
            code,
            message,
        } => {
            let fallback = match status {
                401 => "Unauthorized",
                403 => "Forbidden",
                404 => "NotFound",
                _ => "UnknownError",
            };
            let message = match status {
                401 => "Access is unauthorized".into(),
                403 => {
                    "Access is forbidden. You do not have permission to access this resource".into()
                }
                404 => "The requested resource could not be found".into(),
                _ => message
                    .unwrap_or_else(|| "An error occurred while processing the operation".into()),
            };
            options::error(code.as_deref().unwrap_or(fallback), message)
        }
    }
}
