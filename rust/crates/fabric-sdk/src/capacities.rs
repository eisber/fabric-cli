// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Capacity imports and focused SDK delegation.

use crate::Client;
pub use fabric_sdk_capacities::{
    Capacity, CapacityClient, CapacityExecutionError, CapacityExecutor, CapacityList,
    CapacityListError, CapacityListErrorKind, CapacityListLimits, CapacityRequest,
    CapacityResponse,
};
use std::future::Future;

impl Client {
    /// Lists capacities with the same credential, selection, bounds, and cancellation.
    ///
    /// # Errors
    /// Preserves the focused SDK's typed failures and explicit partial progress.
    pub async fn list_capacities(
        &self,
        executor: &dyn CapacityExecutor,
        limits: CapacityListLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<CapacityList, CapacityListError> {
        CapacityClient::new(self.credential().clone(), self.selection().clone())
            .list_capacities(executor, limits, cancelled)
            .await
    }
}
