// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Raw request construction, without credentials, service facades, or paging.
#![forbid(unsafe_code)]
pub use fabric_core::validate_url;
use fabric_core::{
    ApiRequest, Audience, CoreError, ErrorKind, MAX_REQUEST_BYTES, validate_headers, validate_path,
};
pub use fabric_sdk_operations::{LroConfig, complete, complete_with_config};
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
    let method = method.to_ascii_uppercase();
    if !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&method.as_str())
        || endpoint.is_empty()
        || endpoint.len() > 16 * 1024
        || endpoint.chars().any(|c| c.is_control() || c == ' ')
        || body.len() > MAX_REQUEST_BYTES
    {
        return Err(CoreError::new(
            ErrorKind::InvalidInput,
            "Invalid method, endpoint, or oversized request body",
        ));
    }
    validate_headers(&headers)?;
    validate_path(endpoint.split(['?', '#']).next().unwrap_or_default())?;
    let url = if endpoint.starts_with("https://") {
        Url::parse(endpoint)
    } else {
        if endpoint.contains("://") || endpoint.starts_with("//") {
            return Err(CoreError::new(ErrorKind::InvalidInput, "Unsafe endpoint"));
        }
        Url::parse(&format!(
            "{}/{}",
            audience.base_url(),
            endpoint.trim_start_matches('/')
        ))
    }
    .map_err(|_| CoreError::new(ErrorKind::InvalidInput, "Invalid endpoint URL"))?;
    validate_url(audience, &url)?;
    Ok(ApiRequest {
        method,
        url,
        headers,
        body,
    })
}
