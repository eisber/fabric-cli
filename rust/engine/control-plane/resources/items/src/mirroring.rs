// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! MirroredDatabase status and lifecycle actions, separate from generic item CRUD.

use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};

/// Read mirroring status using the source's POST action.
///
/// # Errors
/// Rejects unsafe IDs and preserves typed execution/service failures.
pub async fn status(
    executor: &mut dyn ResourceOperations,
    workspace: &str,
    item: &str,
) -> Result<ResourceOutcome, ResourceError> {
    action(executor, workspace, item, "getMirroringStatus").await
}

/// Start mirroring after the caller has applied its state/prompt policy.
///
/// # Errors
/// Rejects unsafe IDs and preserves operation failure/uncertainty.
pub async fn start(
    executor: &mut dyn ResourceOperations,
    workspace: &str,
    item: &str,
) -> Result<ResourceOutcome, ResourceError> {
    action(executor, workspace, item, "startMirroring").await
}

/// Stop mirroring after the caller has applied its state/prompt policy.
///
/// # Errors
/// Rejects unsafe IDs and preserves operation failure/uncertainty.
pub async fn stop(
    executor: &mut dyn ResourceOperations,
    workspace: &str,
    item: &str,
) -> Result<ResourceOutcome, ResourceError> {
    action(executor, workspace, item, "stopMirroring").await
}

async fn action(
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
