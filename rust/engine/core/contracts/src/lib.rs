// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Minimal explicit, injectable HTTP and operation foundations. No ambient identity.
#![forbid(unsafe_code)]

use async_trait::async_trait;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    time::{Duration, Instant, SystemTime},
};
pub use tokio_util::sync::CancellationToken;
use url::Url;

pub mod diagnostics;
mod endpoint;
mod identity;
pub use endpoint::Endpoint;
pub use identity::{AZURE_CLI_LOGIN_REQUIRED, Binding};
mod request_metadata;
pub use request_metadata::{
    AgentKind, CLIENT_REQUEST_ID_HEADER, FABRIC_SKILL_HEADER, RequestCommand, RequestMetadata,
    USER_AGENT_HEADER,
};
mod operation;
pub use operation::{
    OperationHandle, OperationKind, ResponseHeadCapture, operation_handle,
    operation_handle_with_endpoint, resolve_operation_url, resolve_operation_url_with_endpoint,
    validate_operation_url,
};

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
    /// Local diagnostic delivery failed; never retry the HTTP request.
    Diagnostic,
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
/// Service-provided job failure text, for explicit final JSON output only.
/// This can contain sensitive data and must never be copied into diagnostics.
#[derive(Clone)]
pub struct JobFailureReason {
    /// Untrusted service text; serialize as JSON, never as terminal markup.
    pub message: String,
}
impl fmt::Debug for JobFailureReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JobFailureReason([REDACTED])")
    }
}

#[derive(Clone, Debug, Serialize)]
struct ErrorDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    service_code: Option<String>,
    #[serde(skip)]
    diagnostic_outcome: Option<diagnostics::DiagnosticOutcome>,
}

/// Sanitized error message with separately held job failure details for final JSON.
/// Never wrap raw provider or HTTP diagnostics in its message; service-provided
/// error text may be included only after [`sanitize_service_message`].
#[derive(Clone, Debug, Serialize)]
pub struct CoreError {
    /// Machine-readable error category.
    pub kind: ErrorKind,
    /// Safe explanatory message.
    pub message: String,
    /// Accepted-operation recovery evidence.
    pub recovery: Option<RecoveryInfo>,
    /// Raw job failure message, exposed only in the final structured response.
    #[serde(skip)]
    pub failure_reason: Option<JobFailureReason>,
    // Keep uncommon metadata off the shared Result error path.
    #[serde(flatten)]
    details: Option<Box<ErrorDetails>>,
}
impl CoreError {
    /// Construct a safe error.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            recovery: None,
            failure_reason: None,
            details: None,
        }
    }

    /// Construct a reporting failure using fixed, truthful outcome wording.
    pub fn diagnostic_failure(outcome: diagnostics::DiagnosticOutcome) -> Self {
        use diagnostics::{DiagnosticOutcome, OperationOutcome};
        let message: String = match outcome {
            DiagnosticOutcome::BeforeSend => "Diagnostic output failed before sending; no request was sent".into(),
            DiagnosticOutcome::Uncertain => "Diagnostic output failed; a request may have been sent, its outcome is uncertain, and remote work may continue".into(),
            DiagnosticOutcome::Received(status) => format!("Diagnostic output failed after HTTP status {status} was received; remote work was not rolled back and may continue"),
            DiagnosticOutcome::Accepted => "Diagnostic output failed after the operation was accepted; remote work may continue".into(),
            DiagnosticOutcome::Terminal(OperationOutcome::Succeeded) => "Diagnostic output failed after the backend operation succeeded; remote work was not rolled back".into(),
            DiagnosticOutcome::Terminal(OperationOutcome::Failed) => "Diagnostic output failed after the backend operation failed".into(),
            DiagnosticOutcome::Terminal(OperationOutcome::Cancelled) => "Diagnostic output failed after the backend operation was cancelled".into(),
        };
        let mut error = Self::new(ErrorKind::Diagnostic, message);
        error.details = Some(Box::new(ErrorDetails {
            service_code: None,
            diagnostic_outcome: Some(outcome),
        }));
        error
    }

    /// Known remote outcome associated with a local diagnostic failure.
    pub fn diagnostic_outcome(&self) -> Option<diagnostics::DiagnosticOutcome> {
        self.details
            .as_ref()
            .and_then(|details| details.diagnostic_outcome)
    }

    /// Construct a service failure with a bounded identifier instead of arbitrary response text.
    pub fn service_failure(message: impl Into<String>, code: Option<&str>) -> Self {
        let mut error = Self::new(ErrorKind::Service, message);
        if let Some(code) = code {
            if !code.is_empty()
                && code.len() <= 128
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            {
                error.details = Some(Box::new(ErrorDetails {
                    service_code: Some(code.to_owned()),
                    diagnostic_outcome: None,
                }));
            } else {
                error
                    .message
                    .push_str("; invalid service error code omitted");
            }
        }
        error
    }

    /// Validated service error identifier, if one was supplied.
    pub fn service_code(&self) -> Option<&str> {
        self.details
            .as_ref()
            .and_then(|details| details.service_code.as_deref())
    }
}

/// Maximum characters of service-provided error text forwarded to users.
pub const MAX_SERVICE_MESSAGE_CHARS: usize = 512;

/// Make service-provided error text safe to display.
///
/// Removes control and bidirectional-override characters (terminal injection),
/// collapses whitespace, drops URL query strings and fragments (which can carry
/// SAS or other signed tokens), and truncates to [`MAX_SERVICE_MESSAGE_CHARS`].
/// Returns `None` when nothing displayable remains.
pub fn sanitize_service_message(raw: &str) -> Option<String> {
    let mut words = Vec::new();
    let visible: String = raw
        .chars()
        .filter(|c| !is_unsafe_display_char(*c))
        .collect();
    for word in visible.split_whitespace() {
        let lower = word.to_ascii_lowercase();
        match [lower.find("http://"), lower.find("https://")]
            .into_iter()
            .flatten()
            .min()
        {
            Some(start) => {
                let end = word[start..]
                    .find(['?', '#'])
                    .map_or(word.len(), |offset| start + offset);
                words.push(&word[..end]);
            }
            None => words.push(word),
        }
    }
    let collapsed = words.join(" ");
    if collapsed.is_empty() {
        return None;
    }
    if collapsed.chars().count() <= MAX_SERVICE_MESSAGE_CHARS {
        return Some(collapsed);
    }
    let mut truncated: String = collapsed
        .chars()
        .take(MAX_SERVICE_MESSAGE_CHARS - 1)
        .collect();
    truncated.push('…');
    Some(truncated)
}
fn is_unsafe_display_char(c: char) -> bool {
    (c.is_control() && !c.is_whitespace())
        || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}
impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for CoreError {}

/// Caller-owned deadline and cancellation; dropping work never rolls back a mutation.
#[derive(Clone)]
pub struct OperationContext {
    /// Local cancellation signal.
    pub cancellation: CancellationToken,
    /// Optional monotonic deadline; None allows cancellable waiting without a time limit.
    pub deadline: Option<Instant>,
}
impl OperationContext {
    /// Start an action with a deadline derived from an injected monotonic time.
    pub fn new(
        now: Instant,
        timeout: Duration,
        cancellation: CancellationToken,
    ) -> Result<Self, CoreError> {
        let deadline = now.checked_add(timeout).ok_or_else(|| {
            CoreError::new(
                ErrorKind::InvalidInput,
                "Operation timeout exceeds clock range",
            )
        })?;
        Ok(Self::with_deadline(Some(deadline), cancellation))
    }
    /// Create an action with an explicit optional deadline.
    pub fn with_deadline(deadline: Option<Instant>, cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            deadline,
        }
    }
    /// Remaining budget using an injected clock; MAX when unbounded.
    pub fn remaining_at(&self, now: Instant) -> Result<Duration, CoreError> {
        if self.cancellation.is_cancelled() {
            return Err(CoreError::new(
                ErrorKind::Cancelled,
                "Operation cancelled locally; backend work may continue",
            ));
        }
        let Some(deadline) = self.deadline else {
            return Ok(Duration::MAX);
        };
        deadline
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
impl fmt::Debug for OperationContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperationContext")
            .field("cancellation", &self.cancellation)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

/// Enforce deadline and cancellation using the supplied clock.
pub async fn bounded_with_clock<T>(
    clock: &dyn Clock,
    context: &OperationContext,
    action: impl Future<Output = T>,
) -> Result<T, CoreError> {
    context.remaining_at(clock.now())?;
    let timeout = async {
        match context.deadline {
            None => std::future::pending::<Result<(), CoreError>>().await,
            Some(deadline) => clock.wait_until(deadline, context).await,
        }
    };
    tokio::select! {
        biased;
        () = context.cancellation.cancelled() => Err(CoreError::new(
            ErrorKind::Cancelled,
            "Operation cancelled locally; backend work may continue",
        )),
        value = action => {
            context.remaining_at(clock.now())?;
            Ok(value)
        },
        result = timeout => result.and_then(|()| Err(CoreError::new(
            ErrorKind::Timeout,
            "Operation deadline reached; backend work may continue",
        ))),
    }
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
    /// Wait until the deadline and return a timeout once it is reached.
    async fn wait_until(
        &self,
        deadline: Instant,
        context: &OperationContext,
    ) -> Result<(), CoreError>;
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
        clock: &dyn Clock,
    ) -> Result<ApiResponse, CoreError>;
    /// Send once while publishing validated response-head recovery before reading
    /// the body. Streaming transports must override this method; the default is
    /// sufficient only for transports returning an already-materialized response.
    async fn send_with_observer(
        &self,
        request: ApiRequest,
        context: &OperationContext,
        clock: &dyn Clock,
        head: &ResponseHeadCapture,
    ) -> Result<ApiResponse, CoreError> {
        let response = self.send(request, context, clock).await?;
        head.record(response.status, &response.headers)?;
        Ok(response)
    }
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
    validate_url_for_host(url, base.host_str().expect("fixed audience host"))
}
pub(crate) fn validate_url_for_host(url: &Url, host: &str) -> Result<(), CoreError> {
    let invalid = |message: &str| Err(CoreError::new(ErrorKind::InvalidInput, message));
    if url.scheme() != "https" {
        return invalid("URL must use https://");
    }
    if url.host_str() != Some(host) || url.port_or_known_default() != Some(443) {
        return invalid(&format!(
            "URL host does not match the selected audience; expected https://{host}"
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return invalid("URL must not contain embedded credentials");
    }
    if url.fragment().is_some() {
        return invalid("URL must not contain a #fragment");
    }
    if url.as_str().len() > 16 * 1024 {
        return invalid("URL exceeds the 16384-byte limit");
    }
    validate_path(url.path())
}
/// Reject encoded separators and traversal before URL normalization can hide them.
pub fn validate_path(path: &str) -> Result<(), CoreError> {
    let lower = path.to_ascii_lowercase();
    let reason = if path.contains('\\') {
        Some("backslashes are not allowed")
    } else if lower.contains("%2f") || lower.contains("%5c") {
        Some("percent-encoded path separators (%2F, %5C) are not allowed")
    } else if lower.contains("%25") {
        Some("double percent-encoding (%25) is not allowed")
    } else if lower.split('/').any(|p| {
        let decoded = p.replace("%2e", ".");
        decoded == "." || decoded == ".."
    }) {
        Some("'.' and '..' path segments are not allowed")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(CoreError::new(
            ErrorKind::InvalidInput,
            format!("Unsafe URL path: {reason}"),
        )),
        None => Ok(()),
    }
}
/// Validate custom headers, rejecting bearer and transport-control injection.
pub fn validate_headers(headers: &BTreeMap<String, String>) -> Result<(), CoreError> {
    const MAX_HEADER_BYTES: usize = 64 * 1024;
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
        USER_AGENT_HEADER,
        CLIENT_REQUEST_ID_HEADER,
        FABRIC_SKILL_HEADER,
    ];
    let invalid = |message: String| Err(CoreError::new(ErrorKind::InvalidInput, message));
    let mut total = 0usize;
    for (name, value) in headers {
        total = total.saturating_add(name.len()).saturating_add(value.len());
        if name.is_empty() {
            return invalid("Custom HTTP header name must not be empty".to_owned());
        }
        let valid_name = name.len() <= 128
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
        if !valid_name {
            return invalid(
                "Custom HTTP header name contains invalid characters or exceeds 128 bytes"
                    .to_owned(),
            );
        }
        // Header names are validated tokens and safe to echo; values never are.
        if forbidden.contains(&name.to_ascii_lowercase().as_str()) {
            return invalid(format!(
                "Custom HTTP header '{name}' is managed by the CLI and cannot be set"
            ));
        }
        if value.bytes().any(|b| b < 32 && b != b'\t' || b == 127) {
            return invalid(format!(
                "Value of custom HTTP header '{name}' contains control characters"
            ));
        }
        if total > MAX_HEADER_BYTES {
            return invalid(format!(
                "Custom HTTP headers exceed the {MAX_HEADER_BYTES}-byte total limit"
            ));
        }
    }
    Ok(())
}

/// Validate bounded, unambiguous response-head fields without cloning them.
pub fn validate_response_headers(headers: &BTreeMap<String, String>) -> Result<(), CoreError> {
    let mut total = 0usize;
    let mut controls = std::collections::BTreeSet::new();
    for (name, value) in headers {
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
    Ok(())
}

/// Validate bounded response data and unambiguous operation-control headers,
/// including responses returned by an injected transport.
pub fn validate_response(response: &ApiResponse) -> Result<(), CoreError> {
    validate_response_headers(&response.headers)?;
    if response.body.len() > MAX_RESPONSE_BYTES {
        return Err(CoreError::new(
            ErrorKind::Protocol,
            format!("Response exceeds the {MAX_RESPONSE_BYTES}-byte size limit"),
        ));
    }
    Ok(())
}
