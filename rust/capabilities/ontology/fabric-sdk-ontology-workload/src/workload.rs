// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Reusable caller-injected private workload execution boundary.

use std::{collections::BTreeMap, fmt, future::Future, pin::Pin};

/// A future driven only by the caller's runtime.
pub type WorkloadFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Logical workload identity, never a hostname or token audience.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkloadIdentity(String);

impl WorkloadIdentity {
    /// Validate an explicit logical identifier; the caller owns its routing binding.
    pub fn new(value: impl Into<String>) -> Result<Self, WorkloadError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err(WorkloadError::invalid("logical workload identity"));
        }
        Ok(Self(value))
    }

    /// Logical identifier supplied by the consumer.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Validated relative route, constructed from unencoded path segments and query pairs.
#[derive(Clone, Eq, PartialEq)]
pub struct WorkloadRoute(String);

impl WorkloadRoute {
    /// Encode each segment and query value once. No URL, authority or continuation URL is accepted.
    pub fn new(segments: &[&str], query: &[(&str, &str)]) -> Result<Self, WorkloadError> {
        if segments.is_empty()
            || segments.iter().any(|s| {
                s.trim().is_empty() || *s == "." || *s == ".." || s.chars().any(char::is_control)
            })
            || query.iter().any(|(k, v)| {
                k.is_empty() || k.chars().any(char::is_control) || v.chars().any(char::is_control)
            })
        {
            return Err(WorkloadError::invalid("relative workload route"));
        }
        let mut route = format!("/{}", segments.iter().map(|s| encode(s)).collect::<Vec<_>>().join("/"));
        for (i, (key, value)) in query.iter().enumerate() {
            route.push(if i == 0 { '?' } else { '&' });
            route.push_str(&encode(key));
            route.push('=');
            route.push_str(&encode(value));
        }
        if route.len() > 32 * 1024 {
            return Err(WorkloadError::invalid("route byte bound"));
        }
        Ok(Self(route))
    }

    /// Encoded relative path and query; not a public Fabric URL.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WorkloadRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WorkloadRoute(<validated private route>)")
    }
}

fn encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    encoded
}

/// Supported single-dispatch HTTP methods.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkloadMethod {
    /// Read a resource.
    Get,
    /// Submit a command or strict create.
    Post,
    /// Apply a schema-checked merge patch.
    Patch,
    /// Delete, reset or unbind according to the endpoint's contract.
    Delete,
}

impl WorkloadMethod {
    /// HTTP method spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

/// Immutable, bounded single-dispatch request. Debug never exposes body or route data.
#[derive(Clone)]
pub struct WorkloadRequest {
    workload: WorkloadIdentity,
    route: WorkloadRoute,
    method: WorkloadMethod,
    content_type: Option<String>,
    body: Vec<u8>,
    max_response_bytes: usize,
}

impl WorkloadRequest {
    /// Validate body/media/bounds before any executor invocation.
    pub fn new(
        workload: WorkloadIdentity,
        route: WorkloadRoute,
        method: WorkloadMethod,
        content_type: Option<&str>,
        body: Vec<u8>,
        max_request_bytes: usize,
        max_response_bytes: usize,
    ) -> Result<Self, WorkloadError> {
        if max_request_bytes == 0 || max_response_bytes == 0 || body.len() > max_request_bytes {
            return Err(WorkloadError::invalid("request/response byte bounds"));
        }
        if content_type.is_some_and(|s| {
            s.len() > 256 || !s.contains('/') || !s.is_ascii() || s.bytes().any(|b| b.is_ascii_control())
        }) || (!body.is_empty() && content_type.is_none())
            || (matches!(method, WorkloadMethod::Get | WorkloadMethod::Delete) && !body.is_empty())
        {
            return Err(WorkloadError::invalid("request method/content type"));
        }
        Ok(Self {
            workload,
            route,
            method,
            content_type: content_type.map(str::to_owned),
            body,
            max_response_bytes,
        })
    }

    /// Caller-bound logical workload, not an ambient endpoint.
    pub fn workload(&self) -> &WorkloadIdentity { &self.workload }
    /// Validated relative workload route.
    pub fn route(&self) -> &WorkloadRoute { &self.route }
    /// Exact method.
    pub fn method(&self) -> WorkloadMethod { self.method }
    /// Exact content type, absent for bodyless reads/deletes.
    pub fn content_type(&self) -> Option<&str> { self.content_type.as_deref() }
    /// Exact serialized body; sensitive caller data, never log implicitly.
    pub fn body(&self) -> &[u8] { &self.body }
    /// Executor MUST enforce this bound during streaming, before buffering more bytes.
    pub fn max_response_bytes(&self) -> usize { self.max_response_bytes }
}

impl fmt::Debug for WorkloadRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkloadRequest")
            .field("workload", &self.workload)
            .field("method", &self.method)
            .field("content_type", &self.content_type)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

/// Observed HTTP response, including rejected and accepted responses without reinterpretation.
#[derive(Clone, Eq, PartialEq)]
pub struct WorkloadResponse {
    /// Exact HTTP status.
    pub status: u16,
    /// Case-insensitive header names. Executor must reject ambiguous duplicate headers.
    pub headers: BTreeMap<String, String>,
    /// Bounded decoded bytes. Error and accepted bodies are retained as well.
    pub body: Vec<u8>,
}

impl WorkloadResponse {
    /// Case-insensitive single header lookup, rejecting ambiguous duplicates.
    pub fn header(&self, name: &str) -> Result<Option<&str>, WorkloadError> {
        let mut values = self.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name));
        let value = values.next().map(|(_, v)| v.as_str());
        if values.next().is_some() {
            return Err(WorkloadError::with_response(
                WorkloadErrorKind::Protocol("duplicate response header"), self.clone(), DispatchState::Unknown,
            ));
        }
        Ok(value)
    }
}

impl fmt::Debug for WorkloadResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkloadResponse").field("status", &self.status).finish_non_exhaustive()
    }
}

/// Evidence about dispatch, not an inferred rollback guarantee.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchState {
    /// Executor proves no submission occurred.
    NotSent,
    /// Service accepted work; not evidence of completion.
    Accepted,
    /// A response proves endpoint completion.
    Completed,
    /// Submission/effects may have occurred; caller must reconcile, never replay automatically.
    Unknown,
}

/// Structured local, transport or provider failure classification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkloadErrorKind {
    /// Invalid input, rejected before dispatch.
    InvalidInput(&'static str),
    /// Unsupported or unavailable capability, rejected before dispatch.
    CapabilityGap(&'static str),
    /// Caller executor failed; details retained only as explicit data.
    Execution,
    /// Provider HTTP error; inspect the retained response.
    Http,
    /// Malformed or unexpected response.
    Protocol(&'static str),
    /// A caller byte/page/item bound was reached.
    Limit,
}

/// Lossless bounded error evidence. Display is deliberately data-free.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkloadError {
    /// Failure classification.
    pub kind: WorkloadErrorKind,
    /// Dispatch/effect certainty; never auto-replay Unknown or Accepted.
    pub dispatch: DispatchState,
    /// Original observed response, when one exists.
    pub response: Option<WorkloadResponse>,
    /// Executor-supplied diagnostic, not implicitly displayed or executed.
    pub detail: Option<String>,
}

impl WorkloadError {
    /// Construct a pre-dispatch validation failure.
    pub fn invalid(reason: &'static str) -> Self {
        Self { kind: WorkloadErrorKind::InvalidInput(reason), dispatch: DispatchState::NotSent, response: None, detail: None }
    }
    /// Preserve response evidence for rejected or malformed results.
    pub fn with_response(kind: WorkloadErrorKind, response: WorkloadResponse, dispatch: DispatchState) -> Self {
        Self { kind, dispatch, response: Some(response), detail: None }
    }
    /// Construct a transport failure with explicit dispatch certainty.
    pub fn execution(dispatch: DispatchState, detail: String) -> Self {
        Self { kind: WorkloadErrorKind::Execution, dispatch, response: None, detail: Some(detail) }
    }
    /// Explicit unsupported capability; no executor invocation.
    pub fn gap(reason: &'static str) -> Self {
        Self { kind: WorkloadErrorKind::CapabilityGap(reason), dispatch: DispatchState::NotSent, response: None, detail: None }
    }
}

impl fmt::Display for WorkloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("private workload request failed or is incomplete; inspect structured evidence")
    }
}
impl std::error::Error for WorkloadError {}

/// Caller-owned private workload executor shared by Ontology and other workload SDKs.
///
/// Resolve the explicit logical identity using a caller-approved binding and identity.
/// Perform exactly ONE dispatch: no implicit retry, redirect, token exchange, polling,
/// sign-in, fallback host, or response following. Bound decoded bytes while streaming.
/// Return every observed HTTP status/body, including 202 and non-2xx, unchanged.
pub trait WorkloadExecutor: Send + Sync {
    /// Dispatch exactly once using the caller's runtime and authentication policy.
    fn execute<'a>(
        &'a self,
        request: &'a WorkloadRequest,
    ) -> WorkloadFuture<'a, Result<WorkloadResponse, WorkloadError>>;
}
