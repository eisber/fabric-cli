// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Compatibility SDK adapters over the service-neutral `fabric-http` engine.
//!
//! Existing imports remain available. Service executor implementations live
//! here, never in the generic engine or transport dependency graph.

use azure_core::{
    credentials::AccessToken,
    http::{HttpClient, Method, Request, Url},
};
use fabric_core::{EndpointBinding, ServiceOrigin};
pub use fabric_http::{
    ExecutionError, ExecutionEvidence, ExecutionFuture, ExecutionOptions, ExecutionTimer,
    ExecutionTrace, HttpExecution, HttpRequest, HttpResponse, PublicOrigin, ResponseProgress,
};
use fabric_sdk::{
    notebooks::{NotebookExecutionError, NotebookExecutor, NotebookRequest, NotebookResponse},
    workspaces::{WorkspaceExecutionError, WorkspaceExecutor, WorkspaceRequest, WorkspaceResponse},
};
use std::{num::NonZeroUsize, sync::Arc, time::Duration};

#[cfg(test)]
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    task::Poll,
};

mod capacities;
pub mod feedback;
mod folders;
mod items;
pub mod native_runtime;
pub mod native_stream;
mod operations;
#[cfg(test)]
mod tests;

/// Existing service executors composed over one neutral HTTP engine.
///
/// The client and timer remain caller-owned; no runtime, credential backend,
/// transport, retry policy or service workflow is installed by this wrapper.
#[derive(Clone)]
pub struct HttpExecutor {
    inner: fabric_http::HttpExecutor,
    client: Arc<dyn HttpClient>,
    timer: Arc<dyn ExecutionTimer>,
}

impl std::fmt::Debug for HttpExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(formatter)
    }
}

impl HttpExecutor {
    /// Store the caller's HTTP client and timer without I/O.
    ///
    /// # Errors
    /// Rejects zero deadlines or maximum delays using the neutral engine.
    pub fn new(
        client: Arc<dyn HttpClient>,
        timer: Arc<dyn ExecutionTimer>,
        options: ExecutionOptions,
    ) -> Result<Self, ExecutionError> {
        Ok(Self {
            inner: fabric_http::HttpExecutor::new(client.clone(), timer.clone(), options)?,
            client,
            timer,
        })
    }

    /// Select the same validated endpoint as the service request context.
    pub fn with_endpoint(mut self, endpoint: EndpointBinding) -> Self {
        self.inner = self.inner.with_endpoint(endpoint);
        self
    }

    /// Scope the same engine to the caller's remaining total budget.
    ///
    /// # Errors
    /// Rejects a zero remaining budget before credentials or transport access.
    pub fn with_timeout(&self, remaining: Duration) -> Result<Self, ExecutionError> {
        Ok(Self {
            inner: self.inner.with_timeout(remaining)?,
            ..self.clone()
        })
    }

    /// Observe exchanges without changing dispatch or retry policy.
    pub fn with_trace(mut self, trace: ExecutionTrace) -> Self {
        self.inner = self.inner.with_trace(trace);
        self
    }

    /// Return the immutable endpoint selected for pre-send origin validation.
    pub fn endpoint(&self) -> &EndpointBinding {
        self.inner.endpoint()
    }

    /// Execute a bounded Fabric request using the unchanged shared policy.
    ///
    /// # Errors
    /// Returns origin/token, transport, response-bound, deadline or retry errors.
    pub async fn execute_fabric(
        &self,
        method: Method,
        url: &Url,
        token: &AccessToken,
        body: &[u8],
        limit: NonZeroUsize,
    ) -> Result<WorkspaceResponse, ExecutionError> {
        self.inner
            .execute_fabric(method, url, token, body, limit)
            .await
    }

    /// Execute against an explicitly selected origin, never an inferred audience.
    ///
    /// # Errors
    /// Returns validation, transport, response-bound, deadline or retry errors.
    #[allow(
        clippy::too_many_arguments,
        reason = "Retains the existing public adapter signature."
    )]
    pub async fn execute_bound(
        &self,
        origin: PublicOrigin,
        method: Method,
        url: &Url,
        token: &AccessToken,
        body: &[u8],
        headers: &[(String, String)],
        limit: NonZeroUsize,
    ) -> Result<WorkspaceResponse, ExecutionError> {
        self.inner
            .execute_bound(origin, method, url, token, body, headers, limit)
            .await
    }

    /// Execute with bounded partial response evidence and no mutation replay.
    ///
    /// # Errors
    /// Returns the original execution or recorder failure.
    #[allow(
        clippy::too_many_arguments,
        reason = "Retains the existing public adapter signature."
    )]
    pub async fn execute_bound_with_progress(
        &self,
        origin: PublicOrigin,
        method: Method,
        url: &Url,
        token: &AccessToken,
        body: &[u8],
        headers: &[(String, String)],
        limit: NonZeroUsize,
        progress: Option<&dyn ResponseProgress>,
    ) -> Result<WorkspaceResponse, ExecutionError> {
        self.inner
            .execute_bound_with_progress(origin, method, url, token, body, headers, limit, progress)
            .await
    }

    /// Wait using the caller's existing bounded asynchronous timer.
    ///
    /// # Errors
    /// Returns delay-bound or deadline failures without extending a service budget.
    pub async fn wait_for(&self, delay: Duration) -> Result<(), ExecutionError> {
        self.inner.wait_for(delay).await
    }

    async fn execute(
        &self,
        method: Method,
        url: &Url,
        token: &AccessToken,
        body: &[u8],
        limit: NonZeroUsize,
    ) -> Result<HttpResponse, ExecutionError> {
        self.inner
            .execute_fabric(method, url, token, body, limit)
            .await
    }

    fn validate_token(token: &AccessToken) -> Result<(), ExecutionError> {
        fabric_http::HttpExecutor::validate_token(token)
    }

    fn authenticated_request(
        method: Method,
        url: &Url,
        token: &AccessToken,
        body: &[u8],
    ) -> Request {
        fabric_http::HttpExecutor::authenticated_request(method, url, token, body)
    }

    async fn execute_request(
        &self,
        request: Request,
        limit: NonZeroUsize,
    ) -> Result<HttpResponse, ExecutionError> {
        self.inner.execute_prepared(request, limit).await
    }
}

impl HttpExecution for HttpExecutor {
    fn execute_http<'a>(
        &'a self,
        request: HttpRequest<'a>,
        progress: Option<&'a dyn ResponseProgress>,
    ) -> ExecutionFuture<'a, Result<HttpResponse, ExecutionError>> {
        self.inner.execute_http(request, progress)
    }
}

impl WorkspaceExecutor for HttpExecutor {
    fn execute<'a>(
        &'a self,
        request: WorkspaceRequest<'a>,
    ) -> ExecutionFuture<'a, Result<WorkspaceResponse, WorkspaceExecutionError>> {
        Box::pin(async move {
            self.execute(
                Method::Get,
                request.url(),
                request.token(),
                &[],
                request.max_response_bytes(),
            )
            .await
            .map_err(workspace_error)
        })
    }
}

impl NotebookExecutor for HttpExecutor {
    fn execute<'a>(
        &'a self,
        request: NotebookRequest<'a>,
    ) -> ExecutionFuture<'a, Result<NotebookResponse, NotebookExecutionError>> {
        Box::pin(async move {
            let method = match request.method() {
                "GET" => Method::Get,
                "POST" => Method::Post,
                _ => return Err(NotebookExecutionError::Failed),
            };
            self.execute(
                method,
                request.url(),
                request.token(),
                request.body(),
                request.max_response_bytes(),
            )
            .await
            .map(|response| NotebookResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
            .map_err(notebook_error)
        })
    }

    fn wait(&self, delay: Duration) -> ExecutionFuture<'_, Result<(), NotebookExecutionError>> {
        Box::pin(async move { self.wait_for(delay).await.map_err(notebook_error) })
    }
}

fn workspace_error(error: ExecutionError) -> WorkspaceExecutionError {
    match error {
        ExecutionError::TimedOut => WorkspaceExecutionError::TimedOut,
        ExecutionError::ResponseLimit => WorkspaceExecutionError::ResponseLimit,
        ExecutionError::RetriesExhausted => WorkspaceExecutionError::RetriesExhausted,
        ExecutionError::InvalidInput | ExecutionError::Transport | ExecutionError::RetryDelay => {
            WorkspaceExecutionError::Failed
        }
    }
}

fn notebook_error(error: ExecutionError) -> NotebookExecutionError {
    match error {
        ExecutionError::TimedOut => NotebookExecutionError::TimedOut,
        ExecutionError::ResponseLimit => NotebookExecutionError::ResponseLimit,
        ExecutionError::RetriesExhausted => NotebookExecutionError::RetriesExhausted,
        ExecutionError::InvalidInput | ExecutionError::Transport | ExecutionError::RetryDelay => {
            NotebookExecutionError::Failed
        }
    }
}
