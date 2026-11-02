// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit Fabric requests and bounded collection traversal over shared HTTP.
//! No identity provider, HTTP backend, runtime, retry or LRO engine is created.

use azure_core::{
    credentials::AccessToken,
    http::{Method, Url},
};
pub use fabric_core::{Audience, CredentialSelection, TokenCredential};
use fabric_sdk_paging::{PagedValues, PagingErrorKind, PagingLimits, TokenPager};
pub use fabric_sdk_workspaces::workspaces::{
    WorkspaceExecutionError as ExecutionError, WorkspaceListLimits as Limits,
    WorkspaceResponse as Response,
};
use futures_util::future::select;
use serde_json::Value;
use std::{
    future::Future,
    num::NonZeroUsize,
    pin::{Pin, pin},
    sync::{Arc, Mutex},
};
use thiserror::Error;

mod bound;
mod operations;
mod query_trace;
pub use bound::{Origin, RawRequestOptions, RequestBounds};
pub use fabric_sdk_operations::{OperationError, OperationErrorKind};
pub use query_trace::{DispatchedQuery, QueryDispatchTrace};

/// Sanitized failures. Service bodies remain available separately in responses.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum ApiError {
    /// Shared operation failure with ID, exact service data and mutation uncertainty.
    #[error("Fabric operation failed or remains incomplete")]
    Operation(Box<fabric_sdk_operations::OperationError>),
    /// A request may already have reached the service. Never retry it implicitly.
    #[error("mutation completion is uncertain: {0}")]
    MutationUncertain(Box<ApiError>),
    /// The injected adapter does not implement this explicitly bound request surface.
    #[error("the injected executor does not support this bound request")]
    UnsupportedRequest,
    /// A structured service failure. Message is untrusted service data, not a
    /// transport diagnostic, credential or instruction.
    #[error("service returned HTTP {status} ({code})")]
    Service {
        /// Actual failing HTTP status.
        status: u16,
        /// Source normalized error code.
        code: String,
        /// Source error message, retained for command output compatibility.
        message: String,
    },
    /// The selection was not bound to the request audience.
    #[error("request audience does not match the explicit selection")]
    InvalidAudience,
    /// An identifier or request configuration was unsafe.
    #[error("invalid request configuration")]
    InvalidInput,
    /// The selected credential failed; no fallback was tried.
    #[error("credential acquisition failed")]
    Credential,
    /// The caller cancelled; a mutation may already have reached the service.
    #[error("request cancelled; cancellation is not rollback")]
    Cancelled,
    /// Shared HTTP execution failed, without provider text.
    #[error("HTTP execution failed: {0:?}")]
    Execution(ExecutionError),
    /// Service error with Python's normalized code.
    #[error("service returned HTTP {status} ({code})")]
    Http {
        /// Actual status.
        status: u16,
        /// Normalized service code, never a success marker.
        code: String,
    },
    /// An accepted/empty status is not a completed collection or mutation.
    #[error("unsupported completion status {0}")]
    UnsupportedStatus(u16),
    /// Successful bytes were not the required JSON shape.
    #[error("invalid response payload")]
    InvalidResponse,
    /// Response bytes were not JSON, distinct from a valid but malformed schema.
    #[error("invalid JSON format")]
    InvalidJson,
    /// A finite response/page/row bound was exceeded.
    #[error("collection or response limit exceeded")]
    Limit,
    /// Active continuation changed origin/path or repeated a token.
    #[error("invalid or repeated continuation")]
    Continuation,
}

/// One validated public Fabric request. Debug deliberately excludes payload/URL.
#[derive(Clone)]
pub struct Request {
    method: Method,
    url: Url,
    body: Vec<u8>,
    origin: Origin,
    headers: Vec<(String, String)>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl Request {
    /// Constructs a route from individually validated segments under Fabric v1.
    pub fn fabric(method: Method, segments: &[&str]) -> Result<Self, ApiError> {
        let base = "https://api.fabric.microsoft.com/v1/";
        let bound = RequestBounds::default().max_url_bytes.get();
        let raw_len = segments.iter().try_fold(base.len() - 1, |sum, part| {
            sum.checked_add(part.len()).and_then(|n| n.checked_add(1))
        });
        if raw_len.is_none_or(|len| len > bound) {
            return Err(ApiError::Limit);
        }
        if segments.is_empty()
            || segments.iter().any(|s| {
                s.is_empty()
                    || matches!(*s, "." | "..")
                    || s.chars()
                        .any(|c| c.is_control() || matches!(c, '/' | '\\' | '?' | '#'))
            })
        {
            return Err(ApiError::InvalidInput);
        }
        let mut url = Url::parse(base).map_err(|_| ApiError::InvalidInput)?;
        url.path_segments_mut()
            .map_err(|_| ApiError::InvalidInput)?
            .pop_if_empty()
            .extend(segments);
        if url.as_str().len() > bound {
            return Err(ApiError::Limit);
        }
        Ok(Self {
            method,
            url,
            body: Vec::new(),
            origin: Origin::Fabric,
            headers: Vec::new(),
        })
    }

    /// Sets JSON request bytes without inventing a transport or content type.
    pub fn with_json(mut self, body: &Value) -> Result<Self, ApiError> {
        self.body = json_body(body, RequestBounds::default().max_body_bytes)?;
        Ok(self)
    }

    /// Borrows the validated URL. Do not log opaque identifiers or query tokens.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Borrows the body for contract inspection, not logging.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Returns the HTTP method.
    pub fn method(&self) -> Method {
        self.method
    }
}

/// Measures with the shared Python-default encoder before allocating a complete
/// request body. Scalar scratch encoding is finite (at most six times the bound);
/// the final body uses the same escaping/numeric implementation as CLI output.
pub fn json_body(value: &Value, limit: NonZeroUsize) -> Result<Vec<u8>, ApiError> {
    fn add(total: &mut usize, amount: usize, limit: usize) -> Result<(), ApiError> {
        *total = total
            .checked_add(amount)
            .filter(|n| *n <= limit)
            .ok_or(ApiError::Limit)?;
        Ok(())
    }
    fn measure(
        value: &Value,
        total: &mut usize,
        limit: usize,
        depth: usize,
    ) -> Result<(), ApiError> {
        if depth > 128 {
            return Err(ApiError::Limit);
        }
        match value {
            Value::Array(values) => {
                add(total, 2, limit)?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        add(total, 2, limit)?;
                    }
                    measure(value, total, limit, depth + 1)?;
                }
            }
            Value::Object(values) => {
                add(total, 2, limit)?;
                for (index, (key, value)) in values.iter().enumerate() {
                    if key.len() > limit {
                        return Err(ApiError::Limit);
                    }
                    if index > 0 {
                        add(total, 2, limit)?;
                    }
                    add(
                        total,
                        fabric_core_json::json_dumps(&Value::String(key.clone())).len(),
                        limit,
                    )?;
                    add(total, 2, limit)?;
                    measure(value, total, limit, depth + 1)?;
                }
            }
            Value::String(text) if text.len() > limit => return Err(ApiError::Limit),
            value => add(total, fabric_core_json::json_dumps(value).len(), limit)?,
        }
        Ok(())
    }
    measure(value, &mut 0, limit.get(), 0)?;
    Ok(fabric_core_json::json_dumps(value).into_bytes())
}

/// Which source algorithm supplies the next collection request.
#[derive(Clone, Copy, Debug)]
pub enum Paging {
    /// Fabric/OneLake header-or-body token, placed in the query.
    Query,
    /// Catalog search: subsequent bodies contain only token and pageSize.
    Catalog {
        /// Original source page size.
        page_size: usize,
    },
}

/// Fully accumulated rows and exact progress; never an incomplete success.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Collection {
    /// Rows in service order, including duplicates.
    pub rows: Vec<Value>,
    /// Successfully decoded pages.
    pub pages: usize,
    /// Next unconsumed opaque token, if known.
    pub next_token: Option<String>,
    /// Original single-page bytes, or source-compatible merged JSON for mutations
    /// whose Python identity detection deliberately searches the response text.
    pub source_text: String,
}

/// Explicit partial progress on any traversal failure.
#[derive(Clone, Debug, Error, PartialEq)]
#[error("{error}")]
pub struct CollectionError {
    /// Cause, without raw provider diagnostics.
    pub error: ApiError,
    /// Fully decoded progress before the failure.
    pub partial: Box<Collection>,
}

/// Transport-neutral request execution. Implementations must enforce bounds and
/// drop local work on cancellation, disable redirects and mutation replay, and
/// must never choose a credential or create an async runtime.
pub trait Executor: Send + Sync {
    /// Uses the caller's existing bounded asynchronous timer for operation polling.
    /// Adapters without a timer fail explicitly rather than blocking or spinning.
    fn wait(
        &self,
        _: std::time::Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionError>> + Send + '_>> {
        Box::pin(std::future::ready(Err(ExecutionError::Failed)))
    }
    /// Advertises the adapter surface without token acquisition or network I/O.
    /// The original Fabric-only seam cannot silently accept other origins/headers.
    fn supports(&self, request: &Request) -> bool {
        request.origin() == Origin::Fabric && request.headers().is_empty()
    }

    /// Executes a validated Fabric request with its already-acquired Fabric token.
    fn execute<'a>(
        &'a self,
        request: &'a Request,
        token: &'a AccessToken,
        limit: NonZeroUsize,
    ) -> Pin<Box<dyn Future<Output = Result<Response, ExecutionError>> + Send + 'a>>;
}

/// Injected credentials, explicit selection, and the existing shared executor.
#[derive(Clone)]
pub struct ApiClient {
    credential: Arc<dyn TokenCredential>,
    selection: CredentialSelection,
    executor: Arc<dyn Executor>,
    query_trace: Option<QueryDispatchTrace>,
}

impl ApiClient {
    /// Stores dependencies without I/O or discovery. Provider binding is caller-owned.
    pub fn new(
        credential: Arc<dyn TokenCredential>,
        selection: CredentialSelection,
        executor: Arc<dyn Executor>,
    ) -> Self {
        Self {
            credential,
            selection,
            executor,
            query_trace: None,
        }
    }

    /// Opts this client into a caller-owned last-dispatch trace. Ordinary clients
    /// remain stateless; neither requests nor collections inherit traced queries.
    /// Cloning this opted-in client shares the explicitly supplied trace.
    pub fn with_query_dispatch_trace(mut self, trace: QueryDispatchTrace) -> Self {
        self.query_trace = Some(trace);
        self
    }

    /// Executes once logically (GET may retry in the shared executor).
    /// Raw statuses are returned, not converted into successful operations.
    pub async fn send(
        &self,
        request: &Request,
        limit: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Response, ApiError> {
        self.send_observed(request, limit, cancelled, &mut false)
            .await
    }

    async fn send_observed(
        &self,
        request: &Request,
        limit: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
        dispatched: &mut bool,
    ) -> Result<Response, ApiError> {
        let operation = async {
            if self.selection.audience() != request.origin().audience() {
                return Err(ApiError::InvalidAudience);
            }
            if !self.executor.supports(request) {
                return Err(ApiError::UnsupportedRequest);
            }
            let token = self
                .credential
                .get_token(&[request.origin().audience().scope()], None)
                .await
                .map_err(|_| ApiError::Credential)?;
            *dispatched = true;
            if let Some(trace) = &self.query_trace {
                trace.record(request);
            }
            let response = self
                .executor
                .execute(request, &token, limit)
                .await
                .map_err(ApiError::Execution)?;
            if response.body.len() > limit.get() {
                return Err(ApiError::Limit);
            }
            Ok(response)
        };
        cancellable(operation, cancelled).await?
    }

    /// Traverses the named array using one reusable finite paging policy.
    /// Invalid rows, origins, cycles, limits and cancellation retain partial progress.
    pub async fn collect(
        &self,
        request: Request,
        field: &str,
        paging: Paging,
        limits: Limits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Collection, CollectionError> {
        self.collect_with(request, field, paging, limits, cancelled, |response| {
            require_status(response, &[200])?;
            serde_json::from_slice(&response.body).map_err(|_| ApiError::InvalidJson)
        })
        .await
    }

    /// Reuses the bounded traversal with a service-owned raw response decoder.
    /// The decoder must reject incomplete/failing statuses and malformed payloads.
    pub async fn collect_with(
        &self,
        request: Request,
        field: &str,
        paging: Paging,
        limits: Limits,
        cancelled: impl Future<Output = ()> + Send,
        decode: fn(&Response) -> Result<Value, ApiError>,
    ) -> Result<Collection, CollectionError> {
        let pager = TokenPager::new(
            request.url.clone(),
            PagingLimits {
                max_pages: limits.max_pages,
                max_items: limits.max_items,
                max_response_bytes: limits.max_response_bytes,
            },
        )
        .map_err(|_| CollectionError {
            error: ApiError::InvalidInput,
            partial: Box::default(),
        })?
        .with_collection_field(field);
        let trace = Arc::new(Mutex::new(SourceTrace::default()));
        let mut fetched = 0usize;
        let result=pager.collect(|page| {
            let mut next=request.clone();
            let trace=trace.clone();
            let prior=fetched;
            fetched+=1;
            async move {
                match paging {
                    Paging::Query=>next.url=page.url().clone(),
                    Paging::Catalog{page_size} if prior>0=>{
                        let token=page.url().query_pairs().find(|(key,_)|key=="continuationToken")
                            .map(|(_,token)|token.into_owned()).ok_or(ApiError::Continuation)?;
                        next=next.with_json(&serde_json::json!({"continuationToken":token,"pageSize":page_size}))?;
                    }
                    Paging::Catalog{..}=>{}
                }
                let response=self.send(&next,page.max_response_bytes(),std::future::pending()).await?;
                let value=decode(&response)?;
                let rows=value.get(field).and_then(Value::as_array).ok_or(ApiError::InvalidResponse)?;
                if rows.iter().any(|row|!row.is_object()) {return Err(ApiError::InvalidResponse);}
                // Preserve the existing family token-shape error independently of
                // the shared collector's permissive source-falsey policy.
                continuation(&response,&value)?;
                trace.lock().expect("source trace lock").observe(value,&response)?;
                Ok(response)
            }
        },cancelled).await;
        match result {
            Ok(progress) => Ok(collection(progress, &trace)),
            Err(error) => {
                let kind = match error.kind {
                    PagingErrorKind::Fetch(error) => error,
                    PagingErrorKind::UnexpectedStatus(status) => {
                        ApiError::UnsupportedStatus(status)
                    }
                    PagingErrorKind::InvalidResponse => ApiError::InvalidResponse,
                    PagingErrorKind::InvalidContinuation
                    | PagingErrorKind::RepeatedContinuation => ApiError::Continuation,
                    PagingErrorKind::ResponseLimit
                    | PagingErrorKind::PageLimit
                    | PagingErrorKind::ItemLimit => ApiError::Limit,
                    PagingErrorKind::Cancelled => ApiError::Cancelled,
                };
                Err(CollectionError {
                    error: kind,
                    partial: Box::new(collection(error.partial, &trace)),
                })
            }
        }
    }
}

#[derive(Default)]
struct SourceTrace {
    value: Option<Value>,
    text: String,
    prior_text: String,
    pages: usize,
}

impl SourceTrace {
    fn observe(&mut self, value: Value, response: &Response) -> Result<(), ApiError> {
        self.prior_text = std::mem::take(&mut self.text);
        if let Some(merged) = self.value.as_mut() {
            let original = merged.as_object_mut().ok_or(ApiError::InvalidResponse)?;
            for (key, target) in original.iter_mut() {
                if let Some(target) = target.as_array_mut()
                    && let Some(next) = value.get(key).and_then(Value::as_array)
                {
                    target.extend(next.iter().cloned());
                }
            }
            original.remove("continuationToken");
            original.remove("continuationUri");
            original.insert("total_pages".into(), Value::from(1));
            self.text = fabric_core_json::json_dumps(merged);
        } else {
            self.text =
                String::from_utf8(response.body.clone()).map_err(|_| ApiError::InvalidResponse)?;
            self.value = Some(value);
        }
        self.pages += 1;
        Ok(())
    }
}

fn collection(progress: PagedValues, trace: &Mutex<SourceTrace>) -> Collection {
    let trace = trace.lock().expect("source trace lock");
    let source_text = if progress.pages_received == trace.pages {
        trace.text.clone()
    } else {
        trace.prior_text.clone()
    };
    Collection {
        rows: progress.value,
        pages: progress.pages_received,
        next_token: progress.next_token,
        source_text,
    }
}

/// Races any caller-owned operation against cancellation, checking cancellation
/// first and dropping unfinished local work. A remote mutation is not rolled back.
pub async fn cancellable<T>(
    operation: impl Future<Output = T>,
    cancelled: impl Future<Output = ()>,
) -> Result<T, ApiError> {
    let cancelled = pin!(cancelled);
    let operation = pin!(operation);
    match select(cancelled, operation).await {
        futures_util::future::Either::Left(_) => Err(ApiError::Cancelled),
        futures_util::future::Either::Right((result, _)) => Ok(result),
    }
}

fn continuation(response: &Response, value: &Value) -> Result<Option<String>, ApiError> {
    if let Some(value) = header(response, "x-ms-continuation") {
        return Ok((!value.is_empty()).then(|| value.to_owned()));
    }
    match value.get("continuationToken") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(false)) => Ok(None),
        Some(Value::Number(value)) if value.as_f64() == Some(0.0) => Ok(None),
        Some(Value::Array(value)) if value.is_empty() => Ok(None),
        Some(Value::Object(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) => Ok((!value.is_empty()).then(|| value.clone())),
        _ => Err(ApiError::Continuation),
    }
}

/// Case-insensitive response-header lookup.
pub fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Applies source HTTP error-code precedence and rejects accepted-as-complete statuses.
pub fn require_status(response: &Response, statuses: &[u16]) -> Result<(), ApiError> {
    if statuses.contains(&response.status) {
        return Ok(());
    }
    if (200..300).contains(&response.status) {
        return Err(ApiError::UnsupportedStatus(response.status));
    }
    let body = serde_json::from_slice::<Value>(&response.body).ok();
    let code = match response.status {
        401 | 403 => header(response, "x-ms-public-api-error-code")
            .filter(|s| !s.is_empty())
            .or_else(|| header(response, "x-ms-error-code").filter(|s| !s.is_empty()))
            .unwrap_or(if response.status == 401 {
                "Unauthorized"
            } else {
                "Forbidden"
            }),
        404 => "NotFound",
        _ => body
            .as_ref()
            .and_then(|v| v.get("errorCode"))
            .and_then(Value::as_str)
            .unwrap_or("UnknownError"),
    };
    if !matches!(response.status, 401 | 403 | 404)
        && let Some(message) = body
            .as_ref()
            .and_then(|b| b.get("message"))
            .and_then(Value::as_str)
    {
        return Err(ApiError::Service {
            status: response.status,
            code: code.to_owned(),
            message: message.to_owned(),
        });
    }
    Err(ApiError::Http {
        status: response.status,
        code: code.to_owned(),
    })
}
