// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::LoadCompletion;
use azure_core::http::Method;
use fabric_cli_output::CommandError;
use fabric_sdk_http::{ExecutionError, HttpExecutor};
use fabric_sdk_operations::{
    OperationError, OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse, OperationSession, OperationWarning,
};
use fabric_sdk_tables::{
    LoadOutcome, TableError, TableErrorKind, TableFuture, TableRequest, TableResponse,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use std::time::Duration;

/// Explicit observer for shared-engine warnings; never silently discarded by the adapter.
pub trait TableOperationObserver: Send + Sync {
    /// Observe source Retry-After fallback/debug information.
    ///
    /// Failure stops the operation while retaining its observed reconciliation evidence.
    fn warning(&self, warning: OperationWarning) -> Result<(), TableError>;
}

/// Reconcile an actual received load202 through the one shared Fabric operation engine.
///
/// No POST is issued here. The initial request counts against `limits`, token
/// acquisition uses the original injected provider/selection, and the existing
/// shared HTTP adapter owns retries, streaming and per-request deadlines.
pub struct FabricLoadCompletion<'a> {
    /// Caller-owned shared HTTP execution and async timer.
    pub http: &'a HttpExecutor,
    /// Finite polling/response/request bounds, including the external load POST.
    pub limits: OperationLimits,
    /// Caller-owned debug/warning presentation, with explicit error propagation.
    pub observer: &'a dyn TableOperationObserver,
}

struct SharedOperationHttp<'a>(&'a HttpExecutor);
impl OperationExecutor for SharedOperationHttp<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> TableFuture<'a, Result<OperationResponse, OperationExecutionError>> {
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
    fn wait(&self, delay: Duration) -> TableFuture<'_, Result<(), OperationExecutionError>> {
        Box::pin(async move { self.0.wait_for(delay).await.map_err(execution_error) })
    }
}

impl LoadCompletion for FabricLoadCompletion<'_> {
    fn complete<'a>(
        &'a self,
        request: TableRequest<'a>,
        response: TableResponse,
        cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>> {
        Box::pin(async move {
            let mut limits = self.limits;
            limits.max_response_bytes = limits.max_response_bytes.min(request.max_response_bytes());
            limits.max_requests = limits.max_requests.min(request.max_requests());
            let mut session = OperationSession::new(
                request.credential(),
                request.selection(),
                SharedOperationHttp(self.http),
                limits,
                cancelled,
            );
            let result = session
                .reconcile_accepted(
                    OperationResponse {
                        status: response.status,
                        body: response.body,
                        headers: response.headers,
                    },
                    CompletionPolicy::PythonSynchronous,
                )
                .await;
            for warning in session.warnings() {
                self.observer.warning(*warning).map_err(|mut error| {
                    error.operation_id = session.operation_id().map(str::to_owned);
                    error.mutation_may_have_occurred = true;
                    error
                })?;
            }
            let response = result.map_err(|error| TableError {
                operation_id: error.operation_id.clone(),
                mutation_may_have_occurred: error.mutation_may_have_occurred,
                kind: TableErrorKind::SharedOperation(Box::new(error)),
            })?;
            match response.status {
                200 | 201 => Ok(LoadOutcome::Completed),
                202 => Ok(LoadOutcome::Accepted {
                    operation_id: session.operation_id().map(str::to_owned),
                }),
                204 => Ok(LoadOutcome::NoOutput),
                _ => Err(TableError {
                    kind: TableErrorKind::InvalidResponse,
                    operation_id: session.operation_id().map(str::to_owned),
                    mutation_may_have_occurred: true,
                }),
            }
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

pub(crate) fn command_error(error: OperationError) -> CommandError {
    let output = fabric_cli_mutations::operation_error_output(&error);
    CommandError::new(output.message, output.error_code)
}
