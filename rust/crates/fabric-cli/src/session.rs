// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Runtime-neutral read invocation, including async metadata discovery.

mod snapshot;

use crate::{PythonQuery, ReadConfig, ReadHost, errors, parser::ReadCommand};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_output::{
    CommandError, OutputEnvelope, Timestamp,
    text::{self, OutputFormat, Presentation, TextOptions, TextPlan},
};
use fabric_sdk::{
    Client, capacities::CapacityExecutor, folders::FolderExecutor, items::ItemExecutor,
    workspaces::WorkspaceExecutor,
};
use fabric_sdk_operations::OperationExecutor;
use std::{future::Future, pin::pin, sync::Arc};

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
        self.base
            .dispatch_with_source_operations(command, Some(self.operations), cancelled)
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
    /// renderer with no colors and ASCII terminal-width properties. Non-ASCII
    /// text requiring unavailable Python properties fails explicitly; JSON is
    /// unrestricted by the terminal-width provider.
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
        let format = OutputFormat::resolve(command.output_format.as_deref(), Some("json"))?;
        let mut cancelled = pin!(cancelled);
        let result = self.execute(command, operations, cancelled.as_mut()).await;
        let envelope = match result {
            Ok(Some(input)) => OutputEnvelope::new(self.timestamp.clone(), input),
            Ok(None) => {
                return Ok(TextPlan {
                    records: Vec::new(),
                    exit_code: 0,
                });
            }
            Err(error) => error.envelope(self.timestamp.clone(), Some(command.name.clone())),
        };
        text::render(
            &envelope,
            format,
            &TextOptions {
                subcommand: Some(&command.name),
                key_value: command.name == "get",
                ..TextOptions::default()
            },
            &text::ascii_width,
            Presentation::default(),
        )
    }

    async fn execute(
        &self,
        command: &ReadCommand,
        operations: Option<&dyn OperationExecutor>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Option<fabric_cli_output::OutputInput>, CommandError> {
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
        if command.output.is_some() || command.force || command.verbose {
            return Err(errors::unsupported(
                "GET export, verbose and definition/confirmation options are not ported",
            ));
        }
        let mut cancelled = pin!(cancelled);
        let mut snapshot = snapshot::Snapshot::default();
        let current = snapshot
            .resolve(self, &self.tenant, self.current_path, cancelled.as_mut())
            .await?;
        // Keep the parser's exact token joining and quote trimming contract.
        let path = command.path.join(" ");
        let path = path.trim_matches(['\'', '"']);
        snapshot
            .resolve(self, &current, path, cancelled.as_mut())
            .await?;
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
        host.with_folders(self.executor)
            .with_scalar_query()
            .execute_with_source_operations(&normalized, operations, cancelled.as_mut())
            .await
    }
}
