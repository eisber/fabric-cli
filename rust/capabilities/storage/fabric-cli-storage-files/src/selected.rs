// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    LocalEntry, LocalError, LocalErrorKind, LocalFileReader, LocalFileSystem, LocalFileWriter,
    LocalKind, LocalReadFile, OsFileSystem,
};
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
/// The default capability provides no recursive traversal or deletion.
/// [`Self::with_tree`] explicitly enables bounded descendants and checked source
/// cleanup without granting access to sibling files or directories.
///
/// Like [`OsFileSystem`], this is not a hostile-process namespace sandbox.
#[derive(Clone, Debug)]
pub struct SelectedFileSystem {
    boundary: PathBuf,
    selected: PathBuf,
    input: PathBuf,
    runtime: Handle,
    tree: bool,
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
            tree: false,
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

    /// Explicitly authorize bounded tree operations beneath this exact selection.
    ///
    /// This does not grant access to siblings or ancestors. Source walks reject
    /// links and special files; writes remain contained within the selected root.
    pub fn with_tree(mut self) -> Self {
        self.tree = true;
        self
    }

    fn mapped(&self, path: &Path) -> Result<PathBuf, LocalError> {
        if self.exact(path).is_ok() {
            return Ok(self.selected.clone());
        }
        if !self.tree || self.input.as_os_str().is_empty() {
            return Err(denied());
        }
        let relative = path.strip_prefix(&self.input).map_err(|_| denied())?;
        if relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(denied());
        }
        Ok(self.selected.join(relative))
    }

    async fn scoped(&self, path: &Path) -> Result<(OsFileSystem, PathBuf), LocalError> {
        let selected = self.mapped(path)?;
        let files = self.activate().await?;
        if self.tree {
            files.tree_root_kind(&self.selected).await?;
        }
        if selected != self.selected {
            if files.kind(&self.selected).await? != LocalKind::Directory {
                return Err(denied());
            }
            return Ok((
                OsFileSystem::new(self.selected.clone(), self.runtime.clone()).await?,
                selected,
            ));
        }
        Ok((files, selected))
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
    fn open_reader<'a>(
        &'a self,
        path: &'a Path,
        expected: &'a LocalEntry,
    ) -> CommandFuture<'a, Result<Box<dyn LocalFileReader>, LocalError>> {
        Box::pin(async move {
            if !self.tree {
                return Err(denied());
            }
            let (files, selected) = self.scoped(path).await?;
            files.open_reader(&selected, expected).await
        })
    }

    fn open_writer<'a>(
        &'a self,
        path: &'a Path,
        length: u64,
        overwrite: bool,
    ) -> CommandFuture<'a, Result<Box<dyn LocalFileWriter>, LocalError>> {
        Box::pin(async move {
            if !self.tree {
                return Err(denied());
            }
            let (files, selected) = self.scoped(path).await?;
            files.open_writer(&selected, length, overwrite).await
        })
    }

    fn kind<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>> {
        Box::pin(async move {
            let (files, selected) = self.scoped(path).await?;
            files.kind(&selected).await
        })
    }

    fn read<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>> {
        Box::pin(async move {
            let (files, selected) = self.scoped(path).await?;
            files.read(&selected, limit).await
        })
    }

    fn read_for_copy<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<LocalReadFile, LocalError>> {
        Box::pin(async move {
            let (files, selected) = self.scoped(path).await?;
            files.read_for_copy(&selected, limit).await
        })
    }

    fn write<'a>(
        &'a self,
        path: &'a Path,
        content: Vec<u8>,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        Box::pin(async move {
            if self.tree {
                let (files, selected) = self.scoped(path).await?;
                return files.write(&selected, content).await;
            }
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
            if exact {
                local.write(&destination, content).await
            } else {
                // A download basename must stay within the selected directory,
                // not merely its parent boundary, even when it is a symlink.
                OsFileSystem::new(self.selected.clone(), self.runtime.clone())
                    .await?
                    .write(&destination, content)
                    .await
            }
        })
    }

    fn write_checked<'a>(
        &'a self,
        path: &'a Path,
        content: Vec<u8>,
        overwrite: bool,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        Box::pin(async move {
            if !self.tree {
                return Err(denied());
            }
            let (files, selected) = self.scoped(path).await?;
            files.write_checked(&selected, content, overwrite).await
        })
    }

    fn snapshot_tree<'a>(
        &'a self,
        path: &'a Path,
        max_entries: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<LocalEntry>, LocalError>> {
        Box::pin(async move {
            if !self.tree {
                return Err(denied());
            }
            let (files, selected) = self.scoped(path).await?;
            files.snapshot_tree(&selected, max_entries).await
        })
    }

    fn canonical_path<'a>(
        &'a self,
        path: &'a Path,
    ) -> CommandFuture<'a, Result<PathBuf, LocalError>> {
        Box::pin(async move {
            if self.tree {
                let selected = self.mapped(path)?;
                let files = self.activate().await?;
                if files.tree_root_kind(&self.selected).await? == LocalKind::Missing {
                    return files.canonical_path(&selected).await;
                }
            }
            let (files, selected) = self.scoped(path).await?;
            files.canonical_path(&selected).await
        })
    }

    fn create_directory<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<(), LocalError>> {
        Box::pin(async move {
            if !self.tree {
                return Err(denied());
            }
            let (files, selected) = self.scoped(path).await?;
            files.create_directory(&selected).await
        })
    }

    fn remove_copied<'a>(
        &'a self,
        path: &'a Path,
        expected: &'a LocalEntry,
    ) -> CommandFuture<'a, Result<(), LocalError>> {
        Box::pin(async move {
            if !self.tree {
                return Err(denied());
            }
            let (files, selected) = self.scoped(path).await?;
            files.remove_copied(&selected, expected).await
        })
    }
}

fn denied() -> LocalError {
    LocalError::new(LocalErrorKind::OutsideBoundary)
}
