// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit OneLake DFS adapter over the existing bounded HTTP execution engine.
//!
//! The focused SDK acquires the storage token from a caller-injected credential.
//! This adapter supplies no credential, runtime, TLS, retry, paging or LRO stack.
//! The shared Fabric-only executor remains unchanged and Fabric-only.

use azure_core::http::Method;
use fabric_sdk_http::{ExecutionError, ExecutionFuture, HttpExecutor, PublicOrigin};
use fabric_sdk_onelake::{
    OneLakeExecutionError, OneLakeExecutor, OneLakeMethod, OneLakeRequest, OneLakeResponse,
};
use std::sync::Arc;

/// A reusable typed DFS executor over a caller-owned shared HTTP executor.
///
/// SDK and CLI consumers can clone this adapter without choosing credentials or
/// opening a connection. The [`fabric_sdk_onelake::OneLakeClient`] owns selection,
/// token acquisition, response validation, cancellation and traversal limits.
#[derive(Clone, Debug)]
pub struct OneLakeHttpExecutor {
    http: Arc<HttpExecutor>,
}

impl OneLakeHttpExecutor {
    /// Store the explicitly supplied shared executor without I/O.
    pub fn new(http: Arc<HttpExecutor>) -> Self {
        Self { http }
    }
}

impl OneLakeExecutor for OneLakeHttpExecutor {
    fn execute<'a>(
        &'a self,
        request: OneLakeRequest<'a>,
    ) -> ExecutionFuture<'a, Result<OneLakeResponse, OneLakeExecutionError>> {
        Box::pin(async move {
            let method = match request.method() {
                OneLakeMethod::Get => Method::Get,
                OneLakeMethod::Head => Method::Head,
            };
            self.http
                .execute_bound(
                    PublicOrigin::OneLake,
                    method,
                    request.url(),
                    request.token(),
                    &[],
                    &[],
                    request.max_response_bytes(),
                )
                .await
                .map(|response| OneLakeResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                })
                .map_err(execution_error)
        })
    }
}

fn execution_error(error: ExecutionError) -> OneLakeExecutionError {
    match error {
        ExecutionError::TimedOut => OneLakeExecutionError::TimedOut,
        ExecutionError::ResponseLimit => OneLakeExecutionError::ResponseLimit,
        ExecutionError::RetriesExhausted => OneLakeExecutionError::RetriesExhausted,
        ExecutionError::InvalidInput | ExecutionError::Transport | ExecutionError::RetryDelay => {
            OneLakeExecutionError::Failed
        }
    }
}
