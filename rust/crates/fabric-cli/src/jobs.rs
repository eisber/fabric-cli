// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Runtime-neutral job composition shared by native and offline hosts.

use crate::{
    errors,
    parser::JobReadCommand,
    session::{ReadSession, snapshot::Snapshot},
};
use fabric_cli_jobs::{
    Confirmation, JobCommand, JobCommandError, JobConfig, JobEvent, JobHost, JobUi, JsonInput,
    NoFiles,
};
use fabric_cli_output::{
    CommandError, OutputEnvelope, OutputStream,
    text::{self, OutputFormat, Presentation, TextOptions, TextPlan},
};
use fabric_sdk_jobs::{
    JobClient, JobExecutionError, JobExecutor, JobFuture, JobLimits, JobRequest, JobRequestHeaders,
    JobResponse, SharedJobOperations,
};
use std::{future::Future, pin::pin, sync::Arc};

/// One job read over the same explicit credential and metadata session as LS/GET.
///
/// This composes existing discovery, resolver, job handlers and rendering. It
/// installs no runtime, environment, credential, transport, retry or polling
/// policy. The job executor must share the caller's HTTP/time configuration.
pub struct JobReadSession<'a, 'session> {
    /// Existing read dependencies used only for command-local metadata discovery.
    pub reads: &'a ReadSession<'session>,
    /// Existing job execution adapter over caller-owned HTTP/time.
    pub jobs: Arc<dyn JobExecutor>,
}

impl JobReadSession<'_, '_> {
    /// Resolve context, execute a read-only leaf and render its actual streams.
    ///
    /// Uses the SDK credential and selection for both discovery and job requests.
    /// Ordinary errors retain source command envelopes and exits, including
    /// Unauthorized exit 4. Rendering and writer failures are not success.
    pub async fn dispatch(
        &self,
        command: &JobReadCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TextPlan, text::TextError> {
        let family = command.family_command();
        JobSession {
            reads: self.reads,
            jobs: Arc::clone(&self.jobs),
            files: &NoFiles,
            ui: &ReadOnlyUi,
            config: JobConfig::default(),
        }
        .dispatch(&family, cancelled)
        .await
    }
}

/// Caller-owned job capabilities over the existing metadata and credential session.
///
/// The family owns execution, polling, timeout and request semantics. This host
/// owns only metadata loading, file/UI capability injection and final rendering.
pub struct JobSession<'a, 'session> {
    /// Existing read dependencies, including the selected SDK credential.
    pub reads: &'a ReadSession<'session>,
    /// Job execution over the same caller-owned HTTP and time policies.
    pub jobs: Arc<dyn JobExecutor>,
    /// Explicit execution/configuration file reader.
    pub files: &'a dyn JsonInput,
    /// Immediate source events and confirmation, with caller-owned streams.
    pub ui: &'a dyn JobUi,
    /// Command-boundary configuration, including timeout cancellation.
    pub config: JobConfig,
}

impl JobSession<'_, '_> {
    /// Load context and invoke the released handler, retaining streams and exits.
    ///
    /// Immediate events go through `ui`; deferred hints are appended after the
    /// final envelope. Dropping this future cancels local work, not remote jobs.
    pub async fn dispatch(
        &self,
        command: &JobCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TextPlan, text::TextError> {
        let format = OutputFormat::resolve(command.output_format.as_deref(), Some("json"))?;
        let mut cancelled = pin!(cancelled);
        let result = async {
            let reads = self.reads;
            if reads.tenant.id() != Some(reads.sdk.selection().tenant())
                || reads.tenant.kind() != fabric_cli_context::NodeKind::Tenant
                || reads.sdk.selection().audience() != fabric_sdk::Audience::Fabric
                || !reads.current_path.starts_with('/')
            {
                return Err(JobCommandError::from(errors::error(
                    "InvalidInput",
                    "Invalid selected read context",
                )));
            }
            JobRequestHeaders::with_skill(command.skill.as_deref())
                .map_err(|error| errors::error("InvalidInput", error.to_string()))?;
            let mut snapshot = Snapshot::default();
            let current = snapshot
                .resolve(reads, &reads.tenant, reads.current_path, cancelled.as_mut())
                .await?;
            let path = command.path.join(" ");
            snapshot
                .resolve(
                    reads,
                    &current,
                    path.trim_matches(['\'', '"']),
                    cancelled.as_mut(),
                )
                .await?;
            let jobs: Arc<dyn JobExecutor> = Arc::new(CheckedJobExecutor(Arc::clone(&self.jobs)));
            let sdk = JobClient::with_context(
                reads.sdk.context().clone(),
                Arc::clone(&jobs),
                JobLimits::default(),
            );
            let operations = SharedJobOperations::from_job_executor_with_context(
                reads.sdk.context().clone(),
                jobs,
                fabric_sdk_operations::OperationLimits::default(),
            );
            JobHost {
                sdk: &sdk,
                operations: Some(&operations),
                lookup: &snapshot,
                current,
                home: reads.home.clone(),
                files: self.files,
                ui: self.ui,
                config: self.config,
                timestamp: reads.timestamp.clone(),
            }
            .execute(command, cancelled.as_mut())
            .await
        }
        .await;
        let (envelope, headers, key_value, after) = match result {
            Ok(Some(output)) => (
                OutputEnvelope::new(self.reads.timestamp.clone(), output.input),
                output.show_headers,
                output.show_key_value_list,
                output.after_output,
            ),
            Ok(None) => {
                return Ok(TextPlan {
                    records: Vec::new(),
                    exit_code: 0,
                });
            }
            Err(error) => (
                error.envelope(self.reads.timestamp.clone(), Some(command.command_path())),
                false,
                false,
                Vec::new(),
            ),
        };
        let mut plan = text::render(
            &envelope,
            format,
            &TextOptions {
                show_headers: headers,
                key_value,
                ..TextOptions::default()
            },
            &text::ascii_width,
            Presentation::default(),
        )?;
        for event in after {
            plan.records.extend(render_job_event(&event)?.records);
        }
        Ok(plan)
    }
}

/// Render source job events with the same explicit non-TTY presentation as results.
///
/// Failed/unknown status tables remain raw stdout, including in JSON mode.
pub fn render_job_event(event: &JobEvent) -> Result<TextPlan, text::TextError> {
    let presentation = Presentation::default();
    Ok(match event {
        JobEvent::Grey(message) => text::render_line(
            message,
            text::Style::Grey,
            OutputStream::Stderr,
            presentation,
        ),
        JobEvent::Warning(message) | JobEvent::LogWarning(message) => {
            text::render_notice(text::Notice::Warning(message), None, presentation)
        }
        JobEvent::Progress(message) => text::render_notice(
            text::Notice::Progress {
                text: message,
                progress: None,
            },
            None,
            presentation,
        ),
        JobEvent::Table(value) => {
            let fields = value
                .as_object()
                .ok_or(text::TextError::InvalidTableEntries)?
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            text::render_table(value, &fields, true, 80, &text::ascii_width, presentation)?
        }
    })
}

/// Native file capability, reading only a file explicitly selected by the command.
pub struct SelectedJobFiles;

impl JsonInput for SelectedJobFiles {
    fn read_json(&self, path: &str) -> Result<serde_json::Value, CommandError> {
        let content = std::fs::read_to_string(path).map_err(|error| {
            errors::error(
                "NotRunnable",
                format!("Invalid JSON content: {path}. Error: {error}"),
            )
        })?;
        fabric_cli_output::parse_data(&content).map_err(|error| {
            errors::error(
                "NotRunnable",
                format!("Invalid JSON content: {path}. Error: {error}"),
            )
        })
    }
}

struct ReadOnlyUi;

pub(crate) struct CheckedJobExecutor(pub(crate) Arc<dyn JobExecutor>);

impl JobExecutor for CheckedJobExecutor {
    fn supports_request_headers(&self, headers: &JobRequestHeaders) -> bool {
        self.0.supports_request_headers(headers)
    }

    fn execute<'a>(
        &'a self,
        request: JobRequest<'a>,
    ) -> JobFuture<'a, Result<JobResponse, JobExecutionError>> {
        Box::pin(async move {
            let method = request.method();
            let schedule_creation = method == azure_core::http::Method::Post
                && request.url().path().ends_with("/schedules");
            let response = self.0.execute(request).await?;
            let consumes_json = (response.status == 200
                && matches!(
                    method,
                    azure_core::http::Method::Get | azure_core::http::Method::Patch
                ))
                || (response.status == 201
                    && (schedule_creation || method == azure_core::http::Method::Get));
            if consumes_json && !response.body.is_empty() {
                // Reject values before serde_json can round oversized integer tokens.
                let json =
                    std::str::from_utf8(&response.body).map_err(|_| JobExecutionError::Failed)?;
                fabric_cli_output::parse_data(json).map_err(|_| JobExecutionError::Failed)?;
            }
            Ok(response)
        })
    }

    fn wait(&self, delay: std::time::Duration) -> JobFuture<'_, Result<(), JobExecutionError>> {
        self.0.wait(delay)
    }
}

impl JobUi for ReadOnlyUi {
    fn emit(&self, _: JobEvent) -> Result<(), CommandError> {
        Err(errors::unsupported("Job reads cannot emit mutation events"))
    }

    fn confirm(&self) -> Confirmation<'_> {
        Box::pin(async { Err(errors::unsupported("Job reads cannot request confirmation")) })
    }
}
