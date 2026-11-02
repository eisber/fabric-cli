// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use azure_core::http::Method;
use fabric_sdk_http::{ExecutionError, HttpExecutor};
use fabric_sdk_tables::{
    LoadOutcome, TableError, TableErrorKind, TableExecutor, TableFuture, TableRequest,
    TableResponse,
};
use std::{
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
};

/// OneLake's separately owned read/properties adapter, not a second filesystem SDK.
pub trait StorageReads: Send + Sync {
    /// Execute a validated OneLake GET/HEAD with the supplied storage token.
    ///
    /// `resource=filesystem` requires generic bounded continuation accumulation.
    /// A raw first listing page is not a complete response for schema extraction.
    fn read<'a>(
        &'a self,
        request: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>>;
}

/// Shared Fabric operation lifecycle adapter for header-bearing 202 responses.
///
/// The callback must preserve the accepted ID on cancellation/error, acquire
/// fresh Fabric tokens from the request's injected provider, and use the shared
/// bounded execution/timer. No default implementation fabricates completion.
pub trait LoadCompletion: Send + Sync {
    /// Reconcile the accepted response using the existing shared LRO engine.
    fn complete<'a>(
        &'a self,
        request: TableRequest<'a>,
        response: TableResponse,
        cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>>;
}

/// Actual TABLE-to-shared-HTTP composition without an SDK-to-facade cycle.
///
/// Storage reads and generic Fabric LRO remain separately injected owners.
/// Immediate 200/201 and header-less 202 need no operation callback.
pub struct SharedTableExecutor<'a> {
    /// Existing, caller-configured HTTP executor; no transport is constructed.
    pub http: &'a HttpExecutor,
    /// Existing OneLake read/properties adapter.
    pub storage: &'a dyn StorageReads,
    /// Existing generic Fabric operation adapter.
    pub operations: &'a dyn LoadCompletion,
}

impl TableExecutor for SharedTableExecutor<'_> {
    fn read<'a>(
        &'a self,
        request: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        self.storage.read(request)
    }

    fn load<'a>(
        &'a self,
        request: TableRequest<'a>,
        mut cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>> {
        Box::pin(async move {
            if poll_fn(|cx| Poll::Ready(cancelled.as_mut().poll(cx).is_ready())).await {
                return Err(TableError::new(TableErrorKind::Cancelled));
            }
            let response = {
                let future = self.http.execute_fabric(
                    Method::Post,
                    request.url(),
                    request.token(),
                    request.body(),
                    request.max_response_bytes(),
                );
                let mut future = pin!(future);
                poll_fn(|cx| {
                    if cancelled.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(Err(TableError::new(TableErrorKind::Cancelled)));
                    }
                    future
                        .as_mut()
                        .poll(cx)
                        .map(|result| result.map_err(execution_error))
                })
                .await
                .map_err(uncertain)?
            };
            let response = TableResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            };
            match response.status {
                200 | 201 => Ok(LoadOutcome::Completed),
                204 => Ok(LoadOutcome::NoOutput),
                202 => {
                    if response.headers.iter().any(|(key, value)| {
                        key.eq_ignore_ascii_case("Location") && value.contains("/jobs/instances/")
                    }) {
                        return Ok(LoadOutcome::Accepted { operation_id: None });
                    }
                    if response.headers.iter().any(|(key, value)| {
                        !value.is_empty()
                            && (key.eq_ignore_ascii_case("Location")
                                || key.eq_ignore_ascii_case("x-ms-operation-id"))
                    }) {
                        let operation_id = response
                            .headers
                            .iter()
                            .find(|(key, _)| key.eq_ignore_ascii_case("x-ms-operation-id"))
                            .map(|(_, value)| value.clone());
                        self.operations
                            .complete(request, response, cancelled)
                            .await
                            .map_err(|mut error| {
                                error.mutation_may_have_occurred = true;
                                if error.operation_id.is_none() {
                                    error.operation_id = operation_id;
                                }
                                error
                            })
                    } else {
                        Ok(LoadOutcome::Accepted { operation_id: None })
                    }
                }
                status => {
                    let data: Option<serde_json::Value> =
                        serde_json::from_slice(&response.body).ok();
                    let header_code = response
                        .headers
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case("x-ms-public-api-error-code"))
                        .or_else(|| {
                            response
                                .headers
                                .iter()
                                .find(|(key, _)| key.eq_ignore_ascii_case("x-ms-error-code"))
                        })
                        .map(|(_, value)| value.clone());
                    let code = match status {
                        401 => Some(header_code.unwrap_or_else(|| "Unauthorized".into())),
                        403 => Some(header_code.unwrap_or_else(|| "Forbidden".into())),
                        404 => Some("NotFound".into()),
                        _ => data
                            .as_ref()
                            .and_then(|v| v.get("errorCode"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned),
                    };
                    Err(uncertain(TableError::new(TableErrorKind::Http {
                        status,
                        code,
                        message: data
                            .as_ref()
                            .and_then(|v| v.get("message"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned),
                    })))
                }
            }
        })
    }
}

fn uncertain(mut error: TableError) -> TableError {
    error.mutation_may_have_occurred = true;
    error
}

fn execution_error(error: ExecutionError) -> TableError {
    TableError::new(match error {
        ExecutionError::TimedOut => TableErrorKind::TimedOut,
        ExecutionError::ResponseLimit => TableErrorKind::Limit,
        ExecutionError::InvalidInput
        | ExecutionError::Transport
        | ExecutionError::RetriesExhausted
        | ExecutionError::RetryDelay => TableErrorKind::Execution,
    })
}
