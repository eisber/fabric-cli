// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Composable handlers for all eight Python `job` commands.
//!
//! This crate is a CLI layer, not an identity provider or process entry point.
//! The front door injects context, SDK, input files, presentation, confirmation,
//! output timestamps and cancellation. See the compatibility matrix for the
//! pinned source and integration boundary.

mod http;
mod parser;
mod payload;

pub use http::HttpJobExecutor;
pub use parser::{JobCommand, JobKind, ScheduleArgs, command_from_matches, job_subcommand};
pub use payload::{JsonInput, NoFiles, execution_data, json_input, schedule_configuration};

use fabric_cli_context::{
    Element, HomeContext, Lookup, NodeKind, PathInput, ResolveOptions, Resolver,
};
use fabric_cli_output::{
    CommandError, Indent, OutputEnvelope, OutputError, OutputInput, OutputStream, RenderPlan,
    Timestamp,
};
use fabric_sdk_jobs::{
    JobClient, JobError, JobOperationReconciler, JobState, JobStatus, JobTarget, JobType,
    OperationError, RunHandle, WaitOptions,
};
use parser::invalid;
use serde_json::{Value, json};
use std::{
    future::{Future, poll_fn},
    io::{self, Write},
    pin::{Pin, pin},
    sync::Arc,
    task::Poll,
};

/// Caller-runtime future for asynchronous confirmation; no library prompt is installed.
pub type Confirmation<'a> = Pin<Box<dyn Future<Output = Result<bool, CommandError>> + Send + 'a>>;

/// Source presentation event, preserving stdout/stderr and logging distinctions.
#[derive(Clone, Debug, PartialEq)]
pub enum JobEvent {
    /// Source grey text on stderr, including empty-line events.
    Grey(String),
    /// Source warning on stderr, not an error envelope.
    Warning(String),
    /// Source progress text on stderr.
    Progress(String),
    /// Logging warning, not a successful command result.
    LogWarning(String),
    /// Source raw stdout table, including the odd JSON-mode failure/unknown branch.
    Table(Value),
}

/// Injected user interaction and immediate event presentation.
///
/// The host must route events correctly and propagate output errors. It owns
/// terminal rendering (including width/color), not the SDK or these handlers.
pub trait JobUi: Send + Sync {
    /// Present one source event; a writer failure must be returned, never swallowed.
    fn emit(&self, event: JobEvent) -> Result<(), CommandError>;
    /// Ask the existing host confirmation question; no ambient terminal is opened here.
    fn confirm(&self) -> Confirmation<'_>;
}

/// Output data and source text-presentation flags for the front door's shared renderer.
#[derive(Clone, Debug)]
pub struct JobOutput {
    /// Source output envelope fields.
    pub input: OutputInput,
    /// List/status tables request headers.
    pub show_headers: bool,
    /// Async/successful execution emits an ID key-value list in text mode.
    pub show_key_value_list: bool,
    /// Source events emitted after the result has been written, not before it.
    pub after_output: Vec<JobEvent>,
}

/// A command failure retaining the original generic-operation reconciliation evidence.
///
/// Operation output is projected only when requested, through the borrowed shared
/// CLI formatter. It is never normalized again through `CommandError::new`.
#[derive(Debug)]
pub enum JobCommandError {
    /// Existing command/context/input/presentation failure.
    Command(CommandError),
    /// Original shared-operation classification, raw diagnostics, ID and uncertainty.
    Operation(Box<OperationError>),
}

impl JobCommandError {
    /// Build the source failure envelope without consuming reconciliation evidence.
    pub fn envelope(&self, timestamp: Timestamp, command: Option<String>) -> OutputEnvelope {
        match self {
            Self::Command(error) => error.envelope(timestamp, command),
            Self::Operation(error) => {
                let mut output = fabric_cli_mutations::operation_error_output(error);
                output.command = command;
                OutputEnvelope::new(timestamp, output)
            }
        }
    }
}

impl From<CommandError> for JobCommandError {
    fn from(error: CommandError) -> Self {
        Self::Command(error)
    }
}

impl JobOutput {
    fn new(data: Option<Value>, message: Option<String>, headers: bool, key_value: bool) -> Self {
        Self {
            input: OutputInput {
                command: Some("job".into()),
                data,
                message,
                ..OutputInput::default()
            },
            show_headers: headers,
            show_key_value_list: key_value,
            after_output: Vec::new(),
        }
    }
}

/// Output failures, which must never become a successful command result.
#[derive(Debug, thiserror::Error)]
pub enum JobWriteError {
    /// The existing output renderer rejected its input.
    #[error(transparent)]
    Output(#[from] OutputError),
    /// A caller-owned output stream failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The host could not present a deferred source event.
    #[error("job output presentation failed")]
    Presentation(CommandError),
}

/// Explicit job configuration snapshot, with the source cancellation default.
#[derive(Clone, Copy, Debug)]
pub struct JobConfig {
    /// `job_cancel_ontimeout != "false"` in Python; defaults to true.
    pub cancel_on_timeout: bool,
}

impl Default for JobConfig {
    fn default() -> Self {
        Self {
            cancel_on_timeout: true,
        }
    }
}

/// Job command composition over real SDK execution and immutable caller-owned context.
pub struct JobHost<'a> {
    /// Real job service with an injected credential/shared executor.
    pub sdk: &'a JobClient,
    /// Shared generic-operation capability, needed only for source-selected LRO responses.
    pub operations: Option<&'a dyn JobOperationReconciler>,
    /// Preloaded metadata; synchronous lookup must never perform network I/O.
    pub lookup: &'a dyn Lookup,
    /// Explicit current Fabric context.
    pub current: Arc<Element>,
    /// Explicit home selection, without account discovery.
    pub home: HomeContext,
    /// JSON input files resolved by the host.
    pub files: &'a dyn JsonInput,
    /// Immediate presentation and asynchronous confirmation.
    pub ui: &'a dyn JobUi,
    /// Configuration captured at the command boundary.
    pub config: JobConfig,
    /// Caller-owned timestamp used only by JSON rendering.
    pub timestamp: Timestamp,
}

impl JobHost<'_> {
    /// Execute a complete job leaf. `None` preserves source no-envelope branches.
    ///
    /// Text and JSON front doors use the same handler; the returned presentation
    /// flags allow the existing host renderer to own text layout. Cancellation
    /// wins ready results and drops local work; it does not undo remote mutations.
    pub async fn execute(
        &self,
        command: &JobCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Option<JobOutput>, JobCommandError> {
        let mut cancelled = pin!(cancelled);
        self.execute_inner(command, cancelled.as_mut()).await
    }

    /// Execute and render through the existing Python JSON envelope implementation.
    ///
    /// Text front doors call [`Self::execute`] and their shared text renderer.
    /// Explicit text requests fail before service I/O in this JSON-only convenience.
    /// This materializes JSON only; use [`Self::run_json`] to emit deferred hints
    /// after writing the result in source order.
    pub async fn dispatch_json(
        &self,
        command: &JobCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<RenderPlan, OutputError> {
        let result = if command
            .output_format
            .as_deref()
            .is_some_and(|value| value != "json")
        {
            Err(JobCommandError::from(command_error(
                "NotSupported",
                "Text output requires the host's text renderer",
            )))
        } else {
            self.execute(command, cancelled).await
        };
        match result {
            Ok(Some(output)) => {
                OutputEnvelope::new(self.timestamp.clone(), output.input).render(Indent::Four)
            }
            Ok(None) => Ok(RenderPlan {
                stream: OutputStream::Stdout,
                exit_code: 0,
                payload: Vec::new(),
            }),
            Err(error) => error
                .envelope(self.timestamp.clone(), Some(command.command_path()))
                .render(Indent::Four),
        }
    }

    /// Execute and write JSON to caller-owned streams, then emit deferred source hints.
    ///
    /// Writer/presentation failures propagate, including after a partial result.
    pub async fn run_json(
        &self,
        command: &JobCommand,
        cancelled: impl Future<Output = ()> + Send,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> Result<u8, JobWriteError> {
        let result = if command
            .output_format
            .as_deref()
            .is_some_and(|value| value != "json")
        {
            Err(JobCommandError::from(command_error(
                "NotSupported",
                "Text output requires the host's text renderer",
            )))
        } else {
            self.execute(command, cancelled).await
        };
        let (plan, after) = match result {
            Ok(Some(output)) => (
                OutputEnvelope::new(self.timestamp.clone(), output.input).render(Indent::Four)?,
                output.after_output,
            ),
            Ok(None) => return Ok(0),
            Err(error) => (
                error
                    .envelope(self.timestamp.clone(), Some(command.command_path()))
                    .render(Indent::Four)?,
                Vec::new(),
            ),
        };
        plan.write_to(stdout, stderr)?;
        for event in after {
            self.ui.emit(event).map_err(JobWriteError::Presentation)?;
        }
        Ok(plan.exit_code)
    }

    async fn execute_inner(
        &self,
        command: &JobCommand,
        mut cancelled: std::pin::Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<Option<JobOutput>, JobCommandError> {
        if poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await {
            return Err(
                command_error("LongRunningOperationCancelled", "Operation cancelled").into(),
            );
        }
        if command.skill.is_some() {
            return Err(command_error(
                "NotSupported",
                "Fabric skill headers require coordinated host header plumbing",
            )
            .into());
        }
        let wait_options = WaitOptions {
            timeout: command.timeout,
            polling_interval: command.polling_interval,
        };
        if matches!(command.kind, JobKind::Run | JobKind::Start) {
            wait_options
                .validate()
                .map_err(|e| sdk_error(e, command.id.as_deref()))?;
        }
        if command.path.is_empty() {
            return Err(invalid("A job path is required").into());
        }
        let resolver = Resolver::new(self.lookup, self.current.clone(), self.home.clone());
        let tokens: Vec<_> = command.path.iter().map(String::as_str).collect();
        let context = resolver
            .resolve(PathInput::Tokens(&tokens), ResolveOptions::default())
            .map_err(|e| command_error(&e.code, e.message))?;
        let job_type = match context.kind() {
            NodeKind::Item(kind) => JobType::from_item_type(kind.as_str()),
            _ => None,
        }
        .ok_or_else(|| {
            command_error(
                "UnsupportedCommand",
                format!(
                    "not supported for command 'job {}'",
                    if command.kind == JobKind::Start {
                        "run"
                    } else {
                        command.kind.as_str()
                    }
                ),
            )
        })?;
        let workspace = context
            .workspace()
            .ok_or_else(|| invalid("A job item requires a workspace"))?;
        let target = JobTarget::new(
            workspace
                .id()
                .ok_or_else(|| invalid("The workspace has no ID"))?,
            context
                .id()
                .ok_or_else(|| invalid("The job item has no ID"))?,
            job_type,
        )
        .map_err(|e| sdk_error(e, None))?;
        let id = || {
            command
                .id
                .as_deref()
                .ok_or_else(|| invalid("A job ID is required"))
        };
        match command.kind {
            JobKind::Start | JobKind::Run => {
                let data = execution_data(command, job_type, self.files)?;
                self.ui.emit(JobEvent::Grey(format!(
                    "{} job ({}) for '{}'...",
                    if command.kind == JobKind::Start {
                        "Starting"
                    } else {
                        "Running"
                    },
                    if command.kind == JobKind::Start {
                        "async"
                    } else {
                        "sync"
                    },
                    context.name()
                )))?;
                let handle = self
                    .sdk
                    .submit(&target, data, cancelled.as_mut())
                    .await
                    .map_err(|e| sdk_error(e, None))?;
                if command.kind == JobKind::Start {
                    self.ui.emit(JobEvent::Grey(String::new()))?;
                    let mut output = JobOutput::new(
                        Some(json!({"id": handle.id()})),
                        Some(format!("Job instance '{}' created", handle.id())),
                        false,
                        true,
                    );
                    output
                        .after_output
                        .push(JobEvent::Grey(status_hint(&context, handle.id())));
                    Ok(Some(output))
                } else {
                    self.ui.emit(JobEvent::Grey(format!(
                        "\u{221f} Job instance '{}' created",
                        handle.id()
                    )))?;
                    self.ui.emit(JobEvent::Grey(match command.timeout {
                        Some(timeout) => format!("\u{221f} Timeout: {timeout} seconds"),
                        None => "\u{221f} Timeout: no timeout specified".into(),
                    }))?;
                    self.wait(
                        &handle,
                        wait_options,
                        true,
                        &command.command_path(),
                        cancelled.as_mut(),
                    )
                    .await
                }
            }
            JobKind::Cancel => {
                let id = id()?;
                self.ui.emit(JobEvent::Grey(format!(
                    "Cancelling job instance for item {} with id: '{id}'...",
                    context.path()
                )))?;
                if command.wait {
                    let Some(handle) = self
                        .sdk
                        .cancel_for_wait(&target, id, self.operations, cancelled.as_mut())
                        .await
                        .map_err(|e| sdk_error(e, Some(id)))?
                    else {
                        return Ok(None);
                    };
                    self.wait(
                        &handle,
                        WaitOptions::default(),
                        false,
                        &command.command_path(),
                        cancelled.as_mut(),
                    )
                    .await
                } else {
                    self.sdk
                        .cancel(&target, id, cancelled.as_mut())
                        .await
                        .map_err(|e| sdk_error(e, Some(id)))?;
                    let mut output = JobOutput::new(
                        None,
                        Some(format!("Job instance '{id}' cancelled (async)")),
                        false,
                        false,
                    );
                    output
                        .after_output
                        .push(JobEvent::Grey(status_hint(&context, id)));
                    Ok(Some(output))
                }
            }
            JobKind::Status => {
                let id = id()?;
                let value = if command.schedule {
                    self.sdk
                        .get_schedule(&target, id, cancelled.as_mut())
                        .await
                        .map_err(|e| sdk_error(e, Some(id)))?
                } else {
                    self.sdk
                        .status(&target, id, cancelled.as_mut())
                        .await
                        .map_err(|e| sdk_error(e, Some(id)))?
                        .into_value()
                };
                Ok(Some(JobOutput::new(Some(value), None, true, false)))
            }
            JobKind::List => {
                let values = self
                    .sdk
                    .list(&target, command.schedule, cancelled.as_mut())
                    .await
                    .map_err(|e| sdk_error(e, None))?
                    .value;
                if values.is_empty() {
                    Ok(Some(JobOutput::new(
                        None,
                        Some(
                            if command.schedule {
                                "No schedules found"
                            } else {
                                "No runs found"
                            }
                            .into(),
                        ),
                        false,
                        false,
                    )))
                } else {
                    let keys: Vec<_> = values[0]
                        .as_object()
                        .ok_or_else(|| sdk_error(JobError::InvalidResponse, None))?
                        .keys()
                        .cloned()
                        .collect();
                    let projected = values
                        .iter()
                        .map(|row| {
                            let mut object = serde_json::Map::new();
                            for key in &keys {
                                if let Some(value) = row.get(key) {
                                    object.insert(key.clone(), value.clone());
                                }
                            }
                            Value::Object(object)
                        })
                        .collect();
                    Ok(Some(JobOutput::new(
                        Some(Value::Array(projected)),
                        None,
                        true,
                        false,
                    )))
                }
            }
            JobKind::Schedule => {
                let config = schedule_configuration(
                    &command.schedule_args,
                    command.input.as_deref(),
                    false,
                    self.files,
                )?;
                self.ui.emit(JobEvent::Grey(format!(
                    "Creating job schedule for '{}'...",
                    context.name()
                )))?;
                let config = config.ok_or_else(|| {
                    CommandError::unexpected(Some(
                        "the JSON object must be str, bytes or bytearray, not NoneType".into(),
                    ))
                })?;
                let payload =
                    payload::schedule_payload(config, json!(command.schedule_args.enable))?;
                let value = self
                    .sdk
                    .create_schedule(&target, &payload, cancelled.as_mut())
                    .await
                    .map_err(|e| {
                        if matches!(&e, JobError::Http { code, .. } if code == "EntityConflict") {
                            command_error("NotSupported", "Only one schedule per item is supported")
                                .into()
                        } else {
                            sdk_error(e, None)
                        }
                    })?;
                Ok(Some(JobOutput::new(
                    None,
                    Some(format!(
                        "Job schedule '{}' created",
                        value["id"].as_str().expect("validated ID")
                    )),
                    false,
                    false,
                )))
            }
            JobKind::Update => {
                let id = id()?;
                let config = schedule_configuration(
                    &command.schedule_args,
                    command.input.as_deref(),
                    true,
                    self.files,
                )?;
                let args = &command.schedule_args;
                if args.enable && args.disable {
                    return Err(invalid("You must specify one of --enable --disable").into());
                }
                let existing = self
                    .sdk
                    .get_schedule(&target, id, cancelled.as_mut())
                    .await
                    .map_err(|e| sdk_error(e, Some(id)))?;
                let payload = if let Some(config) = config {
                    let enabled = if !args.enable && !args.disable {
                        existing
                            .get("enabled")
                            .cloned()
                            .ok_or_else(|| sdk_error(JobError::InvalidResponse, Some(id)))?
                    } else {
                        json!(args.enable)
                    };
                    payload::schedule_payload(config, enabled)?
                } else {
                    let old_config = existing
                        .get("configuration")
                        .ok_or_else(|| sdk_error(JobError::InvalidResponse, Some(id)))?
                        .clone();
                    json!({"configuration": old_config, "enabled": args.enable})
                };
                let value = self
                    .sdk
                    .update_schedule(&target, id, &payload, cancelled.as_mut())
                    .await
                    .map_err(|e| sdk_error(e, Some(id)))?;
                Ok(Some(JobOutput::new(
                    None,
                    Some(format!(
                        "Job schedule {} updated",
                        value["id"].as_str().expect("validated ID")
                    )),
                    false,
                    false,
                )))
            }
            JobKind::Remove => {
                let id = id()?;
                if !command.force {
                    self.ui.emit(JobEvent::Warning(format!("You are about to delete schedule '{id}' from '{}'. This action cannot be undone.", context.name())))?;
                    let mut confirmation = pin!(self.ui.confirm());
                    let confirmed = poll_fn(|cx| {
                        if cancelled.as_mut().poll(cx).is_ready() {
                            Poll::Ready(Err(command_error(
                                "LongRunningOperationCancelled",
                                "Operation cancelled",
                            )))
                        } else {
                            confirmation.as_mut().poll(cx)
                        }
                    })
                    .await?;
                    if !confirmed {
                        return Ok(None);
                    }
                }
                self.ui.emit(JobEvent::Grey(format!(
                    "Removing job schedule '{id}'... from '{}'",
                    context.name()
                )))?;
                self.sdk
                    .remove_schedule(&target, id, cancelled.as_mut())
                    .await
                    .map_err(|e| sdk_error(e, Some(id)))?;
                Ok(Some(JobOutput::new(
                    None,
                    Some(format!("Job schedule '{id}' removed")),
                    false,
                    false,
                )))
            }
        }
    }

    async fn wait(
        &self,
        handle: &RunHandle,
        options: WaitOptions,
        timeout_policy: bool,
        command_path: &str,
        mut cancelled: std::pin::Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<Option<JobOutput>, JobCommandError> {
        let mut observer_error = None;
        let ui = self.ui;
        let result = self
            .sdk
            .wait(
                handle,
                options,
                |status: &JobStatus| {
                    let result = match status.state() {
                        JobState::Failed => ui.emit(JobEvent::Table(status.as_value().clone())),
                        JobState::Unknown => ui
                            .emit(JobEvent::LogWarning(format!(
                                "Job instance '{}' unknown status: {}",
                                handle.id(),
                                status.status()
                            )))
                            .and_then(|()| ui.emit(JobEvent::Table(status.as_value().clone()))),
                        _ => ui.emit(JobEvent::Progress(format!(
                            "Job instance status: {}",
                            status.status()
                        ))),
                    };
                    result.map_err(|error| {
                        observer_error = Some(error);
                        JobError::Observer
                    })
                },
                cancelled.as_mut(),
            )
            .await;
        if let Some(error) = observer_error {
            return Err(error.into());
        }
        match result {
            Ok(status) => match status.state() {
                JobState::Completed => {
                    self.ui.emit(JobEvent::Grey(String::new()))?;
                    let mut output = JobOutput::new(
                        Some(json!({"id": handle.id()})),
                        Some(format!("Job instance '{}' completed", handle.id())),
                        false,
                        true,
                    );
                    output.input.command = Some(command_path.into());
                    Ok(Some(output))
                }
                JobState::Cancelled | JobState::Deduped => {
                    self.ui.emit(JobEvent::Grey(String::new()))?;
                    self.ui.emit(JobEvent::LogWarning(format!(
                        "Job instance {} finished with status: {}",
                        handle.id(),
                        status.status()
                    )))?;
                    Ok(None)
                }
                JobState::Unknown => Ok(None),
                _ => Err(sdk_error(JobError::InvalidResponse, Some(handle.id()))),
            },
            Err(JobError::TimedOut { elapsed_seconds }) if timeout_policy => {
                self.ui.emit(JobEvent::Warning(format!(
                    "Job instance '{}' timed out after {elapsed_seconds} seconds",
                    handle.id()
                )))?;
                if !self.config.cancel_on_timeout {
                    self.ui.emit(JobEvent::Grey("Job still running. To change this behaviour and cancel on timeout, set job_cancel_ontimeout config property to 'true'".into()))?;
                    Ok(None)
                } else {
                    self.ui.emit(JobEvent::Grey(format!("Cancelling job instance '{}' (timeout). To change this behaviour and continue running on timeout, set job_cancel_ontimeout config property to 'false'", handle.id())))?;
                    let cancelled = self
                        .sdk
                        .cancel_for_wait(
                            handle.target(),
                            handle.id(),
                            self.operations,
                            cancelled.as_mut(),
                        )
                        .await
                        .map_err(|e| sdk_error(e, Some(handle.id())))?;
                    if cancelled.is_none() {
                        return Ok(None);
                    }
                    self.ui.emit(JobEvent::Grey(String::new()))?;
                    Ok(Some(JobOutput::new(
                        Some(json!({"id": handle.id()})),
                        Some(format!("Job instance '{}' cancelled (async)", handle.id())),
                        false,
                        true,
                    )))
                }
            }
            Err(error) => Err(sdk_error(error, Some(handle.id()))),
        }
    }
}

fn status_hint(context: &Element, id: &str) -> String {
    format!(
        "\u{2192} To see status run 'job run-status {} --id {id}'",
        context.path()
    )
}

fn command_error(code: &str, message: impl Into<String>) -> CommandError {
    CommandError::new(Some(message.into()), Some(code.into()))
}

fn sdk_error(error: JobError, id: Option<&str>) -> JobCommandError {
    let error = match error {
        JobError::Operation(error) => return JobCommandError::Operation(Box::new(error)),
        JobError::OperationReconcilerRequired => command_error("NotSupported", error.to_string()),
        JobError::Paging(error) => match error.kind {
            fabric_sdk_jobs::PagingErrorKind::Fetch(error) => return sdk_error(error, id),
            fabric_sdk_jobs::PagingErrorKind::Cancelled => {
                command_error("LongRunningOperationCancelled", "Operation cancelled")
            }
            kind => command_error("UnexpectedError", kind.to_string()),
        },
        JobError::Http {
            status,
            code,
            service_error,
            raw_service_error,
            ..
        } => {
            let message = match status {
                401 => "Access is unauthorized".into(),
                403 => {
                    "Access is forbidden. You do not have permission to access this resource".into()
                }
                404 => "The requested resource could not be found".into(),
                _ => raw_service_error
                    .as_deref()
                    .and_then(|value| value.get("message"))
                    .and_then(Value::as_str)
                    .filter(|message| !message.is_empty())
                    .map(str::to_owned)
                    .or_else(|| service_error.map(|e| e.message))
                    .unwrap_or_else(|| "An error occurred while processing the operation".into()),
            };
            command_error(&code, message)
        }
        JobError::Failed { .. } => command_error(
            "JobFailed",
            format!("Job instance '{}' Failed", id.unwrap_or("")),
        ),
        JobError::Cancelled => {
            command_error("LongRunningOperationCancelled", "Operation cancelled")
        }
        JobError::PollingExceedsTimeout { timeout, interval } => invalid(format!(
            "Custom polling interval ({interval}s) cannot be greater than or equal to timeout ({timeout}s)"
        )),
        JobError::InvalidIdentifier | JobError::InvalidPollingInterval => {
            invalid(error.to_string())
        }
        _ => command_error("UnexpectedError", error.to_string()),
    };
    error.into()
}
