// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{ExecutionError, ExecutionFuture, HttpExecutor, PublicOrigin};
use azure_core::http::Method;
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationRequest, OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use std::time::Duration;

impl OperationExecutor for HttpExecutor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> ExecutionFuture<'a, Result<OperationResponse, OperationExecutionError>> {
        Box::pin(async move {
            let method = match request.method() {
                ResourceMethod::Get => Method::Get,
                ResourceMethod::Post => Method::Post,
                ResourceMethod::Patch => Method::Patch,
                ResourceMethod::Delete => Method::Delete,
            };
            let response = match request.origin() {
                PublicOrigin::Fabric => {
                    self.execute_fabric(
                        method,
                        request.url(),
                        request.token(),
                        request.body(),
                        request.max_response_bytes(),
                    )
                    .await
                }
                PublicOrigin::Arm => {
                    self.execute_bound(
                        PublicOrigin::Arm,
                        method,
                        request.url(),
                        request.token(),
                        request.body(),
                        &[],
                        request.max_response_bytes(),
                    )
                    .await
                }
                _ => Err(ExecutionError::InvalidInput),
            };
            response
                .map(|response| OperationResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                })
                .map_err(operation_error)
        })
    }

    fn wait(&self, delay: Duration) -> ExecutionFuture<'_, Result<(), OperationExecutionError>> {
        Box::pin(async move { self.wait_for(delay).await.map_err(operation_error) })
    }
}

fn operation_error(error: ExecutionError) -> OperationExecutionError {
    match error {
        ExecutionError::TimedOut => OperationExecutionError::TimedOut,
        ExecutionError::ResponseLimit => OperationExecutionError::ResponseLimit,
        ExecutionError::RetriesExhausted => OperationExecutionError::RetriesExhausted,
        ExecutionError::InvalidInput | ExecutionError::Transport | ExecutionError::RetryDelay => {
            OperationExecutionError::Failed
        }
    }
}
