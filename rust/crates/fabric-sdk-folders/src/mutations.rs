// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Folder mutation routes over the caller-owned shared operation policy.

use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};

/// Folder metadata and hierarchy mutations.
pub struct FolderMutations;

impl FolderMutations {
    /// Create a folder with exact optional description/parent omission semantics.
    ///
    /// # Errors
    /// Rejects unsafe IDs/payloads and propagates operation failures.
    pub async fn create(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        payload: Value,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Post,
                &["workspaces", workspace, "folders"],
                Some(payload),
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }

    /// Patch folder metadata.
    ///
    /// # Errors
    /// Rejects unsafe IDs/payloads and propagates operation failures.
    pub async fn update(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        folder: &str,
        payload: Value,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Patch,
                &["workspaces", workspace, "folders", folder],
                Some(payload),
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }

    /// Delete a folder without recursively deleting children in the SDK.
    ///
    /// # Errors
    /// Rejects unsafe IDs and propagates incomplete/service failures.
    pub async fn delete(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        folder: &str,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Delete,
                &["workspaces", workspace, "folders", folder],
                None,
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }

    /// Move within a workspace; an omitted parent means the workspace root.
    ///
    /// # Errors
    /// Rejects unsafe route IDs and propagates operation failures.
    pub async fn move_to(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        folder: &str,
        parent: Option<&str>,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Post,
                &["workspaces", workspace, "folders", folder, "move"],
                Some(parent.map_or_else(|| json!({}), |id| json!({"targetFolderId": id}))),
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }
}
