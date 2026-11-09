// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Runtime-neutral read invocation, including async metadata discovery.

pub(crate) mod snapshot;

#[path = "get_options.rs"]
pub mod get_options;

#[path = "get_host.rs"]
pub mod get_host;

use crate::{
    PythonQuery, ReadConfig, ReadHost, errors,
    parser::{ReadCommand, ReadKind},
};
use fabric_cli_context::{Element, HomeContext, NodeKind};
use fabric_cli_output::{
    CommandError, OutputEnvelope, OutputInput, Timestamp,
    text::{self, OutputFormat, Presentation, TextOptions, TextPlan},
};
use fabric_sdk::{
    Client, capacities::CapacityExecutor, folders::FolderExecutor, items::ItemExecutor,
    workspaces::WorkspaceExecutor,
};
use fabric_sdk_operations::{OperationError, OperationExecutor};
use std::{
    future::{Future, poll_fn},
    pin::pin,
    sync::Arc,
    task::Poll,
};

/// Run a parsed root auth command through the same engine as the native adapter.
///
/// The caller binds the broker to its validated explicit account/tenant/client
/// selection and supplies cancellation/time. This creates no runtime, performs
/// no provider discovery and does not implement a replacement identity backend.
#[cfg(any(feature = "windows-broker", feature = "interactive"))]
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

/// Rendered read output retaining its original command failure for the caller.
///
/// The plan's exit status and serialized output are not an error classifier.
/// Callers can inspect the original typed command error without parsing either.
#[derive(Debug)]
pub struct ReadDispatch {
    /// Existing presentation plan, including quiet success and failure output.
    pub plan: TextPlan,
    /// Original command failure, or a workspace operation classification.
    ///
    /// Absent for successful or quiet no-op reads. The original operation is
    /// additionally retained in [`Self::operation_error`], without projection.
    /// Operation rendering uses its separate original output, not this field.
    pub error: Option<CommandError>,
    /// Original workspace operation failure before its command-error projection.
    ///
    /// Retains typed cancellation, accepted IDs, raw terminal error data and
    /// uncertain remote effects. Rendering never consumes or reconstructs it.
    pub operation_error: Option<Box<OperationError>>,
}

pub(crate) struct ReadFailure {
    pub(crate) command: CommandError,
    pub(crate) operation: Option<Box<OperationError>>,
    pub(crate) operation_output: Option<Box<OutputInput>>,
}

impl From<CommandError> for ReadFailure {
    fn from(command: CommandError) -> Self {
        Self {
            command,
            operation: None,
            operation_output: None,
        }
    }
}

impl From<OperationError> for ReadFailure {
    fn from(operation: OperationError) -> Self {
        let projected = fabric_cli_mutations::MutationError::from(operation.resource_error());
        // Preserve source output independently of command-error falsey defaults.
        let operation_output = Box::new(projected.output());
        Self {
            command: CommandError::new(Some(projected.message), Some(projected.code)),
            operation: Some(Box::new(operation)),
            operation_output: Some(operation_output),
        }
    }
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
    /// Dispatch using source GET operation reconciliation before enrichment.
    ///
    /// Other read paths, discovery, projection and stream rendering are unchanged.
    pub async fn dispatch(
        &self,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TextPlan, text::TextError> {
        self.dispatch_with_evidence(command, cancelled)
            .await
            .map(|dispatch| dispatch.plan)
    }

    /// Dispatch with source operation reconciliation and the original command error.
    ///
    /// # Errors
    /// Returns rendering failures unchanged, including any partial presentation.
    /// Renderable command failures remain in [`ReadDispatch::error`].
    pub async fn dispatch_with_evidence(
        &self,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ReadDispatch, text::TextError> {
        self.base
            .dispatch_with_source_operations(command, Some(self.operations), cancelled, None)
            .await
    }

    /// Execute source operations with caller-selected text character properties.
    ///
    /// The provider affects only presentation, not discovery, credentials,
    /// operation reconciliation, projection, or cancellation.
    pub async fn dispatch_with_evidence_and_width(
        &self,
        command: &ReadCommand,
        widths: &(impl text::CharacterWidth + Sync),
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ReadDispatch, text::TextError> {
        self.base
            .dispatch_with_source_operations(
                command,
                Some(self.operations),
                cancelled,
                Some(widths),
            )
            .await
    }
}

impl<'session> ReadSession<'session> {
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
    /// renderer with no colors and the pinned production Unicode provider.
    /// GET uses source raw-data presentation, not key/value rows. Callers can
    /// explicitly select character properties with
    /// [`Self::dispatch_with_evidence_and_width`]; unsupported text remains an
    /// error, while JSON does not require character widths.
    ///
    /// Ordinary service/path/cancellation failures retain product envelopes and
    /// exit codes. Rendering errors, including partial text, remain errors.
    pub async fn dispatch(
        &self,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TextPlan, text::TextError> {
        self.dispatch_with_evidence(command, cancelled)
            .await
            .map(|dispatch| dispatch.plan)
    }

    /// Execute and render while retaining the original typed command failure.
    ///
    /// Successful no-op reads return an empty plan and no error. Command failures
    /// retain the same envelope, streams and exit status as [`Self::dispatch`].
    ///
    /// # Errors
    /// Returns rendering failures unchanged, including any partial presentation.
    /// Renderable command failures remain in [`ReadDispatch::error`].
    pub async fn dispatch_with_evidence(
        &self,
        command: &ReadCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ReadDispatch, text::TextError> {
        self.dispatch_with_source_operations(command, None, cancelled, None)
            .await
    }

    /// Execute with a caller-owned text-width capability and retain failure evidence.
    ///
    /// This changes only rendering. Missing character properties fail explicitly
    /// without a fallback provider or replay; JSON preserves its existing shape.
    pub async fn dispatch_with_evidence_and_width(
        &self,
        command: &ReadCommand,
        widths: &(impl text::CharacterWidth + Sync),
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ReadDispatch, text::TextError> {
        self.dispatch_with_source_operations(command, None, cancelled, Some(widths))
            .await
    }

    async fn dispatch_with_source_operations(
        &self,
        command: &ReadCommand,
        operations: Option<&dyn OperationExecutor>,
        cancelled: impl Future<Output = ()> + Send,
        widths: Option<&(dyn text::CharacterWidth + Sync)>,
    ) -> Result<ReadDispatch, text::TextError> {
        let format = OutputFormat::resolve(command.output_format.as_deref(), Some("json"))?;
        let mut cancelled = pin!(cancelled);
        let result = self.execute(command, operations, cancelled.as_mut()).await;
        let (envelope, error, operation_error) = match result {
            Ok(Some(input)) => (
                OutputEnvelope::new(self.timestamp.clone(), input),
                None,
                None,
            ),
            Ok(None) => {
                return Ok(ReadDispatch {
                    plan: TextPlan {
                        records: Vec::new(),
                        exit_code: 0,
                    },
                    error: None,
                    operation_error: None,
                });
            }
            Err(failure) => {
                let envelope = if let Some(mut input) = failure.operation_output {
                    input.command = Some(command.name.clone());
                    OutputEnvelope::new(self.timestamp.clone(), *input)
                } else {
                    failure
                        .command
                        .envelope(self.timestamp.clone(), Some(command.name.clone()))
                };
                (envelope, Some(failure.command), failure.operation)
            }
        };
        let widths = match widths {
            Some(widths) => widths,
            None => fabric_cli_output::production_unicode::width_provider()?,
        };
        let character_width = |character| widths.width(character);
        let plan = text::render(
            &envelope,
            format,
            &TextOptions {
                subcommand: Some(&command.name),
                ..TextOptions::default()
            },
            &character_width,
            Presentation::default(),
        )?;
        Ok(ReadDispatch {
            plan,
            error,
            operation_error,
        })
    }

    async fn execute(
        &self,
        command: &ReadCommand,
        operations: Option<&dyn OperationExecutor>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Option<fabric_cli_output::OutputInput>, ReadFailure> {
        if self.tenant.id() != Some(self.sdk.selection().tenant())
            || self.tenant.kind() != fabric_cli_context::NodeKind::Tenant
            || self.sdk.selection().audience() != fabric_sdk::Audience::Fabric
            || !self.current_path.starts_with('/')
        {
            return Err(errors::error("InvalidInput", "Invalid selected read context").into());
        }
        get_options::preflight(command)?;
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
        if operations.is_some()
            && normalized.kind == ReadKind::Get
            && matches!(target.kind(), NodeKind::Workspace(_))
        {
            if poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await {
                return Err(
                    errors::error("LongRunningOperationCancelled", "Operation cancelled").into(),
                );
            }
            return host
                .workspace_get_with_evidence(&target, &normalized, operations, cancelled.as_mut())
                .await;
        }
        host.with_folders(self.executor)
            .with_scalar_query()
            .execute_with_source_operations(&normalized, operations, cancelled.as_mut())
            .await
            .map_err(Into::into)
    }
}
