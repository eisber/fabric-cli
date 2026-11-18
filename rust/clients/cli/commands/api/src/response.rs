// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{invalid, json_failure};
use fabric_cli_command_contracts::CommandError;
use fabric_core::ApiResponse;
use serde_json::{Map, Value};

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
pub(super) fn response_data(
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(body: &str, headers: &[(&str, &str)]) -> ApiResponse {
        ApiResponse {
            status: 200,
            body: body.as_bytes().to_vec(),
            headers: headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        }
    }

    #[test]
    fn protected_response_headers_are_removed_before_every_query_shape() {
        for query in [
            "",
            "@",
            ".",
            "headers",
            "values(headers)",
            "headers.Authorization",
            "headers.authorization",
            "headers.\"pRoXy-AuThOrIzAtIoN\"",
            "{all: @, extracted: values(headers)}",
        ] {
            let result = response_data(
                response(
                    r#"{"value":"body"}"#,
                    &[
                        ("Authorization", "AUTH-SECRET-CANARY"),
                        ("authorization", "DUPLICATE-SECRET-CANARY"),
                        ("Proxy-Authorization", "PROXY-SECRET-CANARY"),
                        ("pRoXy-AuThOrIzAtIoN", "MIXED-SECRET-CANARY"),
                        ("x-visible", "visible"),
                    ],
                ),
                true,
                Some(query),
            )
            .unwrap();
            let output = result.to_string();
            assert!(!output.contains("SECRET-CANARY"), "{query}: {output}");
            assert!(
                !output.to_ascii_lowercase().contains("authorization"),
                "{query}: {output}"
            );
            if query == "values(headers)" {
                assert_eq!(result, json!(["visible"]));
            }
        }
        let result = response_data(
            response(r#"{"Authorization":"explicit body data"}"#, &[]),
            true,
            Some("text.Authorization"),
        )
        .unwrap();
        assert_eq!(
            result, "explicit body data",
            "only headers are filtered, not body fields"
        );
    }
}
