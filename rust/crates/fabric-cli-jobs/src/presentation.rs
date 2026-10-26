// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{Confirmation, JobCommand, JobCommandError, JobEvent, JobHost, JobOutput, JobUi};
use fabric_cli_output::{
    CommandError, Indent, OutputEnvelope, OutputError, OutputStream, Timestamp,
    text::{
        self, CharacterWidth, ErrorDecoration, Notice, OutputFormat, Presentation, Style,
        TextError, TextOptions, TextPlan,
    },
};
use std::{
    future::Future,
    io::{self, Write},
    sync::Mutex,
};

/// Caller-injected output selection, text properties and terminal framing.
///
/// This installs no global output configuration, terminal probing, logger or
/// Unicode approximation. Explicit command format takes precedence over `format`.
pub struct JobRenderOptions<'a> {
    /// Already-selected configuration default.
    pub format: OutputFormat,
    /// Existing shared text flags, width and formatting callbacks.
    pub text: TextOptions<'a>,
    /// Source-equivalent character widths supplied by the host.
    pub widths: &'a (dyn CharacterWidth + Send + Sync),
    /// Existing shared caller-owned color/newline/fallback policy.
    pub presentation: Presentation,
}

/// Output failure, retaining partial writes instead of printing a false result.
#[derive(Debug, thiserror::Error)]
pub enum JobRenderError {
    /// The shared JSON renderer rejected its input.
    #[error(transparent)]
    Json(#[from] OutputError),
    /// The shared text renderer rejected a source value or Unicode property.
    #[error(transparent)]
    Text(#[from] TextError),
    /// A caller-owned stream failed, possibly after a partial write.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The shared generic-operation error needs a normalized text-error boundary.
    ///
    /// Retains classification, raw diagnostics, known ID and mutation uncertainty
    /// rather than double-normalizing a source message or formatting a fallback.
    #[error("generic-operation text projection requires a shared normalized-error renderer")]
    OperationTextProjectionRequired(Box<fabric_sdk_jobs::OperationError>),
}

impl JobOutput {
    /// Render the actual result through the shared JSON/text implementation.
    ///
    /// Job table/key-value intent overrides only the corresponding flags;
    /// terminal width, formatting callbacks and presentation remain caller-owned.
    /// Deferred events are separate so hosts preserve result-before-hint order.
    pub fn render(
        &self,
        timestamp: Timestamp,
        options: &JobRenderOptions<'_>,
    ) -> Result<TextPlan, JobRenderError> {
        let envelope = OutputEnvelope::new(timestamp, self.input.clone());
        let text_options = TextOptions {
            show_headers: self.show_headers,
            key_value: self.show_key_value_list,
            ..options.text.clone()
        };
        Ok(text::render(
            &envelope,
            options.format,
            &text_options,
            &|character| options.widths.width(character),
            options.presentation,
        )?)
    }
}

impl JobEvent {
    /// Render one immediate/deferred source event, including raw tables in JSON mode.
    ///
    /// Logging warnings are source UI warnings, not successful result envelopes.
    pub fn render(&self, options: &JobRenderOptions<'_>) -> Result<TextPlan, JobRenderError> {
        Ok(match self {
            Self::Grey(value) => text::render_line(
                value,
                Style::Grey,
                OutputStream::Stderr,
                options.presentation,
            ),
            Self::Warning(value) | Self::LogWarning(value) => {
                text::render_notice(Notice::Warning(value), None, options.presentation)
            }
            Self::Progress(value) => text::render_notice(
                Notice::Progress {
                    text: value,
                    progress: None,
                },
                None,
                options.presentation,
            ),
            Self::Table(value) => {
                let fields = value
                    .as_object()
                    .ok_or(TextError::InvalidTableEntries)?
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>();
                text::render_table(
                    value,
                    &fields,
                    true,
                    options.text.terminal_width.unwrap_or(80),
                    &|character| options.widths.width(character),
                    options.presentation,
                )?
            }
        })
    }
}

struct StreamState<'a, O, E> {
    stdout: &'a mut O,
    stderr: &'a mut E,
    failure: Option<JobRenderError>,
}

struct RenderingUi<'a, 'b, O, E> {
    original: &'a dyn JobUi,
    options: &'a JobRenderOptions<'a>,
    streams: &'a Mutex<StreamState<'b, O, E>>,
}

impl<O: Write + Send, E: Write + Send> RenderingUi<'_, '_, O, E> {
    fn write_event(&self, event: JobEvent) -> Result<(), JobRenderError> {
        let mut streams = self
            .streams
            .lock()
            .map_err(|_| io::Error::other("job output lock poisoned"))?;
        let StreamState { stdout, stderr, .. } = &mut *streams;
        write_rendered(event.render(self.options), stdout, stderr)?;
        Ok(())
    }
}

fn write_rendered(
    rendered: Result<TextPlan, JobRenderError>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<u8, JobRenderError> {
    match rendered {
        Ok(plan) => {
            plan.write_to(stdout, stderr)?;
            Ok(plan.exit_code)
        }
        Err(JobRenderError::Text(error)) => {
            if let Some(partial) = error.partial_output() {
                partial.write_to(stdout, stderr)?;
            }
            Err(JobRenderError::Text(error))
        }
        Err(error) => Err(error),
    }
}

impl<O: Write + Send, E: Write + Send> JobUi for RenderingUi<'_, '_, O, E> {
    fn emit(&self, event: JobEvent) -> Result<(), CommandError> {
        if let Err(error) = self.write_event(event) {
            match self.streams.lock() {
                Ok(mut state) => state.failure = Some(error),
                Err(_) => {
                    return Err(CommandError::unexpected(Some(
                        "job output lock poisoned".into(),
                    )));
                }
            }
            return Err(CommandError::unexpected(Some(
                "job presentation failed".into(),
            )));
        }
        Ok(())
    }

    fn confirm(&self) -> Confirmation<'_> {
        self.original.confirm()
    }
}

impl JobHost<'_> {
    /// Execute and write the complete job command using shared JSON/text output.
    ///
    /// Immediate events, raw failure tables, result output and deferred hints use
    /// the same caller-owned writers in source order. Confirmation delegates to
    /// the existing UI; its `emit` is not called again, avoiding duplicate output.
    /// Output failures stop local work and propagate without fallback writes,
    /// invented cleanup, another HTTP request or a successful command result.
    pub async fn run(
        &self,
        command: &JobCommand,
        cancelled: impl Future<Output = ()> + Send,
        options: &JobRenderOptions<'_>,
        stdout: &mut (impl Write + Send),
        stderr: &mut (impl Write + Send),
    ) -> Result<u8, JobRenderError> {
        let format = OutputFormat::resolve(
            command.output_format.as_deref(),
            Some(match options.format {
                OutputFormat::Json => "json",
                OutputFormat::Text => "text",
            }),
        )?;
        let selected = JobRenderOptions {
            format,
            text: options.text.clone(),
            widths: options.widths,
            presentation: options.presentation,
        };
        let streams = Mutex::new(StreamState {
            stdout,
            stderr,
            failure: None,
        });
        let ui = RenderingUi {
            original: self.ui,
            options: &selected,
            streams: &streams,
        };
        let scoped = JobHost {
            sdk: self.sdk,
            operations: self.operations,
            lookup: self.lookup,
            current: self.current.clone(),
            home: self.home.clone(),
            files: self.files,
            ui: &ui,
            config: self.config,
            timestamp: self.timestamp.clone(),
        };
        let result = scoped.execute(command, cancelled).await;
        let mut state = streams
            .lock()
            .map_err(|_| io::Error::other("job output lock poisoned"))?;
        if let Some(error) = state.failure.take() {
            return Err(error);
        }
        let (rendered, after) = match result {
            Ok(Some(output)) => (
                output.render(self.timestamp.clone(), &selected),
                output.after_output,
            ),
            Ok(None) => return Ok(0),
            Err(error) => (
                error.render(
                    self.timestamp.clone(),
                    Some(&command.command_path()),
                    &selected,
                ),
                Vec::new(),
            ),
        };
        let StreamState { stdout, stderr, .. } = &mut *state;
        let exit = write_rendered(rendered, stdout, stderr)?;
        for event in after {
            write_rendered(event.render(&selected), stdout, stderr)?;
        }
        Ok(exit)
    }
}

impl JobCommandError {
    /// Render a failure through shared output, borrowing the original evidence.
    pub fn render(
        &self,
        timestamp: Timestamp,
        command: Option<&str>,
        options: &JobRenderOptions<'_>,
    ) -> Result<TextPlan, JobRenderError> {
        if options.format == OutputFormat::Json {
            let plan = self
                .envelope(timestamp, command.map(str::to_owned))
                .render(Indent::Four)?;
            return Ok(TextPlan {
                exit_code: plan.exit_code,
                records: vec![plan],
            });
        }
        match self {
            Self::Command(error) => Ok(text::render_error(error, command, options.presentation)),
            Self::Service { source, projection } => {
                let decoration = match source.as_ref() {
                    fabric_sdk_jobs::JobError::Http {
                        status,
                        raw_service_error,
                        ..
                    } if ![401, 403, 404].contains(status) => ErrorDecoration::Fabric {
                        request_id: raw_service_error
                            .as_deref()
                            .and_then(|v| v.get("requestId"))
                            .and_then(serde_json::Value::as_str),
                        details: &[],
                        verbose: false,
                    },
                    _ => ErrorDecoration::Plain,
                };
                Ok(text::render_decorated_error(
                    projection,
                    command,
                    decoration,
                    options.presentation,
                ))
            }
            Self::Operation(error) => Err(JobRenderError::OperationTextProjectionRequired(
                error.clone(),
            )),
        }
    }
}
