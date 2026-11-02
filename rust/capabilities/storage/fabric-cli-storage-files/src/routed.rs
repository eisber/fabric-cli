// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    LocalEntry, LocalError, LocalErrorKind, LocalFileReader, LocalFileSystem, LocalFileWriter,
    LocalKind, LocalReadFile,
};
use fabric_cli_mutations::CommandFuture;
use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Two explicitly selected local endpoints, without drive-wide authority.
///
/// Routing is lexical and side-effect free. The selected capability still owns
/// canonical containment and link checks; an I/O failure is never retried against
/// another capability. Nested aliases select the most specific endpoint.
pub struct RoutedFileSystems {
    roots: Vec<(PathBuf, Arc<dyn LocalFileSystem>)>,
}

impl RoutedFileSystems {
    /// Compose caller-owned endpoint aliases, without probing any filesystem.
    pub fn new(roots: Vec<(PathBuf, Arc<dyn LocalFileSystem>)>) -> Self {
        Self { roots }
    }

    fn select(&self, path: &Path) -> Result<&dyn LocalFileSystem, LocalError> {
        self.roots
            .iter()
            .filter(|(root, _)| !root.as_os_str().is_empty() && path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .map(|(_, files)| files.as_ref())
            .ok_or_else(|| LocalError::new(LocalErrorKind::OutsideBoundary))
    }
}

impl LocalFileSystem for RoutedFileSystems {
    fn open_reader<'a>(
        &'a self,
        path: &'a Path,
        expected: &'a LocalEntry,
    ) -> CommandFuture<'a, Result<Box<dyn LocalFileReader>, LocalError>> {
        Box::pin(async move { self.select(path)?.open_reader(path, expected).await })
    }

    fn open_writer<'a>(
        &'a self,
        path: &'a Path,
        length: u64,
        overwrite: bool,
    ) -> CommandFuture<'a, Result<Box<dyn LocalFileWriter>, LocalError>> {
        Box::pin(async move {
            self.select(path)?
                .open_writer(path, length, overwrite)
                .await
        })
    }

    fn kind<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<LocalKind, LocalError>> {
        Box::pin(async move { self.select(path)?.kind(path).await })
    }
    fn read<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<u8>, LocalError>> {
        Box::pin(async move { self.select(path)?.read(path, limit).await })
    }
    fn read_for_copy<'a>(
        &'a self,
        path: &'a Path,
        limit: NonZeroUsize,
    ) -> CommandFuture<'a, Result<LocalReadFile, LocalError>> {
        Box::pin(async move { self.select(path)?.read_for_copy(path, limit).await })
    }
    fn write<'a>(
        &'a self,
        path: &'a Path,
        content: Vec<u8>,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        Box::pin(async move { self.select(path)?.write(path, content).await })
    }
    fn write_checked<'a>(
        &'a self,
        path: &'a Path,
        content: Vec<u8>,
        overwrite: bool,
    ) -> CommandFuture<'a, Result<usize, LocalError>> {
        Box::pin(async move {
            self.select(path)?
                .write_checked(path, content, overwrite)
                .await
        })
    }
    fn snapshot_tree<'a>(
        &'a self,
        path: &'a Path,
        max_entries: NonZeroUsize,
    ) -> CommandFuture<'a, Result<Vec<LocalEntry>, LocalError>> {
        Box::pin(async move { self.select(path)?.snapshot_tree(path, max_entries).await })
    }
    fn canonical_path<'a>(
        &'a self,
        path: &'a Path,
    ) -> CommandFuture<'a, Result<PathBuf, LocalError>> {
        Box::pin(async move { self.select(path)?.canonical_path(path).await })
    }
    fn create_directory<'a>(&'a self, path: &'a Path) -> CommandFuture<'a, Result<(), LocalError>> {
        Box::pin(async move { self.select(path)?.create_directory(path).await })
    }
    fn remove_copied<'a>(
        &'a self,
        path: &'a Path,
        expected: &'a LocalEntry,
    ) -> CommandFuture<'a, Result<(), LocalError>> {
        Box::pin(async move { self.select(path)?.remove_copied(path, expected).await })
    }
}
