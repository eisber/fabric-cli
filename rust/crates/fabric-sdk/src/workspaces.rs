// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Workspace service imports and delegation preserved by the SDK facade.

use crate::Client;
use fabric_sdk_workspaces::WorkspaceClient;
pub use fabric_sdk_workspaces::metadata::{
    WorkspaceEnrichmentOutcome, WorkspaceMetadata, WorkspaceMetadataError,
    WorkspaceMetadataErrorKind, WorkspaceMetadataResult, WorkspaceMetadataStep, WorkspaceOmission,
};
pub use fabric_sdk_workspaces::workspaces::{
    WorkspaceExecutionError, WorkspaceExecutor, WorkspaceList, WorkspaceListError,
    WorkspaceListErrorKind, WorkspaceListLimits, WorkspaceRequest, WorkspaceResponse,
};
use std::future::Future;

impl Client {
    /// Enrich completed primary metadata through the existing bounded read policy.
    ///
    /// No primary request or operation polling is performed here. The caller must
    /// supply validated completed metadata, not a headerless accepted response.
    ///
    /// # Errors
    /// Preserves the focused SDK's selection, identifier and failed-step evidence.
    pub async fn enrich_workspace_metadata(
        &self,
        executor: &dyn WorkspaceExecutor,
        workspace_id: &str,
        metadata: WorkspaceMetadata,
        limits: WorkspaceListLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<WorkspaceMetadataResult, WorkspaceMetadataError> {
        WorkspaceClient::with_context(self.context().clone())
            .enrich_workspace_metadata(executor, workspace_id, metadata, limits, cancelled)
            .await
    }

    /// Reads ordered workspace metadata and all three bounded optional enrichments.
    ///
    /// Uses the same credential, selection and execution policies as workspace
    /// LIST. Optional omissions are exposed as typed per-step observations.
    ///
    /// # Errors
    ///
    /// Returns primary failures or explicit identity, cancellation, execution and
    /// safety failures, including those encountered during optional enrichments.
    pub async fn get_workspace_metadata(
        &self,
        executor: &dyn WorkspaceExecutor,
        workspace_id: &str,
        limits: WorkspaceListLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<WorkspaceMetadataResult, WorkspaceMetadataError> {
        WorkspaceClient::with_context(self.context().clone())
            .get_workspace_metadata(executor, workspace_id, limits, cancelled)
            .await
    }

    /// Lists workspace pages using the focused workspace SDK implementation.
    ///
    /// Credentials, selection, limits, and cancellation are forwarded unchanged.
    /// This method creates no runtime, transport, or identity backend. Use a
    /// pending cancellation future if cancellation is driven by dropping the call.
    ///
    /// ```no_run
    /// use fabric_sdk::{
    ///     Client,
    ///     workspaces::{WorkspaceExecutor, WorkspaceListError, WorkspaceListLimits},
    /// };
    ///
    /// async fn list(
    ///     client: &Client,
    ///     executor: &dyn WorkspaceExecutor,
    /// ) -> Result<usize, WorkspaceListError> {
    ///     let workspaces = client
    ///         .list_workspaces(
    ///             executor,
    ///             WorkspaceListLimits::default(),
    ///             std::future::pending(),
    ///         )
    ///         .await?;
    ///     Ok(workspaces.value.len())
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns the workspace SDK's typed identity, execution, service, malformed
    /// payload, safety-bound, or cancellation error, preserving partial progress.
    pub async fn list_workspaces(
        &self,
        executor: &dyn WorkspaceExecutor,
        limits: WorkspaceListLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<WorkspaceList, WorkspaceListError> {
        WorkspaceClient::with_context(self.context().clone())
            .list_workspaces(executor, limits, cancelled)
            .await
    }
}
