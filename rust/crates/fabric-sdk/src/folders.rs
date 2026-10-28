// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive folder reads delegated to the focused SDK.

use crate::Client;
pub use fabric_sdk_folders::{
    Folder, FolderClient, FolderError, FolderErrorKind, FolderExecutionError, FolderExecutor,
    FolderGetOptions, FolderList, FolderListError, FolderListLimits, FolderListOptions,
    FolderQueryValue, FolderRequest, FolderResponse,
};
use std::future::Future;

impl Client {
    /// Lists folders using the existing provider and explicit selection.
    ///
    /// # Errors
    /// Preserves typed failures and fully committed partial traversal progress.
    pub async fn list_folders(
        &self,
        executor: &dyn FolderExecutor,
        workspace_id: &str,
        options: FolderListOptions,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<FolderList, FolderListError> {
        FolderClient::with_context(self.context().clone())
            .list_folders(executor, workspace_id, options, cancelled)
            .await
    }

    /// Gets a folder without reconstructing its parent path or fetching children.
    ///
    /// # Errors
    /// Preserves identity, path, execution, service, payload, cancellation and
    /// byte-bound errors from the focused SDK.
    pub async fn get_folder(
        &self,
        executor: &dyn FolderExecutor,
        workspace_id: &str,
        folder_id: &str,
        options: FolderGetOptions,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Folder, FolderError> {
        FolderClient::with_context(self.context().clone())
            .get_folder(executor, workspace_id, folder_id, options, cancelled)
            .await
    }
}
