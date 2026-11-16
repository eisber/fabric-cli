// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_sdk_http::{ExecutionError, HttpExecutor, PublicOrigin, ResponseProgress};
use fabric_sdk_jobs::{
    EndpointBinding, JobExecutionError, JobExecutor, JobFuture, JobRequest, JobRequestHeaders,
    JobResponse,
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationRequest, OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use std::{sync::Arc, time::Duration};

/// Thin host adapter over the existing shared HTTP engine and caller-owned timer.
///
/// This is CLI composition, not a second retry, paging, deadline or transport stack.
/// Direct SDK consumers inject [`JobExecutor`] without depending on the CLI.
pub struct HttpJobExecutor {
    http: HttpExecutor,
}

impl HttpJobExecutor {
    /// Store caller-owned execution/time without I/O or runtime creation.
    pub fn new(http: HttpExecutor) -> Self {
        Self { http }
    }

    /// Inspect the immutable binding selected on the caller's shared executor.
    ///
    /// Job requests require its Fabric service identity, even when a different
    /// service's hostname would otherwise match a public-origin fallback.
    pub fn endpoint(&self) -> &EndpointBinding {
        self.http.endpoint()
    }
}

impl JobExecutor for HttpJobExecutor {
    fn with_run_budget(
        &self,
        remaining: Duration,
    ) -> Result<Arc<dyn JobExecutor>, JobExecutionError> {
        self.http
            .with_timeout(remaining)
            .map(|http| Arc::new(Self::new(http)) as Arc<dyn JobExecutor>)
            .map_err(map_error)
    }

    fn execute_run<'a>(
        &'a self,
        request: JobRequest<'a>,
        recorder: &'a fabric_sdk_jobs::run::RunResponseRecorder,
    ) -> JobFuture<'a, fabric_sdk_jobs::run::RunExchange> {
        Box::pin(async move {
            use fabric_sdk_jobs::run::RunExchange;
            if self.http.endpoint().origin() != PublicOrigin::Fabric {
                return RunExchange::Failed {
                    error: JobExecutionError::Failed,
                    observed: None,
                };
            }
            match self
                .http
                .execute_bound_with_progress(
                    PublicOrigin::Fabric,
                    request.method(),
                    request.url(),
                    request.token(),
                    request.body(),
                    request.headers().as_slice(),
                    request.max_response_bytes(),
                    Some(&RunProgress(recorder)),
                )
                .await
            {
                Ok(response) => RunExchange::Complete(response),
                Err(error) => RunExchange::Failed {
                    error: map_error(error),
                    observed: None,
                },
            }
        })
    }

    fn supports_request_headers(&self, _: &JobRequestHeaders) -> bool {
        true
    }
    fn execute<'a>(
        &'a self,
        request: JobRequest<'a>,
    ) -> JobFuture<'a, Result<JobResponse, JobExecutionError>> {
        Box::pin(async move {
            if self.http.endpoint().origin() != PublicOrigin::Fabric {
                return Err(JobExecutionError::Failed);
            }
            if request.headers().is_empty() {
                return self
                    .http
                    .execute_fabric(
                        request.method(),
                        request.url(),
                        request.token(),
                        request.body(),
                        request.max_response_bytes(),
                    )
                    .await
                    .map_err(map_error);
            }
            self.http
                .execute_bound(
                    PublicOrigin::Fabric,
                    request.method(),
                    request.url(),
                    request.token(),
                    request.body(),
                    request.headers().as_slice(),
                    request.max_response_bytes(),
                )
                .await
                .map_err(map_error)
        })
    }

    fn wait(&self, delay: Duration) -> JobFuture<'_, Result<(), JobExecutionError>> {
        Box::pin(async move { self.http.wait_for(delay).await.map_err(map_error) })
    }
}

struct RunProgress<'a>(&'a fabric_sdk_jobs::run::RunResponseRecorder);

impl ResponseProgress for RunProgress<'_> {
    fn record(&self, response: &JobResponse, complete: bool) -> Result<(), ExecutionError> {
        self.0
            .record(response, complete)
            .map_err(|error| match error {
                fabric_sdk_jobs::run::RunError::ResponseLimit => ExecutionError::ResponseLimit,
                _ => ExecutionError::Transport,
            })
    }

    fn body_omitted(&self, response: &JobResponse) -> Result<(), ExecutionError> {
        self.0
            .record_omitted(response)
            .map_err(|_| ExecutionError::Transport)
    }
}

impl OperationExecutor for HttpJobExecutor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> JobFuture<'a, Result<OperationResponse, OperationExecutionError>> {
        Box::pin(async move {
            let method = match request.method() {
                ResourceMethod::Get => azure_core::http::Method::Get,
                ResourceMethod::Post => azure_core::http::Method::Post,
                ResourceMethod::Patch => azure_core::http::Method::Patch,
                ResourceMethod::Delete => azure_core::http::Method::Delete,
            };
            self.http
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
                .map_err(operation_error)
        })
    }
    fn wait(&self, delay: Duration) -> JobFuture<'_, Result<(), OperationExecutionError>> {
        Box::pin(async move { self.http.wait_for(delay).await.map_err(operation_error) })
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

fn map_error(error: ExecutionError) -> JobExecutionError {
    match error {
        ExecutionError::TimedOut => JobExecutionError::TimedOut,
        ExecutionError::ResponseLimit => JobExecutionError::ResponseLimit,
        ExecutionError::RetriesExhausted => JobExecutionError::RetriesExhausted,
        ExecutionError::InvalidInput | ExecutionError::Transport | ExecutionError::RetryDelay => {
            JobExecutionError::Failed
        }
    }
}
