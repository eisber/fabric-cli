// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source export storage composition over existing asynchronous file/DFS owners.

use super::{context::CommandContext, storage_client};
use crate::session::ReadSession;
use fabric_cli_context::{NodeKind, PathInput, PathKind, ResolveOptions};
use fabric_cli_transfer::{TransferFiles, TransferFuture};
use fabric_deploy::definitions::{
    DefinitionEntry, DefinitionFile, DefinitionFileSystem, DefinitionImportFileSystem,
    DefinitionLimits, ExportStorage, validate_part_path,
};
use fabric_sdk_http::HttpExecutor;
use fabric_sdk_onelake::{OneLakePath, OneLakeUpload, OneLakeWriteLimits, OneLakeWriteOutcome};
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use std::{
    fmt,
    future::pending,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Retains the received DFS responses when an export did not finish.
#[derive(Debug)]
struct IncompleteUpload(OneLakeUpload);

impl fmt::Display for IncompleteUpload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Lakehouse export is incomplete: create={}, flush={:?}",
            self.0.create.status(),
            self.0.flush.map(OneLakeWriteOutcome::status)
        )
    }
}

impl std::error::Error for IncompleteUpload {}

pub(super) struct ExportFiles<'a, 'session> {
    local: &'a mut dyn TransferFiles,
    reads: &'a ReadSession<'session>,
    http: &'a HttpExecutor,
    remote: Option<(PathBuf, OneLakePath)>,
}

impl<'a, 'session> ExportFiles<'a, 'session> {
    pub(super) fn new(
        local: &'a mut dyn TransferFiles,
        reads: &'a ReadSession<'session>,
        http: &'a HttpExecutor,
    ) -> Self {
        Self {
            local,
            reads,
            http,
            remote: None,
        }
    }

    fn remote_path(&self, root: &Path, relative: &str) -> io::Result<Option<OneLakePath>> {
        let Some((selected, remote)) = &self.remote else {
            return Ok(None);
        };
        if selected != root {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Export root changed after selection",
            ));
        }
        validate_part_path(relative, DefinitionLimits::default())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Unsafe export part path"))?;
        OneLakePath::new(
            remote.workspace(),
            remote.item(),
            &format!("{}/{}", remote.relative(), relative),
        )
        .map(Some)
        .map_err(io::Error::other)
    }
}

fn lakehouse_owner(target: &fabric_cli_context::PathElement) -> bool {
    let mut current = target;
    while let Some(parent) = current.parent() {
        if matches!(parent.kind(), PathKind::OneLake(_)) {
            current = parent;
        } else {
            return parent.fabric().is_some_and(
                |item| matches!(item.kind(), NodeKind::Item(kind) if kind.as_str() == "Lakehouse"),
            );
        }
    }
    false
}

impl DefinitionFileSystem for ExportFiles<'_, '_> {
    fn preflight(&mut self, root: &Path, files: &[DefinitionFile]) -> io::Result<()> {
        self.local.preflight(root, files)
    }
    fn create_directory(&mut self, root: &Path, name: &str) -> io::Result<()> {
        self.local.create_directory(root, name)
    }
    fn write(&mut self, root: &Path, file: &DefinitionFile) -> io::Result<()> {
        self.local.write(root, file)
    }
}

impl DefinitionImportFileSystem for ExportFiles<'_, '_> {
    fn entries(&mut self, root: &Path, relative: &str) -> io::Result<Vec<DefinitionEntry>> {
        self.local.entries(root, relative)
    }
    fn read(&mut self, root: &Path, relative: &str, max_bytes: usize) -> io::Result<Vec<u8>> {
        self.local.read(root, relative, max_bytes)
    }
}

impl TransferFiles for ExportFiles<'_, '_> {
    fn resolve_directory(&mut self, input: &str) -> io::Result<PathBuf> {
        self.local.resolve_directory(input)
    }
    fn is_empty(&mut self, root: &Path) -> io::Result<bool> {
        self.local.is_empty(root)
    }
    fn resolve_directory_async<'a>(
        &'a mut self,
        input: &'a str,
    ) -> TransferFuture<'a, io::Result<PathBuf>> {
        self.local.resolve_directory_async(input)
    }
    fn is_empty_async<'a>(&'a mut self, root: &'a Path) -> TransferFuture<'a, io::Result<bool>> {
        self.local.is_empty_async(root)
    }
    fn resolve_export_directory_async<'a>(
        &'a mut self,
        input: &'a str,
    ) -> TransferFuture<'a, io::Result<PathBuf>> {
        Box::pin(async move {
            let mut context = CommandContext::default();
            let current = context
                .resolve(
                    self.reads,
                    self.http,
                    self.reads.tenant.clone(),
                    self.reads.current_path,
                    ResolveOptions::default(),
                    pending(),
                )
                .await
                .map_err(|error| {
                    let output = error.envelope(self.reads.timestamp.clone(), None);
                    io::Error::other(
                        output
                            .result()
                            .message()
                            .unwrap_or("Export context resolution failed")
                            .to_owned(),
                    )
                })?;
            let resolved = context
                .resolve_path(
                    self.reads,
                    self.http,
                    current,
                    PathInput::Scalar(input),
                    ResolveOptions::default(),
                    pending(),
                )
                .await;
            match resolved {
                Err(error) => {
                    let error = error.envelope(self.reads.timestamp.clone(), None);
                    if !matches!(
                        error.result().error_code(),
                        Some(
                            "InvalidPath"
                                | "InvalidItemType"
                                | "InvalidWorkspaceType"
                                | "NotFound"
                                | "NotSupported"
                                | "UnsupportedCommand"
                        )
                    ) {
                        // Failed auth/service discovery is not permission to
                        // replace a remote export with a coincidental local path.
                        return Err(io::Error::other(format!(
                            "{}: {}",
                            error
                                .result()
                                .error_code()
                                .unwrap_or("ExportResolutionFailed"),
                            error
                                .result()
                                .message()
                                .unwrap_or("Export destination resolution failed"),
                        )));
                    }
                    self.remote = None;
                    self.local.resolve_export_directory_async(input).await
                }
                Ok(target)
                    if matches!(target.kind(), PathKind::OneLake(_))
                        && target.root_folder() == Some("Files")
                        && lakehouse_owner(&target) =>
                {
                    let id_path = target.path_id();
                    let mut parts = id_path.trim_matches('/').splitn(3, '/');
                    let path = OneLakePath::new(
                        parts.next().unwrap_or(""),
                        parts.next().unwrap_or(""),
                        parts.next().unwrap_or(""),
                    )
                    .map_err(io::Error::other)?;
                    let root = PathBuf::from(target.path().trim_matches('/'));
                    self.remote = Some((root.clone(), path));
                    Ok(root)
                }

                Ok(_) => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Export is only supported for local directories or Lakehouse Files",
                )),
            }
        })
    }
    fn export_storage(&self, _root: &Path) -> ExportStorage {
        if self.remote.is_some() {
            ExportStorage::Lakehouse
        } else {
            ExportStorage::Local
        }
    }
    fn preflight_async<'a>(
        &'a mut self,
        root: &'a Path,
        files: Vec<DefinitionFile>,
    ) -> TransferFuture<'a, io::Result<Vec<DefinitionFile>>> {
        if self.remote.is_none() {
            return self.local.preflight_async(root, files);
        }
        Box::pin(async move {
            for file in &files {
                self.remote_path(root, &file.path)?;
            }
            Ok(files)
        })
    }
    fn create_directory_async<'a>(
        &'a mut self,
        root: &'a Path,
        name: &'a str,
    ) -> TransferFuture<'a, io::Result<()>> {
        if self.remote.is_none() {
            return self.local.create_directory_async(root, name);
        }
        // Python does not issue mkdir for Lakehouse export.
        Box::pin(async move { self.remote_path(root, name).map(|_| ()) })
    }
    fn write_async<'a>(
        &'a mut self,
        root: &'a Path,
        file: DefinitionFile,
    ) -> TransferFuture<'a, io::Result<()>> {
        if self.remote.is_none() {
            return self.local.write_async(root, file);
        }
        Box::pin(async move {
            let path = self
                .remote_path(root, &file.path)?
                .ok_or_else(|| io::Error::other("Missing selected export storage"))?;
            let position = std::str::from_utf8(&file.bytes)
                .map_or(file.bytes.len(), |text| text.chars().count());
            let result = storage_client(self.reads)
                .upload_export_file(
                    &OneLakeHttpExecutor::new(Arc::new(self.http.clone())),
                    &path,
                    &file.bytes,
                    position,
                    "application/json",
                    OneLakeWriteLimits::default(),
                    pending(),
                )
                .await
                .map_err(io::Error::other)?;
            if result.create.status() != 201
                || !matches!(result.flush, Some(OneLakeWriteOutcome::Completed { .. }))
            {
                return Err(io::Error::other(IncompleteUpload(result)));
            }
            Ok(())
        })
    }
}
