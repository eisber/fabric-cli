// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Runtime-neutral read invocation, including async metadata discovery.

pub(crate) mod snapshot;

#[path = "get_options.rs"]
pub mod get_options;

use crate::{PythonQuery, ReadConfig, ReadHost, errors, parser::ReadCommand};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_output::{
    CommandError, OutputEnvelope, Timestamp, production_unicode,
    text::{self, OutputFormat, Presentation, TextOptions, TextPlan},
};
use fabric_sdk::{
    Client, capacities::CapacityExecutor, folders::FolderExecutor, items::ItemExecutor,
    workspaces::WorkspaceExecutor,
};
use fabric_sdk_operations::OperationExecutor;
use std::{future::Future, pin::pin, sync::Arc};

/// Run a parsed root auth command through the same engine as the native adapter.
///
/// The caller binds the broker to its validated explicit account/tenant/client
/// selection and supplies cancellation/time. This creates no runtime, performs
/// no provider discovery and does not implement a replacement identity backend.
#[cfg(feature = "windows-broker")]
pub async fn run_auth(
    command: &crate::parser::AuthCommand,
    broker: &impl crate::auth_commands::Broker,
    cancelled: impl Future<Output = std::io::Result<()>>,
    deadline: impl Future<Output = ()>,
) -> Result<serde_json::Value, crate::auth_commands::Failure> {
    crate::auth_commands::run(
        command.action,
        command.allow_interaction,
        broker,
        cancelled,
        deadline,
    )
    .await
}

/// The four existing read interfaces; no extra transport or execution policy.
pub trait ReadExecutor:
    WorkspaceExecutor + ItemExecutor + FolderExecutor + CapacityExecutor
{
}

impl<T: WorkspaceExecutor + ItemExecutor + FolderExecutor + CapacityExecutor> ReadExecutor for T {}

/// One explicitly selected command session, reusable by native and offline hosts.
///
/// Resolves metadata asynchronously before invoking the unchanged pure resolver
/// and read handlers. No runtime, environment, clock, account discovery or
/// persistence is installed. Metadata is scoped to this command, never cached
/// across accounts/tenants or emitted as a partial successful listing.
pub struct ReadSession<'a> {
    /// Credential-injected SDK with explicit account, tenant and Fabric audience.
    pub sdk: &'a Client,
    /// Shared executor, ordinarily the caller's `fabric_sdk_http::HttpExecutor`.
    pub executor: &'a dyn ReadExecutor,
    /// Selected tenant root, matching the SDK's credential selection.
    pub tenant: Arc<Element>,
    /// Explicit absolute Fabric working path; `/` selects the tenant root.
    pub current_path: &'a str,
    /// Explicit home selection, including deliberately unavailable/unspecified.
    pub home: HomeContext,
    /// Caller-supplied timestamp; never read implicitly by the library.
    pub timestamp: Timestamp,
    /// Immutable read settings, not ambient Python/Rust configuration.
    pub config: ReadConfig,
}

/// Rendered read output retaining its typed command failure for caller control flow.
///
/// The existing plan-only methods remain unchanged. Hosts must not infer
/// cancellation by inspecting rendered records, messages or numeric exit codes.
pub struct ReadDispatch {
    /// The unchanged command presentation, streams and exit code.
    pub plan: TextPlan,
    /// Original command failure, retained before rendering.
    pub error: Option<CommandError>,
    /// Structured source-operation failure code, retained before presentation.
    ///
    /// Source GET can return an `OutputInput` failure rather than `Err`.
    /// This is that typed field, not a parsed rendered diagnostic.
    pub output_error_code: Option<String>,
}

/// Explicit source-synchronous workspace GET policy over the unchanged read session.
///
/// The borrowed operation executor supplies the same caller-owned HTTP/time
/// policy; default sessions and custom four-trait read executors remain valid.
/// Like the original read host, dispatch runs on the caller's thread; its
/// synchronous lookup interface does not require `Sync` or promise a Send future.
pub struct SourceOperationReadSession<'a, 'session> {
    base: &'a ReadSession<'session>,
    operations: &'a dyn OperationExecutor,
}

impl SourceOperationReadSession<'_, '_> {
    /// Dispatch with result-file support while retaining the typed failure.
    pub async fn dispatch_result_with_files(
        &self,
        command: &ReadCommand,
        files: &mut crate::get_output::ResultFileHost<'_>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ReadDispatch, text::TextError> {
        self.base
            .dispatch_with_files_result(command, Some(self.operations), Some(files), cancelled)
            .await
    }

    /// Dispatch source operations while retaining the typed failure before rendering.
    pub async fn dispatch_result(
        &self,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ReadDispatch, text::TextError> {
        self.base
            .dispatch_with_files_result(command, Some(self.operations), None, cancelled)
            .await
    }

    /// Dispatch GET with an explicit result-file capability and filename clock.
    ///
    /// Querying completes once before destination resolution or any write.
    /// Dropped/failed writes are never retried and are not rolled back.
    pub async fn dispatch_with_result_files(
        &self,
        command: &ReadCommand,
        files: &mut crate::get_output::ResultFileHost<'_>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TextPlan, text::TextError> {
        self.base
            .dispatch_with_files(command, Some(self.operations), Some(files), cancelled)
            .await
    }

    /// Dispatch using source GET operation reconciliation before enrichment.
    ///
    /// Other read paths, discovery, projection and stream rendering are unchanged.
    pub async fn dispatch(
        &self,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TextPlan, text::TextError> {
        self.base
            .dispatch_with_source_operations(command, Some(self.operations), cancelled)
            .await
    }
}

impl<'session> ReadSession<'session> {
    /// Dispatch GET with caller-owned result files, without installing a runtime.
    pub async fn dispatch_with_result_files(
        &self,
        command: &ReadCommand,
        files: &mut crate::get_output::ResultFileHost<'_>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TextPlan, text::TextError> {
        self.dispatch_with_files(command, None, Some(files), cancelled)
            .await
    }

    /// Select source-synchronous workspace GET without changing default contracts.
    ///
    /// Supply the same shared executor used for reads, with its existing timer.
    /// This does not create a runtime, transport, credential provider or poller.
    pub fn with_source_operations<'a>(
        &'a self,
        operations: &'a dyn OperationExecutor,
    ) -> SourceOperationReadSession<'a, 'session> {
        SourceOperationReadSession {
            base: self,
            operations,
        }
    }

    /// Execute a parsed read through real discovery, SDK, query and rendering.
    ///
    /// JSON remains the preview default. Explicit text uses the shared pure
    /// renderer with no colors and pinned Python scalar-width properties. GET
    /// uses source raw-data presentation, not key/value rows. Unicode provider
    /// initialization and formatting failures propagate without ASCII fallback;
    /// JSON does not initialize or depend on the terminal-width provider.
    ///
    /// Ordinary service/path/cancellation failures retain product envelopes and
    /// exit codes. Rendering errors, including partial text, remain errors.
    pub async fn dispatch(
        &self,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TextPlan, text::TextError> {
        self.dispatch_with_source_operations(command, None, cancelled)
            .await
    }

    async fn dispatch_with_source_operations(
        &self,
        command: &ReadCommand,
        operations: Option<&dyn OperationExecutor>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TextPlan, text::TextError> {
        self.dispatch_with_files(command, operations, None, cancelled)
            .await
    }

    async fn dispatch_with_files(
        &self,
        command: &ReadCommand,
        operations: Option<&dyn OperationExecutor>,
        files: Option<&mut crate::get_output::ResultFileHost<'_>>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TextPlan, text::TextError> {
        self.dispatch_with_files_result(command, operations, files, cancelled)
            .await
            .map(|output| output.plan)
    }

    async fn dispatch_with_files_result(
        &self,
        command: &ReadCommand,
        operations: Option<&dyn OperationExecutor>,
        mut files: Option<&mut crate::get_output::ResultFileHost<'_>>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ReadDispatch, text::TextError> {
        let format = OutputFormat::resolve(command.output_format.as_deref(), Some("json"))?;
        let file_default_format = files
            .as_ref()
            .map(|files| OutputFormat::resolve(files.default_output_format, Some("json")))
            .transpose()?;
        let mut cancelled = pin!(cancelled);
        let result = self
            .execute(command, operations, files.is_some(), cancelled.as_mut())
            .await;
        let error = result.as_ref().err().cloned();
        let output_error_code = result
            .as_ref()
            .ok()
            .and_then(|(input, _)| input.as_ref())
            .and_then(|input| input.error_code.clone());
        let envelope = match result {
            Ok((Some(input), name)) => {
                if let (Some(output), Some(files), Some(data)) = (
                    command.output.as_ref().filter(|output| !output.is_empty()),
                    files.as_mut(),
                    &input.data,
                ) && input.status == fabric_cli_output::OutputStatus::Success
                {
                    let (notice, result, storage) =
                        files.write(output, &name, data, cancelled.as_mut()).await;
                    let lakehouse_success = result.is_ok()
                        && storage == Some(fabric_deploy::definitions::ExportStorage::Lakehouse);
                    let format = if lakehouse_success {
                        file_default_format.unwrap_or(format)
                    } else {
                        format
                    };
                    let error = result.as_ref().err().cloned();
                    let plan = crate::get_output::render_completion(
                        result,
                        notice,
                        self.timestamp.clone(),
                        (!lakehouse_success).then_some(command.name.as_str()),
                        format,
                    )?;
                    return Ok(ReadDispatch {
                        plan,
                        error,
                        output_error_code: None,
                    });
                }
                OutputEnvelope::new(self.timestamp.clone(), input)
            }
            Ok((None, _)) => {
                return Ok(ReadDispatch {
                    plan: TextPlan {
                        records: Vec::new(),
                        exit_code: 0,
                    },
                    error: None,
                    output_error_code: None,
                });
            }
            Err(error) => error.envelope(self.timestamp.clone(), Some(command.name.clone())),
        };
        let options = TextOptions {
            subcommand: Some(&command.name),
            ..TextOptions::default()
        };
        let plan = match format {
            OutputFormat::Json => text::render(
                &envelope,
                format,
                &options,
                &text::ascii_width,
                Presentation::default(),
            ),
            OutputFormat::Text => text::render(
                &envelope,
                format,
                &options,
                production_unicode::width_provider()?,
                Presentation::default(),
            ),
        }?;
        Ok(ReadDispatch {
            plan,
            error,
            output_error_code,
        })
    }

    async fn execute(
        &self,
        command: &ReadCommand,
        operations: Option<&dyn OperationExecutor>,
        result_files: bool,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<(Option<fabric_cli_output::OutputInput>, String), CommandError> {
        if self.tenant.id() != Some(self.sdk.selection().tenant())
            || self.tenant.kind() != fabric_cli_context::NodeKind::Tenant
            || self.sdk.selection().audience() != fabric_sdk::Audience::Fabric
            || !self.current_path.starts_with('/')
        {
            return Err(errors::error(
                "InvalidInput",
                "Invalid selected read context",
            ));
        }
        if result_files {
            get_options::preflight_with_files(command)?;
        } else {
            get_options::preflight(command)?;
        }
        let mut cancelled = pin!(cancelled);
        let mut snapshot = snapshot::Snapshot::default();
        // Keep the parser's exact token joining and quote trimming contract.
        let path = command.path.join(" ");
        let path = path.trim_matches(['\'', '"']);
        let current = if path.starts_with('/') || path == "~" || path.starts_with("~/") {
            Arc::clone(&self.tenant)
        } else {
            snapshot
                .resolve(self, &self.tenant, self.current_path, cancelled.as_mut())
                .await?
        };
        let target = snapshot
            .resolve(self, &current, path, cancelled.as_mut())
            .await?;
        get_options::validate_target(command, &target)?;
        let host = ReadHost {
            sdk: self.sdk,
            workspaces: self.executor,
            items: self.executor,
            capacities: self.executor,
            lookup: &snapshot,
            current,
            home: self.home.clone(),
            timestamp: self.timestamp.clone(),
            config: self.config,
            projection: &PythonQuery,
        };
        let mut normalized = command.clone();
        normalized.output_format = Some("json".into());
        if result_files && normalized.output.as_deref() == Some("") {
            normalized.output = None;
        }
        let result = if result_files && command.kind == crate::parser::ReadKind::Get {
            host.execute_read(&normalized, operations, true, cancelled.as_mut())
                .await
        } else {
            host.with_folders(self.executor)
                .with_scalar_query()
                .execute_with_source_operations(&normalized, operations, cancelled.as_mut())
                .await
        }?;
        Ok((result, target.name().to_owned()))
    }
}
