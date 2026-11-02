// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{CopyError, CopyResult, Interruption, LocalFileSystem, StorageFilesHost, UploadResult};
use fabric_cli_context::{PathElement, PathKind};
use fabric_cli_mutations::{
    MutationError, OneLakeWriteError, OneLakeWriteHost, OneLakeWriteResult,
    frontdoor::{ResourceCommand, ResourceContext, ResourceTransferHost},
};
use fabric_cli_output::OutputInput;
use std::{fmt, future::Future, path::Path, pin::pin, sync::Arc};

/// Resource transfer evidence, kept intact across rendering and writer failure.
#[derive(Debug)]
pub enum ResourceFileResult {
    /// Existing remote copy/rename evidence; acceptance is not completion.
    Storage(OneLakeWriteResult),
    /// Physical upload, including actual create/append/flush responses.
    Upload(UploadResult),
    /// Physical download, including local byte count and inspection failures.
    Download(CopyResult),
}

impl ResourceFileResult {
    /// Borrow source output without discarding the underlying transfer receipt.
    pub fn outputs(&self) -> &[OutputInput] {
        match self {
            Self::Storage(result) => &result.outputs,
            Self::Upload(result) => &result.outputs,
            Self::Download(result) => &result.outputs,
        }
    }
}

/// Original command, storage or physical-file failure; never a success fallback.
#[derive(Debug)]
pub enum ResourceFileError {
    /// Resolution, unsupported route or missing explicit filesystem capability.
    Command(MutationError),
    /// Existing remote transfer failure and mutation uncertainty.
    Storage(OneLakeWriteError),
    /// Physical copy stage, local effects and original remote failure.
    Copy(CopyError),
}

impl ResourceFileError {
    /// Render through the existing output policy while retaining this receipt.
    pub fn output(&self) -> OutputInput {
        match self {
            Self::Command(error) => error.output(),
            Self::Storage(error) => error.output(),
            Self::Copy(error) => error.output(),
        }
    }
}

impl fmt::Display for ResourceFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("resource transfer failed; inspect the original typed receipt")
    }
}

impl std::error::Error for ResourceFileError {}

/// Additive resource `cp`/`mv` composition, not definition import/export.
///
/// The metadata owner supplies Fabric-first resolution and any explicit local
/// snapshots. Local methods are deferred to the existing filesystem worker.
/// No file root, current directory, credential or runtime is discovered here.
pub struct ResourceFileHost<'a> {
    /// Existing command-local context/cache boundary.
    pub context: &'a mut dyn ResourceContext,
    /// Released storage handlers, transport, limits and immediate notices.
    pub storage: OneLakeWriteHost<'a>,
    /// Explicit physical file capability; absent does not grant filesystem access.
    pub files: Option<&'a dyn LocalFileSystem>,
}

impl ResourceFileHost<'_> {
    /// Resolve source then target and execute a supported physical/remote route.
    ///
    /// Local copies preserve source force/recursive/collision behavior: the file
    /// handlers ignore those flags, while directory sources reject recursion.
    /// All local moves are rejected without reading, writing or deleting a file.
    /// Remote-only transfers continue through the existing resource adapter.
    ///
    /// # Errors
    /// Preserves metadata/UI boundaries and typed local/remote partial effects.
    /// Cancellation is not rollback; no transfer is replayed after output fails.
    pub async fn execute(
        &mut self,
        command: &ResourceCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ResourceFileResult, ResourceFileError> {
        let (source, target) = ResourceTransferHost::resolve_paths(self.context, command)
            .map_err(ResourceFileError::Command)?;
        self.execute_resolved(command, &source, &target, cancelled)
            .await
    }

    /// Execute after the metadata owner has resolved and support-checked both paths.
    ///
    /// This entry point avoids repeated resolver diagnostics in a central host.
    /// Remote transfers consume these same objects without another lookup.
    ///
    /// # Errors
    /// Same receipt and unsupported-route rules as [`Self::execute`].
    pub async fn execute_resolved(
        &mut self,
        command: &ResourceCommand,
        source: &Arc<PathElement>,
        target: &Arc<PathElement>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ResourceFileResult, ResourceFileError> {
        let moving = match command {
            ResourceCommand::Copy { .. } => false,
            ResourceCommand::Move { .. } => true,
            _ => return Err(invalid("Expected a copy or move command")),
        };
        let local_source = matches!(source.kind(), PathKind::Local(_));
        let local_target = matches!(target.kind(), PathKind::Local(_));
        if !local_source && !local_target {
            return ResourceTransferHost {
                context: self.context,
                storage: OneLakeWriteHost {
                    client: self.storage.client,
                    executor: self.storage.executor,
                    ui: self.storage.ui,
                    read_limits: self.storage.read_limits,
                    write_limits: self.storage.write_limits,
                },
                files: None,
            }
            .execute_resolved(command, source, target, cancelled)
            .await
            .map(ResourceFileResult::Storage)
            .map_err(ResourceFileError::Storage);
        }
        if moving || local_source == local_target {
            return Err(route_error(moving, local_source == local_target));
        }
        if command.args().skill.is_some() {
            return Err(ResourceFileError::Command(MutationError::new(
                "NotSupported",
                "Local copy --skill requires shared storage request-header plumbing",
            )));
        }
        let remote = if local_source { target } else { source };
        if !matches!(remote.kind(), PathKind::OneLake(_)) {
            return Err(route_error(false, false));
        }
        let remote = ResourceTransferHost::storage_target(command, remote)
            .map_err(ResourceFileError::Storage)?;
        let selected = if local_source { source } else { target };
        let path = selected
            .local_metadata()
            .ok_or_else(|| invalid("Missing caller-classified local path"))?;
        let files = self.files.ok_or_else(|| {
            ResourceFileError::Command(MutationError::new(
                "NotSupported",
                "Local copy requires an explicit bounded filesystem capability",
            ))
        })?;
        let mut host = StorageFilesHost {
            client: self.storage.client,
            executor: self.storage.executor,
            local: files,
            ui: self.storage.ui,
            read_limits: self.storage.read_limits,
        };
        let mut cancelled = pin!(cancelled);
        let interrupted = async {
            cancelled.as_mut().await;
            Interruption::Cancelled
        };
        let mut result = if local_source {
            host.upload(
                Path::new(&path.path),
                &remote,
                self.storage.write_limits,
                interrupted,
            )
            .await
            .map(ResourceFileResult::Upload)
        } else {
            host.download(&remote, Path::new(&path.path), interrupted)
                .await
                .map(ResourceFileResult::Download)
        }
        .map_err(ResourceFileError::Copy)?;
        let outputs = match &mut result {
            ResourceFileResult::Upload(result) => &mut result.outputs,
            ResourceFileResult::Download(result) => &mut result.outputs,
            ResourceFileResult::Storage(result) => &mut result.outputs,
        };
        for output in outputs {
            output.command = Some(command.name().into());
        }
        Ok(result)
    }
}

fn invalid(message: &str) -> ResourceFileError {
    ResourceFileError::Command(MutationError::new("InvalidInput", message))
}

fn route_error(moving: bool, both_local: bool) -> ResourceFileError {
    ResourceFileError::Command(MutationError::new(
        if both_local {
            "NotSupported"
        } else {
            "InvalidInput"
        },
        if moving && both_local {
            "mv is not supported for the specified source and destination items types. Check your paths"
        } else {
            "Source and destination must be of the same type. Check your paths"
        },
    ))
}
