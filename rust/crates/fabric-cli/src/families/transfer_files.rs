// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded local transfer work on the caller's existing Tokio blocking executor.

use fabric_cli_transfer::{LocalTransferFiles, TransferFiles, TransferFuture};
use fabric_deploy::definitions::{
    self, DefinitionEntry, DefinitionError, DefinitionErrorKind, DefinitionFile,
    DefinitionFileSystem, DefinitionImportFileSystem, DefinitionLimits, ImportTarget,
    LocalDefinitionFileSystem, PreparedImport,
};
use fabric_deploy::{
    notebook::DefinitionLimits as NotebookDefinitionLimits,
    repository::{DeploymentRepository, NotebookRepository, RepositoryError},
};
use std::{
    fmt, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{runtime::Handle, sync::Mutex};

/// Typed worker failures retained inside the original filesystem error.
#[derive(Debug)]
pub enum TransferWorkerError {
    /// Dropped work was stopped before the next filesystem boundary.
    Cancelled,
    /// The caller's worker failed or was unavailable; effects may be uncertain.
    Unavailable,
    /// A synchronous entry point would block the async caller.
    AsyncRequired,
    /// The configured finite file/byte work bound was exceeded.
    Limit,
}

impl fmt::Display for TransferWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Cancelled => "Local transfer work was cancelled",
            Self::Unavailable => "Local transfer worker is unavailable",
            Self::AsyncRequired => "Local transfer requires its asynchronous file capability",
            Self::Limit => "Local transfer work reached its configured bound",
        })
    }
}

impl std::error::Error for TransferWorkerError {}

fn worker_error(kind: TransferWorkerError) -> io::Error {
    let code = match kind {
        TransferWorkerError::Cancelled => io::ErrorKind::Interrupted,
        TransferWorkerError::AsyncRequired => io::ErrorKind::Unsupported,
        TransferWorkerError::Unavailable | TransferWorkerError::Limit => io::ErrorKind::Other,
    };
    io::Error::new(code, kind)
}

struct Stop(Arc<AtomicBool>);

impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn check_stop(stop: &AtomicBool) -> io::Result<()> {
    if stop.load(Ordering::Acquire) {
        Err(worker_error(TransferWorkerError::Cancelled))
    } else {
        Ok(())
    }
}

fn selected_import_directory(source: &Path, limits: DefinitionLimits) -> io::Result<PathBuf> {
    let mut policy = LocalDefinitionFileSystem::new(limits);
    if let Some(parent) = source.parent() {
        policy.preflight(parent, &[])?;
    } else {
        policy.preflight(source, &[])?;
    }
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.is_file() {
        let parent = source.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Import source has no parent")
        })?;
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid import source"))?;
        policy.preflight(
            parent,
            &[DefinitionFile {
                path: name.to_owned(),
                bytes: Vec::new(),
            }],
        )?;
        Ok(parent.to_owned())
    } else {
        policy.preflight(source, &[])?;
        Ok(source.to_owned())
    }
}

enum WorkerFiles {
    Ready(LocalTransferFiles),
    DeferredImport {
        source: PathBuf,
        original_input: String,
    },
}

impl WorkerFiles {
    fn resolve_import(
        &mut self,
        input: &str,
        limits: DefinitionLimits,
        stop: &AtomicBool,
    ) -> io::Result<PathBuf> {
        match self {
            Self::Ready(files) => files.resolve_import(input),
            Self::DeferredImport {
                source,
                original_input,
            } => {
                if input != original_input.as_str() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Import source argument differs from the explicit selection",
                    ));
                }
                let root = selected_import_directory(source, limits)?;
                check_stop(stop)?;
                let mut files = LocalTransferFiles::new(root, None, limits)?;
                check_stop(stop)?;
                files.bind_import_source(original_input.clone(), source.clone())?;
                check_stop(stop)?;
                let path = files.resolve_import(input)?;
                check_stop(stop)?;
                *self = Self::Ready(files);
                Ok(path)
            }
        }
    }
}

/// OS-backed transfer adapter confined to the released stable-root policy.
///
/// One operation at a time holds the adapter's async mutex, including after a
/// future is dropped. Queued work checks a stop flag before touching the tree;
/// import checks it between entries/read calls. There is no recursive task fanout.
/// Definition transfers retain their released file/traversal/byte limits.
/// Deployment reads and complete repository preparation use their explicit
/// configuration-byte, aggregate Notebook-byte and repository-entry bounds.
/// Both consumers share the same permit and unchanged per-directory path policy.
///
/// A running OS call cannot be cancelled. Dropping a write can leave partial or
/// complete bytes, never rollback. The runtime owner must use bounded shutdown
/// rather than waiting indefinitely for blocking syscalls; this adapter neither
/// creates a runtime nor joins abandoned work in Drop.
pub struct TokioTransferFiles {
    files: Arc<Mutex<WorkerFiles>>,
    runtime: Handle,
    limits: DefinitionLimits,
}

impl TokioTransferFiles {
    /// Defer an explicitly authorized source selection until import resolution.
    ///
    /// This constructor does no filesystem I/O, cwd/home discovery, path
    /// canonicalization or validation. The caller supplies a source candidate
    /// derived from its explicit input and captured path context, plus the exact
    /// normalized source argument. Only the first matching
    /// [`TransferFiles::resolve_import_async`] initializes the selected file or
    /// directory, using the existing serialized worker and all current guards.
    ///
    /// Select this instead of eagerly calling `new(CWD)` for import. The command
    /// handler performs target support/format and Fabric/local classification
    /// before resolution, so remote rejection and earlier source errors occur
    /// without touching this candidate. No remote-name heuristic is used.
    pub fn defer_import_root(
        source: PathBuf,
        original_input: String,
        limits: DefinitionLimits,
        runtime: Handle,
    ) -> Self {
        Self::with_files(
            WorkerFiles::DeferredImport {
                source,
                original_input,
            },
            limits,
            runtime,
        )
    }

    /// Bind an explicitly selected import file/directory and exact source alias.
    ///
    /// The caller resolves only its selected stable root (or file parent), never
    /// service-supplied definition parts. No cwd/home discovery, individual-file
    /// canonicalization, runtime or credential fallback occurs here. Validation
    /// and alias binding use the existing bounded blocking worker. Export and
    /// deployment resolution do not consume this import-only argument alias.
    /// Invoke this after target support/format validation and the existing
    /// metadata boundary's local-input classification, preserving source error
    /// ordering. It does not classify or download remote Fabric inputs.
    ///
    /// # Errors
    /// Preserves the existing root/link/device/alias/containment checks and typed
    /// worker failures. A file source owns only its validated parent directory
    /// and import reads remain bound to the exact selected source.
    pub async fn for_import_root(
        source: PathBuf,
        original_input: String,
        limits: DefinitionLimits,
        runtime: Handle,
    ) -> io::Result<Self> {
        let input = original_input.clone();
        let mut files = Self::defer_import_root(source, original_input, limits, runtime);
        files.resolve_import_async(&input).await?;
        Ok(files)
    }

    /// Validate the explicit root/home on the caller's existing blocking executor.
    ///
    /// Paths are not canonicalized or discovered here: the released link/reparse,
    /// alias, containment and stable-tree checks remain authoritative.
    ///
    /// # Errors
    /// Returns the original native error or a typed worker failure.
    pub async fn new(
        root: PathBuf,
        home: Option<PathBuf>,
        limits: DefinitionLimits,
        runtime: Handle,
    ) -> io::Result<Self> {
        let stop = Stop(Arc::new(AtomicBool::new(false)));
        let flag = stop.0.clone();
        let files = runtime
            .spawn_blocking(move || {
                check_stop(&flag)?;
                LocalTransferFiles::new(root, home, limits)
            })
            .await
            .map_err(|_| worker_error(TransferWorkerError::Unavailable))??;
        Ok(Self::with_files(WorkerFiles::Ready(files), limits, runtime))
    }

    fn with_files(files: WorkerFiles, limits: DefinitionLimits, runtime: Handle) -> Self {
        Self {
            files: Arc::new(Mutex::new(files)),
            runtime,
            limits,
        }
    }

    fn run<T: Send + 'static>(
        &mut self,
        operation: impl FnOnce(&mut LocalTransferFiles, &AtomicBool) -> io::Result<T> + Send + 'static,
    ) -> TransferFuture<'_, io::Result<T>> {
        self.run_state(move |files, stop| match files {
            WorkerFiles::Ready(files) => operation(files, stop),
            WorkerFiles::DeferredImport { .. } => {
                Err(worker_error(TransferWorkerError::AsyncRequired))
            }
        })
    }

    fn run_state<T: Send + 'static>(
        &mut self,
        operation: impl FnOnce(&mut WorkerFiles, &AtomicBool) -> io::Result<T> + Send + 'static,
    ) -> TransferFuture<'_, io::Result<T>> {
        Box::pin(async move {
            // Acquire before spawning: dropped operations keep their permit until
            // the actual worker exits, so another call cannot queue another task.
            let mut files = self.files.clone().lock_owned().await;
            let stop = Stop(Arc::new(AtomicBool::new(false)));
            let flag = stop.0.clone();
            self.runtime
                .spawn_blocking(move || {
                    check_stop(&flag)?;
                    operation(&mut files, &flag)
                })
                .await
                .map_err(|_| worker_error(TransferWorkerError::Unavailable))?
        })
    }

    fn prepare_import_work<'a>(
        &'a mut self,
        root: &'a Path,
        target: ImportTarget<'a>,
        selector: Option<&'a str>,
        limits: DefinitionLimits,
        environment_lifecycle: bool,
    ) -> TransferFuture<'a, Result<PreparedImport, DefinitionError>> {
        let root = root.to_owned();
        let workspace_id = target.workspace_id.to_owned();
        let item_id = target.item_id.map(str::to_owned);
        let item_type = target.item_type.to_owned();
        let display_name = target.display_name.to_owned();
        let folder_id = target.folder_id.map(str::to_owned);
        let selector = selector.map(str::to_owned);
        let limits = DefinitionLimits {
            max_bytes: limits.max_bytes.min(self.limits.max_bytes),
            max_files: limits.max_files.min(self.limits.max_files),
            max_depth: limits.max_depth.min(self.limits.max_depth),
            max_path_bytes: limits.max_path_bytes.min(self.limits.max_path_bytes),
        };
        Box::pin(async move {
            self.run(move |files, stop| {
                let mut filesystem = ImportWorker { files, stop };
                let target = ImportTarget {
                    workspace_id: &workspace_id,
                    item_id: item_id.as_deref(),
                    item_type: &item_type,
                    display_name: &display_name,
                    folder_id: folder_id.as_deref(),
                };
                Ok(if environment_lifecycle {
                    PreparedImport::prepare_environment(
                        &mut filesystem,
                        &root,
                        target,
                        selector.as_deref(),
                        true,
                        limits,
                    )
                } else {
                    definitions::prepare_import(
                        &mut filesystem,
                        &root,
                        target,
                        selector.as_deref(),
                        true,
                        limits,
                    )
                })
            })
            .await
            .map_err(DefinitionErrorKind::FileSystem)?
        })
    }

    fn bounded(&self, files: &[DefinitionFile]) -> io::Result<()> {
        if files.len() > self.limits.max_files.get() {
            return Err(worker_error(TransferWorkerError::Limit));
        }
        let mut remaining = self.limits.max_bytes.get();
        for file in files {
            remaining = remaining
                .checked_sub(file.bytes.len())
                .ok_or_else(|| worker_error(TransferWorkerError::Limit))?;
        }
        Ok(())
    }
}

// Synchronous entry points fail explicitly, never falling back to blocking I/O.
impl DefinitionFileSystem for TokioTransferFiles {
    fn preflight(&mut self, _: &Path, _: &[DefinitionFile]) -> io::Result<()> {
        Err(worker_error(TransferWorkerError::AsyncRequired))
    }

    fn create_directory(&mut self, _: &Path, _: &str) -> io::Result<()> {
        Err(worker_error(TransferWorkerError::AsyncRequired))
    }

    fn write(&mut self, _: &Path, _: &DefinitionFile) -> io::Result<()> {
        Err(worker_error(TransferWorkerError::AsyncRequired))
    }
}

impl DefinitionImportFileSystem for TokioTransferFiles {
    fn entries(&mut self, _: &Path, _: &str) -> io::Result<Vec<DefinitionEntry>> {
        Err(worker_error(TransferWorkerError::AsyncRequired))
    }

    fn read(&mut self, _: &Path, _: &str, _: usize) -> io::Result<Vec<u8>> {
        Err(worker_error(TransferWorkerError::AsyncRequired))
    }
}

impl TransferFiles for TokioTransferFiles {
    fn resolve_directory(&mut self, _: &str) -> io::Result<PathBuf> {
        Err(worker_error(TransferWorkerError::AsyncRequired))
    }

    fn is_empty(&mut self, _: &Path) -> io::Result<bool> {
        Err(worker_error(TransferWorkerError::AsyncRequired))
    }

    fn resolve_directory_async<'a>(
        &'a mut self,
        input: &'a str,
    ) -> TransferFuture<'a, io::Result<PathBuf>> {
        let input = input.to_owned();
        self.run(move |files, _| files.resolve_directory(&input))
    }

    fn resolve_import_async<'a>(
        &'a mut self,
        input: &'a str,
    ) -> TransferFuture<'a, io::Result<PathBuf>> {
        let input = input.to_owned();
        let limits = self.limits;
        self.run_state(move |files, stop| files.resolve_import(&input, limits, stop))
    }

    fn is_empty_async<'a>(&'a mut self, root: &'a Path) -> TransferFuture<'a, io::Result<bool>> {
        let root = root.to_owned();
        self.run(move |files, _| files.is_empty(&root))
    }

    fn read_async<'a>(
        &'a mut self,
        root: &'a Path,
        relative: &'a str,
        max_bytes: usize,
    ) -> TransferFuture<'a, io::Result<Vec<u8>>> {
        let root = root.to_owned();
        let relative = relative.to_owned();
        self.run(move |files, _| files.read(&root, &relative, max_bytes))
    }

    fn prepare_repository_async<'a>(
        &'a mut self,
        root: &'a Path,
        limits: NotebookDefinitionLimits,
        max_entries: usize,
    ) -> TransferFuture<'a, Result<NotebookRepository, RepositoryError>> {
        let root = root.to_owned();
        Box::pin(async move {
            self.run(move |files, stop| {
                Ok(NotebookRepository::discover(
                    &mut ImportWorker { files, stop },
                    &root,
                    limits,
                    max_entries,
                ))
            })
            .await
            .map_err(RepositoryError::FileSystem)?
        })
    }

    fn prepare_deployment_repository_async<'a>(
        &'a mut self,
        root: &'a Path,
        limits: NotebookDefinitionLimits,
        max_entries: usize,
    ) -> TransferFuture<'a, Result<DeploymentRepository, RepositoryError>> {
        let root = root.to_owned();
        let limits = NotebookDefinitionLimits {
            max_bytes: limits.max_bytes.min(self.limits.max_bytes),
        };
        let max_entries = max_entries.min(self.limits.max_files.get());
        Box::pin(async move {
            self.run(move |files, stop| {
                Ok(DeploymentRepository::discover(
                    &mut ImportWorker { files, stop },
                    &root,
                    limits,
                    max_entries,
                ))
            })
            .await
            .map_err(RepositoryError::FileSystem)?
        })
    }

    fn preflight_async<'a>(
        &'a mut self,
        root: &'a Path,
        files: Vec<DefinitionFile>,
    ) -> TransferFuture<'a, io::Result<Vec<DefinitionFile>>> {
        let root = root.to_owned();
        Box::pin(async move {
            self.bounded(&files)?;
            self.run(move |local, _| {
                local.preflight(&root, &files)?;
                Ok(files)
            })
            .await
        })
    }

    fn create_directory_async<'a>(
        &'a mut self,
        root: &'a Path,
        name: &'a str,
    ) -> TransferFuture<'a, io::Result<()>> {
        let root = root.to_owned();
        let name = name.to_owned();
        self.run(move |files, _| files.create_directory(&root, &name))
    }

    fn write_async<'a>(
        &'a mut self,
        root: &'a Path,
        file: DefinitionFile,
    ) -> TransferFuture<'a, io::Result<()>> {
        let root = root.to_owned();
        Box::pin(async move {
            self.bounded(std::slice::from_ref(&file))?;
            self.run(move |files, _| files.write(&root, &file)).await
        })
    }

    fn prepare_import_async<'a>(
        &'a mut self,
        root: &'a Path,
        target: ImportTarget<'a>,
        selector: Option<&'a str>,
        limits: DefinitionLimits,
    ) -> TransferFuture<'a, Result<PreparedImport, DefinitionError>> {
        self.prepare_import_work(root, target, selector, limits, false)
    }

    fn prepare_environment_import_async<'a>(
        &'a mut self,
        root: &'a Path,
        target: ImportTarget<'a>,
        selector: Option<&'a str>,
        limits: DefinitionLimits,
    ) -> TransferFuture<'a, Result<PreparedImport, DefinitionError>> {
        self.prepare_import_work(root, target, selector, limits, true)
    }
}

struct ImportWorker<'a> {
    files: &'a mut LocalTransferFiles,
    stop: &'a AtomicBool,
}

impl DefinitionImportFileSystem for ImportWorker<'_> {
    fn entries(&mut self, root: &Path, relative: &str) -> io::Result<Vec<DefinitionEntry>> {
        check_stop(self.stop)?;
        self.files.entries(root, relative)
    }

    fn read(&mut self, root: &Path, relative: &str, max_bytes: usize) -> io::Result<Vec<u8>> {
        check_stop(self.stop)?;
        self.files.read(root, relative, max_bytes)
    }
}
