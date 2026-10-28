// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{CacheEffect, MutationError, MutationHost, MutationResult, message, require_id};
use fabric_cli_context::{Element, NodeKind};
use fabric_sdk_folders::mutations::FolderMutations;
use fabric_sdk_workspaces::mutations::ResourceOutcome;
use std::sync::Arc;

/// Source folder-move count alongside actual operation/output/cache evidence.
#[derive(Clone, Debug, Default)]
pub struct FolderMoveResult {
    /// Python increments this after dispatch regardless of the returned status;
    /// it is not a separate claim that an accepted operation completed.
    pub moved_count: u32,
    /// Actual operation acceptance/completion, source output and cache effect.
    pub result: MutationResult,
}

impl MutationHost<'_> {
    /// Execute the within-workspace folder branch of `mv`/`move`.
    ///
    /// No confirmation is added to this source helper. A same-parent move is a
    /// no-op; the workspace-root request omits `targetFolderId` rather than sending
    /// null. Only status 200 emits the source message and a parent cache effect.
    ///
    /// # Errors
    /// Rejects non-folder/non-parent inputs and explicitly leaves cross-workspace
    /// definition-copy orchestration to its owned interface instead of invoking
    /// another deployment implementation or deleting originals speculatively.
    pub async fn move_folder(
        &mut self,
        folder: &Arc<Element>,
        parent: &Arc<Element>,
    ) -> Result<FolderMoveResult, MutationError> {
        if folder.kind() != NodeKind::Folder
            || !matches!(parent.kind(), NodeKind::Folder | NodeKind::Workspace(_))
        {
            return Err(MutationError::new(
                "InvalidInput",
                "Folder move requires a folder and workspace/folder parent",
            ));
        }
        let workspace = folder
            .workspace()
            .ok_or_else(|| MutationError::new("InvalidContext", "Missing owning workspace"))?;
        let target_workspace = parent
            .workspace()
            .ok_or_else(|| MutationError::new("InvalidContext", "Missing destination workspace"))?;
        if workspace != target_workspace {
            return Err(MutationError::new(
                "NotSupported",
                "Cross-workspace folder moves require the definition-copy handler",
            ));
        }
        if folder.parent() == Some(parent) {
            return Ok(FolderMoveResult::default());
        }
        let outcome = FolderMutations::move_to(
            self.operations,
            require_id(&workspace)?,
            require_id(folder)?,
            if parent.kind() == NodeKind::Folder {
                Some(require_id(parent)?)
            } else {
                None
            },
        )
        .await?;
        let mut result = MutationResult {
            outcome: Some(outcome.clone()),
            ..MutationResult::default()
        };
        if matches!(outcome, ResourceOutcome::Completed { status: 200, .. }) {
            result.outputs.push(message(format!(
                "Move Completed for Folder '{}' to '{}'",
                folder.name(),
                parent.name()
            )));
            result.effects.push(CacheEffect::MovedFolder {
                target: Arc::clone(folder),
                parent: Arc::clone(parent),
            });
        }
        Ok(FolderMoveResult {
            moved_count: 1,
            result,
        })
    }
}
