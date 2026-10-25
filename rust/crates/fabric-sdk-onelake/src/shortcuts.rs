// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shortcut metadata uses the Fabric API/audience, not DFS storage authentication.

use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::Value;

/// Source conflict policies used by shortcut create/update handlers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutConflictPolicy {
    /// Do not overwrite an existing shortcut.
    Abort,
    /// Force-created shortcut may receive a generated unique name.
    GenerateUniqueName,
    /// Update an already-existing shortcut only.
    OverwriteOnly,
}
impl ShortcutConflictPolicy {
    /// Exact service query spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Abort => "Abort",
            Self::GenerateUniqueName => "GenerateUniqueName",
            Self::OverwriteOnly => "OverwriteOnly",
        }
    }
}

/// Fabric shortcut metadata route, with validated identifiers and display names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutPath {
    workspace: String,
    item: String,
    path: String,
    name: String,
}
impl ShortcutPath {
    /// Validate an explicit shortcut location, not an arbitrary service URL.
    ///
    /// # Errors
    /// Rejects unsafe workspace/item identifiers, empty/traversing folder/name
    /// segments, separators in the name and control characters.
    pub fn new(workspace: &str, item: &str, path: &str, name: &str) -> Result<Self, ResourceError> {
        ResourceOperation::new(
            ResourceMethod::Get,
            &["workspaces", workspace, "items", item],
            None,
            CompletionPolicy::PythonSynchronous,
        )?;
        let value = Self {
            workspace: workspace.into(),
            item: item.into(),
            path: path.into(),
            name: name.into(),
        };
        value.operation(ResourceMethod::Get)?;
        Ok(value)
    }

    fn operation(&self, method: ResourceMethod) -> Result<ResourceOperation, ResourceError> {
        let mut segments = vec![
            "workspaces",
            self.workspace.as_str(),
            "items",
            self.item.as_str(),
            "shortcuts",
        ];
        segments.extend(self.path.split('/'));
        segments.push(&self.name);
        ResourceOperation::encoded(method, &segments, None, CompletionPolicy::PythonSynchronous)
    }
}

/// Shortcut metadata endpoints over the existing shared Fabric operation policy.
///
/// This type owns no credentials/runtime. The injected policy must explicitly
/// select Fabric, even when the source/target data resides in OneLake storage.
pub struct ShortcutMutations;
impl ShortcutMutations {
    /// Create/update a shortcut with exact JSON and finite conflict policy.
    ///
    /// # Errors
    /// Rejects unsafe IDs/non-object payloads and propagates service/operation
    /// failures with original reconciliation evidence and no write replay.
    pub async fn create(
        executor: &mut dyn ResourceOperations,
        workspace: &str,
        item: &str,
        payload: Value,
        policy: ShortcutConflictPolicy,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(
                ResourceOperation::new(
                    ResourceMethod::Post,
                    &["workspaces", workspace, "items", item, "shortcuts"],
                    Some(payload),
                    CompletionPolicy::PythonSynchronous,
                )?
                .with_query(&[("shortcutConflictPolicy", policy.as_str())])?,
            )
            .await
    }

    /// Read actual shortcut metadata through its source Fabric endpoint.
    ///
    /// # Errors
    /// Returns typed identity, service, cancellation and response failures.
    pub async fn get(
        executor: &mut dyn ResourceOperations,
        path: &ShortcutPath,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor.execute(path.operation(ResourceMethod::Get)?).await
    }

    /// Delete shortcut metadata, never recursively delete its storage target.
    ///
    /// # Errors
    /// Retains service acceptance/completion/uncertainty without replay.
    pub async fn delete(
        executor: &mut dyn ResourceOperations,
        path: &ShortcutPath,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor
            .execute(path.operation(ResourceMethod::Delete)?)
            .await
    }
}
