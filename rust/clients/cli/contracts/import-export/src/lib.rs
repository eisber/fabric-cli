// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared CLI import/export interfaces and caller-owned file-policy adapters.
//!
//! This package owns no import/export command workflow or service discovery.
//! The retained argument shape also contains the legacy deployment fields.

#![cfg_attr(test, deny(unnameable_test_items))]

pub mod execution;
mod files;
mod local;
pub mod parser;

pub use execution::DefinitionCommandContext;
pub use local::LocalTransferFiles;
pub use parser::{TransferCommand, TransferKind, process_tokens};

use fabric_cli_context::Element;
use fabric_cli_output::{CommandError, OutputInput};
use fabric_deploy::definitions::{
    self, CommandSupport, DefinitionError, DefinitionFile, DefinitionFileSystem,
    DefinitionImportFileSystem, DefinitionLimits, ImportTarget, PreparedImport,
};
use fabric_deploy::{
    notebook::DefinitionLimits as NotebookDefinitionLimits,
    repository::{DeploymentRepository, NotebookRepository, RepositoryError},
};
use std::{
    fmt,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
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

impl fmt::Display for TransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Transfer command failed; inspect original operation and file evidence")
    }
}

impl std::error::Error for TransferError {}
