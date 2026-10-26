// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{ResourceCommand, ResourceContext, gap, parser, shortcut_destination};
use crate::{
    CommandFuture, MutationError, NoticeKind, OneLakeResourceKind, OneLakeWriteError,
    OneLakeWriteHost, OneLakeWriteResult, OneLakeWriteTarget, file_destination,
    validate_copy_destination,
};
use fabric_cli_context::{LocalKind, LocalMetadata, OneLakeKind, PathElement, PathKind, ResolveOptions};
use fabric_sdk_onelake::OneLakePath;
use std::{future::Future, sync::Arc};

/// Bounded local-file input supplied by the existing filesystem host.
pub trait ResourceFiles: Send {
    /// Read the caller-classified file without ambient path or credential discovery.
    ///
    /// The host must enforce `max_bytes` while reading, not only after allocation.
    /// It returns its structured read error rather than an empty success.
    fn read<'a>(
        &'a mut self,
        file: &'a LocalMetadata,
        max_bytes: usize,
    ) -> CommandFuture<'a, Result<Vec<u8>, MutationError>>;
}

/// Runtime-neutral `cp`/`mv` adapter over the released OneLake write handlers.
///
/// The separate result preserves create/append/flush and partial-error evidence.
/// The main host selects this for storage transfers instead of flattening them
/// into Fabric resource outcomes or installing another storage HTTP engine.
pub struct ResourceTransferHost<'a> {
    /// The same source-compatible command-local context/cache boundary.
    pub context: &'a mut dyn ResourceContext,
    /// Existing client, shared storage executor, UI and read/write limits.
    pub storage: OneLakeWriteHost<'a>,
    /// Explicit local file reader; absence rejects only local-source copies.
    pub files: Option<&'a mut dyn ResourceFiles>,
}

impl ResourceTransferHost<'_> {
    /// Execute a OneLake copy/move or local-file upload using the existing helpers.
    ///
    /// Source and destination resolution, local classification, progress and
    /// read ordering are part of this adapter. Transport, upload stages,
    /// cancellation and mutation uncertainty remain in the existing SDK.
    ///
    /// # Errors
    /// Definition, recursive, collision-policy and local-destination branches
    /// fail explicitly; no accepted stage becomes a completed result or retry.
    pub async fn execute(
        &mut self,
        command: &ResourceCommand,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeWriteResult, OneLakeWriteError> {
        let (destination, moving, force, recursive, collision) = match command {
            ResourceCommand::Copy {
                destination, force, recursive, block_on_path_collision, ..
            } => (destination, false, *force, *recursive, *block_on_path_collision),
            ResourceCommand::Move { destination, force, recursive, .. } => {
                (destination, true, *force, *recursive, false)
            }
            _ => return Err(local(parser::invalid("Expected a copy or move command"))),
        };
        if command.args().skill.is_some() {
            return Err(local(gap(command, "--skill storage request headers")));
        }
        if recursive || collision {
            return Err(local(gap(command, "recursive/collision-policy transfer options")));
        }
        let source = self.context.resolve(
            &command.args().path,
            ResolveOptions {
                raise_error: true,
                supports_local_path: !moving,
            },
        ).map_err(local)?;
        let target = self.context.resolve(
            destination,
            ResolveOptions {
                raise_error: false,
                supports_local_path: !moving,
            },
        ).map_err(local)?;
        if !matches!(target.kind(), PathKind::OneLake(_)) {
            return Err(local(gap(command, "Fabric definition or local-destination transfer")));
        }
        let target = storage_target(command, &target)?;
        let mut result = if moving {
            if !matches!(source.kind(), PathKind::OneLake(_)) {
                return Err(local(gap(command, "Fabric definition or local-source move")));
            }
            self.storage.rename(
                &storage_target(command, &source)?, &target, force, cancelled,
            ).await?
        } else if matches!(source.kind(), PathKind::OneLake(_)) {
            self.storage.copy_file(
                &storage_target(command, &source)?, &target, cancelled,
            ).await?
        } else {
            validate_copy_destination(&target)?;
            match source.kind() {
                PathKind::Local(LocalKind::Directory) => {
                    return Err(local(MutationError::new("NotSupported", "Recursive copy not supported")));
                }
                PathKind::Local(LocalKind::File) => {}
                PathKind::Local(_) => {
                    return Err(local(MutationError::new("InvalidPath", "Invalid source, expected file")));
                }
                _ => return Err(local(gap(command, "Fabric definition copy"))),
            }
            let file = source.local_metadata()
                .ok_or_else(|| local(parser::invalid("Missing caller-classified local file")))?;
            let target = file_destination(&target, &file.name)?;
            let files = self.files.as_deref_mut()
                .ok_or_else(|| local(gap(command, "local copy requires an injected file reader")))?;
            self.storage.ui.notice(NoticeKind::Progress, &format!(
                "Copying '{}' \u{2192} '{}'...", file.path, target.display_path,
            ));
            let bytes = files.read(file, self.storage.write_limits.max_request_bytes.get()).await
                .map_err(local)?;
            self.storage.upload_bytes(&target, &file.name, &bytes, cancelled).await?
        };
        for output in &mut result.outputs {
            output.command = Some(command.name().into());
        }
        Ok(result)
    }
}

fn storage_target(
    command: &ResourceCommand,
    target: &Arc<PathElement>,
) -> Result<OneLakeWriteTarget, OneLakeWriteError> {
    let destination = shortcut_destination(command, target).map_err(local)?;
    let kind = match target.kind() {
        PathKind::OneLake(OneLakeKind::File) => OneLakeResourceKind::File,
        PathKind::OneLake(OneLakeKind::Folder) => OneLakeResourceKind::Directory,
        PathKind::OneLake(OneLakeKind::Shortcut) => OneLakeResourceKind::Shortcut,
        PathKind::OneLake(OneLakeKind::Table) => OneLakeResourceKind::Table,
        PathKind::OneLake(OneLakeKind::Undefined) => OneLakeResourceKind::Undefined,
        _ => return Err(local(gap(command, "expected a OneLake transfer path"))),
    };
    Ok(OneLakeWriteTarget {
        path: OneLakePath::new(
            &destination.workspace, &destination.item, &destination.local_path,
        ).map_err(OneLakeWriteError::Read)?,
        item_type: destination.item_type,
        kind,
        display_path: destination.display_path,
    })
}

fn local(error: MutationError) -> OneLakeWriteError {
    OneLakeWriteError::Command(error)
}
