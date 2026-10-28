// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared bounded Fabric request, mutation and operation-result reconciliation.
//!
//! Extracted from the existing Notebook policy. Endpoint clients and Notebook
//! publication use one engine; no transport, HTTP retry stack or runtime is added.
//! Sessions default to public Fabric; an explicit immutable request context binds
//! the initial request, operation polling and result retrieval to one endpoint.

use azure_core::{credentials::AccessToken, http::Url};
pub use fabric_core::Audience;
use fabric_core::{
    AuthenticatedExecution, AuthenticatedFuture, CredentialSelection, EndpointBinding,
    RequestAuthentication, RequestContext, RequestError, ServiceOrigin, TokenCredential,
};
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

/// Validated relative Fabric operation, bound to an endpoint when executed.
pub struct FabricOperation {
    method: ResourceMethod,
    path: String,
    body: Vec<u8>,
    completion: CompletionPolicy,
    source_read_operation_ids: bool,
}

impl FabricOperation {
    /// Validated method, without executing the operation.
    pub const fn method(&self) -> ResourceMethod {
        self.method
    }

    /// Validated relative API path and encoded query, never an arbitrary origin.
    pub fn path(&self) -> &str {
        &self.path
    }

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
        let url = public_operation_url(path)?;
        let raw_path = path
            .split('?')
            .next()
            .ok_or(OperationErrorKind::InvalidInput)?;
        if !path.starts_with("/v1/")
            || url.path() != raw_path
            || raw_path.contains(['\\', '%'])
            || raw_path
                .split('/')
                .skip(1)
                .any(|segment| segment.is_empty())
        {
            return Err(OperationErrorKind::InvalidInput);
        }
        Self::validate_operation_body(method, &body)?;
        Ok(Self {
            method,
            path: relative_operation_path(&url),
            body,
            completion,
            source_read_operation_ids: false,
        })
    }

    /// Build a relative Fabric operation from literal path and query components.
    ///
    /// This constructor does not accept a URL, hostname, pre-encoded path or
    /// traversal. Unicode, spaces and literal percent characters are encoded as
    /// component data. Execution remains bound to the caller's operation session.
    /// The raw-path constructor's stricter percent policy is unchanged.
    ///
    /// # Errors
    /// Rejects paths outside `v1`, empty/dot/traversing components, separators,
    /// control characters, empty query names and invalid operation bodies.
    pub fn from_path_segments(
        method: ResourceMethod,
        segments: &[&str],
        query: &[(&str, &str)],
        body: Vec<u8>,
        completion: CompletionPolicy,
    ) -> Result<Self, OperationErrorKind> {
        if segments.first() != Some(&"v1") || segments.len() < 2 || segments.len() > 256 {
            return Err(OperationErrorKind::InvalidInput);
        }
        Self::validate_operation_body(method, &body)?;
        let resource = ResourceOperation::encoded(method, &segments[1..], None, completion)
            .map_err(|_| OperationErrorKind::InvalidInput)?;
        let resource = if query.is_empty() {
            resource
        } else {
            resource
                .with_query(query)
                .map_err(|_| OperationErrorKind::InvalidInput)?
        };
        let mut operation = Self::from_resource(&resource)?;
        let url = public_operation_url(&operation.path)?;
        if url.as_str().len() > 16 * 1024 {
            return Err(OperationErrorKind::Limit);
        }
        operation.body = body;
        Ok(operation)
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
        let url = public_operation_url(operation.path())?;
        if !url.path().starts_with("/v1/") {
            return Err(OperationErrorKind::InvalidInput);
        }
        Ok(Self {
            method: operation.method(),
            path: relative_operation_path(&url),
            body,
            completion: operation.completion(),
            source_read_operation_ids: false,
        })
    }

    fn validate_operation_body(
        method: ResourceMethod,
        body: &[u8],
    ) -> Result<(), OperationErrorKind> {
        if (method == ResourceMethod::Get && !body.is_empty())
            || (method != ResourceMethod::Get
                && !body.is_empty()
                && !serde_json::from_slice::<serde_json::Value>(body)
                    .is_ok_and(|body| body.is_object()))
        {
            return Err(OperationErrorKind::InvalidInput);
        }
        Ok(())
    }

    /// Permit bounded opaque operation IDs for an explicit source-synchronous GET.
    ///
    /// Default operations, Notebook calls, mutations and external acceptance
    /// reconciliation retain their GUID-only policy. This opt-in accepts at most
    /// 256 ASCII letters, digits, hyphens or underscores in a single response ID;
    /// selected-origin checks, response bounds and the shared poller are unchanged.
    ///
    /// # Errors
    /// Rejects any method or completion policy except GET/PythonSynchronous.
    pub fn with_source_read_operation_ids(mut self) -> Result<Self, OperationErrorKind> {
        if self.method != ResourceMethod::Get
            || self.completion != CompletionPolicy::PythonSynchronous
        {
            return Err(OperationErrorKind::InvalidInput);
        }
        self.source_read_operation_ids = true;
        Ok(self)
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
            .field("origin", &self.url.origin().ascii_serialization())
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
    ///
    /// Do not follow redirects: the URL is already bound to the selected endpoint.
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
    /// Exact parsed failure data, never included in Display automatically.
    pub raw_service_error: Option<Box<serde_json::Value>>,
}

impl OperationError {
    /// Project an owned resource-error view while retaining this original evidence.
    ///
    /// Uses the same mapping as the existing `ResourceOperations` implementation;
    /// it neither formats raw service data nor invents an HTTP response/status.
    /// The caller retains the typed kind, operation ID and uncertain-side-effect
    /// evidence on this original error.
    pub fn resource_error(&self) -> ResourceError {
        resource_error(self.clone())
    }
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
    endpoint: EndpointBinding,
    executor: E,
    limits: OperationLimits,
    cancelled: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    cancelled_observed: bool,
    requests: usize,
    operation_id: Option<String>,
    mutation_may_have_occurred: bool,
    warnings: Vec<OperationWarning>,
    raw_service_error: Option<Box<serde_json::Value>>,
}

/// Source debug-warning outcomes, exposed rather than silently discarding fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationWarning {
    /// Python falls back to ten seconds when Retry-After is not an integer.
    InvalidRetryAfter,
}

impl<'a, E: OperationExecutor> OperationSession<'a, E> {
    /// Store explicit caller inputs without any I/O or runtime initialization.
    ///
    /// Uses public Fabric. [`Self::with_context`] explicitly selects an endpoint
    /// without changing this borrowed-credential constructor's default.
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
            endpoint: EndpointBinding::public(ServiceOrigin::Fabric),
            executor,
            limits,
            cancelled: Box::pin(cancelled),
            cancelled_observed: false,
            requests: 0,
            operation_id: None,
            mutation_may_have_occurred: false,
            warnings: Vec::new(),
            raw_service_error: None,
        }
    }

    /// Bind every request, poll and result link to an explicit immutable context.
    ///
    /// The session borrows the caller's credential and selection and owns a clone
    /// of its endpoint binding. Construction performs no I/O. Service, audience
    /// and URL validation precede credential acquisition; no other host is trusted
    /// implicitly and no mutation is replayed.
    pub fn with_context(
        context: &'a RequestContext,
        executor: E,
        limits: OperationLimits,
        cancelled: impl Future<Output = ()> + Send + 'a,
    ) -> Self {
        let mut session = Self::new(
            context.credential().as_ref(),
            context.selection(),
            executor,
            limits,
            cancelled,
        );
        session.endpoint = context.endpoint().clone();
        session
    }

    /// Inspect reconciliation evidence without interpreting acceptance as completion.
    pub fn evidence(&self, kind: OperationErrorKind) -> OperationError {
        let raw = if matches!(
            &kind,
            OperationErrorKind::Http { .. } | OperationErrorKind::OperationFailed { .. }
        ) {
            self.raw_service_error.clone()
        } else {
            None
        };
        OperationError {
            kind,
            operation_id: self.operation_id.clone(),
            mutation_may_have_occurred: self.mutation_may_have_occurred,
            raw_service_error: raw,
        }
    }

    /// Accepted operation identifier, if reconciliation has observed one.
    pub fn operation_id(&self) -> Option<&str> {
        self.operation_id.as_deref()
    }

    /// Source polling fallbacks observed in this session.
    pub fn warnings(&self) -> &[OperationWarning] {
        &self.warnings
    }

    /// Check the existing cancellation signal between caller-owned phases.
    ///
    /// A pending signal returns immediately. Observed cancellation stays latched,
    /// so its completed future is never polled again. This performs no I/O or
    /// credential work and does not change request budgets or mutation evidence.
    ///
    /// # Errors
    /// Returns cancellation with the session's operation ID and uncertainty.
    pub async fn check_cancelled(&mut self) -> Result<(), OperationError> {
        Self::run(
            &mut self.cancelled,
            &mut self.cancelled_observed,
            std::future::ready(()),
        )
        .await
        .map_err(|kind| self.evidence(kind))
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

    /// Reconcile a caller's already-received 202 without replaying its request.
    ///
    /// Use a fresh session. The external initial request counts against the
    /// request budget and is conservatively marked as possibly mutating.
    /// Only the shared bounded GET polling/result policy runs here.
    ///
    /// # Errors
    /// Rejects non-202 input or a previously used session, enforces the response
    /// byte bound, and preserves known operation ID even on immediate cancellation.
    pub async fn reconcile_accepted(
        &mut self,
        response: OperationResponse,
        completion: CompletionPolicy,
    ) -> Result<OperationResponse, OperationError> {
        let result = async {
            if response.status != 202 || self.requests != 0 || self.mutation_may_have_occurred {
                return Err(OperationErrorKind::InvalidInput);
            }
            self.requests = 1;
            self.mutation_may_have_occurred = true;
            self.operation_id = header(&response, "x-ms-operation-id")?
                .filter(|id| valid_id(id))
                .map(str::to_owned);
            if self.operation_id.is_none() && completion != CompletionPolicy::PythonSynchronous {
                self.operation_id = accepted_id(&response)?;
            }
            Self::run(
                &mut self.cancelled,
                &mut self.cancelled_observed,
                std::future::ready(()),
            )
            .await?;
            if self.selection.audience() != Audience::Fabric {
                return Err(OperationErrorKind::InvalidAudience);
            }
            if self.endpoint.origin() != ServiceOrigin::Fabric {
                return Err(OperationErrorKind::InvalidInput);
            }
            if response.body.len() > self.limits.max_response_bytes.get() {
                return Err(OperationErrorKind::Limit);
            }
            self.resolve_accepted(response, completion, false).await
        }
        .await;
        result.map_err(|kind| self.evidence(kind))
    }

    async fn execute_inner(
        &mut self,
        operation: FabricOperation,
    ) -> Result<OperationResponse, OperationErrorKind> {
        let python = operation.completion == CompletionPolicy::PythonSynchronous;
        let url = service_url(&self.endpoint, &operation.path)?;
        let response = if operation.method == ResourceMethod::Get && !python {
            let response = self
                .request(operation.method, url, operation.body, false)
                .await?;
            ensure_status(&response, &[200, 201, 204])?;
            return Ok(response);
        } else {
            if operation.method != ResourceMethod::Get && self.mutation_may_have_occurred {
                return Err(OperationErrorKind::InvalidInput);
            }
            self.request(
                operation.method,
                url,
                operation.body,
                operation.source_read_operation_ids,
            )
            .await?
        };
        if response.status != 202 {
            ensure_source_status(&response, &[200, 201, 204], python)?;
            return Ok(response);
        }
        self.resolve_accepted(
            response,
            operation.completion,
            operation.source_read_operation_ids,
        )
        .await
    }

    async fn resolve_accepted(
        &mut self,
        mut response: OperationResponse,
        completion: CompletionPolicy,
        source_read_operation_ids: bool,
    ) -> Result<OperationResponse, OperationErrorKind> {
        let python = completion == CompletionPolicy::PythonSynchronous;
        if completion == CompletionPolicy::ReturnAccepted {
            if let Some(id) = accepted_id(&response)? {
                self.operation_id = Some(id.clone());
                if let Some(uri) = header(&response, "Location")? {
                    validate_link(&self.endpoint, uri, &format!("/v1/operations/{id}"))?;
                }
            }
            return Ok(response);
        }
        let mut original = python.then(|| response.clone());
        let id = if python {
            let location = header(&response, "Location")?.unwrap_or("");
            let operation_id = header(&response, "x-ms-operation-id")?;
            if source_read_operation_ids && operation_id.is_some_and(|id| !valid_source_id(id)) {
                return Err(OperationErrorKind::InvalidResponse);
            }
            let id = operation_id.unwrap_or("");
            if !location.is_empty() {
                validate_source_origin(&self.endpoint, location)?;
            }
            if location.contains("/jobs/instances/") || (location.is_empty() && id.is_empty()) {
                return Ok(response);
            }
            if source_read_operation_ids {
                if id.is_empty() {
                    return Err(OperationErrorKind::InvalidResponse);
                }
            } else if !id.is_empty() && !valid_id(id) {
                return Err(OperationErrorKind::InvalidResponse);
            }
            id.to_owned()
        } else {
            accepted_id(&response)?.ok_or(OperationErrorKind::InvalidResponse)?
        };
        let path = format!("/v1/operations/{id}");
        self.operation_id = (!id.is_empty()).then_some(id);
        if !python && let Some(uri) = header(&response, "Location")? {
            validate_link(&self.endpoint, uri, &path)?;
        }
        loop {
            if self.requests >= self.limits.max_requests.get() {
                return Err(OperationErrorKind::Limit);
            }
            let delay = match header(&response, "Retry-After")? {
                Some(value) if python => source_poll_delay(value, &mut self.warnings)?,
                Some(value) => Duration::from_secs(
                    value
                        .parse()
                        .map_err(|_| OperationErrorKind::InvalidResponse)?,
                ),
                None => Duration::from_secs(if python { 10 } else { 1 }),
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
                .request(
                    ResourceMethod::Get,
                    service_url(&self.endpoint, &path)?,
                    Vec::new(),
                    false,
                )
                .await?;
            ensure_source_status(&response, &[200], python)?;
            if python
                && let Some(uri) = header(&response, "Location")?.filter(|uri| !uri.is_empty())
            {
                validate_source_origin(&self.endpoint, uri)?;
            }
            let state: serde_json::Value = serde_json::from_slice(&response.body)
                .map_err(|_| OperationErrorKind::InvalidResponse)?;
            if !state.is_object() {
                return Err(OperationErrorKind::InvalidResponse);
            }
            match state.get("status").and_then(serde_json::Value::as_str) {
                Some("NotStarted" | "Running") if !python => {
                    if let Some(uri) = header(&response, "Location")? {
                        validate_link(&self.endpoint, uri, &path)?;
                    }
                }
                Some("Succeeded") | Some("Completed") if python => {
                    if header(&response, "Location")?
                        .filter(|uri| !uri.is_empty())
                        .is_none()
                    {
                        let mut original =
                            original.take().ok_or(OperationErrorKind::InvalidResponse)?;
                        original.status = 200;
                        return Ok(original);
                    }
                    let result = self
                        .request(
                            ResourceMethod::Get,
                            service_url(&self.endpoint, &format!("{path}/result"))?,
                            Vec::new(),
                            false,
                        )
                        .await?;
                    match ensure_source_status(&result, &[200, 201, 202, 204], true) {
                        Ok(()) => return Ok(result),
                        Err(OperationErrorKind::Http {
                            code: Some(code), ..
                        }) if code == "OperationHasNoResult" => {
                            let mut original =
                                original.take().ok_or(OperationErrorKind::InvalidResponse)?;
                            original.status = 200;
                            return Ok(original);
                        }
                        Err(error) => return Err(error),
                    }
                }
                Some("Succeeded") => {
                    let result_path = format!("{path}/result");
                    if let Some(uri) = header(&response, "Location")? {
                        validate_link(&self.endpoint, uri, &result_path)?;
                    }
                    if completion == CompletionPolicy::WaitForResult {
                        let result = self
                            .request(
                                ResourceMethod::Get,
                                service_url(&self.endpoint, &result_path)?,
                                Vec::new(),
                                false,
                            )
                            .await?;
                        ensure_status(&result, &[200])?;
                        return Ok(result);
                    }
                    return Ok(response);
                }
                Some(status @ ("Failed" | "Cancelled")) => {
                    self.raw_service_error = Some(Box::new(
                        state
                            .get("error")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    ));
                    return Err(OperationErrorKind::OperationFailed {
                        status: status.to_owned(),
                        error: state
                            .get("error")
                            .cloned()
                            .and_then(|error| serde_json::from_value(error).ok())
                            .map(Box::new),
                    });
                }
                _ if python => {}
                _ => return Err(OperationErrorKind::InvalidResponse),
            }
        }
    }

    async fn request(
        &mut self,
        method: ResourceMethod,
        url: Url,
        body: Vec<u8>,
        source_read_operation_ids: bool,
    ) -> Result<OperationResponse, OperationErrorKind> {
        self.raw_service_error = None;
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
        let authentication =
            RequestAuthentication::new(self.credential, self.selection, &self.endpoint);
        let mut execution = SessionExecution {
            executor: &self.executor,
            method,
            body,
            max_response_bytes: self.limits.max_response_bytes,
            requests: &mut self.requests,
            mutation_may_have_occurred: &mut self.mutation_may_have_occurred,
            source_read_operation_ids,
            accepted_operation_id: None,
        };
        let cancelled = &mut self.cancelled;
        let observed = &mut self.cancelled_observed;
        let mut cancelled = pin!(poll_fn(|cx| {
            if *observed {
                return Poll::Ready(());
            }
            let result = cancelled.as_mut().poll(cx);
            if result.is_ready() {
                *observed = true;
            }
            result
        }));
        let response = authentication
            .execute(
                ServiceOrigin::Fabric,
                &url,
                &mut execution,
                cancelled.as_mut(),
            )
            .await;
        if matches!(&response, Err(RequestError::Cancelled))
            && self.operation_id.is_none()
            && let Some(id) = execution.accepted_operation_id.take()
        {
            self.operation_id = Some(id);
        }
        let response = response.map_err(|error| match error {
            RequestError::InvalidAudience => OperationErrorKind::InvalidAudience,
            RequestError::InvalidEndpoint => OperationErrorKind::InvalidInput,
            RequestError::Credential => OperationErrorKind::Credential,
            RequestError::Execution(OperationExecutionError::ResponseLimit) => {
                OperationErrorKind::Limit
            }
            RequestError::Execution(error) => OperationErrorKind::Execution(error),
            RequestError::Cancelled => OperationErrorKind::Cancelled,
        })?;
        if response.body.len() > self.limits.max_response_bytes.get() {
            return Err(OperationErrorKind::Limit);
        }
        if !(200..300).contains(&response.status) {
            self.raw_service_error = serde_json::from_slice(&response.body).ok().map(Box::new);
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

struct SessionExecution<'a, E> {
    executor: &'a E,
    method: ResourceMethod,
    body: Vec<u8>,
    max_response_bytes: NonZeroUsize,
    requests: &'a mut usize,
    mutation_may_have_occurred: &'a mut bool,
    source_read_operation_ids: bool,
    accepted_operation_id: Option<String>,
}

impl<E: OperationExecutor> AuthenticatedExecution for SessionExecution<'_, E> {
    type Response = OperationResponse;
    type Error = OperationExecutionError;

    fn execute<'a>(
        &'a mut self,
        url: &'a Url,
        token: &'a AccessToken,
    ) -> AuthenticatedFuture<'a, Self::Response, Self::Error> {
        Box::pin(async move {
            *self.requests += 1;
            if self.method != ResourceMethod::Get {
                *self.mutation_may_have_occurred = true;
            }
            let response = self
                .executor
                .execute(OperationRequest {
                    method: self.method,
                    url: url.clone(),
                    body: std::mem::take(&mut self.body),
                    token,
                    max_response_bytes: self.max_response_bytes,
                })
                .await?;
            if response.status == 202 {
                self.accepted_operation_id = if self.source_read_operation_ids {
                    header(&response, "x-ms-operation-id")
                        .ok()
                        .flatten()
                        .filter(|id| valid_source_id(id))
                        .map(str::to_owned)
                } else {
                    accepted_id(&response).ok().flatten()
                };
            }
            Ok(response)
        })
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
        raw_service_error: error.raw_service_error,
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

fn validate_source_origin(endpoint: &EndpointBinding, uri: &str) -> Result<(), OperationErrorKind> {
    if uri
        .chars()
        .any(|character| character.is_control() || character == '\\')
    {
        return Err(OperationErrorKind::InvalidLink);
    }
    let url = endpoint
        .base_url()
        .join(uri)
        .map_err(|_| OperationErrorKind::InvalidLink)?;
    endpoint
        .validate_url(&url)
        .map_err(|_| OperationErrorKind::InvalidLink)
}

fn public_operation_url(path: &str) -> Result<Url, OperationErrorKind> {
    let endpoint = EndpointBinding::public(ServiceOrigin::Fabric);
    let url = Url::parse(&format!(
        "{}{path}",
        endpoint.base_url().as_str().trim_end_matches('/')
    ))
    .map_err(|_| OperationErrorKind::InvalidInput)?;
    endpoint
        .validate_url(&url)
        .map_err(|_| OperationErrorKind::InvalidInput)?;
    Ok(url)
}

fn relative_operation_path(url: &Url) -> String {
    format!(
        "{}{}",
        url.path(),
        url.query()
            .map(|query| format!("?{query}"))
            .unwrap_or_default()
    )
}

fn source_poll_delay(
    value: &str,
    warnings: &mut Vec<OperationWarning>,
) -> Result<Duration, OperationErrorKind> {
    let value = value.trim();
    let negative = value.starts_with('-');
    let digits = value
        .strip_prefix('+')
        .or_else(|| value.strip_prefix('-'))
        .unwrap_or(value);
    if !digits.is_empty()
        && digits.as_bytes()[0].is_ascii_digit()
        && digits.as_bytes()[digits.len() - 1].is_ascii_digit()
        && !digits.contains("__")
        && digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'_')
    {
        let seconds = digits
            .replace('_', "")
            .parse::<u64>()
            .map_err(|_| OperationErrorKind::Limit)?;
        if negative && seconds != 0 {
            return Err(OperationErrorKind::InvalidResponse);
        }
        return Ok(Duration::from_secs(seconds));
    }
    warnings.push(OperationWarning::InvalidRetryAfter);
    Ok(Duration::from_secs(10))
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

fn valid_source_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn service_url(endpoint: &EndpointBinding, path: &str) -> Result<Url, OperationErrorKind> {
    endpoint
        .url(path)
        .map_err(|_| OperationErrorKind::InvalidInput)
}

fn validate_link(
    endpoint: &EndpointBinding,
    uri: &str,
    path: &str,
) -> Result<(), OperationErrorKind> {
    endpoint
        .validate_link(uri, path)
        .map(|_| ())
        .map_err(|_| OperationErrorKind::InvalidLink)
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

fn ensure_source_status(
    response: &OperationResponse,
    allowed: &[u16],
    python: bool,
) -> Result<(), OperationErrorKind> {
    if !python || allowed.contains(&response.status) {
        return ensure_status(response, allowed);
    }
    let kind = ensure_status(response, allowed);
    let Err(OperationErrorKind::Http { status, error, .. }) = kind else {
        return kind;
    };
    let code = match status {
        401 | 403 => Some(
            header(response, "x-ms-public-api-error-code")?
                .filter(|code| !code.is_empty())
                .or(header(response, "x-ms-error-code")?.filter(|code| !code.is_empty()))
                .unwrap_or(if status == 401 {
                    "Unauthorized"
                } else {
                    "Forbidden"
                })
                .to_owned(),
        ),
        404 => Some("NotFound".into()),
        _ => serde_json::from_slice::<serde_json::Value>(&response.body)
            .ok()
            .and_then(|value| {
                value
                    .get("errorCode")
                    .and_then(serde_json::Value::as_str)
                    .filter(|code| !code.is_empty())
                    .map(str::to_owned)
            }),
    };
    Err(OperationErrorKind::Http {
        status,
        code,
        error,
    })
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
