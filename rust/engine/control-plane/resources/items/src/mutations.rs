// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Generic item mutation routes over the caller-owned shared operation policy.

use crate::ItemCollection;
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::Value;

/// Generic and source-mapped item mutation operations.
pub struct ItemMutations;

impl ItemMutations {
    /// Read mirroring status using the source's POST action, not an invented GET.
    ///
    /// # Errors
    /// Rejects unsafe IDs and preserves typed execution/service failures.
    pub async fn mirroring_status(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        item: &str,
    ) -> Result<ResourceOutcome, ResourceError> {
        Self::mirroring_action(executor, workspace, item, "getMirroringStatus").await
    }

    /// Start mirroring after the CLI has applied its state/prompt policy.
    ///
    /// # Errors
    /// Rejects unsafe IDs and preserves operation failure/uncertainty.
    pub async fn start_mirroring(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        item: &str,
    ) -> Result<ResourceOutcome, ResourceError> {
        Self::mirroring_action(executor, workspace, item, "startMirroring").await
    }

    /// Stop mirroring after the CLI has applied its state/prompt policy.
    ///
    /// # Errors
    /// Rejects unsafe IDs and preserves operation failure/uncertainty.
    pub async fn stop_mirroring(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        item: &str,
    ) -> Result<ResourceOutcome, ResourceError> {
        Self::mirroring_action(executor, workspace, item, "stopMirroring").await
    }

    /// Create through the type-specific collection with Python's synchronous policy.
    ///
    /// # Errors
    /// Rejects unsafe IDs/payloads and preserves incomplete or failed operations.
    pub async fn create(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        collection: ItemCollection,
        payload: Value,
    ) -> Result<ResourceOutcome, ResourceError> {
        Self::create_with_completion(
            executor,
            workspace,
            collection,
            payload,
            CompletionPolicy::PythonSynchronous,
        )
        .await
    }

    /// Create through the same collection using explicit caller observation intent.
    ///
    /// # Errors
    /// Rejects unsafe IDs/payloads and preserves acceptance, failure and uncertainty.
    pub async fn create_with_completion(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        collection: ItemCollection,
        payload: Value,
        completion: CompletionPolicy,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Post,
                &["workspaces", workspace, collection.as_str()],
                Some(payload),
                completion,
            )?)
            .await
    }

    async fn mirroring_action(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        item: &str,
        action: &str,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Post,
                &["workspaces", workspace, "mirroredDatabases", item, action],
                None,
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }

    /// Patch type-specific item metadata, not its separately owned definition.
    ///
    /// # Errors
    /// Rejects unsafe IDs/payloads and preserves service or policy failures.
    pub async fn update(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        collection: ItemCollection,
        item: &str,
        payload: Value,
    ) -> Result<ResourceOutcome, ResourceError> {
        Self::update_with_completion(
            executor,
            workspace,
            collection,
            item,
            payload,
            CompletionPolicy::PythonSynchronous,
        )
        .await
    }

    /// Patch metadata with explicit observation intent, never a source replacement.
    ///
    /// # Errors
    /// Retains validation, provider failure and accepted-operation evidence.
    pub async fn update_with_completion(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        collection: ItemCollection,
        item: &str,
        payload: Value,
        completion: CompletionPolicy,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Patch,
                &["workspaces", workspace, collection.as_str(), item],
                Some(payload),
                completion,
            )?)
            .await
    }

    /// Delete via the generic items route, as Python does even for typed items.
    ///
    /// # Errors
    /// Rejects unsafe IDs and preserves uncertain mutation evidence.
    pub async fn delete(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        item: &str,
        hard: bool,
    ) -> Result<ResourceOutcome, ResourceError> {
        Self::delete_with_completion(
            executor,
            workspace,
            item,
            hard,
            CompletionPolicy::PythonSynchronous,
        )
        .await
    }

    /// Delete through the same generic route with explicit observation intent.
    ///
    /// Hard-delete intent is unchanged; no missing soft-delete feature triggers a
    /// permanent fallback or a second dispatch.
    ///
    /// # Errors
    /// Preserves validation, native rejection and uncertain mutation evidence.
    pub async fn delete_with_completion(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        item: &str,
        hard: bool,
        completion: CompletionPolicy,
    ) -> Result<ResourceOutcome, ResourceError> {
        let operation = ResourceOperation::new(
            ResourceMethod::Delete,
            &["workspaces", workspace, "items", item],
            None,
            completion,
        )?;
        executor
            .execute(if hard {
                operation.with_hard_delete()?
            } else {
                operation
            })
            .await
    }
}
