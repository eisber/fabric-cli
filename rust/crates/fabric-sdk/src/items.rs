// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive generic item APIs delegated to the focused item SDK.

use crate::Client;
pub use fabric_sdk_items::{
    Item, ItemClient, ItemError, ItemErrorKind, ItemExecutionError, ItemExecutor, ItemGetOptions,
    ItemList, ItemListError, ItemListLimits, ItemListOptions, ItemRequest, ItemResponse,
};
use std::future::Future;

impl Client {
    /// Lists workspace items using the same injected credential and selection.
    ///
    /// # Errors
    /// Preserves the focused SDK's typed failures and explicit partial progress.
    pub async fn list_items(
        &self,
        executor: &dyn ItemExecutor,
        workspace_id: &str,
        options: ItemListOptions,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ItemList, ItemListError> {
        ItemClient::new(self.credential().clone(), self.selection().clone())
            .list_items(executor, workspace_id, options, cancelled)
            .await
    }

    /// Gets one generic item without specialized collection or definition requests.
    ///
    /// # Errors
    /// Preserves the focused SDK's typed identity, execution, service, payload,
    /// path, cancellation and response-bound failures.
    pub async fn get_item(
        &self,
        executor: &dyn ItemExecutor,
        workspace_id: &str,
        item_id: &str,
        options: ItemGetOptions,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Item, ItemError> {
        ItemClient::new(self.credential().clone(), self.selection().clone())
            .get_item(executor, workspace_id, item_id, options, cancelled)
            .await
    }
}
