// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Workspace listing and shared bounded read/paging policies.

use crate::{
    Page, WorkspaceClient,
    models::{FabricServiceError, Workspace},
};
use azure_core::{credentials::AccessToken, http::Url};
use fabric_core::{
    Audience, AuthenticatedExecution, EndpointBinding, RequestError, ServiceOrigin, cancel_with,
};
use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    future::Future,
    num::NonZeroUsize,
    pin::{Pin, pin},
};

/// One workspace GET request bound to the caller's validated endpoint.
///
/// Debug output deliberately excludes the URL query and injected token.
pub struct WorkspaceRequest<'a> {
    url: Url,
    token: &'a AccessToken,
    max_response_bytes: NonZeroUsize,
}

impl WorkspaceRequest<'_> {
    /// Returns the HTTP method, always GET.
    pub const fn method(&self) -> &'static str {
        "GET"
    }

    /// Returns the validated URL; do not log its opaque continuation token.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Returns the caller-provided bearer token; never log or persist its secret.
    pub fn token(&self) -> &AccessToken {
        self.token
    }

    /// Returns the response-body byte bound the executor must enforce while reading.
    pub const fn max_response_bytes(&self) -> NonZeroUsize {
        self.max_response_bytes
    }

    /// Returns the request content type used by the Python source.
    pub const fn content_type(&self) -> &'static str {
        "application/json"
    }
}

impl fmt::Debug for WorkspaceRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceRequest")
            .field("method", &"GET")
            .field("host", &self.url.host_str())
            .field("token", &"<injected>")
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

/// An executor response, before typed payload validation.
///
/// This type does not imply success. The operation checks status and body.
/// Headers are read case-insensitively.
#[derive(Clone, Debug)]
pub struct WorkspaceResponse {
    /// HTTP status returned by the executor.
    pub status: u16,
    /// Untrusted response body, subject to the request's byte limit.
    pub body: Vec<u8>,
    /// Response headers, including continuation and service error codes.
    pub headers: BTreeMap<String, String>,
}

/// Explicit executor failure, without untrusted transport diagnostic text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceExecutionError {
    /// Request execution failed.
    Failed,
    /// The executor's request deadline elapsed.
    TimedOut,
    /// The executor cancelled execution.
    Cancelled,
    /// The agreed executor retry budget was exhausted.
    RetriesExhausted,
    /// Streaming execution stopped before exceeding the response byte bound.
    ResponseLimit,
}

/// Injection boundary for this operation, not a production transport implementation.
///
/// Implementations must enforce the response byte bound and their agreed
/// timeout/retry policy, must not follow redirects or choose credentials, and
/// must release local request resources when the returned future is dropped.
/// A dropped future is cancellation, not evidence of a completed response.
pub trait WorkspaceExecutor: Send + Sync {
    /// Executes one validated GET using the supplied Fabric token.
    fn execute<'a>(
        &'a self,
        request: WorkspaceRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>;
}

/// Explicit finite traversal and allocation limits, all required to be nonzero.
#[derive(Clone, Copy, Debug)]
pub struct WorkspaceListLimits {
    /// Maximum successfully decoded pages.
    pub max_pages: NonZeroUsize,
    /// Maximum accumulated items per collection, including duplicates.
    pub max_items: NonZeroUsize,
    /// Maximum bytes per response, also passed to the executor.
    pub max_response_bytes: NonZeroUsize,
}

impl Default for WorkspaceListLimits {
    fn default() -> Self {
        Self {
            max_pages: NonZeroUsize::new(100).unwrap(),
            max_items: NonZeroUsize::new(100_000).unwrap(),
            max_response_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
        }
    }
}

/// Fully decoded traversal progress, never a silent incomplete-success marker.
///
/// Returned as success only when the service supplies no truthy continuation
/// token. On failure, this value is available explicitly as `error.partial`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorkspaceList {
    /// Workspaces in page order, preserving duplicates and service types.
    pub value: Vec<Workspace>,
    /// Actual count of fully decoded and accumulated pages, not Python `total_pages`.
    pub pages_received: usize,
    /// Token for the next unconsumed page, if known. Treat it as opaque data.
    pub next_token: Option<String>,
}

/// Distinct failure causes for workspace listing.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkspaceListErrorKind {
    /// Selection was not explicitly for Fabric. No token or request was acquired.
    InvalidAudience,
    /// The caller's credential failed; no alternate provider was attempted.
    Credential,
    /// The injected executor reported a typed failure.
    Execution(WorkspaceExecutionError),
    /// Fabric returned a failing status; structured data is retained when valid.
    Http {
        /// HTTP response status.
        status: u16,
        /// Python-compatible service error code, or its defined fallback.
        code: String,
        /// Parsed service data, absent if the body was not a structured error.
        service_error: Option<Box<FabricServiceError>>,
    },
    /// A 202/LRO or 204/empty response is not a complete workspace-list page.
    UnsupportedStatus(u16),
    /// A successful response did not contain a valid typed workspace page.
    InvalidResponse,
    /// A continuation URI did not match the allowed public Fabric endpoint.
    InvalidContinuation,
    /// A continuation token repeated, indicating a traversal cycle.
    RepeatedContinuation,
    /// More pages remained after the caller's page bound.
    PageLimit,
    /// A decoded page would exceed the caller's total item bound.
    ItemLimit,
    /// A response exceeded the caller's byte bound.
    ResponseLimit,
    /// The caller's cancellation future completed.
    Cancelled,
}

/// An explicit failed or incomplete list operation with all committed progress.
///
/// Display output is deliberately static: it does not echo URLs, service
/// messages, provider diagnostics, or continuation tokens.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceListError {
    /// Failure classification.
    pub kind: WorkspaceListErrorKind,
    /// Previously accumulated pages. Never treat this as a complete listing.
    pub partial: WorkspaceList,
}

impl fmt::Display for WorkspaceListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self.kind {
            WorkspaceListErrorKind::InvalidAudience => {
                "workspace listing requires the Fabric audience"
            }
            WorkspaceListErrorKind::Credential => {
                "the injected credential could not acquire a Fabric token"
            }
            WorkspaceListErrorKind::Execution(_) => "workspace request execution failed",
            WorkspaceListErrorKind::Http { .. } => "Fabric rejected the workspace request",
            WorkspaceListErrorKind::UnsupportedStatus(_) => {
                "the response is not a completed workspace-list page"
            }
            WorkspaceListErrorKind::InvalidResponse => {
                "Fabric returned an invalid workspace-list page"
            }
            WorkspaceListErrorKind::InvalidContinuation => {
                "Fabric returned an invalid workspace continuation URI"
            }
            WorkspaceListErrorKind::RepeatedContinuation => {
                "Fabric repeated a workspace continuation token"
            }
            WorkspaceListErrorKind::PageLimit => "workspace pagination reached its page bound",
            WorkspaceListErrorKind::ItemLimit => "workspace pagination reached its item bound",
            WorkspaceListErrorKind::ResponseLimit => {
                "the workspace response exceeded its byte bound"
            }
            WorkspaceListErrorKind::Cancelled => "workspace listing was cancelled",
        };
        f.write_str(message)
    }
}

impl std::error::Error for WorkspaceListError {}

impl WorkspaceClient {
    /// Lists workspace pages through injected execution, without installing a transport.
    ///
    /// The caller supplies a cancellation future; use `std::future::pending()`
    /// if cancellation is handled solely by dropping this operation's future.
    /// Cancellation takes priority when it and execution become ready together.
    /// In-flight credential/executor futures are dropped when cancelled.
    ///
    /// ```no_run
    /// use fabric_sdk_workspaces::{
    ///     WorkspaceClient, WorkspaceExecutor, WorkspaceListError, WorkspaceListLimits,
    /// };
    ///
    /// async fn list(
    ///     client: &WorkspaceClient,
    ///     executor: &dyn WorkspaceExecutor,
    /// ) -> Result<usize, WorkspaceListError> {
    ///     let workspaces = client
    ///         .list_workspaces(
    ///             executor,
    ///             WorkspaceListLimits::default(),
    ///             std::future::pending(),
    ///         )
    ///         .await?;
    ///     Ok(workspaces.value.len())
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns typed identity, execution, service, malformed-payload, cancellation
    /// or safety-bound failures with explicit partial progress. Never returns
    /// partial data as a complete successful listing.
    pub async fn list_workspaces(
        &self,
        executor: &dyn WorkspaceExecutor,
        limits: WorkspaceListLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<WorkspaceList, WorkspaceListError> {
        let mut progress = PageProgress::default();
        let mut cancelled = pin!(cancelled);
        let result = self
            .collect_workspaces(executor, limits, &mut progress, cancelled.as_mut())
            .await;
        let progress = WorkspaceList {
            value: progress.value,
            pages_received: progress.pages_received,
            next_token: progress.next_token,
        };
        match result {
            Ok(()) => Ok(progress),
            Err(kind) => Err(WorkspaceListError {
                kind,
                partial: progress,
            }),
        }
    }

    async fn collect_workspaces(
        &self,
        executor: &dyn WorkspaceExecutor,
        limits: WorkspaceListLimits,
        progress: &mut PageProgress<Workspace>,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<(), WorkspaceListErrorKind> {
        or_cancel(std::future::ready(()), cancelled.as_mut()).await?;
        if self.selection().audience() != Audience::Fabric {
            return Err(WorkspaceListErrorKind::InvalidAudience);
        }
        loop {
            let mut url = self
                .context()
                .endpoint()
                .url("/v1/workspaces")
                .map_err(|_| WorkspaceListErrorKind::InvalidContinuation)?;
            if let Some(token) = &progress.next_token {
                url.query_pairs_mut()
                    .append_pair("continuationToken", token);
            }
            let response = self
                .read_workspace(executor, url, limits.max_response_bytes, cancelled.as_mut())
                .await?;
            let page: Page<Workspace> = serde_json::from_slice(&response.body)
                .map_err(|_| WorkspaceListErrorKind::InvalidResponse)?;
            if !progress.append(
                page,
                &response,
                limits,
                "/v1/workspaces",
                self.context().endpoint(),
            )? {
                return Ok(());
            }
        }
    }
}

pub(crate) struct PageProgress<T> {
    pub(crate) value: Vec<T>,
    pub(crate) pages_received: usize,
    pub(crate) next_token: Option<String>,
    seen: HashSet<String>,
}

impl<T> Default for PageProgress<T> {
    fn default() -> Self {
        Self {
            value: Vec::new(),
            pages_received: 0,
            next_token: None,
            seen: HashSet::new(),
        }
    }
}

impl<T> PageProgress<T> {
    pub(crate) fn append(
        &mut self,
        page: Page<T>,
        response: &WorkspaceResponse,
        limits: WorkspaceListLimits,
        path: &str,
        endpoint: &EndpointBinding,
    ) -> Result<bool, WorkspaceListErrorKind> {
        if page.value.len() > limits.max_items.get() - self.value.len() {
            return Err(WorkspaceListErrorKind::ItemLimit);
        }
        self.value.extend(page.value);
        self.pages_received += 1;
        self.next_token = header(response, "x-ms-continuation")
            .map(str::to_owned)
            .or(page.continuation_token)
            .filter(|token| !token.is_empty());
        let Some(next) = &self.next_token else {
            return Ok(false);
        };
        if let Some(uri) = page.continuation_uri {
            validate_continuation_path(&uri, path, endpoint)?;
        }
        if !self.seen.insert(next.clone()) {
            return Err(WorkspaceListErrorKind::RepeatedContinuation);
        }
        if self.pages_received >= limits.max_pages.get() {
            return Err(WorkspaceListErrorKind::PageLimit);
        }
        Ok(true)
    }
}

impl WorkspaceClient {
    pub(crate) async fn read_workspace(
        &self,
        executor: &dyn WorkspaceExecutor,
        url: Url,
        limit: NonZeroUsize,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<WorkspaceResponse, WorkspaceListErrorKind> {
        let response = self
            .context()
            .authentication()
            .execute(
                ServiceOrigin::Fabric,
                &url,
                &mut WorkspaceRead { executor, limit },
                cancelled.as_mut(),
            )
            .await
            .map_err(|error| match error {
                RequestError::InvalidAudience => WorkspaceListErrorKind::InvalidAudience,
                RequestError::InvalidEndpoint => WorkspaceListErrorKind::InvalidContinuation,
                RequestError::Credential => WorkspaceListErrorKind::Credential,
                RequestError::Cancelled => WorkspaceListErrorKind::Cancelled,
                RequestError::Execution(WorkspaceExecutionError::ResponseLimit) => {
                    WorkspaceListErrorKind::ResponseLimit
                }
                RequestError::Execution(error) => WorkspaceListErrorKind::Execution(error),
            })?;
        if response.body.len() > limit.get() {
            return Err(WorkspaceListErrorKind::ResponseLimit);
        }
        match response.status {
            200 | 201 => Ok(response),
            202 | 204 => Err(WorkspaceListErrorKind::UnsupportedStatus(response.status)),
            _ => Err(service_failure(&response)),
        }
    }
}

struct WorkspaceRead<'a> {
    executor: &'a dyn WorkspaceExecutor,
    limit: NonZeroUsize,
}

impl AuthenticatedExecution for WorkspaceRead<'_> {
    type Response = WorkspaceResponse;
    type Error = WorkspaceExecutionError;

    fn execute<'a>(
        &'a mut self,
        url: &'a Url,
        token: &'a AccessToken,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'a>> {
        self.executor.execute(WorkspaceRequest {
            url: url.clone(),
            token,
            max_response_bytes: self.limit,
        })
    }
}

pub(crate) async fn or_cancel<T>(
    operation: impl Future<Output = T>,
    cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
) -> Result<T, WorkspaceListErrorKind> {
    cancel_with(operation, cancelled)
        .await
        .map_err(|_| WorkspaceListErrorKind::Cancelled)
}

pub(crate) fn header<'a>(response: &'a WorkspaceResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn service_failure(response: &WorkspaceResponse) -> WorkspaceListErrorKind {
    let body = serde_json::from_slice::<serde_json::Value>(&response.body).ok();
    let code = match response.status {
        401 | 403 => header(response, "x-ms-public-api-error-code")
            .filter(|code| !code.is_empty())
            .or_else(|| header(response, "x-ms-error-code").filter(|code| !code.is_empty()))
            .unwrap_or(if response.status == 401 {
                "Unauthorized"
            } else {
                "Forbidden"
            })
            .to_owned(),
        404 => "NotFound".to_owned(),
        _ => body
            .as_ref()
            .and_then(|body| body.get("errorCode"))
            .and_then(serde_json::Value::as_str)
            .filter(|code| !code.is_empty())
            .unwrap_or("UnknownError")
            .to_owned(),
    };
    let service_error = body
        .and_then(|body| serde_json::from_value::<FabricServiceError>(body).ok())
        .map(Box::new);
    WorkspaceListErrorKind::Http {
        status: response.status,
        code,
        service_error,
    }
}

pub(crate) fn validate_continuation_path(
    uri: &str,
    path: &str,
    endpoint: &EndpointBinding,
) -> Result<(), WorkspaceListErrorKind> {
    if uri.contains('\\') || uri.chars().any(char::is_control) {
        return Err(WorkspaceListErrorKind::InvalidContinuation);
    }
    let url = Url::parse(uri).map_err(|_| WorkspaceListErrorKind::InvalidContinuation)?;
    if endpoint.validate_url(&url).is_err() || url.path() != path {
        return Err(WorkspaceListErrorKind::InvalidContinuation);
    }

    Ok(())
}

#[cfg(test)]
mod tests;
