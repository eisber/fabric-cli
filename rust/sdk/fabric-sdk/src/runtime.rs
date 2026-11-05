// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Validated runtime requests for caller-owned Notebook and Livy execution.
//!
//! These contracts do not select credentials, install a transport, retry a
//! mutation, or create a runtime. Workload authorization remains caller-owned.

use azure_core::http::Url;
use serde_json::Value;
use std::{fmt, future::Future, pin::Pin};

/// A runtime HTTP method, independent of any CLI spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMethod {
    /// Observe a resource.
    Get,
    /// Submit an operation or invoke a provider-defined read.
    Post,
    /// Replace or create according to the native endpoint contract.
    Put,
    /// Modify declared fields.
    Patch,
    /// Delete or terminate according to the native endpoint contract.
    Delete,
    /// Inspect response metadata.
    Head,
}

/// Declared remote effect; acceptance is never proof of completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeEffect {
    /// Read existing metadata or results.
    Read,
    /// Create a resource.
    Create,
    /// Modify persistent configuration or content.
    Update,
    /// Delete a resource.
    Delete,
    /// Execute code, potentially consuming capacity and changing data.
    Execute,
    /// Request remote cancellation.
    Cancel,
    /// Control a session or kernel lifecycle.
    Lifecycle,
}

/// Sanitized request-validation and execution failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeError {
    /// Invalid endpoint, identifier, payload, header or bound.
    InvalidInput,
    /// The chosen provider/profile does not advertise this capability.
    UnsupportedCapability,
    /// A finite request or response bound was exceeded.
    Limit,
    /// The caller stopped local observation; remote work may remain active.
    Cancelled,
    /// The local deadline expired; no cancellation or replay is implied.
    TimedOut,
    /// The service returned an unexpected response.
    InvalidResponse,
    /// A dispatched mutation has no authoritative outcome.
    OutcomeUnknown,
    /// A native HTTP request failed with its actual status.
    Http(u16),
}

impl fmt::Display for NativeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidInput => "invalid runtime request",
            Self::UnsupportedCapability => "runtime capability is unsupported",
            Self::Limit => "runtime bound exceeded",
            Self::Cancelled => "local runtime observation cancelled",
            Self::TimedOut => "runtime deadline expired",
            Self::InvalidResponse => "invalid runtime response",
            Self::OutcomeUnknown => "remote mutation outcome is unknown",
            Self::Http(_) => "runtime HTTP request failed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for NativeError {}

/// One explicit service base, supplied by a caller rather than discovered.
#[derive(Clone, Eq, PartialEq)]
pub struct NativeEndpoint {
    base: Url,
}

impl NativeEndpoint {
    /// Validate an HTTPS base, or an HTTP loopback base for offline transports.
    ///
    /// # Errors
    /// Rejects credentials, queries, fragments, ambiguous paths and non-loopback HTTP.
    pub fn new(base: Url) -> Result<Self, NativeError> {
        let loopback = base.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
        if base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || !(base.scheme() == "https" || (base.scheme() == "http" && loopback))
            || base.path().contains(['\\', '%'])
            || base.path().split('/').any(|part| matches!(part, "." | ".."))
        {
            return Err(NativeError::InvalidInput);
        }
        Ok(Self { base })
    }

    /// Borrow the caller-selected base URL.
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// Build an exact owner-scoped request using independently validated segments.
    ///
    /// # Errors
    /// Rejects route-changing identifiers before transport or credential access.
    pub fn request(
        &self,
        method: NativeMethod,
        segments: &[&str],
        query: &[(&str, String)],
        body: Option<Value>,
        effect: NativeEffect,
    ) -> Result<NativeRequest, NativeError> {
        for segment in segments {
            validate_segment(segment)?;
        }
        if body.as_ref().is_some_and(|value| !value.is_object())
            || (matches!(method, NativeMethod::Get | NativeMethod::Head) && body.is_some())
            || query.iter().any(|(name, value)| {
                name.is_empty()
                    || name.chars().any(char::is_control)
                    || value.chars().any(char::is_control)
            })
        {
            return Err(NativeError::InvalidInput);
        }
        let mut url = self.base.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| NativeError::InvalidInput)?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(name, value)| (*name, value.as_str())));
        }
        Ok(NativeRequest {
            method,
            url,
            body,
            effect,
            accept: "application/json".into(),
        })
    }
}

impl fmt::Debug for NativeEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NativeEndpoint(<caller-selected>)")
    }
}

/// Reject route separators, escapes, controls and dot segments in an opaque ID.
///
/// # Errors
/// Returns [`NativeError::InvalidInput`] without echoing supplied values.
pub fn validate_segment(value: &str) -> Result<(), NativeError> {
    if value.is_empty()
        || matches!(value, "." | "..")
        || value.contains(['/', '\\', '%', '?', '#'])
        || value.chars().any(char::is_control)
    {
        return Err(NativeError::InvalidInput);
    }
    Ok(())
}

/// A validated JSON or bodyless request; Debug never prints code or resource IDs.
#[derive(Clone, PartialEq)]
pub struct NativeRequest {
    method: NativeMethod,
    url: Url,
    body: Option<Value>,
    effect: NativeEffect,
    accept: String,
}

impl NativeRequest {
    /// Actual native method, not the user-facing verb.
    pub const fn method(&self) -> NativeMethod {
        self.method
    }
    /// Exact selected endpoint and scoped route.
    pub fn url(&self) -> &Url {
        &self.url
    }
    /// Provider-native JSON body, including executable code when supplied.
    pub fn body(&self) -> Option<&Value> {
        self.body.as_ref()
    }
    /// Declared remote effect.
    pub const fn effect(&self) -> NativeEffect {
        self.effect
    }
    /// Explicit advertised response media type.
    pub fn accept(&self) -> &str {
        &self.accept
    }
    /// Select a declared response representation without changing the route.
    ///
    /// # Errors
    /// Rejects empty values and header-injection characters.
    pub fn with_accept(mut self, accept: &str) -> Result<Self, NativeError> {
        if accept.is_empty() || accept.chars().any(char::is_control) {
            return Err(NativeError::InvalidInput);
        }
        self.accept = accept.into();
        Ok(self)
    }
}

impl fmt::Debug for NativeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeRequest")
            .field("method", &self.method)
            .field("effect", &self.effect)
            .field("target", &"<scoped>")
            .field("body", &"<redacted>")
            .finish()
    }
}

/// Bounded caller-supplied response; raw payloads are not suitable for logging.
pub struct NativeResponse {
    /// Actual native HTTP status.
    pub status: u16,
    /// Bytes read within the caller's response budget.
    pub body: Vec<u8>,
    /// Provider response media type.
    pub content_type: Option<String>,
}

/// Execution seam for the host's existing authenticated HTTP stack.
///
/// Implementations must bind authorization to the selected service, enforce
/// deadlines/streaming byte limits, reject redirects and never replay mutations.
/// No credential discovery, transport, retry engine or async runtime is installed.
pub trait NativeExecutor: Send + Sync {
    /// Execute this validated request exactly once with the caller's byte bound.
    fn execute<'a>(
        &'a self,
        request: &'a NativeRequest,
        max_response_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<NativeResponse, NativeError>> + Send + 'a>>;
}
