// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Injected Fabric resource routing and complete collection scans.
#![forbid(unsafe_code)]

use fabric_core::{
    ApiResponse, Audience, Clock, CoreError, Endpoint, ErrorKind, HttpExecutor, MAX_RESPONSE_BYTES,
    OperationContext, diagnostics::RequestId, sanitize_service_message,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

/// A resource selector, parsed before credentials or network access.
#[derive(Clone, Debug)]
pub enum Selector {
    /// Canonical resource identifier.
    Id(String),
    /// Exact display name.
    Name(String),
}

impl Selector {
    /// Parse a single resource name or a valid GUID.
    pub fn parse(text: &str) -> Result<Self, CoreError> {
        if text.is_empty()
            || text.len() > 4096
            || text
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\'))
        {
            return Err(invalid("Expected a single resource name or GUID"));
        }
        Ok(match Uuid::parse_str(text) {
            Ok(id) => Self::Id(id.hyphenated().to_string()),
            Err(_) => Self::Name(text.to_owned()),
        })
    }
}

fn invalid(message: &str) -> CoreError {
    CoreError::new(ErrorKind::InvalidInput, message)
}

fn protocol(message: &str) -> CoreError {
    CoreError::new(ErrorKind::Protocol, message)
}

fn service_failure(response: &ApiResponse) -> CoreError {
    let json = fabric_core_json::decode(&response.body).ok();
    let objects = [
        json.as_ref().and_then(|value| value.get("error")),
        json.as_ref().and_then(|value| value.get("failureReason")),
        json.as_ref(),
    ];
    let field = |key| {
        objects
            .iter()
            .flatten()
            .find_map(|value| value.get(key).and_then(Value::as_str))
    };
    let code = objects
        .iter()
        .flatten()
        .find_map(|value| {
            value
                .get("errorCode")
                .and_then(Value::as_str)
                .or_else(|| value.get("code").and_then(Value::as_str))
        })
        .or_else(|| response.header("x-ms-public-api-error-code"))
        .or_else(|| response.header("x-ms-error-code"));
    let mut message = format!(
        "Fabric resource request failed with HTTP {}",
        response.status
    );
    if let Some(detail) = field("message").and_then(sanitize_service_message) {
        message.push_str(": ");
        message.push_str(&detail);
    }
    if let Some(request_id) = ["requestId", "x-ms-request-id"]
        .into_iter()
        .find_map(|name| response.header(name).and_then(RequestId::parse))
        .or_else(|| field("requestId").and_then(RequestId::parse))
    {
        message.push_str(&format!(" (request ID: {request_id})"));
    }
    CoreError::service_failure(message, code)
}

fn encode_token(token: &str) -> String {
    let bytes = token.as_bytes();
    let mut encoded = String::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%'
            && index + 2 < bytes.len()
            && bytes[index + 1].is_ascii_hexdigit()
            && bytes[index + 2].is_ascii_hexdigit()
        {
            encoded.push('%');
            encoded.push(char::from(bytes[index + 1].to_ascii_uppercase()));
            encoded.push(char::from(bytes[index + 2].to_ascii_uppercase()));
            index += 3;
        } else {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                encoded.push(char::from(byte));
            } else {
                use std::fmt::Write;
                write!(encoded, "%{byte:02X}").expect("writing to a string cannot fail");
            }
            index += 1;
        }
    }
    encoded
}

/// Send a single request to a Fabric resource endpoint.
pub async fn request(
    executor: &dyn HttpExecutor,
    service: &Endpoint,
    context: &OperationContext,
    method: &str,
    endpoint: &str,
    body: Vec<u8>,
) -> Result<ApiResponse, CoreError> {
    if service.audience() != Audience::Fabric {
        return Err(invalid("Resource requests require a Fabric endpoint"));
    }
    let mut headers = BTreeMap::new();
    if !body.is_empty() {
        headers.insert("content-type".into(), "application/json".into());
    }
    let request =
        fabric_sdk_api::build_request_with_endpoint(service, method, endpoint, headers, body)?;
    let response = executor
        .execute(request, service.audience(), context)
        .await?;
    fabric_core::validate_response(&response)?;
    if !(200..300).contains(&response.status) {
        return Err(service_failure(&response));
    }
    Ok(response)
}

/// Visit every page using only the service's continuation token on the fixed origin.
///
/// The callback may retain only the values it needs. It must not publish
/// results until the scan completes successfully.
pub async fn scan(
    executor: &dyn HttpExecutor,
    clock: &dyn Clock,
    service: &Endpoint,
    context: &OperationContext,
    endpoint: &str,
    mut visit: impl FnMut(Value) -> Result<(), CoreError>,
) -> Result<(), CoreError> {
    let mut token: Option<String> = None;
    let mut seen = HashSet::new();
    let mut token_bytes = 0usize;
    loop {
        context.remaining_at(clock.now())?;
        let endpoint = match &token {
            Some(token) => format!("{endpoint}?continuationToken={}", encode_token(token)),
            None => endpoint.to_owned(),
        };
        let response = request(executor, service, context, "GET", &endpoint, Vec::new()).await?;
        let value = fabric_core_json::decode(&response.body)
            .map_err(|_| protocol("Malformed resource collection JSON"))?;
        let Value::Object(mut object) = value else {
            return Err(protocol("Resource collection must be an object"));
        };
        let Some(Value::Array(entries)) = object.remove("value") else {
            return Err(protocol("Resource collection must contain a value array"));
        };
        for entry in entries {
            if !entry.is_object() {
                return Err(protocol("Resource collection contains a non-object"));
            }
            visit(entry)?;
        }
        token = match object.get("continuationToken") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if value.is_empty() => None,
            Some(Value::String(value)) if !value.is_empty() && value.len() <= 8192 => {
                token_bytes = token_bytes
                    .checked_add(value.len())
                    .filter(|length| *length <= MAX_RESPONSE_BYTES)
                    .ok_or_else(|| {
                        protocol("Resource continuation history exceeds allocation bound")
                    })?;
                if !seen.insert(encode_token(value)) {
                    return Err(protocol("Resource continuation token cycle"));
                }
                Some(value.clone())
            }
            _ => return Err(protocol("Invalid resource continuation token")),
        };
        if token.is_none() {
            if object
                .get("continuationUri")
                .is_some_and(|value| !value.is_null())
            {
                return Err(protocol("Resource continuation URI has no usable token"));
            }
            return Ok(());
        }
    }
}

/// Fetch every page as one native array, enforcing the encoded aggregate limit.
pub async fn list(
    executor: &dyn HttpExecutor,
    clock: &dyn Clock,
    service: &Endpoint,
    context: &OperationContext,
    endpoint: &str,
) -> Result<Value, CoreError> {
    let mut entries = Vec::new();
    let mut length = 2usize;
    scan(executor, clock, service, context, endpoint, |entry| {
        let size = serde_json::to_vec(&entry)
            .map_err(|_| protocol("Cannot encode resource collection"))?
            .len();
        length = length
            .checked_add(size + usize::from(!entries.is_empty()))
            .filter(|length| *length <= MAX_RESPONSE_BYTES)
            .ok_or_else(|| protocol("Complete resource collection exceeds allocation bound"))?;
        entries.push(entry);
        Ok(())
    })
    .await?;
    Ok(Value::Array(entries))
}

/// Resolve an ID directly or find exactly one exact display name across all pages.
pub async fn resolve(
    executor: &dyn HttpExecutor,
    clock: &dyn Clock,
    service: &Endpoint,
    context: &OperationContext,
    endpoint: &str,
    selector: &Selector,
) -> Result<String, CoreError> {
    context.remaining_at(clock.now())?;
    let Selector::Name(name) = selector else {
        let Selector::Id(id) = selector else {
            unreachable!()
        };
        return Ok(id.clone());
    };
    let mut candidates = Vec::new();
    let mut count = 0usize;
    scan(executor, clock, service, context, endpoint, |entry| {
        if entry.get("displayName").and_then(Value::as_str) == Some(name) {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                .ok_or_else(|| protocol("Matching resource has an invalid ID"))?;
            count = count.saturating_add(1);
            if candidates.len() < 16 {
                candidates.push(id.hyphenated().to_string());
            }
        }
        Ok(())
    })
    .await?;
    match count {
        0 => Err(invalid("No resource matches the supplied display name")),
        1 => Ok(candidates.remove(0)),
        _ => Err(invalid(&format!(
            "Ambiguous resource display name; matching IDs: {}{}",
            candidates.join(", "),
            if count > candidates.len() {
                ", ..."
            } else {
                ""
            }
        ))),
    }
}
