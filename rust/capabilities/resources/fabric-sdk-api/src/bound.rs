// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{ApiError, Audience, Request};
use azure_core::http::{Method, Url};
use std::num::NonZeroUsize;

/// Finite public source origin/audience pairs. No arbitrary host inference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    /// Fabric v1, with Fabric scope.
    Fabric,
    /// OneLake DFS, with storage scope.
    OneLake,
    /// Azure Resource Manager, with ARM scope.
    Arm,
    /// Power BI v1.0/myorg, explicitly preserving Python's Fabric scope.
    PowerBi,
}

impl Origin {
    /// Required explicit credential audience. Power BI intentionally uses the
    /// pinned Python Fabric scope; this is not an inferred token fallback.
    pub const fn audience(self) -> Audience {
        match self {
            Self::Fabric | Self::PowerBi => Audience::Fabric,
            Self::OneLake => Audience::OneLake,
            Self::Arm => Audience::Arm,
        }
    }

    /// Fixed HTTPS origin, including the source API prefix for relative endpoints.
    pub const fn base_url(self) -> &'static str {
        match self {
            Self::Fabric => "https://api.fabric.microsoft.com/v1/",
            Self::OneLake => "https://onelake.dfs.fabric.microsoft.com/",
            Self::Arm => "https://management.azure.com/",
            Self::PowerBi => "https://api.powerbi.com/v1.0/myorg/",
        }
    }
}

/// Finite allocation/request bounds, validated before token acquisition.
#[derive(Clone, Copy, Debug)]
pub struct RequestBounds {
    /// Maximum serialized URL bytes.
    pub max_url_bytes: NonZeroUsize,
    /// Maximum number of supplied header pairs before deduplication.
    pub max_headers: NonZeroUsize,
    /// Maximum combined header name/value/wire-separator bytes.
    pub max_header_bytes: NonZeroUsize,
    /// Maximum request body bytes.
    pub max_body_bytes: NonZeroUsize,
}

impl Default for RequestBounds {
    fn default() -> Self {
        Self {
            max_url_bytes: NonZeroUsize::new(16 * 1024).unwrap(),
            max_headers: NonZeroUsize::new(64).unwrap(),
            max_header_bytes: NonZeroUsize::new(16 * 1024).unwrap(),
            max_body_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
        }
    }
}

/// Raw request arguments after the command layer parses key/value pairs.
#[derive(Clone, Debug)]
pub struct RawRequestOptions {
    /// Explicit source audience/origin selector.
    pub origin: Origin,
    /// Source-supported method.
    pub method: Method,
    /// Relative endpoint; arbitrary hosts and credentials are rejected.
    pub endpoint: String,
    /// Source `-P` query pairs. URI query values override these in place.
    pub query: Vec<(String, String)>,
    /// Custom non-authentication headers, checked case-insensitively.
    pub headers: Vec<(String, String)>,
    /// Already-prepared input bytes; no filesystem or multipart bridge is implicit.
    pub body: Vec<u8>,
    /// Finite per-request bounds.
    pub bounds: RequestBounds,
}

fn put(pairs: &mut Vec<(String, String)>, key: String, value: String) {
    if let Some(pair) = pairs.iter_mut().find(|p| p.0 == key) {
        pair.1 = value;
    } else {
        pairs.push((key, value));
    }
}

fn quote_plus(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut result = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(char::from(byte))
            }
            b' ' => result.push('+'),
            byte => {
                result.push('%');
                result.push(char::from(HEX[usize::from(byte >> 4)]));
                result.push(char::from(HEX[usize::from(byte & 15)]));
            }
        }
    }
    result
}

impl Request {
    /// Adds ordered source query parameters, encoding opaque values exactly once.
    /// Duplicate keys replace their value in place; URLs remain finitely bounded.
    pub fn with_query(mut self, query: &[(String, String)]) -> Result<Self, ApiError> {
        let raw = query
            .iter()
            .try_fold(self.url.as_str().len(), |length, (key, value)| {
                length
                    .checked_add(key.len())
                    .and_then(|n| n.checked_add(value.len()))
                    .and_then(|n| n.checked_add(2))
            });
        if raw.is_none_or(|length| length > RequestBounds::default().max_url_bytes.get()) {
            return Err(ApiError::Limit);
        }
        let mut pairs: Vec<_> = self.url.query_pairs().into_owned().collect();
        for (key, value) in query {
            put(&mut pairs, key.clone(), value.clone());
        }
        let encoded = pairs
            .iter()
            .map(|(key, value)| format!("{}={}", quote_plus(key), quote_plus(value)))
            .collect::<Vec<_>>()
            .join("&");
        self.url
            .set_query((!encoded.is_empty()).then_some(encoded.as_str()));
        if self.url.as_str().len() > RequestBounds::default().max_url_bytes.get() {
            return Err(ApiError::Limit);
        }
        Ok(self)
    }

    /// Validates and constructs one finite source-origin raw request.
    /// No alternate host, account, token, redirect or auth-header override is inferred.
    pub fn raw(options: RawRequestOptions) -> Result<Self, ApiError> {
        let RawRequestOptions {
            origin,
            method,
            endpoint,
            query,
            headers,
            body,
            bounds,
        } = options;
        if !matches!(
            method,
            Method::Get | Method::Post | Method::Delete | Method::Put | Method::Patch
        ) || endpoint.contains("://")
            || endpoint.starts_with("//")
            || endpoint
                .chars()
                .any(|c| c.is_control() || matches!(c, '\\' | '#'))
            || endpoint.len() > bounds.max_url_bytes.get()
        {
            return Err(ApiError::InvalidInput);
        }
        let (path, uri_query) = endpoint
            .split_once('?')
            .map_or((endpoint.as_str(), None), |(p, q)| (p, Some(q)));
        if path.split('/').any(|part| {
            matches!(
                part.to_ascii_lowercase().replace("%2e", ".").as_str(),
                "." | ".."
            )
        }) {
            return Err(ApiError::InvalidInput);
        }
        let mut url = Url::parse(&format!(
            "{}{}",
            origin.base_url(),
            path.trim_start_matches('/')
        ))
        .map_err(|_| ApiError::InvalidInput)?;
        let base = Url::parse(origin.base_url()).map_err(|_| ApiError::InvalidInput)?;
        if url.origin() != base.origin()
            || !url.path().starts_with(base.path())
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(ApiError::InvalidInput);
        }
        let mut pairs = Vec::new();
        for (key, value) in query {
            put(&mut pairs, key, value);
        }
        if let Some(query) = uri_query {
            if query.contains('?') {
                return Err(ApiError::InvalidInput);
            }
            for pair in query.split('&').filter(|s| !s.is_empty()) {
                let (key, value) = pair.split_once('=').ok_or(ApiError::InvalidInput)?;
                if value.contains('=') {
                    return Err(ApiError::InvalidInput);
                }
                put(&mut pairs, key.into(), value.into());
            }
        }
        let encoded = pairs
            .iter()
            .map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v)))
            .collect::<Vec<_>>()
            .join("&");
        if !encoded.is_empty() {
            url.set_query(Some(&encoded));
        }
        if url.as_str().len() > bounds.max_url_bytes.get()
            || body.len() > bounds.max_body_bytes.get()
            || headers.len() > bounds.max_headers.get()
        {
            return Err(ApiError::Limit);
        }
        let mut supplied = Vec::new();
        let mut bytes = 0usize;
        for (name, value) in headers {
            bytes = bytes
                .checked_add(name.len())
                .and_then(|n| n.checked_add(value.len()))
                .and_then(|n| n.checked_add(4))
                .ok_or(ApiError::Limit)?;
            if bytes > bounds.max_header_bytes.get() {
                return Err(ApiError::Limit);
            }
            let name = name.to_ascii_lowercase();
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                || value.chars().any(char::is_control)
                || matches!(
                    name.as_str(),
                    "cookie"
                        | "set-cookie"
                        | "authorization"
                        | "host"
                        | "proxy-authorization"
                        | "proxy-authenticate"
                        | "connection"
                        | "transfer-encoding"
                        | "content-length"
                        | "upgrade"
                        | "te"
                        | "trailer"
                )
            {
                return Err(ApiError::InvalidInput);
            }
            if bytes > bounds.max_header_bytes.get() {
                return Err(ApiError::Limit);
            }
            put(&mut supplied, name, value);
        }
        Ok(Self {
            origin,
            method,
            url,
            headers: supplied,
            body,
        })
    }

    /// Returns the fixed source origin/audience binding.
    pub const fn origin(&self) -> Origin {
        self.origin
    }

    /// Returns only validated non-authentication headers.
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(origin: Origin) -> RawRequestOptions {
        RawRequestOptions {
            origin,
            method: Method::Get,
            endpoint: "resources".into(),
            query: Vec::new(),
            headers: Vec::new(),
            body: Vec::new(),
            bounds: RequestBounds::default(),
        }
    }

    #[test]
    fn exact_json_url_header_and_count_bounds_are_measured_not_inferred() {
        let value = serde_json::json!({"escaped":"\n\u{0001}"});
        let expected = fabric_core_json::json_dumps(&value).into_bytes();
        assert_eq!(
            crate::json_body(&value, NonZeroUsize::new(expected.len()).unwrap()).unwrap(),
            expected
        );
        assert_eq!(
            crate::json_body(&value, NonZeroUsize::new(expected.len() - 1).unwrap()).unwrap_err(),
            ApiError::Limit
        );
        let bound = RequestBounds::default().max_url_bytes.get();
        let id = "a".repeat(bound - Origin::Fabric.base_url().len());
        assert_eq!(
            Request::fabric(Method::Get, &[&id])
                .unwrap()
                .url()
                .as_str()
                .len(),
            bound
        );
        assert_eq!(
            Request::fabric(Method::Get, &[&(id + "a")]).unwrap_err(),
            ApiError::Limit
        );
        let mut input = options(Origin::Fabric);
        input.bounds.max_header_bytes = NonZeroUsize::new(13).unwrap();
        input.headers = vec![("x-test".into(), "abc".into())];
        assert!(Request::raw(input.clone()).is_ok());
        input.headers[0].1.push('d');
        assert_eq!(Request::raw(input).unwrap_err(), ApiError::Limit);
        let mut input = options(Origin::Fabric);
        input.bounds.max_headers = NonZeroUsize::new(2).unwrap();
        input.headers = vec![("x-a".into(), "a".into()), ("x-b".into(), "b".into())];
        assert!(Request::raw(input.clone()).is_ok());
        input.headers.push(("x-c".into(), "c".into()));
        assert_eq!(Request::raw(input).unwrap_err(), ApiError::Limit);
    }

    #[test]
    fn finite_origins_bind_source_prefix_and_scope_without_token_inference() {
        for origin in [
            Origin::Fabric,
            Origin::OneLake,
            Origin::Arm,
            Origin::PowerBi,
        ] {
            let request = Request::raw(options(origin)).unwrap();
            assert_eq!(
                request.url().as_str(),
                format!("{}resources", origin.base_url())
            );
            assert_eq!(request.origin(), origin);
        }
        assert_eq!(Origin::PowerBi.audience(), Audience::Fabric);
    }

    #[test]
    fn unsafe_headers_hosts_paths_and_exact_allocation_bounds_fail_closed() {
        for endpoint in [
            "https://foreign.invalid/",
            "//foreign.invalid/",
            "../resources",
            "%2e%2e/resources",
            "resources#secret",
            "resources\\other",
        ] {
            let mut input = options(Origin::Fabric);
            input.endpoint = endpoint.into();
            assert!(Request::raw(input).is_err(), "{endpoint}");
        }
        for name in [
            "Authorization",
            "HOST",
            "Proxy-Authorization",
            "Content-Length",
            "bad header",
        ] {
            let mut input = options(Origin::Fabric);
            input.headers = vec![(name.into(), "value".into())];
            assert_eq!(Request::raw(input).unwrap_err(), ApiError::InvalidInput);
        }
        let mut input = options(Origin::Fabric);
        input.headers = vec![("x-test".into(), "injected\r\nHost: other".into())];
        assert_eq!(Request::raw(input).unwrap_err(), ApiError::InvalidInput);
        let mut input = options(Origin::Fabric);
        input.bounds.max_body_bytes = NonZeroUsize::new(3).unwrap();
        input.body = b"abc".to_vec();
        assert!(Request::raw(input.clone()).is_ok());
        input.body.push(b'd');
        assert_eq!(Request::raw(input).unwrap_err(), ApiError::Limit);
    }

    #[test]
    fn source_query_precedence_and_python_urlencode_punctuation_are_preserved() {
        let mut input = options(Origin::Fabric);
        input.endpoint = "resources?x=from%20uri".into();
        input.query = vec![
            ("x".into(), "overridden".into()),
            ("punctuation".into(), " *~".into()),
        ];
        assert_eq!(
            Request::raw(input).unwrap().url().query(),
            Some("x=from%2520uri&punctuation=+%2A~")
        );
    }
}
