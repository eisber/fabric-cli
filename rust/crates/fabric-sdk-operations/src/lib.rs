// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared bounded Fabric request, mutation and operation-result reconciliation.
//!
//! Extracted from the existing Notebook policy. Endpoint clients and Notebook
//! publication use one engine; no transport, HTTP retry stack or runtime is added.

use azure_core::{credentials::AccessToken, http::Url};
pub use fabric_core::Audience;
use fabric_core::{CredentialSelection, TokenCredential};
use fabric_sdk_workspaces::{
    FabricServiceError,
    mutations::{
        CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
        ResourceOutcome,
    },
};
use std::{
    collections::BTreeMap,
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::{Pin, pin},
    task::Poll,
    time::Duration,
};

const ROOT: &str = "https://api.fabric.microsoft.com";

/// Validated, fixed-origin operation specification.
pub struct FabricOperation {
    method: ResourceMethod,
    url: Url,
    body: Vec<u8>,
    completion: CompletionPolicy,
}

impl FabricOperation {
    /// Construct a public Fabric operation, never an arbitrary origin.
    ///
    /// # Errors
    /// Rejects paths outside `/v1/`, URL traversal/authority, malformed URLs,
    /// nonempty GET bodies, or non-JSON mutation bodies.
    pub fn new(
        method: ResourceMethod,
        path: &str,
        body: Vec<u8>,
        completion: CompletionPolicy,
    ) -> Result<Self, OperationErrorKind> {
        let url =
            Url::parse(&format!("{ROOT}{path}")).map_err(|_| OperationErrorKind::InvalidInput)?;
        let raw_path = path
            .split('?')
            .next()
            .ok_or(OperationErrorKind::InvalidInput)?;
        if !path.starts_with("/v1/")
            || !valid_origin(&url)
            || url.path() != raw_path
            || raw_path.contains(['\\', '%'])
            || raw_path
                .split('/')
                .skip(1)
                .any(|segment| segment.is_empty())
            || (method == ResourceMethod::Get && !body.is_empty())
            || (method != ResourceMethod::Get
                && !body.is_empty()
                && !serde_json::from_slice::<serde_json::Value>(&body)
                    .is_ok_and(|body| body.is_object()))
        {
            return Err(OperationErrorKind::InvalidInput);
        }
        Ok(Self {
            method,
            url,
            body,
            completion,
        })
    }

    /// Convert a focused endpoint's validated operation without changing payloads.
    ///
    /// # Errors
    /// Returns explicit path/body validation errors before credential acquisition.
    pub fn from_resource(operation: &ResourceOperation) -> Result<Self, OperationErrorKind> {
        let body = operation
            .body()
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|_| OperationErrorKind::InvalidInput)?
            .unwrap_or_default();
        Self::new(
            operation.method(),
            operation.path(),
            body,
            operation.completion(),
        )
    }
}

/// A validated request with caller-injected bearer token and body bound.
pub struct OperationRequest<'a> {
    method: ResourceMethod,
    url: Url,
    body: Vec<u8>,
    token: &'a AccessToken,
    max_response_bytes: NonZeroUsize,
}

impl OperationRequest<'_> {
    /// Finite method.
    pub const fn method(&self) -> ResourceMethod {
        self.method
    }
    /// Fixed-origin validated Fabric URL.
    pub fn url(&self) -> &Url {
        &self.url
    }
    /// Exact serialized body; empty differs from an empty JSON object.
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    /// Injected token; never log or persist it.
    pub fn token(&self) -> &AccessToken {
        self.token
    }
    /// Streaming byte bound for the existing shared executor.
    pub const fn max_response_bytes(&self) -> NonZeroUsize {
        self.max_response_bytes
    }
}

impl fmt::Debug for OperationRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperationRequest")
            .field("method", &self.method)
            .field("origin", &ROOT)
            .field("body", &"<resource>")
            .field("token", &"<injected>")
            .finish()
    }
}

/// Bounded raw response, retaining actual completion/acceptance status.
#[derive(Clone, Debug)]
pub struct OperationResponse {
    /// Actual HTTP status.
    pub status: u16,
    /// Exact response bytes.
    pub body: Vec<u8>,
    /// Headers, read case-insensitively with duplicate rejection.
    pub headers: BTreeMap<String, String>,
}

/// Sanitized failures from the caller's existing HTTP execution engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationExecutionError {
    /// Transport failed; a dispatched mutation may already have occurred.
    Failed,
    /// Request deadline elapsed.
    TimedOut,
    /// Local execution cancelled.
    Cancelled,
    /// Streaming byte bound reached.
    ResponseLimit,
    /// Finite read retry budget exhausted; no mutation was replayed.
    RetriesExhausted,
}

/// Thin adapter to caller-owned shared HTTP execution and asynchronous time.
pub trait OperationExecutor: Send + Sync {
    /// Execute one request; never replay POST/PATCH/DELETE mutations.
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>;
    /// Wait using the caller's runtime, never a blocking sleep or busy loop.
    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>>;
}

/// Finite bounds for requests, response allocation and service polling intervals.
#[derive(Clone, Copy, Debug)]
pub struct OperationLimits {
    /// Total requests, including polling and result retrieval.
    pub max_requests: NonZeroUsize,
    /// Maximum bytes per response.
    pub max_response_bytes: NonZeroUsize,
    /// Largest accepted Retry-After interval.
    pub max_poll_delay: Duration,
}

impl Default for OperationLimits {
    fn default() -> Self {
        Self {
            max_requests: NonZeroUsize::new(100).unwrap(),
            max_response_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
            max_poll_delay: Duration::from_secs(60),
        }
    }
}

/// Shared classification retaining the original Notebook policy distinctions.
#[derive(Clone, Debug, PartialEq)]
pub enum OperationErrorKind {
    /// Unsafe or malformed operation input.
    InvalidInput,
    /// Selection was not explicitly for Fabric.
    InvalidAudience,
    /// Injected credential failed.
    Credential,
    /// Shared executor failed.
    Execution(OperationExecutionError),
    /// Service rejected the request.
    Http {
        /// HTTP status.
        status: u16,
        /// Structured/header error code when supplied.
        code: Option<String>,
        /// Structured service data retained independently of display text.
        error: Option<Box<FabricServiceError>>,
    },
    /// Malformed body, header or unknown operation state.
    InvalidResponse,
    /// Invalid fixed-origin operation/result link.
    InvalidLink,
    /// Request/byte/poll-delay safety bound reached.
    Limit,
    /// Service reported a failed/cancelled terminal operation.
    OperationFailed {
        /// Exact terminal service state.
        status: String,
        /// Structured service error, when supplied.
        error: Option<Box<FabricServiceError>>,
    },
    /// Local cancellation; never evidence of rollback.
    Cancelled,
}

/// Failure with explicit operation reconciliation and uncertain-side-effect data.
#[derive(Clone, Debug, PartialEq)]
pub struct OperationError {
    /// Typed failure.
    pub kind: OperationErrorKind,
    /// Accepted operation ID, when known.
    pub operation_id: Option<String>,
    /// True before a mutating request future can first be polled.
    pub mutation_may_have_occurred: bool,
}

impl fmt::Display for OperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Fabric operation failed or is incomplete; inspect classification and reconciliation evidence")
    }
}

impl std::error::Error for OperationError {}

/// One bounded operation session. A mutation is never silently replayed.
pub struct OperationSession<'a, E> {
    credential: &'a dyn TokenCredential,
    selection: &'a CredentialSelection,
    executor: E,
    limits: OperationLimits,
    cancelled: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    cancelled_observed: bool,
    requests: usize,
    operation_id: Option<String>,
    mutation_may_have_occurred: bool,
}

impl<'a, E: OperationExecutor> OperationSession<'a, E> {
    /// Store explicit caller inputs without any I/O or runtime initialization.
    pub fn new(
        credential: &'a dyn TokenCredential,
        selection: &'a CredentialSelection,
        executor: E,
        limits: OperationLimits,
        cancelled: impl Future<Output = ()> + Send + 'a,
    ) -> Self {
        Self {
            credential,
            selection,
            executor,
            limits,
            cancelled: Box::pin(cancelled),
            cancelled_observed: false,
            requests: 0,
            operation_id: None,
            mutation_may_have_occurred: false,
        }
    }

    /// Inspect reconciliation evidence without interpreting acceptance as completion.
    pub fn evidence(&self, kind: OperationErrorKind) -> OperationError {
        OperationError {
            kind,
            operation_id: self.operation_id.clone(),
            mutation_may_have_occurred: self.mutation_may_have_occurred,
        }
    }

    /// Accepted operation identifier, if reconciliation has observed one.
    pub fn operation_id(&self) -> Option<&str> {
        self.operation_id.as_deref()
    }

    /// Execute a validated operation and optionally reconcile its accepted state.
    ///
    /// # Errors
    /// Preserves identity, service, bounds, cancellation and uncertain mutation
    /// evidence. Only one mutation may be dispatched per session.
    pub async fn execute(
        &mut self,
        operation: FabricOperation,
    ) -> Result<OperationResponse, OperationError> {
        self.execute_inner(operation)
            .await
            .map_err(|kind| self.evidence(kind))
    }

    async fn execute_inner(
        &mut self,
        operation: FabricOperation,
    ) -> Result<OperationResponse, OperationErrorKind> {
        let mut response = if operation.method == ResourceMethod::Get {
            let response = self
                .request(operation.method, operation.url, operation.body)
                .await?;
            ensure_status(&response, &[200, 201, 204])?;
            return Ok(response);
        } else {
            if self.mutation_may_have_occurred {
                return Err(OperationErrorKind::InvalidInput);
            }
            self.request(operation.method, operation.url, operation.body)
                .await?
        };
        if response.status != 202 {
            ensure_status(&response, &[200, 201, 204])?;
            return Ok(response);
        }
        if operation.completion == CompletionPolicy::ReturnAccepted {
            if let Some(id) = accepted_id(&response)? {
                self.operation_id = Some(id.clone());
                if let Some(uri) = header(&response, "Location")? {
                    validate_link(uri, &format!("/v1/operations/{id}"))?;
                }
            }
            return Ok(response);
        }
        let id = accepted_id(&response)?.ok_or(OperationErrorKind::InvalidResponse)?;
        let path = format!("/v1/operations/{id}");
        self.operation_id = Some(id);
        if let Some(uri) = header(&response, "Location")? {
            validate_link(uri, &path)?;
        }
        loop {
            if self.requests >= self.limits.max_requests.get() {
                return Err(OperationErrorKind::Limit);
            }
            let delay = match header(&response, "Retry-After")? {
                Some(value) => Duration::from_secs(
                    value
                        .parse()
                        .map_err(|_| OperationErrorKind::InvalidResponse)?,
                ),
                None => Duration::from_secs(1),
            };
            if delay > self.limits.max_poll_delay {
                return Err(OperationErrorKind::Limit);
            }
            Self::run(
                &mut self.cancelled,
                &mut self.cancelled_observed,
                self.executor.wait(delay),
            )
            .await?
            .map_err(OperationErrorKind::Execution)?;
            response = self
                .request(ResourceMethod::Get, service_url(&path)?, Vec::new())
                .await?;
            ensure_status(&response, &[200])?;
            let state: serde_json::Value = serde_json::from_slice(&response.body)
                .map_err(|_| OperationErrorKind::InvalidResponse)?;
            match state.get("status").and_then(serde_json::Value::as_str) {
                Some("NotStarted" | "Running") => {
                    if let Some(uri) = header(&response, "Location")? {
                        validate_link(uri, &path)?;
                    }
                }
                Some("Succeeded") => {
                    let result_path = format!("{path}/result");
                    if let Some(uri) = header(&response, "Location")? {
                        validate_link(uri, &result_path)?;
                    }
                    if operation.completion == CompletionPolicy::WaitForResult {
                        let result = self
                            .request(ResourceMethod::Get, service_url(&result_path)?, Vec::new())
                            .await?;
                        ensure_status(&result, &[200])?;
                        return Ok(result);
                    }
                    return Ok(response);
                }
                Some(status @ ("Failed" | "Cancelled")) => {
                    return Err(OperationErrorKind::OperationFailed {
                        status: status.to_owned(),
                        error: state
                            .get("error")
                            .cloned()
                            .and_then(|error| serde_json::from_value(error).ok())
                            .map(Box::new),
                    });
                }
                _ => return Err(OperationErrorKind::InvalidResponse),
            }
        }
    }

    async fn request(
        &mut self,
        method: ResourceMethod,
        url: Url,
        body: Vec<u8>,
    ) -> Result<OperationResponse, OperationErrorKind> {
        Self::run(
            &mut self.cancelled,
            &mut self.cancelled_observed,
            std::future::ready(()),
        )
        .await?;
        if self.selection.audience() != Audience::Fabric {
            return Err(OperationErrorKind::InvalidAudience);
        }
        if self.requests >= self.limits.max_requests.get() {
            return Err(OperationErrorKind::Limit);
        }
        let scopes = [Audience::Fabric.scope()];
        let token = Self::run(
            &mut self.cancelled,
            &mut self.cancelled_observed,
            self.credential.get_token(&scopes, None),
        )
        .await?
        .map_err(|_| OperationErrorKind::Credential)?;
        self.requests += 1;
        let request = OperationRequest {
            method,
            url,
            body,
            token: &token,
            max_response_bytes: self.limits.max_response_bytes,
        };
        Self::run(
            &mut self.cancelled,
            &mut self.cancelled_observed,
            std::future::ready(()),
        )
        .await?;
        if method != ResourceMethod::Get {
            self.mutation_may_have_occurred = true;
        }
        let response = Self::run(
            &mut self.cancelled,
            &mut self.cancelled_observed,
            self.executor.execute(request),
        )
        .await?
        .map_err(|error| match error {
            OperationExecutionError::ResponseLimit => OperationErrorKind::Limit,
            error => OperationErrorKind::Execution(error),
        })?;
        if response.body.len() > self.limits.max_response_bytes.get() {
            return Err(OperationErrorKind::Limit);
        }
        Ok(response)
    }

    async fn run<T>(
        cancelled: &mut Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
        observed: &mut bool,
        future: impl Future<Output = T>,
    ) -> Result<T, OperationErrorKind> {
        if *observed {
            return Err(OperationErrorKind::Cancelled);
        }
        let mut future = pin!(future);
        poll_fn(|cx| {
            if cancelled.as_mut().poll(cx).is_ready() {
                *observed = true;
                return Poll::Ready(Err(OperationErrorKind::Cancelled));
            }
            future.as_mut().poll(cx).map(Ok)
        })
        .await
    }
}

impl<E: OperationExecutor> ResourceOperations for OperationSession<'_, E> {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        Box::pin(async move {
            let operation = FabricOperation::from_resource(&operation)
                .map_err(|kind| resource_error(self.evidence(kind)))?;
            let response = OperationSession::execute(self, operation)
                .await
                .map_err(resource_error)?;
            if response.status == 202 {
                return Ok(ResourceOutcome::Accepted {
                    operation_id: self.operation_id().map(str::to_owned),
                });
            }
            let data = if response.body.is_empty() {
                None
            } else {
                Some(serde_json::from_slice(&response.body).map_err(|_| {
                    resource_error(self.evidence(OperationErrorKind::InvalidResponse))
                })?)
            };
            Ok(ResourceOutcome::Completed {
                status: response.status,
                data,
            })
        })
    }
}

fn resource_error(error: OperationError) -> ResourceError {
    let mut result = ResourceError {
        code: String::new(),
        message: error.to_string(),
        status: None,
        service_error: None,
        operation_id: error.operation_id,
        mutation_may_have_occurred: error.mutation_may_have_occurred,
    };
    result.code = match error.kind {
        OperationErrorKind::Http {
            status,
            code,
            error,
        } => {
            result.status = Some(status);
            result.service_error = error;
            match status {
                401 => code.unwrap_or_else(|| "Unauthorized".into()),
                403 => code.unwrap_or_else(|| "Forbidden".into()),
                404 => "NotFound".into(),
                _ => code.unwrap_or_else(|| "UnknownError".into()),
            }
        }
        OperationErrorKind::OperationFailed { status, error } => {
            result.service_error = error;
            if status == "Cancelled" {
                "LongRunningOperationCancelled".into()
            } else {
                "LongRunningOperationFailed".into()
            }
        }
        OperationErrorKind::InvalidInput => "InvalidInput".into(),
        OperationErrorKind::InvalidAudience => "InvalidAudience".into(),
        OperationErrorKind::Credential => "Credential".into(),
        OperationErrorKind::Execution(OperationExecutionError::TimedOut) => "TimedOut".into(),
        OperationErrorKind::Execution(OperationExecutionError::Cancelled)
        | OperationErrorKind::Cancelled => "LongRunningOperationCancelled".into(),
        OperationErrorKind::Execution(OperationExecutionError::ResponseLimit)
        | OperationErrorKind::Limit => "Limit".into(),
        OperationErrorKind::Execution(OperationExecutionError::RetriesExhausted) => {
            "MaxRetriesExceeded".into()
        }
        OperationErrorKind::Execution(OperationExecutionError::Failed) => "ExecutionFailed".into(),
        OperationErrorKind::InvalidResponse => "InvalidResponse".into(),
        OperationErrorKind::InvalidLink => "InvalidLink".into(),
    };
    result
}

fn valid_origin(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("api.fabric.microsoft.com")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn accepted_id(response: &OperationResponse) -> Result<Option<String>, OperationErrorKind> {
    if let Some(id) = header(response, "x-ms-operation-id")? {
        if !valid_id(id) {
            return Err(OperationErrorKind::InvalidResponse);
        }
        return Ok(Some(id.to_owned()));
    }
    let Some(uri) = header(response, "Location")? else {
        return Ok(None);
    };
    let url = Url::parse(uri).map_err(|_| OperationErrorKind::InvalidLink)?;
    let id = url
        .path()
        .rsplit('/')
        .next()
        .filter(|id| valid_id(id))
        .ok_or(OperationErrorKind::InvalidLink)?;
    Ok(Some(id.to_owned()))
}

fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn service_url(path: &str) -> Result<Url, OperationErrorKind> {
    Url::parse(&format!("{ROOT}{path}")).map_err(|_| OperationErrorKind::InvalidInput)
}

fn validate_link(uri: &str, path: &str) -> Result<(), OperationErrorKind> {
    let url = Url::parse(uri).map_err(|_| OperationErrorKind::InvalidLink)?;
    if !valid_origin(&url) || url.path() != path || url.query().is_some() {
        return Err(OperationErrorKind::InvalidLink);
    }
    Ok(())
}

fn header<'a>(
    response: &'a OperationResponse,
    name: &str,
) -> Result<Option<&'a str>, OperationErrorKind> {
    let mut matches = response
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name));
    let value = matches.next().map(|(_, value)| value.as_str());
    if matches.next().is_some() {
        return Err(OperationErrorKind::InvalidResponse);
    }
    Ok(value)
}

fn ensure_status(response: &OperationResponse, allowed: &[u16]) -> Result<(), OperationErrorKind> {
    if allowed.contains(&response.status) {
        return Ok(());
    }
    Err(OperationErrorKind::Http {
        status: response.status,
        code: header(response, "x-ms-public-api-error-code")?
            .or(header(response, "x-ms-error-code")?)
            .filter(|code| !code.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                serde_json::from_slice::<serde_json::Value>(&response.body)
                    .ok()
                    .and_then(|body| {
                        body.get("errorCode")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
            }),
        error: serde_json::from_slice(&response.body).ok().map(Box::new),
    })
}
