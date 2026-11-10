// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Workspace endpoint operations and compatibility reexports of shared contracts.

pub use fabric_sdk_resource_contracts::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};

/// Workspace endpoint operations, independent of CLI or identity implementations.
pub struct WorkspaceMutations;

impl WorkspaceMutations {
    /// Read primary workspace metadata without optional enrichment requests.
    ///
    /// # Errors
    /// Rejects unsafe IDs and preserves source-synchronous service failures.
    pub async fn metadata(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Get,
                &["workspaces", workspace],
                None,
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }

    /// Create a workspace from an exact source-shaped JSON payload.
    ///
    /// # Errors
    /// Rejects non-object payloads and propagates policy/service failures.
    pub async fn create(
        executor: &mut dyn ResourceOperations,
        payload: Value,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Post,
                &["workspaces"],
                Some(payload),
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }

    /// Patch workspace metadata or its separately routed Spark settings.
    ///
    /// # Errors
    /// Rejects unsafe IDs and propagates explicit operation failures.
    pub async fn update(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        payload: Value,
        spark_settings: bool,
    ) -> Result<ResourceOutcome, ResourceError> {
        let mut segments = vec!["workspaces", workspace];
        if spark_settings {
            segments.extend(["spark", "settings"]);
        }
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Patch,
                &segments,
                Some(payload),
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }

    /// Delete a workspace; acceptance remains distinct from completion.
    ///
    /// # Errors
    /// Rejects unsafe IDs and propagates failures without replay.
    pub async fn delete(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Delete,
                &["workspaces", workspace],
                None,
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }

    /// Assign a workspace to a capacity.
    ///
    /// # Errors
    /// Rejects unsafe workspace IDs and propagates policy/service failures.
    pub async fn assign_capacity(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        capacity: &str,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Post,
                &["workspaces", workspace, "assignToCapacity"],
                Some(json!({"capacityId": capacity})),
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }

    /// Unassign a workspace from its current capacity.
    ///
    /// # Errors
    /// Rejects unsafe workspace IDs and propagates policy/service failures.
    pub async fn unassign_capacity(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(ResourceOperation::new(
                ResourceMethod::Post,
                &["workspaces", workspace, "unassignFromCapacity"],
                None,
                CompletionPolicy::PythonSynchronous,
            )?)
            .await
    }
}
