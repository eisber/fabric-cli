// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::CommandFuture;
use azure_core::{credentials::TokenCredential, http::Method};
use fabric_sdk_http::{ExecutionError, HttpExecutor};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse, OperationSession, OperationWarning,
};
use fabric_sdk_workspaces::{
    CredentialSelection,
    mutations::{
        ResourceError, ResourceMethod, ResourceOperation, ResourceOperations, ResourceOutcome,
    },
};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

struct SharedHttp<'a>(&'a HttpExecutor);

impl OperationExecutor for SharedHttp<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> CommandFuture<'a, Result<OperationResponse, OperationExecutionError>> {
        Box::pin(async move {
            let method = match request.method() {
                ResourceMethod::Get => Method::Get,
                ResourceMethod::Post => Method::Post,
                ResourceMethod::Patch => Method::Patch,
                ResourceMethod::Delete => Method::Delete,
            };
            self.0
                .execute_fabric(
                    method,
                    request.url(),
                    request.token(),
                    request.body(),
                    request.max_response_bytes(),
                )
                .await
                .map(|response| OperationResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                })
                .map_err(execution_error)
        })
    }

    fn wait(&self, delay: Duration) -> CommandFuture<'_, Result<(), OperationExecutionError>> {
        Box::pin(async move { self.0.wait_for(delay).await.map_err(execution_error) })
    }
}

/// Actual CLI-to-SDK composition over the shared HTTP and operation policies.
///
/// The caller provides the credential, explicit selection, existing executor
/// and cancellation future. No identity discovery, runtime or transport is added.
pub struct FabricMutationBackend<'a> {
    credential: Arc<dyn TokenCredential>,
    selection: CredentialSelection,
    executor: &'a HttpExecutor,
    limits: OperationLimits,
    cancelled: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    warnings: Vec<OperationWarning>,
}

impl<'a> FabricMutationBackend<'a> {
    /// Store explicit caller-owned dependencies without acquiring a token or I/O.
    pub fn new(
        credential: Arc<dyn TokenCredential>,
        selection: CredentialSelection,
        executor: &'a HttpExecutor,
        limits: OperationLimits,
        cancelled: impl Future<Output = ()> + Send + 'a,
    ) -> Self {
        Self {
            credential,
            selection,
            executor,
            limits,
            cancelled: Box::pin(cancelled),
            warnings: Vec::new(),
        }
    }

    /// Source-compatible polling fallbacks, for the caller's existing debug logger.
    pub fn warnings(&self) -> &[OperationWarning] {
        &self.warnings
    }
}

impl ResourceOperations for FabricMutationBackend<'_> {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> CommandFuture<'_, Result<ResourceOutcome, ResourceError>> {
        Box::pin(async move {
            let mut session = OperationSession::new(
                self.credential.as_ref(),
                &self.selection,
                SharedHttp(self.executor),
                self.limits,
                self.cancelled.as_mut(),
            );
            let result = ResourceOperations::execute(&mut session, operation).await;
            let warnings = session.warnings().to_vec();
            drop(session);
            self.warnings.extend(warnings);
            result
        })
    }
}

fn execution_error(error: ExecutionError) -> OperationExecutionError {
    match error {
        ExecutionError::TimedOut => OperationExecutionError::TimedOut,
        ExecutionError::ResponseLimit => OperationExecutionError::ResponseLimit,
        ExecutionError::RetriesExhausted => OperationExecutionError::RetriesExhausted,
        ExecutionError::InvalidInput | ExecutionError::Transport | ExecutionError::RetryDelay => {
            OperationExecutionError::Failed
        }
    }
}
