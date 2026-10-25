// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Capacity adapter over the existing private HTTP execution policy.

use crate::{ExecutionFuture, HttpExecutor, workspace_error};
use azure_core::http::Method;
use fabric_sdk::capacities::{
    CapacityExecutionError, CapacityExecutor, CapacityRequest, CapacityResponse,
};

impl CapacityExecutor for HttpExecutor {
    fn execute<'a>(
        &'a self,
        request: CapacityRequest<'a>,
    ) -> ExecutionFuture<'a, Result<CapacityResponse, CapacityExecutionError>> {
        Box::pin(async move {
            self.execute(
                Method::Get,
                request.url(),
                request.token(),
                &[],
                request.max_response_bytes(),
            )
            .await
            .map_err(workspace_error)
        })
    }
}
