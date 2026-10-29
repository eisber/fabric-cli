// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{LocalError, LocalErrorKind, LocalFileSystem, LocalKind, LocalReadFile, OsFileSystem};
use fabric_cli_mutations::CommandFuture;
use std::{
    io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};
use tokio::runtime::Handle;

/// Deferred authority for one caller-selected native path, not its whole drive.
///
/// Construction performs no I/O, cwd/home discovery, expansion or classification.
/// The metadata owner first selects a local route. Each method then activates
/// the existing bounded OS worker and its unchanged containment/symlink policy.
/// Reads/inspection are restricted to the exact selected spelling; writes may
/// also select one direct basename inside an existing selected directory.
/// No recursive traversal, parent creation, deletion or local move is provided.
///
/// Like [`OsFileSystem`], this is not a hostile-process namespace sandbox.
#[derive(Clone, Debug)]
pub struct SelectedFileSystem {
    boundary: PathBuf,
    selected: PathBuf,
    input: PathBuf,
    runtime: Handle,
}

impl SelectedFileSystem {
    /// Bind an explicit existing boundary and absolute selected candidate.
    ///
    /// Validation is deferred until a method is polled, so remote classification
    /// and ready cancellation do not touch the local filesystem. Relative input
    /// must first be bound by the caller to its captured local base, not by an
    /// ambient cwd lookup here. The boundary alone never authorizes other paths.
    pub fn defer(boundary: PathBuf, selected: PathBuf, runtime: Handle) -> Self {
        Self {
            boundary,
            input: selected.clone(),
            selected,
            runtime,
        }
    }

    /// Bind the exact original path spelling to the caller's absolute candidate.
    ///
    /// This preserves relative path output without reading the process cwd.
    /// An empty alias remains invalid, never shorthand for the selected root.
    pub fn with_input(mut self, input: PathBuf) -> Self {
        self.input = input;
        self
    }

    /// Exact caller-selected candidate, without filesystem activation.
    pub fn selected(&self) -> &Path {
        &self.selected
    }

    async fn activate(&self) -> Result<OsFileSystem, LocalError> {
        if !self.boundary.is_absolute() || !self.selected.is_absolute() {
            return Err(denied());
        }
        OsFileSystem::new(self.boundary.clone(), self.runtime.clone()).await
    }

    fn exact(&self, path: &Path) -> Result<(), LocalError> {
        if path.as_os_str().is_empty() {
            return Err(LocalError::new(LocalErrorKind::Io(io::Error::from(
                io::ErrorKind::NotFound,
            ))));
        }
        if path.as_os_str() == self.input.as_os_str() {
            Ok(())
        } else {
            Err(denied())
        }
    }
}

impl LocalFileSystem for SelectedFileSystem {
    fn kind<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>> {
        Box::pin(async move {
            self.exact(path)?;
            self.activate().await?.kind(&self.selected).await
        })
    }

    fn read<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>> {
        Box::pin(async move {
            self.exact(path)?;
            self.activate().await?.read(&self.selected, limit).await
        })
    }

    fn read_for_copy<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<LocalReadFile, LocalError>> {
        Box::pin(async move {
            self.exact(path)?;
            self.activate()
                .await?
                .read_for_copy(&self.selected, limit)
                .await
        })
    }

    fn write<'a>(
        &'a self,
        path: &'a Path,
        content: Vec<u8>,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        Box::pin(async move {
            if path.as_os_str().is_empty() || self.input.as_os_str().is_empty() {
                return Err(LocalError::new(LocalErrorKind::Io(io::Error::from(
                    io::ErrorKind::NotFound,
                ))));
            }
            let exact = self.exact(path).is_ok();
            let destination = if exact {
                self.selected.clone()
            } else {
                let name = path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .ok_or_else(denied)?;
                let expected = crate::local::child_path(&self.input, name)?;
                if path.as_os_str() != expected.as_os_str() {
                    return Err(denied());
                }
                crate::local::child_path(&self.selected, name)?
            };
            let local = self.activate().await?;
            if !exact && local.kind(&self.selected).await? != LocalKind::Directory {
                return Err(denied());
            }
            local.write(&destination, content).await
        })
    }
}

fn denied() -> LocalError {
    LocalError::new(LocalErrorKind::OutsideBoundary)
}
