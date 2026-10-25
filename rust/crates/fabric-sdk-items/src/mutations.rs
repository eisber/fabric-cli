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
    /// Create through the type-specific collection and wait for actual item data.
    ///
    /// # Errors
    /// Rejects unsafe IDs/payloads and preserves incomplete or failed operations.
    pub async fn create(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        collection: ItemCollection,
        payload: Value,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Post,
                &["workspaces", workspace, collection.as_str()],
                Some(payload),
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
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Patch,
                &["workspaces", workspace, collection.as_str(), item],
                Some(payload),
                CompletionPolicy::PythonSynchronous,
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
        let operation = ResourceOperation::new(
            ResourceMethod::Delete,
            &["workspaces", workspace, "items", item],
            None,
            CompletionPolicy::PythonSynchronous,
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
