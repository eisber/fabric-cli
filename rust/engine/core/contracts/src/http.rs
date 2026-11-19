// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Service-neutral, caller-authenticated HTTP exchanges and response evidence.

use crate::ServiceOrigin;
use azure_core::{
    credentials::AccessToken,
    http::{Method, Url},
};
use std::{collections::BTreeMap, fmt, future::Future, num::NonZeroUsize, pin::Pin};

/// A caller-runtime future; dropping it cancels local observation, not remote work.
pub type HttpFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One bounded exchange with an explicitly selected origin and caller-owned token.
///
/// The executor validates the selected endpoint and headers before dispatch.
/// Opaque token claims cannot be inspected; the caller must acquire the token
/// for the selected origin's audience. No credential provider is selected here.
pub struct HttpRequest<'a> {
    /// Explicit service origin, never inferred from the URL.
    pub origin: ServiceOrigin,
    /// HTTP method; only GET participates in the buffered read retry policy.
    pub method: Method,
    /// URL checked against the executor's selected endpoint.
    pub url: &'a Url,
    /// Caller-acquired access token.
    pub token: &'a AccessToken,
    /// Already serialized request bytes.
    pub body: &'a [u8],
    /// Additional headers, excluding authentication and transport overrides.
    pub headers: &'a [(String, String)],
    /// Finite decoded response-body limit.
    pub max_response_bytes: NonZeroUsize,
}

impl fmt::Debug for HttpRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRequest")
            .field("origin", &self.origin)
            .field("method", &self.method)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

/// Raw executor response, before any service-specific interpretation.
///
/// A status does not imply completed service work. The body is untrusted and
/// bounded by the request; header lookup by a service may be case-insensitive.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    /// HTTP status returned by the executor.
    pub status: u16,
    /// Untrusted response body, subject to the request's byte limit.
    pub body: Vec<u8>,
    /// Response headers, including continuation and service error codes.
    pub headers: BTreeMap<String, String>,
}

/// Sanitized execution failures without URLs, bodies, tokens or provider text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpExecutionError {
    /// A zero deadline or unsupported request configuration was supplied.
    InvalidInput,
    /// The caller's HTTP client or response stream failed.
    Transport,
    /// The overall HTTP deadline elapsed.
    TimedOut,
    /// The decoded response stream exceeded the operation's byte bound.
    ResponseLimit,
    /// A read exhausted its finite retry budget.
    RetriesExhausted,
    /// A retry interval was invalid or exceeded the caller's bound.
    RetryDelay,
}

impl fmt::Display for HttpExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidInput => "invalid HTTP execution configuration",
            Self::Transport => "HTTP transport or response streaming failed",
            Self::TimedOut => "HTTP execution deadline elapsed",
            Self::ResponseLimit => "HTTP response exceeded its byte bound",
            Self::RetriesExhausted => "HTTP read retry budget exhausted",
            Self::RetryDelay => "HTTP retry interval is invalid or exceeds its bound",
        })
    }
}

impl std::error::Error for HttpExecutionError {}

/// Optional bounded response progress, including headers before body suspension.
///
/// Recorders must not log raw provider data. A partial response is not a
/// complete empty body; callers retain this evidence independently of the future.
pub trait ResponseProgress: Send + Sync {
    /// Retain the actual bounded prefix or complete response.
    fn record(&self, response: &HttpResponse, complete: bool) -> Result<(), HttpExecutionError>;
    /// Mark decoded-body overflow without manufacturing a complete body.
    fn body_omitted(&self, response: &HttpResponse) -> Result<(), HttpExecutionError>;
}

/// Injected, service-neutral bounded HTTP execution.
///
/// Implementations must not install a runtime or acquire ambient credentials.
/// Mutations must never be automatically replayed. Dropping the future releases
/// local resources without proving rollback or cancelling remote work.
pub trait HttpExecution: Send + Sync {
    /// Execute a validated exchange while retaining optional partial evidence.
    fn execute_http<'a>(
        &'a self,
        request: HttpRequest<'a>,
        progress: Option<&'a dyn ResponseProgress>,
    ) -> HttpFuture<'a, Result<HttpResponse, HttpExecutionError>>;
}
