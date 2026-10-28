// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{CommandError, CommandValue, python_truthy};
use azure_core::http::Method;
use fabric_sdk_api::{ApiClient, Origin, RawRequestOptions, Request, RequestBounds};
use serde_json::{Value, json};
use std::{future::Future, io::Read, num::NonZeroUsize, path::Path};

/// Explicit source API input; no hidden identity/runtime or filesystem discovery.
#[derive(Clone, Debug)]
pub enum ApiInput {
    /// JSON object/file contents.
    Json(Value),
    /// A bounded local file upload, with an explicit caller-selected MIME boundary.
    Multipart {
        /// Basename only, not an absolute local path.
        file_name: String,
        /// Bounded bytes read by the host/input helper.
        content: Vec<u8>,
        /// Caller-selected ASCII MIME boundary.
        boundary: String,
    },
}

/// Raw command inputs after the front door's source-compatible nargs processing.
#[derive(Clone, Debug)]
pub struct ApiOptions {
    /// Explicit public source audience/origin; source defaults to Fabric.
    pub origin: Origin,
    /// One of the five source verbs.
    pub method: Method,
    /// Relative source API endpoint.
    pub endpoint: String,
    /// Comma-separated key=value pairs.
    pub params: Option<String>,
    /// Comma-separated non-authentication header pairs.
    pub headers: Option<String>,
    /// Explicit prepared input, or no body.
    pub input: Option<ApiInput>,
    /// Already-joined source JMESPath expression.
    pub query: Option<String>,
    /// Include response headers in the status/text payload.
    pub show_headers: bool,
    /// Explicit finite allocation bounds.
    pub bounds: RequestBounds,
}

fn pairs(value: Option<&str>, headers: bool) -> Result<Vec<(String, String)>, CommandError> {
    let mut result: Vec<(String, String)> = Vec::new();
    for pair in value
        .filter(|v| !v.is_empty())
        .into_iter()
        .flat_map(|v| v.split(','))
    {
        let (key, value) = pair.split_once('=').ok_or_else(|| {
            CommandError::new(
                "InvalidInput",
                if headers {
                    "Invalid format for headers. Use key=value pairs separated by commas."
                } else {
                    "Invalid format for query parameters. Use key=value pairs separated by commas."
                },
            )
        })?;
        let key = key.trim();
        let value = value.trim();
        if let Some(existing) = result.iter_mut().find(|p| p.0 == key) {
            existing.1 = value.into();
        } else {
            result.push((key.into(), value.into()));
        }
    }
    Ok(result)
}

/// Constructs the validated raw request before acquiring any credential.
pub fn build_api_request(options: &ApiOptions) -> Result<Request, CommandError> {
    let mut headers = pairs(options.headers.as_deref(), true)?;
    let body = match &options.input {
        None => Vec::new(),
        Some(ApiInput::Json(value)) => {
            fabric_sdk_api::json_body(value, options.bounds.max_body_bytes)
                .map_err(CommandError::from)?
        }
        Some(ApiInput::Multipart {
            file_name,
            content,
            boundary,
        }) => {
            if boundary.is_empty()
                || boundary.len() > 70
                || !boundary
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
                || file_name.is_empty()
                || file_name.len() > options.bounds.max_header_bytes.get()
                || file_name
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '"' | '/' | '\\'))
                || content.len() > options.bounds.max_body_bytes.get()
            {
                return Err(CommandError::new(
                    "InvalidInput",
                    "Invalid multipart input configuration",
                ));
            }
            let mut bytes=format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n\r\n").into_bytes();
            let suffix = format!("\r\n--{boundary}--\r\n");
            if bytes
                .len()
                .checked_add(content.len())
                .and_then(|n| n.checked_add(suffix.len()))
                .is_none_or(|len| len > options.bounds.max_body_bytes.get())
            {
                return Err(CommandError::new(
                    "InvalidInput",
                    "API input exceeds its byte bound",
                ));
            }
            bytes.extend_from_slice(content);
            bytes.extend_from_slice(suffix.as_bytes());
            if !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            {
                headers.push((
                    "content-type".into(),
                    format!("multipart/form-data; boundary={boundary}"),
                ));
            }
            bytes
        }
    };
    Request::raw(RawRequestOptions {
        origin: options.origin,
        method: options.method,
        endpoint: options.endpoint.clone(),
        query: pairs(options.params.as_deref(), false)?,
        headers,
        body,
        bounds: options.bounds,
    })
    .map_err(CommandError::from)
}

/// Parses source `-i` tokens with a bounded real local file consumer.
/// Missing files fail explicitly instead of silently issuing a bodyless write.
pub fn read_api_input(
    tokens: &[String],
    boundary: &str,
    limit: NonZeroUsize,
) -> Result<Option<ApiInput>, CommandError> {
    if tokens.is_empty() {
        return Ok(None);
    }
    let joined = tokens.join(" ");
    let text = joined.trim_matches(['"', '\'']);
    if text.starts_with('{') && text.ends_with('}') {
        if text.len() > limit.get() {
            return Err(CommandError::new(
                "InvalidInput",
                "API input exceeds its byte bound",
            ));
        }
        return serde_json::from_str(text)
            .map(ApiInput::Json)
            .map(Some)
            .map_err(|_| CommandError::new("InvalidJson", "Invalid JSON format"));
    }
    let path = Path::new(text);
    let file = std::fs::File::open(path).map_err(|_| {
        CommandError::new(
            "InvalidPath",
            "The specified file or directory does not exist",
        )
    })?;
    if !file
        .metadata()
        .map_err(|_| CommandError::new("InvalidPath", "Cannot read API input metadata"))?
        .is_file()
    {
        return Err(CommandError::new("InvalidPath", "API input is not a file"));
    }
    let mut bytes = Vec::new();
    file.take(limit.get().saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| CommandError::new("InvalidPath", "Cannot read API input file"))?;
    if bytes.len() > limit.get() {
        return Err(CommandError::new(
            "InvalidInput",
            "API input exceeds its byte bound",
        ));
    }
    if path.extension().is_some_and(|s| s == "json") {
        return serde_json::from_slice(&bytes)
            .map(ApiInput::Json)
            .map(Some)
            .map_err(|_| CommandError::new("InvalidJson", "Invalid JSON format"));
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| CommandError::new("InvalidPath", "Invalid API input filename"))?;
    Ok(Some(ApiInput::Multipart {
        file_name: name.into(),
        content: bytes,
        boundary: boundary.into(),
    }))
}

/// Executes source raw-response semantics: HTTP failures are status/text data.
/// Empty query results retain the whole payload rather than suppressing it.
pub async fn api(
    client: &ApiClient,
    options: &ApiOptions,
    limit: NonZeroUsize,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<CommandValue, CommandError> {
    let request = build_api_request(options)?;
    let response = client
        .send(&request, limit, cancelled)
        .await
        .map_err(CommandError::from)?;
    let mut payload = json!({"status_code":response.status});
    if options.show_headers {
        payload["headers"] = json!(response.headers);
    }
    payload["text"] = if response.body.iter().all(u8::is_ascii_whitespace) {
        json!("(Empty)")
    } else {
        serde_json::from_slice(&response.body)
            .map_err(|_| CommandError::new("InvalidJson", "Invalid JSON format"))?
    };
    if let Some(query) = options.query.as_deref().filter(|q| !q.is_empty()) {
        let projected = fabric_cli_query::search(&payload, query).map_err(|_| {
            CommandError::new(
                "InvalidInput",
                "Invalid jmespath query (https://jmespath.org)",
            )
        })?;
        if python_truthy(&projected) {
            payload = projected;
        }
    }
    Ok(CommandValue {
        data: Some(payload),
        ..CommandValue::default()
    })
}
