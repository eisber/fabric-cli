// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    CredentialSelection, JobError, JobFuture, JobOperationReconciler, JobResponse, TokenCredential,
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::CompletionPolicy;
use std::{fmt, future::pending, sync::Arc, time::Duration};

/// Concrete job cancellation bridge to the single shared Fabric LRO engine.
///
/// Accepts an already-received response, never submits or replays a mutation.
/// The shared session owns polling, result retrieval, request/delay/byte bounds,
/// credential acquisition and reconciliation evidence. No runtime or HTTP stack
/// is created here.
pub struct SharedJobOperations {
    credential: Arc<dyn TokenCredential>,
    selection: CredentialSelection,
    executor: Arc<dyn OperationExecutor>,
    limits: OperationLimits,
}

impl SharedJobOperations {
    /// Store explicit caller-owned dependencies without acquiring credentials or I/O.
    pub fn new(
        credential: Arc<dyn TokenCredential>,
        selection: CredentialSelection,
        executor: Arc<dyn OperationExecutor>,
        limits: OperationLimits,
    ) -> Self {
        Self {
            credential,
            selection,
            executor,
            limits,
        }
    }
}

impl fmt::Debug for SharedJobOperations {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedJobOperations")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl JobOperationReconciler for SharedJobOperations {
    fn reconcile<'a>(
        &'a self,
        accepted: &'a JobResponse,
    ) -> JobFuture<'a, Result<JobResponse, JobError>> {
        self.reconcile_with_cancellation(accepted, Box::pin(pending()))
    }

    fn reconcile_with_cancellation<'a>(
        &'a self,
        accepted: &'a JobResponse,
        cancelled: JobFuture<'a, ()>,
    ) -> JobFuture<'a, Result<JobResponse, JobError>> {
        Box::pin(async move {
            let mut session = OperationSession::new(
                self.credential.as_ref(),
                &self.selection,
                BorrowedExecutor(self.executor.as_ref()),
                self.limits,
                cancelled,
            );
            let response = session
                .reconcile_accepted(
                    OperationResponse {
                        status: accepted.status,
                        body: accepted.body.clone(),
                        headers: accepted.headers.clone(),
                    },
                    CompletionPolicy::PythonSynchronous,
                )
                .await
                .map_err(JobError::Operation)?;
            Ok(JobResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
        })
    }
}

struct BorrowedExecutor<'a>(&'a dyn OperationExecutor);

impl OperationExecutor for BorrowedExecutor<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> JobFuture<'a, Result<OperationResponse, OperationExecutionError>> {
        self.0.execute(request)
    }
    fn wait(&self, delay: Duration) -> JobFuture<'_, Result<(), OperationExecutionError>> {
        self.0.wait(delay)
    }
}
