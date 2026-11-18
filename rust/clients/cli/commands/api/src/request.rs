// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{args::parse_polling_config, invalid, json_failure};
use clap::ArgMatches;
use fabric_cli_command_contracts::{CommandError, Host};
use fabric_core::{ApiRequest, Audience, Endpoint, MAX_REQUEST_BYTES};
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use std::{collections::BTreeMap, io::Read, path::Path};

/// Prepared API command; all local input validation has completed.
pub(super) struct PreparedApi {
    /// Validated service origin and token audience.
    pub(super) endpoint: Endpoint,
    /// Fully validated request, without authorization.
    pub(super) request: ApiRequest,
    /// Optional local response projection.
    pub(super) query: Option<String>,
    /// Include service headers only on explicit request.
    pub(super) show_headers: bool,
    /// Return the actual initial response, without polling or background work.
    pub(super) asynchronous: bool,
    /// Optional polling interval and overall timeout.
    pub(super) lro_config: fabric_sdk_api::LroConfig,
}

/// Validate request bytes, headers, query, and origin before reading identity.
pub(super) fn prepare(matches: &ArgMatches, host: &dyn Host) -> Result<PreparedApi, CommandError> {
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
    let variable = Endpoint::environment_variable(audience);
    let endpoint = match host.environment(variable) {
        Some(value) => Endpoint::from_hostname(
            audience,
            value
                .to_str()
                .ok_or_else(|| invalid(format!("Invalid hostname in {variable}")))?,
        )?,
        None => Endpoint::public(audience),
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
    let request = fabric_sdk_api::build_request_with_endpoint(
        &endpoint,
        value("method").unwrap_or("get"),
        value("endpoint").unwrap_or_default(),
        headers,
        body,
    )?;
    Ok(PreparedApi {
        endpoint,
        request,
        query,
        show_headers: matches.get_flag("show-headers"),
        asynchronous,
        lro_config,
    })
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

#[cfg(test)]
mod tests {
    #[test]
    fn rejected_headers_precede_payload_file_access() {
        let matches = crate::command()
            .try_get_matches_from([
                "api",
                "items",
                "--headers",
                r#"{"Authorization":"HEADER-CANARY"}"#,
                "--payload",
                "/MISSING-PAYLOAD-CANARY.json",
            ])
            .unwrap();
        let error = super::prepare(&matches).err().unwrap();
        let message = format!("{error:?}");
        assert!(message.contains("--headers"));
        assert!(!message.contains("CANARY"));
        assert!(!message.contains("InvalidPath"));
    }
}
