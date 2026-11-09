// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Deferred OS metadata for explicit physical-copy arguments.

use super::resource_files::{ResourceFileInputs, ResourceLocalSnapshot, selected_local_path};
use fabric_cli_context::{LocalKind, LocalMetadata};
use fabric_cli_mutations::{CommandFuture, MutationError};
use fabric_cli_storage_files::{LocalFileSystem, LocalKind as FileKind, SelectedFileSystem};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::runtime::Handle;

/// Native physical-copy capabilities bound to a captured directory and runtime.
///
/// Construction performs no I/O. Metadata and access predicates execute on the
/// caller's blocking pool only after Fabric-first classification asks for them.
pub struct NativeResourceFileInputs {
    base: PathBuf,
    runtime: Handle,
}

impl NativeResourceFileInputs {
    /// Capture explicit local inputs without reading cwd, metadata or credentials.
    pub fn new(base: PathBuf, runtime: Handle) -> Self {
        Self { base, runtime }
    }
}

impl ResourceFileInputs for NativeResourceFileInputs {
    fn snapshot<'a>(
        &'a self,
        input: &'a str,
    ) -> CommandFuture<'a, Result<ResourceLocalSnapshot, MutationError>> {
        Box::pin(async move {
            let selected = selected_local_path(&self.base, input)?;
            let boundary = selected.parent().unwrap_or(&selected).to_owned();
            let files = Arc::new(
                SelectedFileSystem::defer(boundary, selected.clone(), self.runtime.clone())
                    .with_input(PathBuf::from(input)),
            );
            let kind = files
                .kind(Path::new(input))
                .await
                .map_err(|error| MutationError::new("InvalidPath", error.to_string()))?;
            let input = input.to_owned();
            let base = self.base.clone();
            self.runtime
                .spawn_blocking(move || {
                    let directory = Path::new(&input)
                        .parent()
                        .unwrap_or(Path::new(&input))
                        .to_str()
                        .ok_or_else(|| MutationError::new("InvalidPath", "Invalid local dirname"))?
                        .to_owned();
                    let directory_path = if directory.is_empty() {
                        base
                    } else {
                        selected_local_path(&base, &directory)?
                    };
                    let readable = if kind == FileKind::Missing {
                        false
                    } else {
                        access(&selected, false)?
                    };
                    let name = input
                        .rsplit(std::path::is_separator)
                        .next()
                        .unwrap_or("")
                        .to_owned();
                    let snapshot = ResourceLocalSnapshot {
                        metadata: LocalMetadata {
                            path: input.clone(),
                            name,
                            id: format!("local:{input}"),
                            kind: match kind {
                                FileKind::File => LocalKind::File,
                                FileKind::Directory => LocalKind::Directory,
                                FileKind::Missing | FileKind::Other => LocalKind::Other,
                            },
                        },
                        exists: kind != FileKind::Missing,
                        readable,
                        directory,
                        directory_writable: access(&directory_path, true)?,
                        empty_path_exists: false,
                        descriptor_exists: [stdin_exists(), stdout_exists()],
                        files,
                    };
                    Ok(snapshot)
                })
                .await
                .map_err(|_| {
                    MutationError::new(
                        "LocalWorkerFailed",
                        "Local metadata worker did not complete",
                    )
                })?
        })
    }
}

#[cfg(windows)]
fn access(path: &Path, write: bool) -> Result<bool, MutationError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(!write || metadata.is_dir() || !metadata.permissions().readonly()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(MutationError::new("InvalidPath", error.to_string())),
    }
}

#[cfg(unix)]
fn access(path: &Path, write: bool) -> Result<bool, MutationError> {
    let mode = if write {
        rustix::fs::Access::WRITE_OK
    } else {
        rustix::fs::Access::READ_OK
    };
    match rustix::fs::access(path, mode) {
        Ok(()) => Ok(true),
        Err(
            rustix::io::Errno::ACCESS
            | rustix::io::Errno::PERM
            | rustix::io::Errno::NOENT
            | rustix::io::Errno::NOTDIR,
        ) => Ok(false),
        Err(error) => Err(MutationError::new("InvalidPath", error.to_string())),
    }
}

#[cfg(windows)]
fn stdin_exists() -> bool {
    use std::os::windows::io::AsHandle;
    std::io::stdin()
        .as_handle()
        .try_clone_to_owned()
        .and_then(|handle| fs::File::from(handle).metadata())
        .is_ok()
}

#[cfg(windows)]
fn stdout_exists() -> bool {
    use std::os::windows::io::AsHandle;
    std::io::stdout()
        .as_handle()
        .try_clone_to_owned()
        .and_then(|handle| fs::File::from(handle).metadata())
        .is_ok()
}

#[cfg(unix)]
fn stdin_exists() -> bool {
    use std::os::fd::AsFd;
    std::io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .and_then(|handle| fs::File::from(handle).metadata())
        .is_ok()
}

#[cfg(unix)]
fn stdout_exists() -> bool {
    use std::os::fd::AsFd;
    std::io::stdout()
        .as_fd()
        .try_clone_to_owned()
        .and_then(|handle| fs::File::from(handle).metadata())
        .is_ok()
}
