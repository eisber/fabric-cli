// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Thin transfer commands over the released definition library and shared policy.
//!
//! The caller owns discovery, credentials, runtime, local roots, UI and cache.
//! No HTTP implementation, deployment algorithm or second front door lives here.

mod bulk_export;
mod export;
mod files;
mod import;
mod import_environment;
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
    repository::{DeploymentRepository, NotebookRepository, RepositoryError},
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
    /// Validate source path classification before local import I/O or prompting.
    ///
    /// Python recognizes OneLake paths but deliberately rejects remote import;
    /// it never downloads them. A metadata-backed caller must preserve that
    /// classification and source errors. Local-only injected callers may retain
    /// this default and leave containment/existence checks to their file policy.
    fn validate_import_input<'a>(
        &'a self,
        _input: &'a str,
    ) -> TransferFuture<'a, Result<(), CommandError>> {
        Box::pin(async { Ok(()) })
    }

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
    /// Resolve an import directory or regular file on an appropriate file worker.
    ///
    /// The default retains existing directory-only injected adapters. OS-backed
    /// async callers invoke this synchronous capability only on their worker.
    fn resolve_import(&mut self, input: &str) -> io::Result<PathBuf> {
        self.resolve_directory(input)
    }

    /// Resolve an existing confined import directory or regular file.
    ///
    /// The pinned source's `os.walk(file)` yields an empty parts array; it does
    /// not import the file's contents. Async OS hosts must override this method
    /// on their existing worker, retaining link/device/network/containment checks.
    /// The default preserves existing directory-only injected adapters.
    fn resolve_import_async<'a>(
        &'a mut self,
        input: &'a str,
    ) -> TransferFuture<'a, io::Result<PathBuf>> {
        self.resolve_directory_async(input)
    }

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

    /// Prepare all source-supported deployment item types as one owned snapshot.
    ///
    /// The legacy Notebook capability remains independent. This default is for
    /// synchronous injected file policies; native async adapters must override it
    /// using their existing serialized blocking worker and bounded discovery.
    fn prepare_deployment_repository_async<'a>(
        &'a mut self,
        root: &'a Path,
        limits: NotebookDefinitionLimits,
        max_entries: usize,
    ) -> TransferFuture<'a, Result<DeploymentRepository, RepositoryError>> {
        Box::pin(async move {
            DeploymentRepository::discover(&mut files::ImportFiles(self), root, limits, max_entries)
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

    /// Prepare Environment parts on the caller's filesystem worker.
    ///
    /// This opts into the command's separate readiness/publish lifecycle without
    /// weakening the legacy standalone generic import contract. OS-backed hosts
    /// must override it, retaining stop checks and original typed loader errors.
    fn prepare_environment_import_async<'a>(
        &'a mut self,
        root: &'a Path,
        target: ImportTarget<'a>,
        selector: Option<&'a str>,
        limits: DefinitionLimits,
    ) -> TransferFuture<'a, Result<PreparedImport, DefinitionError>> {
        Box::pin(async move {
            PreparedImport::prepare_environment(
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
                        DefinitionErrorKind::FileSystem(error) => (
                            if error.kind() == io::ErrorKind::Unsupported {
                                "NotSupported"
                            } else {
                                "InvalidPath"
                            },
                            error.to_string(),
                        ),
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
