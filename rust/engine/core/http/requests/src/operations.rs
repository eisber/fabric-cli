// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{ApiClient, ApiError, ExecutionError, Executor, Origin, Request, Response, header};
use azure_core::http::Method;
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use std::{
    future::Future,
    num::NonZeroUsize,
    pin::{Pin, pin},
};

struct Adapter<'a> {
    executor: &'a dyn Executor,
    trace: Option<&'a crate::QueryDispatchTrace>,
}

fn execution(error: ExecutionError) -> OperationExecutionError {
    match error {
        ExecutionError::Failed => OperationExecutionError::Failed,
        ExecutionError::TimedOut => OperationExecutionError::TimedOut,
        ExecutionError::Cancelled => OperationExecutionError::Cancelled,
        ExecutionError::RetriesExhausted => OperationExecutionError::RetriesExhausted,
        ExecutionError::ResponseLimit => OperationExecutionError::ResponseLimit,
    }
}

impl OperationExecutor for Adapter<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            let specification = Request {
                method: match request.method() {
                    ResourceMethod::Get => Method::Get,
                    ResourceMethod::Post => Method::Post,
                    ResourceMethod::Patch => Method::Patch,
                    ResourceMethod::Delete => Method::Delete,
                },
                url: request.url().clone(),
                body: request.body().to_vec(),
                origin: match request.origin() {
                    fabric_sdk_operations::ServiceOrigin::Fabric => Origin::Fabric,
                    fabric_sdk_operations::ServiceOrigin::Arm => Origin::Arm,
                    _ => return Err(OperationExecutionError::Failed),
                },
                headers: Vec::new(),
            };
            if !self.executor.supports(&specification) {
                return Err(OperationExecutionError::Failed);
            }
            if let Some(trace) = self.trace {
                trace.record(&specification);
            }
            let response = self
                .executor
                .execute(
                    &specification,
                    request.token(),
                    request.max_response_bytes(),
                )
                .await
                .map_err(execution)?;
            Ok(OperationResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
        })
    }

    fn wait(
        &self,
        delay: std::time::Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(async move { self.executor.wait(delay).await.map_err(execution) })
    }
}

impl ApiClient {
    pub(crate) async fn send_arm_get(
        &self,
        request: &Request,
        limit: NonZeroUsize,
        mut limits: OperationLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Response, ApiError> {
        if request.origin() != Origin::Arm
            || request.method() != Method::Get
            || !request.body().is_empty()
            || !request.headers().is_empty()
        {
            return Err(ApiError::InvalidInput);
        }
        limits.max_response_bytes = limits.max_response_bytes.min(limit);
        let path = format!(
            "{}{}",
            request.url().path(),
            request
                .url()
                .query()
                .map(|query| format!("?{query}"))
                .unwrap_or_default()
        );
        let operation = fabric_sdk_operations::ArmGetOperation::new(&path)
            .map_err(|_| ApiError::InvalidInput)?;
        let mut session = OperationSession::for_arm(
            self.credential.as_ref(),
            &self.selection,
            Adapter {
                executor: self.executor.as_ref(),
                trace: self.query_trace.as_ref(),
            },
            limits,
            cancelled,
        );
        let response = session
            .execute_arm_get(operation)
            .await
            .map_err(|error| ApiError::Operation(Box::new(error)))?;
        Ok(Response {
            status: response.status,
            body: response.body,
            headers: response.headers,
        })
    }

    /// Executes one Fabric mutation then applies the released Python synchronous
    /// operation policy to its actual accepted response. The initial write is
    /// never replayed; operation IDs and uncertain side effects survive errors.
    /// Raw `api` callers use `send` instead and never implicitly poll.
    pub async fn send_completed(
        &self,
        request: &Request,
        limit: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Response, ApiError> {
        if request.origin() != Origin::Fabric {
            return Err(ApiError::InvalidAudience);
        }
        let mut cancelled = pin!(cancelled);
        let mut dispatched = false;
        let response = self
            .send_observed(request, limit, cancelled.as_mut(), &mut dispatched)
            .await
            .map_err(|error| {
                if dispatched
                    && matches!(
                        error,
                        ApiError::Execution(_) | ApiError::Cancelled | ApiError::Limit
                    )
                {
                    ApiError::MutationUncertain(Box::new(error))
                } else {
                    error
                }
            })?;
        if response.status != 202
            || header(&response, "location").is_some_and(|s| s.contains("/jobs/instances/"))
        {
            return Ok(response);
        }
        let limits = OperationLimits {
            max_response_bytes: limit,
            ..OperationLimits::default()
        };
        let mut session = OperationSession::new(
            self.credential.as_ref(),
            &self.selection,
            Adapter {
                executor: self.executor.as_ref(),
                trace: self.query_trace.as_ref(),
            },
            limits,
            cancelled.as_mut(),
        );
        let response = session
            .reconcile_accepted(
                OperationResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                },
                CompletionPolicy::PythonSynchronous,
            )
            .await
            .map_err(|error| ApiError::Operation(Box::new(error)))?;
        Ok(Response {
            status: response.status,
            body: response.body,
            headers: response.headers,
        })
    }
}
