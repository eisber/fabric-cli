// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Raw request construction, without credentials, service facades, or paging.
#![forbid(unsafe_code)]
pub use fabric_core::validate_url;
use fabric_core::{
    ApiRequest, Audience, CoreError, Endpoint, ErrorKind, MAX_REQUEST_BYTES, validate_headers,
    validate_path,
};
pub use fabric_sdk_operations::{
    LroConfig, PollObserver, PollProgress, complete, complete_observed,
    complete_observed_with_endpoint, complete_with_config,
};
use std::collections::BTreeMap;
use url::Url;

/// Validate and construct a raw API request before credential acquisition.
///
/// Query text is preserved by URL parsing, not decoded/re-encoded as parameters.
/// Payload is pre-encoded JSON or multipart; there is no ambient file I/O.
pub fn build_request(
    audience: Audience,
    method: &str,
    endpoint: &str,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
) -> Result<ApiRequest, CoreError> {
    build_request_with_endpoint(&Endpoint::public(audience), method, endpoint, headers, body)
}

/// Construct a request against an explicitly validated service endpoint.
pub fn build_request_with_endpoint(
    service: &Endpoint,
    method: &str,
    endpoint: &str,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
) -> Result<ApiRequest, CoreError> {
    let method = method.to_ascii_uppercase();
    let invalid = |message: String| CoreError::new(ErrorKind::InvalidInput, message);
    if !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&method.as_str()) {
        return Err(invalid(
            "Unsupported HTTP method; use GET, POST, PUT, PATCH, or DELETE".to_owned(),
        ));
    }
    if endpoint.is_empty() {
        return Err(invalid("Endpoint must not be empty".to_owned()));
    }
    if endpoint.len() > 16 * 1024 {
        return Err(invalid(format!(
            "Endpoint is {} bytes, exceeding the 16384-byte limit",
            endpoint.len()
        )));
    }
    if endpoint.chars().any(|c| c.is_control() || c == ' ') {
        return Err(invalid(
            "Endpoint must not contain spaces or control characters; percent-encode them"
                .to_owned(),
        ));
    }
    if body.len() > MAX_REQUEST_BYTES {
        return Err(invalid(format!(
            "Request body is {} bytes, exceeding the {MAX_REQUEST_BYTES}-byte limit",
            body.len()
        )));
    }
    validate_headers(&headers)?;
    validate_path(endpoint.split(['?', '#']).next().unwrap_or_default())?;
    let url = if endpoint.starts_with("https://") {
        Url::parse(endpoint)
    } else {
        if endpoint.contains("://") || endpoint.starts_with("//") {
            return Err(CoreError::new(
                ErrorKind::InvalidInput,
                "Endpoint must be a relative path or an https:// URL",
            ));
        }
        Url::parse(&format!(
            "{}/{}",
            service.base_url().as_str().trim_end_matches('/'),
            endpoint.trim_start_matches('/')
        ))
    }
    .map_err(|_| CoreError::new(ErrorKind::InvalidInput, "Endpoint is not a valid URL"))?;
    service.validate_url(&url)?;
    Ok(ApiRequest {
        method,
        url,
        headers,
        body,
    })
}
