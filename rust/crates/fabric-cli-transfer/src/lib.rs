// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Thin transfer commands over the released definition library and shared policy.
//!
//! The caller owns discovery, credentials, runtime, local roots, UI and cache.
//! No HTTP implementation, deployment algorithm or second front door lives here.

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
    DefinitionLimits, DefinitionTarget, ImportTarget,
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
    /// Resolve a source local directory inside the caller's explicitly owned roots.
    ///
    /// Remote Fabric paths must fail explicitly, never become local directories.
    fn resolve_directory(&mut self, input: &str) -> io::Result<PathBuf>;
    /// Check the selected existing directory, without unbounded enumeration.
    fn is_empty(&mut self, root: &Path) -> io::Result<bool>;
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
    /// Source cache changes; caller must apply these before rendering results.
    pub effects: Vec<ImportCacheEffect>,
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
    /// Earlier confirmed cache effects.
    pub effects: Vec<ImportCacheEffect>,
}

impl From<CommandError> for TransferError {
    fn from(error: CommandError) -> Self {
        Self {
            failure: TransferFailure::Command(error),
            completed_paths: Vec::new(),
            effects: Vec::new(),
        }
    }
}

impl From<DefinitionError> for TransferError {
    fn from(error: DefinitionError) -> Self {
        Self {
            completed_paths: error.completed_paths.clone(),
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
                        DefinitionErrorKind::Limit => (
                            "InvalidOperation",
                            "Definition transfer limit reached".into(),
                        ),
                        DefinitionErrorKind::UnsafePath | DefinitionErrorKind::Collision => (
                            "InvalidPath",
                            "Unsafe or conflicting definition path".into(),
                        ),
                        DefinitionErrorKind::ConfirmationRequired => {
                            ("InvalidOperation", "Confirmation is required".into())
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

async fn phase<T>(
    work: impl Future<Output = Result<T, CommandError>>,
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
    /// Synchronous local filesystem work must run on the caller's appropriate
    /// worker. Cancellation drops metadata/UI phases; SDK sessions retain original
    /// dispatch evidence. No request or partial write is retried or rolled back.
    pub async fn execute(
        &mut self,
        command: &TransferCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<TransferResult, TransferError> {
        if command.kind == TransferKind::Deploy {
            return Err(command_error("NotSupported",
                "deploy --config requires the unported CICD configuration, parameter validation, planning and command-scoped bulk-flag lifecycle; the bounded Notebook publisher is not this command").into());
        }
        if command.kind == TransferKind::BulkExport && !command.recursive {
            return Err(command_error(
                "InvalidOperation",
                "The --recursive flag is required for bulk-export",
            )
            .into());
        }
        let mut cancelled: Cancellation<'_> = Box::pin(cancelled);
        phase(std::future::ready(Ok(())), &mut cancelled).await?;
        let context = phase(
            self.metadata.resolve(command.kind, &command.path),
            &mut cancelled,
        )
        .await?;
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
                let root = self.directory(&output)?;
                result.completed_paths = self
                    .export_one(&context.target, command, &root, cancelled)
                    .await?;
            } else {
                result.declined = true;
            }
            // Python emits this message even when its single-item prompt is declined.
            result
                .outputs
                .push(message(format!("'{}' exported", context.target.name())));
            return Ok(result);
        }
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
        let root = self.directory(&output)?;
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
        let mut count = 0;
        for item in supported
            .into_iter()
            .filter(|item| selected.contains(&item.element.name()))
        {
            match self
                .export_one(&item.element, command, &root, cancelled)
                .await
            {
                Ok(paths) => result.completed_paths.extend(paths),
                Err(mut error) => {
                    let mut paths = result.completed_paths;
                    paths.append(&mut error.completed_paths);
                    error.completed_paths = paths;
                    return Err(error);
                }
            }
            count += 1;
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
        root: &Path,
        cancelled: &mut Cancellation<'_>,
    ) -> Result<Vec<String>, TransferError> {
        let (workspace, item_type) = item_parts(item)?;
        let item_id = require_id(item)?;
        let mut session = OperationSession::new(
            self.client.credential().as_ref(),
            self.client.selection(),
            BorrowedExecutor(self.executor),
            self.operation_limits,
            cancelled.as_mut(),
        );
        let progress = format!(
            "Exporting '{}' \u{2192} '{}/{}'...",
            item.path().trim_matches('/'),
            root.display(),
            item.name(),
        );
        let mut files = NotifyingFiles {
            inner: self.files,
            ui: self.ui,
            progress: Some(progress),
        };
        let outcome = definitions::export_item(
            &mut session,
            &mut files,
            DefinitionTarget {
                workspace_id: require_id(&workspace)?,
                item_id,
                item_type,
            },
            &item.name(),
            root,
            command.format.as_deref(),
            true,
            self.definition_limits,
        )
        .await?;
        Ok(outcome.completed_paths)
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
                    "Bulk-export requires a workspace or folder",
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
        let root = self.directory(&output)?;
        let nonempty = !self.files.is_empty(&root).map_err(local_error)?;
        if nonempty
            && !command.force
            && !phase(
                self.ui.confirm(&format!(
                    "Output folder '{}' is not empty. Do you want to proceed?",
                    root.display(),
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
            return Err(command_error("InvalidOperation", "Bulk-export target is empty").into());
        }
        let path = context.target.path();
        let snapshot = BulkExportSnapshot {
            workspace_id: require_id(&workspace)?,
            from_path: path.trim_matches('/'),
            scope,
            items: &items,
        };
        let mut session = OperationSession::new(
            self.client.credential().as_ref(),
            self.client.selection(),
            BorrowedExecutor(self.executor),
            self.operation_limits,
            cancelled.as_mut(),
        );
        let mut files = NotifyingFiles {
            inner: self.files,
            ui: self.ui,
            progress: Some(format!(
                "Bulk-exporting '{}' \u{2192} '{}'...",
                snapshot.from_path,
                root.display()
            )),
        };
        let outcome = definitions::bulk_export(
            &mut session,
            &mut files,
            snapshot,
            &root,
            true,
            true,
            self.definition_limits,
        )
        .await?;
        let exported = type_counts(&outcome.supported_ids, &items)?;
        let skipped = type_counts(&outcome.skipped_ids, &items)?;
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
        let root = self.directory(&input)?;
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
        let prepared = definitions::prepare_import(
            self.files,
            &root,
            ImportTarget {
                workspace_id: require_id(&workspace)?,
                item_id: item.id(),
                item_type,
                display_name: item.short_name(),
                folder_id,
            },
            command.format.as_deref(),
            true,
            self.definition_limits,
        )?;
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
            ..TransferResult::default()
        })
    }

    fn directory(&mut self, input: &str) -> Result<PathBuf, TransferError> {
        if input.is_empty() {
            return Err(command_error("InvalidPath", "No such file or directory").into());
        }
        self.files.resolve_directory(input).map_err(local_error)
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

fn type_counts(ids: &[String], items: &[BulkItem]) -> Result<Map<String, Value>, TransferError> {
    let mut counts = Map::new();
    for id in ids {
        let item = items
            .iter()
            .find(|item| item.id == *id)
            .ok_or_else(|| invalid("Incomplete bulk metadata snapshot"))?;
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

struct NotifyingFiles<'a> {
    inner: &'a mut dyn TransferFiles,
    ui: &'a dyn TransferUi,
    progress: Option<String>,
}

impl DefinitionFileSystem for NotifyingFiles<'_> {
    fn preflight(&mut self, root: &Path, files: &[DefinitionFile]) -> io::Result<()> {
        self.inner.preflight(root, files)?;
        if let Some(progress) = self.progress.take() {
            self.ui
                .emit(TransferEvent::Grey(progress))
                .map_err(|_| io::Error::other("Transfer progress output failed"))?;
        }
        Ok(())
    }

    fn create_directory(&mut self, root: &Path, name: &str) -> io::Result<()> {
        self.inner.create_directory(root, name)
    }

    fn write(&mut self, root: &Path, file: &DefinitionFile) -> io::Result<()> {
        self.inner.write(root, file)
    }
}

/// Render actual outputs using the existing shared JSON/text implementation.
///
/// Immediate UI events have already been emitted. Cache effects remain the
/// caller's responsibility, including when a subsequent output writer fails.
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
