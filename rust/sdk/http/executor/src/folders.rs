// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Folder reads through the existing private shared HTTP execution engine.

use crate::{ExecutionError, ExecutionFuture, HttpExecutor};
use azure_core::http::Method;
use fabric_sdk_folders::{FolderExecutionError, FolderExecutor, FolderRequest, FolderResponse};

impl FolderExecutor for HttpExecutor {
    fn execute<'a>(
        &'a self,
        request: FolderRequest<'a>,
    ) -> ExecutionFuture<'a, Result<FolderResponse, FolderExecutionError>> {
        Box::pin(async move {
            self.execute(
                Method::Get,
                request.url(),
                request.token(),
                &[],
                request.max_response_bytes(),
            )
            .await
            .map_err(|error| match error {
                ExecutionError::TimedOut => FolderExecutionError::TimedOut,
                ExecutionError::ResponseLimit => FolderExecutionError::ResponseLimit,
                ExecutionError::RetriesExhausted => FolderExecutionError::RetriesExhausted,
                ExecutionError::InvalidInput
                | ExecutionError::Transport
                | ExecutionError::RetryDelay => FolderExecutionError::Failed,
            })
            .map(|response| FolderResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
        })
    }
}
