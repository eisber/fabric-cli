// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Validated runtime requests for caller-owned Notebook and Livy execution.
//!
//! These contracts do not select credentials, install a transport, retry a
//! mutation, or create a runtime. Workload authorization remains caller-owned.

use azure_core::http::Url;
use serde_json::Value;
use std::{collections::BTreeMap, fmt, future::Future, pin::Pin};

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
            || base
                .path()
                .split('/')
                .any(|part| matches!(part, "." | ".."))
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
            content_type: "application/json".into(),
            bytes: None,
            headers: BTreeMap::new(),
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
    content_type: String,
    bytes: Option<Vec<u8>>,
    headers: BTreeMap<String, String>,
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
    /// Media type of the request body.
    pub fn content_type(&self) -> &str {
        &self.content_type
    }
    /// Explicit non-JSON bytes; never reserialize these as a JSON string.
    pub fn bytes(&self) -> Option<&[u8]> {
        self.bytes.as_deref()
    }
    /// Provider-specific headers, excluding host-controlled authorization.
    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
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

    /// Attach bounded explicit media bytes to a bodyless write request.
    ///
    /// # Errors
    /// Rejects GET/HEAD, an existing JSON body, invalid media and excess bytes.
    pub fn with_bytes(
        mut self,
        bytes: Vec<u8>,
        content_type: &str,
        max_request_bytes: usize,
    ) -> Result<Self, NativeError> {
        if matches!(self.method, NativeMethod::Get | NativeMethod::Head)
            || self.body.is_some()
            || max_request_bytes == 0
            || content_type.is_empty()
            || content_type.chars().any(char::is_control)
        {
            return Err(NativeError::InvalidInput);
        }
        if bytes.len() > max_request_bytes {
            return Err(NativeError::Limit);
        }
        self.bytes = Some(bytes);
        self.content_type = content_type.into();
        Ok(self)
    }

    /// Add a native protocol header without overriding host security controls.
    ///
    /// Header names are stored case-insensitively; duplicate fields fail.
    ///
    /// # Errors
    /// Rejects invalid/duplicate names, control bytes and transport-owned fields.
    pub fn with_header(mut self, name: &str, value: &str) -> Result<Self, NativeError> {
        let name = name.to_ascii_lowercase();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || value.chars().any(char::is_control)
            || matches!(
                name.as_str(),
                "authorization"
                    | "proxy-authorization"
                    | "cookie"
                    | "host"
                    | "connection"
                    | "transfer-encoding"
                    | "content-length"
                    | "content-type"
                    | "accept"
            )
            || self.headers.contains_key(&name)
        {
            return Err(NativeError::InvalidInput);
        }
        self.headers.insert(name, value.into());
        Ok(self)
    }

    /// Execute once through the caller's authenticated and bounded HTTP stack.
    ///
    /// The actual status/body remain available for capability-specific receipt
    /// decoding. A successful request can still report accepted or failed work.
    ///
    /// # Errors
    /// Rejects a zero response bound without dispatch. Preserves caller failures
    /// and rejects an executor response that exceeded the promised byte limit.
    pub async fn execute(
        &self,
        executor: &dyn NativeExecutor,
        max_response_bytes: usize,
    ) -> Result<NativeResponse, NativeError> {
        if max_response_bytes == 0 {
            return Err(NativeError::InvalidInput);
        }
        let response = executor.execute(self, max_response_bytes).await?;
        if response.body.len() > max_response_bytes {
            return Err(NativeError::Limit);
        }
        Ok(response)
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
    /// Actual bounded response headers; values can be sensitive and must not be logged.
    pub headers: BTreeMap<String, String>,
}

impl NativeResponse {
    /// Read one case-insensitive header without accepting ambiguous duplicates.
    ///
    /// This exposes observed metadata, not a concurrent-write guarantee.
    ///
    /// # Errors
    /// Rejects duplicate case variants, control bytes or excess header metadata.
    pub fn header(&self, name: &str) -> Result<Option<&str>, NativeError> {
        if self.headers.len() > 64 {
            return Err(NativeError::Limit);
        }
        let mut bytes = 0_usize;
        let mut result = None;
        for (key, value) in &self.headers {
            bytes = bytes
                .checked_add(key.len())
                .and_then(|bytes| bytes.checked_add(value.len()))
                .ok_or(NativeError::Limit)?;
            if bytes > 16 * 1024 {
                return Err(NativeError::Limit);
            }
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
                || value.chars().any(char::is_control)
            {
                return Err(NativeError::InvalidResponse);
            }
            if key.eq_ignore_ascii_case(name) {
                if result.is_some() {
                    return Err(NativeError::InvalidResponse);
                }
                result = Some(value.as_str());
            }
        }
        Ok(result)
    }
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

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn endpoint() -> NativeEndpoint {
        NativeEndpoint::new(
            Url::parse("https://example.test/v1/workspaces/workspace/lakehouses/lake/livyApi/versions/2024-04-01")
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn exact_owner_segments_and_query_values_cannot_change_route() {
        let request = endpoint()
            .request(
                NativeMethod::Get,
                &["sessions", "owner id", "statements", "1"],
                &[("parts", "1&other=wrong".into())],
                None,
                NativeEffect::Read,
            )
            .unwrap();
        assert!(
            request
                .url()
                .path()
                .ends_with("/sessions/owner%20id/statements/1")
        );
        assert_eq!(
            request.url().query_pairs().collect::<Vec<_>>(),
            vec![("parts".into(), "1&other=wrong".into())]
        );
    }

    #[test]
    fn invalid_inputs_fail_before_execution() {
        for id in ["", ".", "..", "a/b", "a\\b", "a%2fb", "a?b", "a#b", "a\0b"] {
            assert!(
                endpoint()
                    .request(
                        NativeMethod::Get,
                        &["sessions", id],
                        &[],
                        None,
                        NativeEffect::Read
                    )
                    .is_err()
            );
        }
        for body in [json!([]), json!(null), json!("not-object"), json!({})] {
            assert!(
                endpoint()
                    .request(
                        NativeMethod::Get,
                        &["sessions"],
                        &[],
                        Some(body),
                        NativeEffect::Read
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn endpoints_reject_credentials_and_non_loopback_plain_http() {
        for value in [
            "http://example.test",
            "https://user:password@example.test",
            "https://example.test?account=wrong",
            "https://example.test#fragment",
            "file:///sessions",
            "https://example.test/a%2fb",
        ] {
            assert!(NativeEndpoint::new(Url::parse(value).unwrap()).is_err());
        }
        for value in [
            "http://localhost:9000",
            "http://127.0.0.1:9000",
            "http://[::1]:9000",
        ] {
            assert!(NativeEndpoint::new(Url::parse(value).unwrap()).is_ok());
        }
    }

    #[test]
    fn diagnostics_never_print_code_or_scoped_identifiers() {
        let request = endpoint()
            .request(
                NativeMethod::Post,
                &["sessions", "private-owner", "statements"],
                &[],
                Some(json!({"code": "private-code"})),
                NativeEffect::Execute,
            )
            .unwrap();
        let diagnostic = format!("{request:?}");
        for private in ["private-owner", "private-code", "example.test", "workspace"] {
            assert!(!diagnostic.contains(private));
        }
        assert!(
            request
                .clone()
                .with_accept("application/json\r\nunsafe: yes")
                .is_err()
        );
        assert_eq!(
            request
                .with_accept("application/vnd.livy.arrow-chunks")
                .unwrap()
                .accept(),
            "application/vnd.livy.arrow-chunks"
        );
    }

    #[test]
    fn binary_media_and_native_headers_remain_explicit_and_bounded() {
        let request = endpoint()
            .request(
                NativeMethod::Put,
                &["filesystem", "workdir", "image.png"],
                &[],
                None,
                NativeEffect::Update,
            )
            .unwrap()
            .with_bytes(vec![0, 255, 0], "image/png", 3)
            .unwrap()
            .with_header("Ms-FileSystem-Entry-Type", "file")
            .unwrap();
        assert_eq!(request.bytes(), Some([0, 255, 0].as_slice()));
        assert_eq!(request.content_type(), "image/png");
        assert!(request.body().is_none());
        assert!(
            request
                .clone()
                .with_bytes(vec![0; 4], "image/png", 3)
                .is_err()
        );
        assert!(
            request
                .clone()
                .with_header("ms-filesystem-entry-type", "folder")
                .is_err()
        );
        for header in [
            "Host",
            "Authorization",
            "content-Length",
            "bad header",
            "Accept",
        ] {
            assert!(request.clone().with_header(header, "value").is_err());
        }
        assert!(request.with_header("safe-name", "bad\r\nheader").is_err());
    }

    struct Executor {
        calls: AtomicUsize,
        fail: bool,
        size: usize,
    }

    impl NativeExecutor for Executor {
        fn execute<'a>(
            &'a self,
            _: &'a NativeRequest,
            _: usize,
        ) -> Pin<Box<dyn Future<Output = Result<NativeResponse, NativeError>> + Send + 'a>>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                if self.fail {
                    return Err(NativeError::OutcomeUnknown);
                }
                Ok(NativeResponse {
                    status: 202,
                    body: vec![0; self.size],
                    content_type: Some("application/json".into()),
                    headers: BTreeMap::new(),
                })
            })
        }
    }

    #[test]
    fn observed_response_metadata_is_bounded_and_case_insensitive() {
        let mut response = NativeResponse {
            status: 200,
            body: Vec::new(),
            content_type: None,
            headers: BTreeMap::from([("ETag".into(), "\"opaque\"".into())]),
        };
        assert_eq!(response.header("etag").unwrap(), Some("\"opaque\""));
        assert_eq!(response.header("retry-after").unwrap(), None);
        response.headers.insert("etag".into(), "\"other\"".into());
        assert_eq!(response.header("etag"), Err(NativeError::InvalidResponse));
        response.headers.clear();
        response.headers.insert("ETag".into(), "\r\nunsafe".into());
        assert_eq!(response.header("etag"), Err(NativeError::InvalidResponse));
        response
            .headers
            .insert("ETag".into(), "x".repeat(16 * 1024));
        assert_eq!(response.header("etag"), Err(NativeError::Limit));
    }

    #[tokio::test]
    async fn dispatch_once_preserves_acceptance_and_unknown_outcomes() {
        let request = endpoint()
            .request(
                NativeMethod::Post,
                &["sessions"],
                &[],
                Some(json!({})),
                NativeEffect::Create,
            )
            .unwrap();
        let executor = Executor {
            calls: AtomicUsize::new(0),
            fail: false,
            size: 2,
        };
        assert!(request.execute(&executor, 0).await.is_err());
        assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
        assert_eq!(request.execute(&executor, 2).await.unwrap().status, 202);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            request.execute(&executor, 1).await,
            Err(NativeError::Limit)
        ));
        let failed = Executor {
            calls: AtomicUsize::new(0),
            fail: true,
            size: 0,
        };
        assert!(matches!(
            request.execute(&failed, 2).await,
            Err(NativeError::OutcomeUnknown)
        ));
        assert_eq!(failed.calls.load(Ordering::SeqCst), 1);
    }
}
