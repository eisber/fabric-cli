// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    CredentialSelection, JobError, JobExecutionError, JobExecutor, JobFuture,
    JobOperationReconciler, JobRequest, JobRequestHeaders, JobResponse, TokenCredential,
};
use azure_core::http::Method;
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
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
    job_executor: Option<Arc<dyn JobExecutor>>,
    headers: JobRequestHeaders,
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
            job_executor: None,
            headers: JobRequestHeaders::default(),
        }
    }

    /// Inspect the explicit context used for operation follow-ups.
    pub fn request_headers(&self) -> &JobRequestHeaders {
        &self.headers
    }

    /// Compose the same job executor for shared LRO follow-ups and scoped headers.
    ///
    /// This adapts already-validated shared requests, not another LRO/HTTP policy.
    /// The existing [`Self::new`] remains unchanged for generic operation executors.
    pub fn from_job_executor(
        credential: Arc<dyn TokenCredential>,
        selection: CredentialSelection,
        executor: Arc<dyn JobExecutor>,
        limits: OperationLimits,
    ) -> Self {
        Self {
            credential,
            selection,
            executor: Arc::new(JobOperationsExecutor {
                executor: executor.clone(),
                headers: JobRequestHeaders::default(),
            }),
            limits,
            job_executor: Some(executor),
            headers: JobRequestHeaders::default(),
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
    fn request_headers(&self) -> JobRequestHeaders {
        self.headers.clone()
    }
    fn with_request_headers(
        &self,
        headers: &JobRequestHeaders,
    ) -> Result<Arc<dyn JobOperationReconciler>, JobError> {
        if headers.is_empty() && self.headers.is_empty() {
            return Ok(Arc::new(Self {
                credential: self.credential.clone(),
                selection: self.selection.clone(),
                executor: self.executor.clone(),
                limits: self.limits,
                job_executor: self.job_executor.clone(),
                headers: headers.clone(),
            }));
        }
        let executor = self
            .job_executor
            .as_ref()
            .ok_or(JobError::RequestHeadersUnsupported)?;
        if !executor.supports_request_headers(headers) {
            return Err(JobError::RequestHeadersUnsupported);
        }
        Ok(Arc::new(Self {
            credential: self.credential.clone(),
            selection: self.selection.clone(),
            executor: Arc::new(JobOperationsExecutor {
                executor: executor.clone(),
                headers: headers.clone(),
            }),
            limits: self.limits,
            job_executor: Some(executor.clone()),
            headers: headers.clone(),
        }))
    }
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

struct JobOperationsExecutor {
    executor: Arc<dyn JobExecutor>,
    headers: JobRequestHeaders,
}

impl OperationExecutor for JobOperationsExecutor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> JobFuture<'a, Result<OperationResponse, OperationExecutionError>> {
        Box::pin(async move {
            let method = match request.method() {
                ResourceMethod::Get => Method::Get,
                ResourceMethod::Post => Method::Post,
                ResourceMethod::Patch => Method::Patch,
                ResourceMethod::Delete => Method::Delete,
            };
            self.executor
                .execute(JobRequest {
                    method,
                    url: request.url().clone(),
                    token: request.token(),
                    body: request.body().to_vec(),
                    max_response_bytes: request.max_response_bytes(),
                    headers: self.headers.clone(),
                })
                .await
                .map(|response| OperationResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                })
                .map_err(operation_execution_error)
        })
    }

    fn wait(&self, delay: Duration) -> JobFuture<'_, Result<(), OperationExecutionError>> {
        Box::pin(async move {
            self.executor
                .wait(delay)
                .await
                .map_err(operation_execution_error)
        })
    }
}

fn operation_execution_error(error: JobExecutionError) -> OperationExecutionError {
    match error {
        JobExecutionError::Failed => OperationExecutionError::Failed,
        JobExecutionError::TimedOut => OperationExecutionError::TimedOut,
        JobExecutionError::Cancelled => OperationExecutionError::Cancelled,
        JobExecutionError::ResponseLimit => OperationExecutionError::ResponseLimit,
        JobExecutionError::RetriesExhausted => OperationExecutionError::RetriesExhausted,
    }
}
