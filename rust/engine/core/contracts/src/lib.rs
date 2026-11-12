// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Minimal explicit, injectable HTTP and operation foundations. No ambient identity.
#![forbid(unsafe_code)]

use async_trait::async_trait;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fmt,
    time::{Duration, Instant, SystemTime},
};
pub use tokio_util::sync::CancellationToken;
use url::Url;

/// Maximum encoded request allocation.
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
/// Maximum wire and decoded response allocation.
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// Fixed service audience; Power BI deliberately uses the Fabric token scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Audience {
    /// Fabric REST.
    Fabric,
    /// OneLake.
    Storage,
    /// Azure Resource Manager.
    Azure,
    /// Power BI REST.
    PowerBi,
}
impl Audience {
    /// Fixed public API base URL.
    pub fn base_url(self) -> &'static str {
        match self {
            Self::Fabric => "https://api.fabric.microsoft.com/v1",
            Self::Storage => "https://onelake.dfs.fabric.microsoft.com",
            Self::Azure => "https://management.azure.com",
            Self::PowerBi => "https://api.powerbi.com/v1.0/myorg",
        }
    }
    /// Fixed credential scope.
    pub fn scope(self) -> &'static str {
        match self {
            Self::Fabric | Self::PowerBi => "https://api.fabric.microsoft.com/.default",
            Self::Storage => "https://storage.azure.com/.default",
            Self::Azure => "https://management.azure.com/.default",
        }
    }
}

/// Stable safe error category.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum ErrorKind {
    /// Input rejected before authentication.
    InvalidInput,
    /// Credential acquisition or binding failed.
    Unauthorized,
    /// Uncertain network failure.
    Transport,
    /// Unexpected service protocol.
    Protocol,
    /// Local deadline reached, not backend cancellation.
    Timeout,
    /// Local cancellation, not backend cancellation.
    Cancelled,
    /// Terminal service failure.
    Service,
}
/// Safe evidence for manually recovering an already accepted operation.
#[derive(Clone, Debug, Serialize)]
pub struct RecoveryInfo {
    /// Validated operation identifier, when available.
    pub operation_id: Option<String>,
    /// Validated status URL with query omitted to avoid disclosing secrets.
    pub status_url: String,
    /// Backend completion is not established by this error.
    pub outcome_uncertain: bool,
}
/// Sanitized error; never wrap raw provider or HTTP diagnostics in its message.
#[derive(Clone, Debug, Serialize)]
pub struct CoreError {
    /// Machine-readable error category.
    pub kind: ErrorKind,
    /// Safe explanatory message.
    pub message: String,
    /// Accepted-operation recovery evidence.
    pub recovery: Option<RecoveryInfo>,
}
impl CoreError {
    /// Construct a safe error.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            recovery: None,
        }
    }
}
impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for CoreError {}

/// Caller-owned deadline and cancellation; dropping work never rolls back a mutation.
#[derive(Clone, Debug)]
pub struct OperationContext {
    /// Local cancellation signal.
    pub cancellation: CancellationToken,
    /// Monotonic action deadline.
    pub deadline: Instant,
}
impl OperationContext {
    /// Start a real-clock bounded action.
    pub fn new(timeout: Duration, cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            deadline: Instant::now() + timeout,
        }
    }
    /// Remaining real-clock budget, also checking cancellation.
    pub fn remaining(&self) -> Result<Duration, CoreError> {
        self.remaining_at(Instant::now())
    }
    /// Remaining budget using an injected clock.
    pub fn remaining_at(&self, now: Instant) -> Result<Duration, CoreError> {
        if self.cancellation.is_cancelled() {
            return Err(CoreError::new(
                ErrorKind::Cancelled,
                "Operation cancelled locally; backend work may continue",
            ));
        }
        self.deadline
            .checked_duration_since(now)
            .filter(|d| !d.is_zero())
            .ok_or_else(|| {
                CoreError::new(
                    ErrorKind::Timeout,
                    "Operation deadline reached; backend work may continue",
                )
            })
    }
}
/// Secret bearer token with expiry; Debug intentionally redacts the token.
pub struct AccessToken {
    /// Sensitive bearer value; never log.
    pub token: String,
    /// Provider expiry.
    pub expires_on: SystemTime,
}
impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken([REDACTED])")
    }
}
/// Explicitly injected identity; libraries never discover credentials.
#[async_trait]
pub trait TokenCredential: Send + Sync {
    /// Acquire a scope-bound token within the caller's remaining deadline.
    async fn token(
        &self,
        scope: &str,
        context: &OperationContext,
    ) -> Result<AccessToken, CoreError>;
}
/// Fully materialized bounded HTTP request. Debug is deliberately redacted.
#[derive(Clone)]
pub struct ApiRequest {
    /// Uppercase HTTP method.
    pub method: String,
    /// Validated service URL.
    pub url: Url,
    /// Caller headers before execution; executor adds authorization.
    pub headers: BTreeMap<String, String>,
    /// Encoded payload, including any multipart framing.
    pub body: Vec<u8>,
}
impl fmt::Debug for ApiRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiRequest([REDACTED])")
    }
}
/// Actual HTTP response; raw unsuccessful initial statuses remain inspectable.
#[derive(Clone)]
pub struct ApiResponse {
    /// Actual HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: BTreeMap<String, String>,
    /// Bounded decoded bytes.
    pub body: Vec<u8>,
}
impl ApiResponse {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}
impl fmt::Debug for ApiResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ApiResponse(status={})", self.status)
    }
}
/// Injectable monotonic clock, wall clock, and cancellation-aware sleep.
#[async_trait]
pub trait Clock: Send + Sync {
    /// Monotonic current time.
    fn now(&self) -> Instant;
    /// Wall time for HTTP-date interpretation.
    fn wall_now(&self) -> SystemTime;
    /// Optional caller-owned safe diagnostics sink, never raw service headers.
    fn diagnostic(&self, _message: &'static str) {}
    /// Sleep no longer than the context budget; never poll early.
    async fn sleep(&self, delay: Duration, context: &OperationContext) -> Result<(), CoreError>;
}
/// Injected one-attempt native or synthetic transport, without credentials.
#[async_trait]
pub trait HttpTransport: Send + Sync {
    /// Send once, bounded by the supplied context.
    async fn send(
        &self,
        request: ApiRequest,
        context: &OperationContext,
    ) -> Result<ApiResponse, CoreError>;
}
/// Authenticated execution shared by raw API and LRO.
#[async_trait]
pub trait HttpExecutor: Send + Sync {
    /// Execute exactly once, including initial GET requests and raw HTTP failures.
    async fn execute(
        &self,
        request: ApiRequest,
        audience: Audience,
        context: &OperationContext,
    ) -> Result<ApiResponse, CoreError>;
    /// Execute a safe polling/result GET, optionally retrying within the same budget.
    async fn execute_poll(
        &self,
        request: ApiRequest,
        audience: Audience,
        context: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        self.execute(request, audience, context).await
    }
}

/// Reject unsafe origins, credentials, fragments, and URL traversal before authentication.
pub fn validate_url(audience: Audience, url: &Url) -> Result<(), CoreError> {
    let base = Url::parse(audience.base_url()).expect("fixed audience URL");
    if url.scheme() != "https"
        || url.host_str() != base.host_str()
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.as_str().len() > 16 * 1024
    {
        return Err(CoreError::new(
            ErrorKind::InvalidInput,
            "URL must use the selected service HTTPS origin without credentials or fragments",
        ));
    }
    validate_path(url.path())
}
/// Reject encoded separators and traversal before URL normalization can hide them.
pub fn validate_path(path: &str) -> Result<(), CoreError> {
    let lower = path.to_ascii_lowercase();
    if path.contains('\\')
        || lower.contains("%2f")
        || lower.contains("%5c")
        || lower.contains("%25")
        || lower.split('/').any(|p| {
            let decoded = p.replace("%2e", ".");
            decoded == "." || decoded == ".."
        })
    {
        return Err(CoreError::new(ErrorKind::InvalidInput, "Unsafe URL path"));
    }
    Ok(())
}
/// Validate custom headers, rejecting bearer and transport-control injection.
pub fn validate_headers(headers: &BTreeMap<String, String>) -> Result<(), CoreError> {
    let forbidden = [
        "authorization",
        "proxy-authorization",
        "host",
        "connection",
        "content-length",
        "transfer-encoding",
        "upgrade",
        "te",
        "trailer",
        "keep-alive",
        "proxy-connection",
        "expect",
    ];
    let mut total = 0usize;
    for (name, value) in headers {
        total = total.saturating_add(name.len()).saturating_add(value.len());
        if forbidden.contains(&name.to_ascii_lowercase().as_str())
            || name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || value.bytes().any(|b| b < 32 && b != b'\t' || b == 127)
            || total > 64 * 1024
        {
            return Err(CoreError::new(
                ErrorKind::InvalidInput,
                "Unsafe or oversized custom HTTP headers",
            ));
        }
    }
    Ok(())
}

/// Validate bounded response data and unambiguous operation-control headers,
/// including responses returned by an injected transport.
pub fn validate_response(response: &ApiResponse) -> Result<(), CoreError> {
    let mut total = 0usize;
    let mut controls = std::collections::BTreeSet::new();
    for (name, value) in &response.headers {
        total = total.saturating_add(name.len()).saturating_add(value.len());
        let lower = name.to_ascii_lowercase();
        if total > 64 * 1024
            || name.is_empty()
            || name.bytes().any(|b| b <= 32 || b >= 127)
            || value.bytes().any(|b| b < 32 && b != b'\t' || b == 127)
            || ([
                "location",
                "azure-asyncoperation",
                "x-ms-operation-id",
                "retry-after",
                "content-encoding",
            ]
            .contains(&lower.as_str())
                && !controls.insert(lower))
        {
            return Err(CoreError::new(
                ErrorKind::Protocol,
                "Malformed, duplicate, or oversized HTTP response headers",
            ));
        }
    }
    if response.body.len() > MAX_RESPONSE_BYTES {
        return Err(CoreError::new(
            ErrorKind::Protocol,
            "Response exceeds allocation bound",
        ));
    }
    Ok(())
}
