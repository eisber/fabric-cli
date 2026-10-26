// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Runtime-neutral read-only job composition shared by native and offline hosts.

use crate::{
    errors,
    parser::JobReadCommand,
    session::{ReadSession, snapshot::Snapshot},
};
use fabric_cli_jobs::{
    Confirmation, JobCommandError, JobConfig, JobEvent, JobHost, JobUi, NoFiles,
};
use fabric_cli_output::{
    CommandError, OutputEnvelope,
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
        let format = OutputFormat::resolve(command.output_format.as_deref(), Some("json"))?;
        let family = command.family_command();
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
            let sdk = JobClient::new(
                Arc::clone(reads.sdk.credential()),
                reads.sdk.selection().clone(),
                Arc::clone(&jobs),
                JobLimits::default(),
            );
            let operations = SharedJobOperations::from_job_executor(
                Arc::clone(reads.sdk.credential()),
                reads.sdk.selection().clone(),
                jobs,
                fabric_sdk_operations::OperationLimits::default(),
            );
            JobHost {
                sdk: &sdk,
                operations: Some(&operations),
                lookup: &snapshot,
                current,
                home: reads.home.clone(),
                files: &NoFiles,
                ui: &ReadOnlyUi,
                config: JobConfig::default(),
                timestamp: reads.timestamp.clone(),
            }
            .execute(&family, cancelled.as_mut())
            .await
        }
        .await;
        let (envelope, headers, key_value) = match result {
            Ok(Some(output)) => {
                if !output.after_output.is_empty() {
                    return Err(text::TextError::UnsupportedFormat(
                        "Deferred events are not supported for job reads".into(),
                    ));
                }
                (
                    OutputEnvelope::new(self.reads.timestamp.clone(), output.input),
                    output.show_headers,
                    output.show_key_value_list,
                )
            }
            Ok(None) => {
                return Ok(TextPlan {
                    records: Vec::new(),
                    exit_code: 0,
                });
            }
            Err(error) => (
                error.envelope(self.reads.timestamp.clone(), Some(family.command_path())),
                false,
                false,
            ),
        };
        text::render(
            &envelope,
            format,
            &TextOptions {
                show_headers: headers,
                key_value,
                ..TextOptions::default()
            },
            &text::ascii_width,
            Presentation::default(),
        )
    }
}

struct ReadOnlyUi;

struct CheckedJobExecutor(Arc<dyn JobExecutor>);

impl JobExecutor for CheckedJobExecutor {
    fn supports_request_headers(&self, headers: &JobRequestHeaders) -> bool {
        self.0.supports_request_headers(headers)
    }

    fn execute<'a>(
        &'a self,
        request: JobRequest<'a>,
    ) -> JobFuture<'a, Result<JobResponse, JobExecutionError>> {
        Box::pin(async move {
            let response = self.0.execute(request).await?;
            if response.status == 200 {
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
