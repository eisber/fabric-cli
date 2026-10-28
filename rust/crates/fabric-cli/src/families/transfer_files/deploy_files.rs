// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Read-only, configuration-authorized deploy paths on the shared file worker.

use super::{ImportWorker, TransferWorkerError, check_stop, serialized_work, worker_error};
use fabric_cli_transfer::{LocalTransferFiles, TransferFiles, TransferFuture};
use fabric_deploy::{
    definitions::{
        DefinitionEntry, DefinitionFile, DefinitionFileSystem, DefinitionImportFileSystem,
        DefinitionLimits, LocalDefinitionFileSystem, validate_part_path,
    },
    notebook::DefinitionLimits as NotebookDefinitionLimits,
    repository::{DeploymentRepository, NotebookRepository, RepositoryError},
};
use std::{
    io,
    path::{Component, Path, PathBuf, Prefix},
    sync::{Arc, atomic::AtomicBool},
};
use tokio::{runtime::Handle, sync::Mutex};

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Unauthorized or unsafe deployment file selection",
    )
}

fn directory(path: &Path, limits: DefinitionLimits, stop: &AtomicBool) -> io::Result<PathBuf> {
    if !path.is_absolute() || path.as_os_str().len() > 32 * 1024 {
        return Err(invalid());
    }
    let mut policy = LocalDefinitionFileSystem::new(limits);
    let mut selected = PathBuf::new();
    for component in path.components() {
        check_stop(stop)?;
        match component {
            Component::Prefix(prefix)
                if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) =>
            {
                return Err(invalid());
            }
            Component::ParentDir => {
                // Check the physical prefix before cancelling it lexically.
                policy.preflight(&selected, &[])?;
                if selected.file_name().is_none() || !selected.pop() {
                    return Err(invalid());
                }
            }
            Component::CurDir => {}
            Component::Normal(name) => {
                validate_part_path(
                    name.to_str().ok_or_else(invalid)?,
                    DefinitionLimits::default(),
                )
                .map_err(|_| invalid())?;
                selected.push(name);
            }
            other => selected.push(other),
        }
    }
    policy.preflight(&selected, &[])?;
    check_stop(stop)?;
    Ok(selected)
}

struct Selection {
    root: PathBuf,
    files: LocalTransferFiles,
    configuration: bool,
    read_available: bool,
}

struct LocalConfiguration {
    config: PathBuf,
    argument: String,
    loaded: bool,
    sealed: bool,
    selection: Option<Selection>,
}

impl LocalConfiguration {
    fn resolve(
        &mut self,
        input: &str,
        limits: DefinitionLimits,
        stop: &AtomicBool,
    ) -> io::Result<PathBuf> {
        if self.sealed {
            return Err(invalid());
        }
        let configuration = !self.loaded;
        let requested = if configuration {
            let argument = Path::new(&self.argument);
            let alias = argument
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            if Path::new(input) != alias || argument.file_name() != self.config.file_name() {
                return Err(invalid());
            }
            self.config.parent().ok_or_else(invalid)?.to_owned()
        } else {
            PathBuf::from(input)
        };
        let root = directory(&requested, limits, stop)?;
        let files = LocalTransferFiles::new(root.clone(), None, limits)?;
        check_stop(stop)?;
        self.selection = Some(Selection {
            root: root.clone(),
            files,
            configuration,
            read_available: true,
        });
        Ok(root)
    }

    fn read(
        &mut self,
        root: &Path,
        name: &str,
        max_bytes: usize,
        limits: DefinitionLimits,
        stop: &AtomicBool,
    ) -> io::Result<Vec<u8>> {
        if self.sealed || name.contains(['/', '\\']) {
            return Err(invalid());
        }
        let selected = self.selection.as_mut().ok_or_else(invalid)?;
        if selected.root != root
            || !selected.read_available
            || selected.configuration
                && self.config.file_name().and_then(|name| name.to_str()) != Some(name)
        {
            return Err(invalid());
        }
        selected.read_available = false;
        check_stop(stop)?;
        selected.files.preflight(
            root,
            &[DefinitionFile {
                path: name.into(),
                bytes: Vec::new(),
            }],
        )?;
        let bytes = selected
            .files
            .read(root, name, max_bytes.min(limits.max_bytes.get()))?;
        check_stop(stop)?;
        if selected.configuration {
            self.loaded = true;
        }
        Ok(bytes)
    }

    fn repository(&mut self, root: &Path) -> io::Result<&mut LocalTransferFiles> {
        let selected = self.selection.as_mut().ok_or_else(invalid)?;
        if !self.loaded
            || selected.configuration
            || selected.root != root
            || root.file_name().is_none()
        {
            return Err(invalid());
        }
        self.sealed = true;
        Ok(&mut selected.files)
    }
}

/// Deploy-only local-file authority for one explicit CLI configuration.
///
/// Construction performs no I/O or cwd/home lookup. The first directory/read
/// pair is pinned to the caller's explicit configuration path and argument.
/// Subsequent directory selections are assertions by the trusted deploy
/// consumer that the path came from that local configuration, parameter or
/// template document; never pass service response paths to this capability.
/// `DeployHost` makes these calls only during its local preparation phase.
///
/// Each selection permits one named file read or a confined repository
/// snapshot. Snapshot preparation seals further path selection/file reads,
/// while allowing the legacy-to-generic snapshot fallback on that same root.
/// There is no drive-wide repository authority or local write capability.
/// All operations use the existing serialized worker/Stop policy.
pub struct TokioDeployFiles {
    state: Arc<Mutex<LocalConfiguration>>,
    runtime: Handle,
    limits: DefinitionLimits,
}

impl TokioDeployFiles {
    /// Defer one configuration and its local-document-driven file selections.
    ///
    /// `config_candidate` is the full caller-selected absolute file path;
    /// `config_argument` is the exact scalar supplied to the deploy consumer.
    /// Root selection checks links/reparse/device aliases before normalizing
    /// explicit local `..` selectors. Definition part filenames are never
    /// canonicalized or granted authority outside their checked root.
    pub fn defer_config(
        config_candidate: PathBuf,
        config_argument: String,
        limits: DefinitionLimits,
        runtime: Handle,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(LocalConfiguration {
                config: config_candidate,
                argument: config_argument,
                loaded: false,
                sealed: false,
                selection: None,
            })),
            runtime,
            limits,
        }
    }

    fn run<T: Send + 'static>(
        &mut self,
        operation: impl FnOnce(&mut LocalConfiguration, &AtomicBool) -> io::Result<T> + Send + 'static,
    ) -> TransferFuture<'_, io::Result<T>> {
        serialized_work(self.state.clone(), self.runtime.clone(), operation)
    }
}

impl DefinitionFileSystem for TokioDeployFiles {
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

impl DefinitionImportFileSystem for TokioDeployFiles {
    fn entries(&mut self, _: &Path, _: &str) -> io::Result<Vec<DefinitionEntry>> {
        Err(worker_error(TransferWorkerError::AsyncRequired))
    }
    fn read(&mut self, _: &Path, _: &str, _: usize) -> io::Result<Vec<u8>> {
        Err(worker_error(TransferWorkerError::AsyncRequired))
    }
}

impl TransferFiles for TokioDeployFiles {
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
        let limits = self.limits;
        self.run(move |state, stop| state.resolve(&input, limits, stop))
    }
    fn read_async<'a>(
        &'a mut self,
        root: &'a Path,
        relative: &'a str,
        max_bytes: usize,
    ) -> TransferFuture<'a, io::Result<Vec<u8>>> {
        let root = root.to_owned();
        let relative = relative.to_owned();
        let limits = self.limits;
        self.run(move |state, stop| state.read(&root, &relative, max_bytes, limits, stop))
    }
    fn prepare_repository_async<'a>(
        &'a mut self,
        root: &'a Path,
        limits: NotebookDefinitionLimits,
        max_entries: usize,
    ) -> TransferFuture<'a, Result<NotebookRepository, RepositoryError>> {
        let root = root.to_owned();
        let limits = NotebookDefinitionLimits {
            max_bytes: limits.max_bytes.min(self.limits.max_bytes),
        };
        let max_entries = max_entries.min(self.limits.max_files.get());
        Box::pin(async move {
            self.run(move |state, stop| {
                let files = state.repository(&root)?;
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
            self.run(move |state, stop| {
                let files = state.repository(&root)?;
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
}
