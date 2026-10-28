// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Thin transfer commands over the released definition library and shared policy.
//!
//! The caller owns discovery, credentials, runtime, local roots, UI and cache.
//! No HTTP implementation, deployment algorithm or second front door lives here.

mod files;
mod local;
mod parser;

pub use local::LocalTransferFiles;
pub use parser::{
    TransferCommand, TransferKind, command_from_matches, process_tokens, transfer_subcommands,
};

use fabric_cli_context::{Element, NodeKind};
use fabric_cli_output::{
    CommandError, OutputEnvelope, OutputInput, OutputStatus, Timestamp,
    text::{self, CharacterWidth, OutputFormat, Presentation, TextError, TextOptions, TextPlan},
};
use fabric_deploy::definitions::{
    self, BulkExportSnapshot, BulkItem, BulkScope, CommandSupport, DefinitionError,
    DefinitionErrorKind, DefinitionFile, DefinitionFileSystem, DefinitionImportFileSystem,
    DefinitionLimits, DefinitionTarget, ImportTarget, PreparedImport,
};
use fabric_deploy::{
    notebook::DefinitionLimits as NotebookDefinitionLimits,
    repository::{NotebookRepository, RepositoryError},
};
use fabric_sdk::Client;
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse, OperationSession,
};
use parser::{command_error, invalid};
use serde_json::{Map, Value, json};
use std::{
    fmt,
    future::{Future, poll_fn},
    io::{self, Write},
    path::{Path, PathBuf},
    pin::{Pin, pin},
    sync::Arc,
    task::Poll,
    time::Duration,
};

/// Caller-runtime asynchronous capability.
pub type TransferFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Source presentation, kept separate from result envelopes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransferEvent {
    /// Source grey stderr diagnostic/progress.
    Grey(String),
    /// Source warning, routed by the caller's existing warning/logger policy.
    Warning(String),
}

/// Caller-owned UI; no terminal is opened by this library.
pub trait TransferUi: Send + Sync {
    /// Present immediately, propagating stream failures.
    fn emit(&self, event: TransferEvent) -> Result<(), CommandError>;
    /// Ask the exact source confirmation.
    fn confirm<'a>(&'a self, message: &'a str) -> TransferFuture<'a, Result<bool, CommandError>>;
    /// Select source names in configuration-sorted display order.
    fn select<'a>(
        &'a self,
        names: &'a [String],
    ) -> TransferFuture<'a, Result<Vec<String>, CommandError>>;
}

/// One complete cached item and its source support-check outcome.
#[derive(Clone, Debug)]
pub struct TransferItem {
    /// Existing immutable context metadata.
    pub element: Arc<Element>,
    /// Source true/false/Fabric-exception distinction for the selected command.
    pub support: CommandSupport,
}

/// Complete caller-owned command metadata, not a second cache/discovery engine.
#[derive(Clone, Debug)]
pub struct TransferContext {
    /// Resolved source target, including an unresolved item for import creation.
    pub target: Arc<Element>,
    /// Complete workspace or recursively collected folder items in source order.
    pub items: Vec<TransferItem>,
    /// Workspace export's configuration-sorted names, before UI selection.
    pub selection_order: Vec<String>,
}

/// Existing context owner resolves paths asynchronously and checks command support.
pub trait TransferMetadata: Send + Sync {
    /// Resolve only the source target before warnings and export preconditions.
    ///
    /// Existing immutable injected snapshots remain compatible through `resolve`.
    fn resolve_target<'a>(
        &'a self,
        command: TransferKind,
        path: &'a [String],
    ) -> TransferFuture<'a, Result<TransferContext, CommandError>> {
        self.resolve(command, path)
    }

    /// Complete workspace/folder discovery at its source orchestration boundary.
    ///
    /// Export warns before workspace collection. Bulk confirms its local output
    /// preconditions before collection. Existing complete injected snapshots pass
    /// through unchanged; no second discovery engine is installed.
    fn complete_snapshot<'a>(
        &'a self,
        _command: TransferKind,
        context: TransferContext,
    ) -> TransferFuture<'a, Result<TransferContext, CommandError>> {
        Box::pin(async move { Ok(context) })
    }

    /// Reuse existing discovery/cache policy; never hide network I/O in a sync lookup.
    ///
    /// Import resolution allows a missing destination item but requires its real
    /// parent/workspace. Other targets require IDs. Workspace/folder collections
    /// must be complete, not a partial page. Support failures retain source codes.
    fn resolve<'a>(
        &'a self,
        command: TransferKind,
        path: &'a [String],
    ) -> TransferFuture<'a, Result<TransferContext, CommandError>>;
}

/// Definition file policy plus caller-owned path selection and emptiness checks.
pub trait TransferFiles: DefinitionFileSystem + DefinitionImportFileSystem + Send {
    /// Resolve export output; import continues using its independent input policy.
    fn resolve_export_directory_async<'a>(
        &'a mut self,
        input: &'a str,
    ) -> TransferFuture<'a, io::Result<PathBuf>> {
        self.resolve_directory_async(input)
    }

    /// Select source storage bytes for an already resolved output root.
    fn export_storage(&self, _root: &Path) -> definitions::ExportStorage {
        definitions::ExportStorage::Local
    }

    /// Source presentation path, separate from the confined physical write root.
    fn export_display_path(&self, root: &Path, input: &str) -> String {
        if input.starts_with('~')
            || self.export_storage(root) == definitions::ExportStorage::Lakehouse
        {
            root.display().to_string()
        } else {
            input.to_owned()
        }
    }

    /// Resolve a source local directory inside the caller's explicitly owned roots.
    ///
    /// Remote Fabric paths must fail explicitly, never become local directories.
    fn resolve_directory(&mut self, input: &str) -> io::Result<PathBuf>;
    /// Check the selected existing directory, without unbounded enumeration.
    fn is_empty(&mut self, root: &Path) -> io::Result<bool>;

    /// Resolve a directory on the caller's filesystem worker.
    ///
    /// The default preserves synchronous injected adapters. OS-backed async hosts
    /// must override these methods to keep filesystem work off runtime threads.
    fn resolve_directory_async<'a>(
        &'a mut self,
        input: &'a str,
    ) -> TransferFuture<'a, io::Result<PathBuf>> {
        Box::pin(async move { self.resolve_directory(input) })
    }

    /// Check emptiness on the caller's worker, preserving bounded entry policy.
    fn is_empty_async<'a>(&'a mut self, root: &'a Path) -> TransferFuture<'a, io::Result<bool>> {
        Box::pin(async move { self.is_empty(root) })
    }

    /// Read exact bounded bytes using the same confined path policy as import.
    ///
    /// Deployment configuration and parameter files share this capability.
    /// OS-backed hosts must offload it just like definition file reads.
    fn read_async<'a>(
        &'a mut self,
        root: &'a Path,
        relative: &'a str,
        max_bytes: usize,
    ) -> TransferFuture<'a, io::Result<Vec<u8>>> {
        Box::pin(async move { self.read(root, relative, max_bytes) })
    }

    /// Prepare a complete owned deployment repository before remote work.
    ///
    /// Reuse the released discovery, file order, aggregate bounds and Notebook
    /// validation. No borrowed filesystem capability or worker guard may escape.
    /// The default retains synchronous injected-adapter compatibility; OS-backed
    /// async hosts must run the whole bounded discovery on their file worker.
    fn prepare_repository_async<'a>(
        &'a mut self,
        root: &'a Path,
        limits: NotebookDefinitionLimits,
        max_entries: usize,
    ) -> TransferFuture<'a, Result<NotebookRepository, RepositoryError>> {
        Box::pin(async move {
            NotebookRepository::discover(&mut files::ImportFiles(self), root, limits, max_entries)
        })
    }

    /// Preflight all destinations before any write, returning the unchanged plan.
    ///
    /// Owned bounded bytes let a worker take the plan without an extra copy.
    fn preflight_async<'a>(
        &'a mut self,
        root: &'a Path,
        files: Vec<DefinitionFile>,
    ) -> TransferFuture<'a, io::Result<Vec<DefinitionFile>>> {
        Box::pin(async move {
            self.preflight(root, &files)?;
            Ok(files)
        })
    }

    /// Create an item directory, including an empty definition.
    fn create_directory_async<'a>(
        &'a mut self,
        root: &'a Path,
        name: &'a str,
    ) -> TransferFuture<'a, io::Result<()>> {
        Box::pin(async move { self.create_directory(root, name) })
    }

    /// Write one exact bounded file; cancellation/drop is not rollback.
    fn write_async<'a>(
        &'a mut self,
        root: &'a Path,
        file: DefinitionFile,
    ) -> TransferFuture<'a, io::Result<()>> {
        Box::pin(async move { self.write(root, &file) })
    }

    /// Prepare a complete import before mutation using the released file loader.
    ///
    /// Worker implementations must retain traversal, exclusion and raw-byte
    /// semantics, finite bounds, and the original typed preparation errors.
    fn prepare_import_async<'a>(
        &'a mut self,
        root: &'a Path,
        target: ImportTarget<'a>,
        selector: Option<&'a str>,
        limits: DefinitionLimits,
    ) -> TransferFuture<'a, Result<PreparedImport, DefinitionError>> {
        Box::pin(async move {
            definitions::prepare_import(
                &mut files::ImportFiles(self),
                root,
                target,
                selector,
                true,
                limits,
            )
        })
    }
}

/// Source cache upsert after import, including declined overwrite of an existing item.
#[derive(Clone, Debug)]
pub struct ImportCacheEffect {
    /// Original immutable item context.
    pub target: Arc<Element>,
    /// Actual completed create ID, or the pre-existing item ID.
    pub item_id: String,
}

/// Handler output and actual file/cache effects for the single central host.
#[derive(Clone, Debug, Default)]
pub struct TransferResult {
    /// Ordered source result calls; empty on source-silent branches.
    pub outputs: Vec<OutputInput>,
    /// Bulk summary requests source key/value text rendering.
    pub key_value: bool,
    /// Fully written relative paths, retaining earlier workspace-item progress.
    pub completed_paths: Vec<String>,
    /// Completed export IDs in workspace execution or bulk response-index order.
    pub exported_ids: Vec<String>,
    /// Known bulk IDs skipped by source support/index policy, in source order.
    pub skipped_ids: Vec<String>,
    /// Source cache changes, applied after successful result delivery.
    ///
    /// Source-silent overwrite declines still upsert. Output failure must not
    /// silently apply deferred cache changes or trigger a mutation replay.
    pub effects: Vec<ImportCacheEffect>,
    /// Source-excluded import cache paths retained by the released file loader.
    pub excluded_paths: Vec<String>,
    /// A UI decline, not service cancellation or rollback.
    pub declined: bool,
}

/// Original failure, retained separately from its public output projection.
#[derive(Debug)]
pub enum TransferFailure {
    /// Context, UI, input or explicit source-gap failure.
    Command(CommandError),
    /// Released library classification and original operation/file evidence.
    Definition(Box<DefinitionError>),
}

/// Failure retaining earlier file and cache effects; no automatic replay is safe.
#[derive(Debug)]
pub struct TransferError {
    /// Original failure, including operation ID and dispatch uncertainty.
    pub failure: TransferFailure,
    /// Earlier completed files, plus current library progress when available.
    pub completed_paths: Vec<String>,
    /// Earlier workspace item exports that completed before this failure.
    pub exported_ids: Vec<String>,
    /// Earlier confirmed cache effects.
    pub effects: Vec<ImportCacheEffect>,
}

impl From<CommandError> for TransferError {
    fn from(error: CommandError) -> Self {
        Self {
            failure: TransferFailure::Command(error),
            completed_paths: Vec::new(),
            exported_ids: Vec::new(),
            effects: Vec::new(),
        }
    }
}

impl From<DefinitionError> for TransferError {
    fn from(error: DefinitionError) -> Self {
        Self {
            completed_paths: error.completed_paths.clone(),
            exported_ids: Vec::new(),
            failure: TransferFailure::Definition(Box::new(error)),
            effects: Vec::new(),
        }
    }
}

impl TransferError {
    /// Shared source envelope, without discarding reconciliation evidence.
    pub fn envelope(&self, timestamp: Timestamp, command: Option<String>) -> OutputEnvelope {
        let mut output = match &self.failure {
            TransferFailure::Command(error) => return error.envelope(timestamp, command),
            TransferFailure::Definition(error) => {
                if let DefinitionErrorKind::Operation(operation) = &error.kind {
                    fabric_cli_mutations::operation_error_output(operation)
                } else {
                    let (code, message) = match &error.kind {
                        DefinitionErrorKind::InvalidInput => {
                            ("InvalidInput", "Invalid transfer input".into())
                        }
                        DefinitionErrorKind::InvalidDefinition => (
                            "InvalidDefinitionPayload",
                            "Invalid item definition payload".into(),
                        ),
                        DefinitionErrorKind::Limit => {
                            ("Limit", "Definition transfer limit reached".into())
                        }
                        DefinitionErrorKind::UnsafePath | DefinitionErrorKind::Collision => (
                            "InvalidPath",
                            "Unsafe or conflicting definition path".into(),
                        ),
                        DefinitionErrorKind::ConfirmationRequired => {
                            ("ConfirmationRequired", "Confirmation is required".into())
                        }
                        DefinitionErrorKind::DefinitionAccessDenied(_) => {
                            ("Unauthorized", "Item definition access denied".into())
                        }
                        DefinitionErrorKind::Unsupported(branch) => {
                            ("NotSupported", format!("Unported source branch: {branch}"))
                        }
                        DefinitionErrorKind::FileSystem(error) => {
                            ("InvalidPath", error.to_string())
                        }
                        DefinitionErrorKind::Operation(_) => unreachable!(),
                    };
                    OutputInput {
                        status: OutputStatus::Failure,
                        error_code: Some(code.into()),
                        message: Some(message),
                        ..OutputInput::default()
                    }
                }
            }
        };
        output.command = command;
        OutputEnvelope::new(timestamp, output)
    }
}

impl fmt::Display for TransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Transfer command failed; inspect original operation and file evidence")
    }
}

impl std::error::Error for TransferError {}

struct BorrowedExecutor<'a, E: ?Sized>(&'a E);

impl<E: OperationExecutor + ?Sized> OperationExecutor for BorrowedExecutor<'_, E> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> TransferFuture<'a, Result<OperationResponse, OperationExecutionError>> {
        self.0.execute(request)
    }

    fn wait(&self, delay: Duration) -> TransferFuture<'_, Result<(), OperationExecutionError>> {
        self.0.wait(delay)
    }
}

type Cancellation<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

async fn phase<T, E: Into<TransferError>>(
    work: impl Future<Output = Result<T, E>>,
    cancelled: &mut Cancellation<'_>,
) -> Result<T, TransferError> {
    let mut work = pin!(work);
    poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(command_error(
                "LongRunningOperationCancelled",
                "Operation cancelled",
            )
            .into()));
        }
        work.as_mut()
            .poll(cx)
            .map(|result| result.map_err(Into::into))
    })
    .await
}

/// Runtime-neutral family host; every external capability is caller-injected.
pub struct TransferHost<'a, E: ?Sized> {
    /// Existing credential/selection facade; no identity discovery.
    pub client: &'a Client,
    /// Existing shared HTTP adapter and asynchronous time.
    pub executor: &'a E,
    /// Complete asynchronous metadata/context owner.
    pub metadata: &'a dyn TransferMetadata,
    /// Stable caller-owned file policy.
    pub files: &'a mut dyn TransferFiles,
    /// Source confirmation, selection, warning and progress policy.
    pub ui: &'a dyn TransferUi,
    /// Finite library file/path/byte bounds.
    pub definition_limits: DefinitionLimits,
    /// Existing bounded shared operation policy.
    pub operation_limits: OperationLimits,
    /// Source command-line mode rejects interactive workspace selection.
    pub command_line: bool,
}

impl<E: OperationExecutor + ?Sized> TransferHost<'_, E> {
    /// Execute a family leaf; returned effects are real, not planned CLI coverage.
    ///
    /// OS-backed hosts override the asynchronous file capabilities with a bounded
    /// caller-owned worker. Cancellation drops metadata/UI phases; SDK sessions
    /// retain original dispatch evidence. No request or write is retried or rolled
    /// back, and dropping a worker future cannot interrupt an OS syscall.
    pub async fn execute(
        &mut self,
        command: &TransferCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TransferResult, TransferError> {
        OutputFormat::resolve(command.output_format.as_deref(), Some("text"))
            .map_err(|error| error.command_error())?;
        if command.kind == TransferKind::Deploy {
            return Err(command_error("NotSupported",
                "Config deployment requires the native deployment host, not the definition transfer host").into());
        }
        if command.kind == TransferKind::BulkExport && !command.recursive {
            return Err(command_error(
                "InvalidOperation",
                "The --recursive flag is required when bulk-exporting a Workspace or Folder",
            )
            .into());
        }
        let mut cancelled: Cancellation<'_> = Box::pin(cancelled);
        phase(
            std::future::ready(Ok::<_, CommandError>(())),
            &mut cancelled,
        )
        .await?;
        let context = phase(
            self.metadata.resolve_target(command.kind, &command.path),
            &mut cancelled,
        )
        .await?;
        self.check_snapshot(&context)?;
        match command.kind {
            TransferKind::Export => self.export(command, context, &mut cancelled).await,
            TransferKind::BulkExport => self.bulk(command, context, &mut cancelled).await,
            TransferKind::Import => self.import(command, context, &mut cancelled).await,
            TransferKind::Deploy => unreachable!(),
        }
    }

    async fn export(
        &mut self,
        command: &TransferCommand,
        context: TransferContext,
        cancelled: &mut Cancellation<'_>,
    ) -> Result<TransferResult, TransferError> {
        let workspace = matches!(context.target.kind(), NodeKind::Workspace(_));
        if !workspace && !matches!(context.target.kind(), NodeKind::Item(_)) {
            return Err(
                command_error("NotSupported", "Export requires an item or workspace").into(),
            );
        }
        if workspace && self.command_line && !(command.force || command.all) {
            return Err(command_error("NotSupported", "Not supported in 'cli' mode").into());
        }
        if command.force {
            self.ui.emit(TransferEvent::Warning(
                "Item definition is exported without its sensitivity label and its data".into(),
            ))?;
        }
        let output = process_tokens(
            command
                .output
                .as_deref()
                .ok_or_else(|| invalid("Output path is required"))?,
        );
        let mut result = TransferResult::default();
        if !workspace {
            if command.force || phase(self.ui.confirm(SINGLE_CONFIRM), cancelled).await? {
                result.completed_paths = self
                    .export_one(&context.target, command, None, &output, cancelled)
                    .await?;
                result
                    .exported_ids
                    .push(require_id(&context.target)?.into());
            } else {
                result.declined = true;
            }
            // Python emits this message even when its single-item prompt is declined.
            result
                .outputs
                .push(message(format!("'{}' exported", context.target.name())));
            return Ok(result);
        }
        let context = phase(
            self.metadata.complete_snapshot(command.kind, context),
            cancelled,
        )
        .await?;
        self.check_snapshot(&context)?;
        if context.items.is_empty() {
            return Err(command_error("InvalidOperation", "Your workspace is empty").into());
        }
        let supported: Vec<_> = context
            .items
            .iter()
            .filter(|item| item.support == CommandSupport::Supported)
            .collect();
        if supported.is_empty() {
            return Err(command_error(
                "NotSupported",
                "Export not possible. Existing items cannot be imported/exported",
            )
            .into());
        }
        let root = phase(self.export_directory(&output), cancelled).await?;
        let names = &context.selection_order;
        let mut expected = supported
            .iter()
            .map(|item| item.element.name())
            .collect::<Vec<_>>();
        let mut actual = names.clone();
        expected.sort();
        actual.sort();
        if actual != expected {
            return Err(
                invalid("Complete configuration-sorted export selection is required").into(),
            );
        }
        let selected = if command.all {
            names.clone()
        } else {
            phase(self.ui.select(names), cancelled).await?
        };
        if selected.iter().any(|name| !names.contains(name)) {
            return Err(invalid("Selection contains an unknown item").into());
        }
        if selected.is_empty() {
            result.outputs.push(message("No items selected"));
            return Ok(result);
        }
        self.ui.emit(TransferEvent::Grey(selected.join("\n")))?;
        self.ui
            .emit(TransferEvent::Grey("------------------------------".into()))?;
        if !command.force && !phase(self.ui.confirm(SINGLE_CONFIRM), cancelled).await? {
            result.declined = true;
            return Ok(result);
        }
        let filtered_items = supported
            .into_iter()
            .filter(|item| selected.contains(&item.element.name()))
            .collect::<Vec<_>>();
        let count = filtered_items.len();
        for item in filtered_items {
            match self
                .export_one(&item.element, command, Some(&root), &output, cancelled)
                .await
            {
                Ok(paths) => {
                    result.completed_paths.extend(paths);
                    result.exported_ids.push(require_id(&item.element)?.into());
                }
                Err(mut error) => {
                    let mut paths = result.completed_paths;
                    paths.append(&mut error.completed_paths);
                    error.completed_paths = paths;
                    let mut exported_ids = result.exported_ids;
                    exported_ids.append(&mut error.exported_ids);
                    error.exported_ids = exported_ids;
                    return Err(error);
                }
            }
        }
        result.outputs.push(message(format!(
            "{count} {} exported successfully",
            if count > 1 { "items" } else { "item" }
        )));
        Ok(result)
    }

    async fn export_one(
        &mut self,
        item: &Arc<Element>,
        command: &TransferCommand,
        root: Option<&Path>,
        output: &str,
        cancelled: &mut Cancellation<'_>,
    ) -> Result<Vec<String>, TransferError> {
        let (workspace, item_type) = item_parts(item)?;
        let item_id = require_id(item)?;
        let mut session = OperationSession::with_context(
            self.client.context(),
            BorrowedExecutor(self.executor),
            self.operation_limits,
            cancelled.as_mut(),
        );
        let read = definitions::read_item_definition(
            &mut session,
            DefinitionTarget {
                workspace_id: require_id(&workspace)?,
                item_id,
                item_type,
            },
            command.format.as_deref(),
        )
        .await?;
        let root = match root {
            Some(root) => root.to_owned(),
            None => files::export_directory(&mut session, self.files, output).await?,
        };
        let progress = format!(
            "Exporting '{}' \u{2192} '{}/{}'...",
            item.path().trim_matches('/'),
            self.files.export_display_path(&root, output),
            item.name(),
        );
        let mut files = files::ExportFiles::default();
        let storage = self.files.export_storage(&root);
        definitions::export_read_item(
            &mut session,
            &mut files,
            read,
            item_type,
            &item.name(),
            &root,
            self.definition_limits,
            storage,
        )
        .await
        .map_err(files::ExportFiles::preparation_error)?;
        Ok(files
            .write(&mut session, self.files, &root, self.ui, progress)
            .await?)
    }

    async fn bulk(
        &mut self,
        command: &TransferCommand,
        context: TransferContext,
        cancelled: &mut Cancellation<'_>,
    ) -> Result<TransferResult, TransferError> {
        let scope = match context.target.kind() {
            NodeKind::Workspace(_) => BulkScope::Workspace,
            NodeKind::Folder => BulkScope::Folder,
            _ => {
                return Err(command_error(
                    "InvalidOperation",
                    format!(
                        "'bulk-export' requires a workspace or folder path, but got '{}'",
                        context.target.name()
                    ),
                )
                .into());
            }
        };
        let output = process_tokens(
            command
                .output
                .as_deref()
                .ok_or_else(|| invalid("Output path is required"))?,
        );
        let root = phase(self.export_directory(&output), cancelled).await?;
        if self.files.export_storage(&root) != definitions::ExportStorage::Local {
            return Err(command_error(
                "InvalidInput",
                "Bulk-export output must be an existing local directory",
            )
            .into());
        }
        let display = self.files.export_display_path(&root, &output);
        let nonempty = !phase(
            async { self.files.is_empty_async(&root).await.map_err(local_error) },
            cancelled,
        )
        .await?;
        if nonempty
            && !command.force
            && !phase(
                self.ui.confirm(&format!(
                    "Output folder '{}' is not empty. Do you want to proceed?",
                    display,
                )),
                cancelled,
            )
            .await?
        {
            return Ok(TransferResult {
                declined: true,
                ..TransferResult::default()
            });
        }
        if command.force {
            self.ui.emit(TransferEvent::Warning(
                "Item definitions are exported without their sensitivity labels".into(),
            ))?;
            if nonempty {
                self.ui.emit(TransferEvent::Warning(
                    "Exporting to a non-empty output folder".into(),
                ))?;
            }
        } else if !phase(
            self.ui.confirm(
                "Item definitions are exported without their sensitivity labels. Are you sure?",
            ),
            cancelled,
        )
        .await?
        {
            return Ok(TransferResult {
                declined: true,
                ..TransferResult::default()
            });
        }
        let context = phase(
            self.metadata.complete_snapshot(command.kind, context),
            cancelled,
        )
        .await?;
        self.check_snapshot(&context)?;
        let workspace = context
            .target
            .workspace()
            .ok_or_else(|| invalid("Workspace metadata is required"))?;
        let items: Vec<_> = context
            .items
            .iter()
            .map(|item| {
                let (_, item_type) = item_parts(&item.element)?;
                Ok(BulkItem {
                    id: require_id(&item.element)?.into(),
                    item_type: item_type.into(),
                    support: item.support,
                })
            })
            .collect::<Result<_, TransferError>>()?;
        if items.is_empty() {
            return Err(command_error(
                "InvalidOperation",
                format!("No items found in '{}'", context.target.name()),
            )
            .into());
        }
        if scope == BulkScope::Folder
            && !items
                .iter()
                .any(|item| item.support == CommandSupport::Supported)
        {
            let message = if items
                .iter()
                .any(|item| item.support == CommandSupport::Rejected)
            {
                "No exportable items found. All items have unsupported types".into()
            } else {
                format!("No items found in '{}'", context.target.name())
            };
            return Err(command_error("InvalidOperation", message).into());
        }
        let path = context.target.path();
        let snapshot = BulkExportSnapshot {
            workspace_id: require_id(&workspace)?,
            from_path: path.trim_matches('/'),
            scope,
            items: &items,
        };
        let mut session = OperationSession::with_context(
            self.client.context(),
            BorrowedExecutor(self.executor),
            self.operation_limits,
            cancelled.as_mut(),
        );
        let progress = format!(
            "Bulk-exporting '{}' \u{2192} '{}'...",
            snapshot.from_path, display
        );
        let mut files = files::ExportFiles::default();
        let mut outcome = definitions::bulk_export_source(
            &mut session,
            &mut files,
            snapshot,
            &root,
            true,
            true,
            self.definition_limits,
        )
        .await
        .map_err(|error| {
            if matches!(
                error.kind,
                DefinitionErrorKind::Unsupported(
                    "bulk target has no exportable items" | "bulk folder has no exportable items"
                )
            ) {
                command_error(
                    "InvalidOperation",
                    "No exportable items found. All items have unsupported types",
                )
                .into()
            } else {
                TransferError::from(files::ExportFiles::preparation_error(error))
            }
        })?;
        outcome.completed_paths = files
            .write(&mut session, self.files, &root, self.ui, progress)
            .await?;
        let (exported_items, skipped_items) = bulk_summary_items(
            scope,
            &items,
            &outcome.supported_ids,
            &outcome.skipped_ids,
            &outcome.indexed_ids,
        )?;
        let exported = type_counts(exported_items)?;
        let skipped = type_counts(skipped_items)?;
        let mut summary = format!(
            "Exported {} items to '{output}'",
            outcome.supported_ids.len()
        );
        if !outcome.skipped_ids.is_empty() {
            let types = skipped
                .iter()
                .map(|(kind, count)| format!("{kind} ({count})"))
                .collect::<Vec<_>>()
                .join(", ");
            summary.push_str(&format!(
                ". Skipped {} items due to unsupported item types: {types}",
                outcome.skipped_ids.len()
            ));
        }
        Ok(TransferResult {
            outputs: vec![OutputInput {
                data: Some(json!([{
                    "exported": outcome.supported_ids.len(), "exported_types": exported,
                    "skipped": outcome.skipped_ids.len(), "skipped_types": skipped, "output": output,
                }])),
                message: Some(summary),
                ..OutputInput::default()
            }],
            key_value: true,
            completed_paths: outcome.completed_paths,
            exported_ids: outcome.supported_ids,
            skipped_ids: outcome.skipped_ids,
            ..TransferResult::default()
        })
    }

    async fn import(
        &mut self,
        command: &TransferCommand,
        context: TransferContext,
        cancelled: &mut Cancellation<'_>,
    ) -> Result<TransferResult, TransferError> {
        let item = &context.target;
        let (workspace, item_type) = item_parts(item)?;
        definitions::resolve_format(item_type, command.format.as_deref())
            .map_err(|kind| TransferError::from(DefinitionError::from(kind)))?;
        let input = process_tokens(
            command
                .input
                .as_deref()
                .ok_or_else(|| invalid("Input path is required"))?,
        );
        let root = phase(self.directory(&input), cancelled).await?;
        if !command.force && !phase(self.ui.confirm("Are you sure?"), cancelled).await? {
            return Ok(TransferResult {
                declined: true,
                ..TransferResult::default()
            });
        }
        let folder_id = item
            .parent()
            .filter(|parent| parent.kind() == NodeKind::Folder)
            .map(|parent| require_id(parent))
            .transpose()?;
        let prepared = phase(
            self.files.prepare_import_async(
                &root,
                ImportTarget {
                    workspace_id: require_id(&workspace)?,
                    item_id: item.id(),
                    item_type,
                    display_name: item.short_name(),
                    folder_id,
                },
                command.format.as_deref(),
                self.definition_limits,
            ),
            cancelled,
        )
        .await?;
        if item.id().is_some() {
            self.ui.emit(TransferEvent::Warning(
                "An item with the same name exists".into(),
            ))?;
            if !command.force && !phase(self.ui.confirm("Overwrite?"), cancelled).await? {
                return Ok(TransferResult {
                    effects: vec![ImportCacheEffect {
                        target: item.clone(),
                        item_id: require_id(item)?.into(),
                    }],
                    declined: true,
                    ..TransferResult::default()
                });
            }
        }
        self.ui.emit(TransferEvent::Grey(format!(
            "Importing{} '{}' \u{2192} '{}'...",
            if item.id().is_some() { " (update)" } else { "" },
            root.display(),
            item.path(),
        )))?;
        let mut session = OperationSession::new(
            self.client.credential().as_ref(),
            self.client.selection(),
            BorrowedExecutor(self.executor),
            self.operation_limits,
            cancelled.as_mut(),
        );
        let outcome = prepared.execute(&mut session).await?;
        Ok(TransferResult {
            outputs: vec![message(format!("'{}' imported", item.name()))],
            effects: vec![ImportCacheEffect {
                target: item.clone(),
                item_id: outcome.item_id,
            }],
            excluded_paths: outcome.excluded_paths,
            ..TransferResult::default()
        })
    }

    async fn directory(&mut self, input: &str) -> Result<PathBuf, TransferError> {
        if input.is_empty() {
            return Err(command_error("InvalidPath", "No such file or directory").into());
        }

        fn check_snapshot(&self, context: &TransferContext) -> Result<(), TransferError> {
            if context.items.len() > self.definition_limits.max_files.get()
                || context.selection_order.len() > self.definition_limits.max_files.get()
            {
                return Err(DefinitionError::from(DefinitionErrorKind::Limit).into());
            }
            Ok(())
        }

        async fn export_directory(&mut self, input: &str) -> Result<PathBuf, TransferError> {
            if input.is_empty() {
                return Err(command_error("InvalidPath", "No such file or directory").into());
            }
            self.files
                .resolve_export_directory_async(input)
                .await
                .map_err(local_error)
        }
        self.files
            .resolve_directory_async(input)
            .await
            .map_err(local_error)
    }
}

const SINGLE_CONFIRM: &str =
    "Item definition is exported without its sensitivity label. Are you sure?";

fn message(text: impl Into<String>) -> OutputInput {
    OutputInput {
        message: Some(text.into()),
        ..OutputInput::default()
    }
}

fn local_error(error: io::Error) -> TransferError {
    DefinitionError::from(DefinitionErrorKind::FileSystem(error)).into()
}

fn require_id(element: &Element) -> Result<&str, TransferError> {
    element
        .id()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| invalid("Existing resource ID is required").into())
}

fn item_parts(item: &Arc<Element>) -> Result<(Arc<Element>, &'static str), TransferError> {
    let NodeKind::Item(item_type) = item.kind() else {
        return Err(command_error("NotSupported", "This transfer requires a Fabric item").into());
    };
    Ok((
        item.workspace()
            .ok_or_else(|| invalid("Workspace metadata is required"))?,
        item_type.as_str(),
    ))
}

fn bulk_summary_items<'a>(
    scope: BulkScope,
    items: &'a [BulkItem],
    supported: &[String],
    skipped: &[String],
    indexed: &[String],
) -> Result<(Vec<&'a BulkItem>, Vec<&'a BulkItem>), TransferError> {
    if scope == BulkScope::Folder {
        return Ok((
            items
                .iter()
                .filter(|item| item.support == CommandSupport::Supported)
                .collect(),
            items
                .iter()
                .filter(|item| item.support == CommandSupport::Rejected)
                .collect(),
        ));
    }
    let by_id: std::collections::HashMap<_, _> =
        items.iter().map(|item| (item.id.as_str(), item)).collect();
    let known = |id: &String| {
        by_id
            .get(id.as_str())
            .copied()
            .ok_or_else(|| TransferError::from(invalid("Incomplete bulk metadata snapshot")))
    };
    let exported = supported.iter().map(known).collect::<Result<Vec<_>, _>>()?;
    let mut skipped_items = skipped
        .iter()
        .filter(|id| indexed.contains(id))
        .map(known)
        .collect::<Result<Vec<_>, _>>()?;
    // Source's final missing-item pass appends the original cached objects,
    // not the last-by-ID lookup used for response-index entries.
    skipped_items.extend(items.iter().filter(|item| !indexed.contains(&item.id)));
    Ok((exported, skipped_items))
}

fn type_counts(items: Vec<&BulkItem>) -> Result<Map<String, Value>, TransferError> {
    let mut counts = Map::new();
    for item in items {
        let count = counts.entry(item.item_type.clone()).or_insert(json!(0));
        *count = json!(
            count
                .as_u64()
                .ok_or_else(|| invalid("Invalid summary count"))?
                + 1
        );
    }
    Ok(counts)
}

/// Render actual outputs using the existing shared JSON/text implementation.
///
/// Immediate UI events have already been emitted. Cache effects remain the
/// caller's responsibility after successful delivery, as in the source handler.
/// A writer failure retains the result for evidence but does not replay mutation.
pub fn render_result(
    result: &TransferResult,
    command: &TransferCommand,
    timestamp: Timestamp,
    format: OutputFormat,
    presentation: Presentation,
    widths: &impl CharacterWidth,
) -> Result<TextPlan, TextError> {
    let format = selected_format(command, format)?;
    let mut plan = TextPlan {
        records: Vec::new(),
        exit_code: 0,
    };
    for output in &result.outputs {
        let mut output = output.clone();
        output.command = Some(command.kind.as_str().into());
        let rendered = text::render(
            &OutputEnvelope::new(timestamp.clone(), output),
            format,
            &TextOptions {
                key_value: result.key_value,
                ..TextOptions::default()
            },
            widths,
            presentation,
        )?;
        plan.records.extend(rendered.records);
        plan.exit_code = rendered.exit_code;
    }
    Ok(plan)
}

fn selected_format(
    command: &TransferCommand,
    config: OutputFormat,
) -> Result<OutputFormat, TextError> {
    OutputFormat::resolve(
        command.output_format.as_deref(),
        Some(match config {
            OutputFormat::Json => "json",
            OutputFormat::Text => "text",
        }),
    )
}

/// Render failures with the existing JSON/text stream and exit-code policy.
///
/// The returned plan does not consume original operation IDs, partial file paths,
/// cache effects or dispatch uncertainty retained by the error.
pub fn render_error(
    error: &TransferError,
    command: &TransferCommand,
    timestamp: Timestamp,
    format: OutputFormat,
    presentation: Presentation,
) -> Result<TextPlan, TextError> {
    let envelope = error.envelope(timestamp, Some(command.kind.as_str().into()));
    match selected_format(command, format)? {
        OutputFormat::Json => {
            let record = envelope.render(fabric_cli_output::Indent::Four)?;
            Ok(TextPlan {
                exit_code: record.exit_code,
                records: vec![record],
            })
        }
        OutputFormat::Text => Ok(text::render_error(
            &CommandError::new(
                envelope.result().message().map(str::to_owned),
                envelope.result().error_code().map(str::to_owned),
            ),
            Some(command.kind.as_str()),
            presentation,
        )),
    }
}

/// Write a rendered result to caller-owned streams; partial writes propagate.
pub fn write_result(
    plan: &TextPlan,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    plan.write_to(stdout, stderr)?;
    Ok(plan.exit_code)
}
