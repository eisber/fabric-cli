// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Narrow donor-adapted raw API parsing, input validation, and response projection.
#![forbid(unsafe_code)]

use clap::{Arg, ArgAction, ArgMatches, Command};
use fabric_cli_command_contracts::CommandError;
use fabric_core::{ApiRequest, ApiResponse, Audience, MAX_REQUEST_BYTES};
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, io::Read, path::Path, time::Duration};

/// Prepared API command; all local input validation has completed.
pub struct PreparedApi {
    /// Fixed token/origin audience.
    pub audience: Audience,
    /// Fully validated request, without authorization.
    pub request: ApiRequest,
    /// Optional local response projection.
    pub query: Option<String>,
    /// Include service headers only on explicit request.
    pub show_headers: bool,
    /// Return the actual initial response, without polling or background work.
    pub asynchronous: bool,
    /// Optional polling interval and overall timeout.
    pub lro_config: fabric_sdk_api::LroConfig,
}

/// Long-only approved API parser, with explicit per-command polling configuration.
pub fn command() -> Command {
    Command::new("api")
        .about("Send a raw API request; recognized operations poll by default until completion or local cancellation")
        .disable_help_flag(true)
        .arg(Arg::new("help").long("help").action(ArgAction::Help))
        .arg(Arg::new("endpoint").required(true))
        .arg(
            Arg::new("polling-config")
                .long("polling-config")
                .value_name("JSON")
                .help("Optional polling JSON: poll_interval and timeout are positive numbers of seconds; both keys are optional and timeout has no default. Cannot be combined with --async. Backend work may continue after timeout"),
        )
        .arg(
            Arg::new("method")
                .long("method")
                .default_value("get")
                .value_parser(["get", "post", "delete", "put", "patch"]),
        )
        .arg(
            Arg::new("audience")
                .long("audience")
                .default_value("fabric")
                .value_parser(["fabric", "storage", "azure", "powerbi"]),
        )
        .arg(
            Arg::new("payload")
                .long("payload")
                .help("Inline JSON object, JSON file, or multipart file; cannot be used with --method delete"),
        )
        .arg(
            Arg::new("headers")
                .long("headers")
                .value_name("JSON")
                .help("Inline JSON object of string headers: Accept, Content-Type, If-Match, If-None-Match, Prefer, Range"),
        )
        .arg(
            Arg::new("query").long("query").help(
                "Local JMESPath on the response object: status_code, text, optional headers",
            ),
        )
        .arg(
            Arg::new("show-headers")
                .long("show-headers")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("async")
                .long("async")
                .action(ArgAction::SetTrue)
                .help("Send once and return the initial response without polling; cannot be combined with --polling-config"),
        )
}

/// Validate request bytes, headers, query, and origin before reading auth/config.
pub fn prepare(matches: &ArgMatches) -> Result<PreparedApi, CommandError> {
    let value = |name| matches.get_one::<String>(name).map(String::as_str);
    let asynchronous = matches.get_flag("async");
    if asynchronous && value("polling-config").is_some() {
        return Err(invalid(
            "--async cannot be combined with --polling-config: async mode returns only the initial response and does not support polling",
        ));
    }
    if value("method") == Some("delete") && value("payload").is_some() {
        return Err(invalid("--payload cannot be combined with --method delete"));
    }
    let lro_config = parse_polling_config(value("polling-config"))?;
    let audience = match value("audience") {
        Some("fabric") => Audience::Fabric,
        Some("storage") => Audience::Storage,
        Some("azure") => Audience::Azure,
        Some("powerbi") => Audience::PowerBi,
        _ => {
            return Err(invalid(
                "Invalid --audience; supported values are fabric, storage, azure, and powerbi",
            ));
        }
    };
    // Pinned process_nargs strips boundary quote characters on queries.
    let query = value("query").map(|text| text.trim_matches(['"', '\'']).to_owned());
    if let Some(query) = query.as_deref() {
        if query.len() > 16 * 1024 {
            return Err(invalid("JMESPath query exceeds the 16384-byte limit"));
        }
        if !query.is_empty() {
            fabric_cli_query::validate(query)
                .map_err(|_| invalid("Invalid jmespath query (https://jmespath.org)"))?;
        }
    }
    let mut headers = parse_headers(value("headers"))?;
    let body = load_input(value("payload"), &mut headers)?;
    // Python applies this default even when input is absent/null. Multipart
    // preparation already inserted its boundary unless the caller overrode it.
    headers
        .entry("content-type".into())
        .or_insert_with(|| "application/json".into());
    let request = fabric_sdk_api::build_request(
        audience,
        value("method").unwrap_or("get"),
        value("endpoint").unwrap_or_default(),
        headers,
        body,
    )?;
    Ok(PreparedApi {
        audience,
        request,
        query,
        show_headers: matches.get_flag("show-headers"),
        asynchronous,
        lro_config,
    })
}

fn parse_polling_config(text: Option<&str>) -> Result<fabric_sdk_api::LroConfig, CommandError> {
    let Some(text) = text else {
        return Ok(fabric_sdk_api::LroConfig::default());
    };
    if text.len() > 16 * 1024 {
        return Err(invalid("--polling-config exceeds the 16384-byte limit"));
    }
    let json = fabric_core_json::decode(text.as_bytes()).map_err(|_| {
        invalid(json_failure(
            "--polling-config is not valid JSON",
            text.as_bytes(),
        ))
    })?;
    let object = json
        .as_object()
        .ok_or_else(|| invalid("Polling configuration must be a JSON object"))?;
    if object.contains_key("polling_interval") {
        return Err(invalid(
            "Unknown polling configuration option 'polling_interval'; use 'poll_interval'",
        ));
    }
    if object
        .keys()
        .any(|key| key != "poll_interval" && key != "timeout")
    {
        return Err(invalid(
            "Unknown polling configuration option; supported keys are poll_interval and timeout",
        ));
    }
    let duration = |key: &str| -> Result<Option<Duration>, CommandError> {
        let Some(value) = object.get(key).filter(|value| !value.is_null()) else {
            return Ok(None);
        };
        let seconds = value
            .as_f64()
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(|| {
                CommandError::new(
                    "InvalidInput",
                    format!("Invalid {key}: expected a positive JSON number of seconds"),
                )
            })?;
        Duration::try_from_secs_f64(seconds).map(Some).map_err(|_| {
            CommandError::new(
                "InvalidInput",
                format!("Invalid {key}: duration exceeds supported range"),
            )
        })
    };
    fabric_sdk_api::LroConfig::new(duration("poll_interval")?, duration("timeout")?)
        .map_err(Into::into)
}

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::new("InvalidInput", message)
}

/// Parse a bounded JSON string map with a case-insensitive allowlist and no duplicates.
pub fn parse_headers(text: Option<&str>) -> Result<BTreeMap<String, String>, CommandError> {
    let Some(text) = text else {
        return Ok(BTreeMap::new());
    };
    if text.len() > 64 * 1024 {
        return Err(invalid("--headers exceeds the 65536-byte limit"));
    }
    // A typed visitor preserves duplicate keys and avoids serde_json's private
    // arbitrary-precision Value marker interpretation for untrusted object keys.
    struct Headers(BTreeMap<String, String>);
    impl<'de> Deserialize<'de> for Headers {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct HeaderVisitor;
            impl<'de> Visitor<'de> for HeaderVisitor {
                type Value = Headers;
                fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    formatter.write_str("an allowed HTTP header object with string values")
                }
                fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Headers, M::Error> {
                    let mut headers = BTreeMap::new();
                    while let Some((name, value)) = map.next_entry::<String, String>()? {
                        let name = name.to_ascii_lowercase();
                        if ![
                            "accept",
                            "content-type",
                            "if-match",
                            "if-none-match",
                            "prefer",
                            "range",
                        ]
                        .contains(&name.as_str())
                        {
                            return Err(serde::de::Error::custom("Header is not allowed"));
                        }
                        if headers.insert(name, value).is_some() {
                            return Err(serde::de::Error::custom("Duplicate header"));
                        }
                    }
                    Ok(Headers(headers))
                }
            }
            deserializer.deserialize_map(HeaderVisitor)
        }
    }
    let Headers(headers) = serde_json::from_str::<Headers>(text).map_err(|_| {
        invalid("--headers must be a JSON object of string values with no duplicate names; allowed headers: Accept, Content-Type, If-Match, If-None-Match, Prefer, Range")
    })?;
    fabric_core::validate_headers(&headers)?;
    Ok(headers)
}

fn load_input(
    input: Option<&str>,
    headers: &mut BTreeMap<String, String>,
) -> Result<Vec<u8>, CommandError> {
    let Some(input) = input else {
        return Ok(Vec::new());
    };
    let text = input.trim_matches(['"', '\'']);
    if text.len() > MAX_REQUEST_BYTES {
        return Err(invalid(format!(
            "--payload is {} bytes, exceeding the {MAX_REQUEST_BYTES}-byte limit",
            text.len()
        )));
    }
    if text.starts_with('{') && text.ends_with('}') {
        return json_body(text.as_bytes(), headers);
    }
    let path = Path::new(text);
    // Reject special files before open: opening a named pipe can otherwise block indefinitely.
    let metadata = std::fs::metadata(path).map_err(|error| input_file_error(&error))?;
    if !metadata.is_file() {
        return Err(invalid(
            "API input path must be a regular file, not a directory or device",
        ));
    }
    if metadata.len() > MAX_REQUEST_BYTES as u64 {
        return Err(input_file_too_large(metadata.len()));
    }
    let file = std::fs::File::open(path).map_err(|error| input_file_error(&error))?;
    if !file
        .metadata()
        .map_err(|_| invalid("Cannot inspect API input file after opening it"))?
        .is_file()
    {
        return Err(invalid(
            "API input path must be a regular file, not a directory or device",
        ));
    }
    let mut bytes = Vec::new();
    file.take((MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("Cannot read API input file: I/O error while reading"))?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(input_file_too_large(bytes.len() as u64));
    }
    if path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        return json_body(&bytes, headers);
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("API input file name must be valid UTF-8 for multipart upload"))?;
    multipart(name, &bytes, headers)
}

/// Describe a JSON failure by position only; content is never echoed.
fn json_failure(prefix: &str, bytes: &[u8]) -> String {
    match serde_json::from_slice::<Value>(bytes) {
        Err(error) if error.is_eof() => format!(
            "{prefix}: unexpected end of input at line {}, column {}",
            error.line(),
            error.column()
        ),
        Err(error) if error.is_syntax() => format!(
            "{prefix}: syntax error at line {}, column {}",
            error.line(),
            error.column()
        ),
        _ => format!(
            "{prefix}: nesting exceeds 128 levels or a number is outside the supported range"
        ),
    }
}

fn input_file_error(error: &std::io::Error) -> CommandError {
    let reason = match error.kind() {
        std::io::ErrorKind::NotFound => "file not found",
        std::io::ErrorKind::PermissionDenied => "permission denied",
        _ => "the file cannot be accessed",
    };
    CommandError::new(
        "InvalidPath",
        format!("Cannot read API input file: {reason}"),
    )
}

fn input_file_too_large(length: u64) -> CommandError {
    invalid(format!(
        "API input file is {length} bytes, exceeding the {MAX_REQUEST_BYTES}-byte limit"
    ))
}

fn json_body(
    bytes: &[u8],
    headers: &mut BTreeMap<String, String>,
) -> Result<Vec<u8>, CommandError> {
    let value = fabric_core_json::decode(bytes).map_err(|_| {
        CommandError::new(
            "InvalidJson",
            json_failure("API input is not valid JSON", bytes),
        )
    })?;
    // Pinned Python passes parsed null as input=None, so it sends no JSON body.
    if value.is_null() {
        return Ok(Vec::new());
    }
    let bytes = fabric_core_json::json_dumps(&value).into_bytes();
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(invalid(format!(
            "Encoded API input exceeds the {MAX_REQUEST_BYTES}-byte limit"
        )));
    }
    headers
        .entry("content-type".into())
        .or_insert_with(|| "application/json".into());
    Ok(bytes)
}

fn multipart(
    name: &str,
    content: &[u8],
    headers: &mut BTreeMap<String, String>,
) -> Result<Vec<u8>, CommandError> {
    if name.is_empty()
        || name.len() > 1024
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '/' | '\\'))
    {
        return Err(invalid(
            "API input file name must be 1-1024 characters without quotes, slashes, or control characters",
        ));
    }
    // Deterministic collision-free boundary for these bounded caller-owned bytes.
    let mut boundary = "fab-rs-multipart".to_owned();
    while content
        .windows(boundary.len())
        .any(|bytes| bytes == boundary.as_bytes())
    {
        boundary.push('x');
        if boundary.len() > 70 {
            return Err(invalid(
                "Cannot frame multipart input: file content collides with every candidate boundary",
            ));
        }
    }
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\r\n"
    )
    .into_bytes();
    let suffix = format!("\r\n--{boundary}--\r\n");
    if body
        .len()
        .checked_add(content.len())
        .and_then(|n| n.checked_add(suffix.len()))
        .is_none_or(|length| length > MAX_REQUEST_BYTES)
    {
        return Err(invalid(format!(
            "Multipart API input exceeds the {MAX_REQUEST_BYTES}-byte limit"
        )));
    }
    body.extend_from_slice(content);
    body.extend_from_slice(suffix.as_bytes());
    headers
        .entry("content-type".into())
        .or_insert_with(|| format!("multipart/form-data; boundary={boundary}"));
    Ok(body)
}

/// Validate initial response JSON before reconciliation can replace its body.
/// Empty bodies use the same whitespace policy as final output.
pub fn validate_response_body(body: &[u8]) -> Result<(), CommandError> {
    response_body(body).map(|_| ())
}

fn response_body(bytes: &[u8]) -> Result<Value, CommandError> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        CommandError::new(
            "InvalidJson",
            "Service response is not valid JSON: body is not valid UTF-8",
        )
    })?;
    if text
        .chars()
        .all(|c| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
    {
        Ok(Value::String(String::new()))
    } else {
        fabric_core_json::decode(bytes).map_err(|_| {
            CommandError::new(
                "InvalidJson",
                json_failure("Service response is not valid JSON", bytes),
            )
        })
    }
}

/// Shape the response, querying the object placed inside `result.data`.
///
/// Queries use the HTTP wrapper, matching Python raw API. Body fields require
/// explicit `text` paths. Empty queries retain the wrapper; empty results never fall back.
pub fn response_data(
    response: ApiResponse,
    show_headers: bool,
    query: Option<&str>,
) -> Result<Value, CommandError> {
    let body = response_body(&response.body)?;
    let mut payload = Map::new();
    payload.insert("status_code".into(), Value::from(response.status));
    if show_headers {
        payload.insert(
            "headers".into(),
            serde_json::to_value(
                response
                    .headers
                    .into_iter()
                    .filter(|(name, _)| {
                        !name.eq_ignore_ascii_case("authorization")
                            && !name.eq_ignore_ascii_case("proxy-authorization")
                    })
                    .collect::<std::collections::BTreeMap<_, _>>(),
            )
            .map_err(|_| invalid("Invalid response headers"))?,
        );
    }
    payload.insert("text".into(), body);
    let payload = Value::Object(payload);
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        return fabric_cli_query::search(&payload, query)
            .map_err(|_| invalid("Invalid jmespath query (https://jmespath.org)"));
    }
    Ok(payload)
}
