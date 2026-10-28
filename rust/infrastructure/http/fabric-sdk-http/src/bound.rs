// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::ExecutionError;
use azure_core::http::{Method, Url};
pub use fabric_sdk::ServiceOrigin as PublicOrigin;

pub(crate) fn validate_request(
    method: Method,
    url: &Url,
    body: &[u8],
    headers: &[(String, String)],
) -> Result<(), ExecutionError> {
    if !matches!(
        method,
        Method::Get | Method::Head | Method::Post | Method::Put | Method::Patch | Method::Delete
    ) || url.as_str().len() > 16 * 1024
        || body.len() > 4 * 1024 * 1024
        || headers.len() > 64
    {
        return Err(ExecutionError::InvalidInput);
    }
    let mut bytes = 0_usize;
    for (name, value) in headers {
        bytes = bytes
            .checked_add(name.len())
            .and_then(|n| n.checked_add(value.len()))
            .and_then(|n| n.checked_add(4))
            .ok_or(ExecutionError::InvalidInput)?;
        if bytes > 16 * 1024
            || name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || value.chars().any(char::is_control)
            || matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization"
                    | "host"
                    | "proxy-authorization"
                    | "proxy-authenticate"
                    | "cookie"
                    | "set-cookie"
                    | "connection"
                    | "transfer-encoding"
                    | "content-length"
                    | "upgrade"
                    | "te"
                    | "trailer"
            )
        {
            return Err(ExecutionError::InvalidInput);
        }
    }
    Ok(())
}
