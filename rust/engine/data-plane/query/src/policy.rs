// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{EngineBinding, KqlQuery};
use fabric_sdk::runtime::{NativeDeadline, NativeEffect, NativeError, NativeMethod, NativeRequest};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(crate) const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestBody {
    db: String,
    csl: String,
    properties: Properties,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Properties {
    #[serde(rename = "Options")]
    options: ReadOptions,
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ReadOptions {
    query_language: String,
    request_readonly: bool,
    request_readonly_hardline: bool,
    request_callout_disabled: bool,
    request_remote_entities_disabled: bool,
    request_external_data_disabled: bool,
    request_impersonation_disabled: bool,
    request_sandboxed_execution_disabled: bool,
    results_progressive_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    servertimeout: Option<String>,
}

impl ReadOptions {
    fn new(timeout: Option<Duration>) -> Self {
        Self {
            query_language: "kql".into(),
            request_readonly: true,
            request_readonly_hardline: true,
            request_callout_disabled: true,
            request_remote_entities_disabled: true,
            request_external_data_disabled: true,
            request_impersonation_disabled: true,
            request_sandboxed_execution_disabled: true,
            results_progressive_enabled: false,
            servertimeout: timeout.map(format_timeout),
        }
    }
}

impl RequestBody {
    pub(crate) fn new(database: &str, text: &str, timeout: Option<Duration>) -> Self {
        Self {
            db: database.into(),
            csl: text.into(),
            properties: Properties {
                options: ReadOptions::new(timeout),
            },
        }
    }
}

fn format_timeout(timeout: Duration) -> String {
    let secs = timeout.as_secs();
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60,
        timeout.subsec_millis()
    )
}

fn parse_timeout(value: &str) -> Result<Duration, NativeError> {
    if value.len() != 12
        || !value.bytes().enumerate().all(|(index, byte)| match index {
            2 | 5 => byte == b':',
            8 => byte == b'.',
            _ => byte.is_ascii_digit(),
        })
    {
        return Err(NativeError::InvalidInput);
    }
    let number = |start, end| {
        value[start..end]
            .parse::<u64>()
            .map_err(|_| NativeError::InvalidInput)
    };
    let hours = number(0, 2)?;
    let minutes = number(3, 5)?;
    let seconds = number(6, 8)?;
    if minutes >= 60 || seconds >= 60 {
        return Err(NativeError::InvalidInput);
    }
    let timeout =
        Duration::from_millis(((hours * 60 + minutes) * 60 + seconds) * 1000 + number(9, 12)?);
    if timeout.is_zero() || timeout > Duration::from_secs(3600) {
        return Err(NativeError::InvalidInput);
    }
    Ok(timeout)
}

pub(crate) fn validate_request(
    binding: &EngineBinding,
    request: &NativeRequest,
    deadline: &NativeDeadline,
) -> Result<(), NativeError> {
    deadline.remaining()?;
    let expected = binding.endpoint().request(
        NativeMethod::Post,
        &["v2", "rest", "query"],
        &[],
        None,
        NativeEffect::Read,
    )?;
    if request.url() != expected.url()
        || request.method() != NativeMethod::Post
        || request.effect() != NativeEffect::Read
        || request.body().is_some()
        || request.accept() != "application/json"
        || request.content_type() != "application/json; charset=utf-8"
        || request.headers().len() != 1
    {
        return Err(NativeError::InvalidInput);
    }
    let correlation = request
        .headers()
        .get("x-ms-client-request-id")
        .ok_or(NativeError::InvalidInput)?;
    let bytes = request.bytes().ok_or(NativeError::InvalidInput)?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(NativeError::Limit);
    }
    // Typed decoding rejects duplicate fields as well as policy extensions.
    let body: RequestBody = serde_json::from_slice(bytes).map_err(|_| NativeError::InvalidInput)?;
    let timeout = body
        .properties
        .options
        .servertimeout
        .as_deref()
        .ok_or(NativeError::InvalidInput)
        .and_then(parse_timeout)?;
    if body.db != binding.database()
        || body.properties.options != ReadOptions::new(Some(timeout))
        || KqlQuery::new(&body.csl, correlation).is_err()
        || timeout > deadline.remaining()?
    {
        return Err(NativeError::InvalidInput);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;
    use crate::QueryLimits;
    use azure_core::http::Url;
    use fabric_v2_contracts::ItemIdentity;
    use serde_json::{Value, json};

    fn binding() -> EngineBinding {
        EngineBinding::kusto(
            "kql",
            "1",
            ItemIdentity::new(
                "11111111-1111-1111-1111-111111111111",
                "22222222-2222-2222-2222-222222222222",
            )
            .unwrap(),
            Url::parse("https://offline.kusto.fabric.microsoft.com/").unwrap(),
            "exact-db",
            "https://api.kusto.windows.net/.default",
            "account",
            "tenant",
        )
        .unwrap()
    }

    fn request(binding: &EngineBinding) -> NativeRequest {
        KqlQuery::new(".drop table T; // \u{03bb}", "query;test")
            .unwrap()
            .with_server_timeout(Duration::from_secs(1))
            .unwrap()
            .request(
                binding,
                QueryLimits::new(MAX_REQUEST_BYTES, 1024, 10, 4, 4, 8).unwrap(),
            )
            .unwrap()
    }

    fn with_body(binding: &EngineBinding, body: Vec<u8>) -> NativeRequest {
        binding
            .endpoint()
            .request(
                NativeMethod::Post,
                &["v2", "rest", "query"],
                &[],
                None,
                NativeEffect::Read,
            )
            .unwrap()
            .with_bytes(body, "application/json; charset=utf-8", MAX_REQUEST_BYTES)
            .unwrap()
            .with_header("x-ms-client-request-id", "query;test")
            .unwrap()
    }

    #[test]
    fn fixed_policy_preflight_preserves_text_but_rejects_all_policy_changes() {
        let binding = binding();
        let deadline = NativeDeadline::new(Duration::from_secs(30)).unwrap();
        let request = request(&binding);
        assert_eq!(validate_request(&binding, &request, &deadline), Ok(()));
        let body: Value = serde_json::from_slice(request.bytes().unwrap()).unwrap();
        for (field, changed) in [
            ("query_language", json!("sql")),
            ("request_readonly", json!(false)),
            ("request_readonly_hardline", json!(false)),
            ("request_callout_disabled", json!(false)),
            ("request_remote_entities_disabled", json!(false)),
            ("request_external_data_disabled", json!(false)),
            ("request_impersonation_disabled", json!(false)),
            ("request_sandboxed_execution_disabled", json!(false)),
            ("results_progressive_enabled", json!(true)),
            ("servertimeout", json!("00:00:00.000")),
            ("servertimeout", json!("00:00:31.000")),
            ("servertimeout", Value::Null),
            ("unknown", json!(true)),
        ] {
            let mut changed_body = body.clone();
            changed_body["properties"]["Options"][field] = changed;
            assert_eq!(
                validate_request(
                    &binding,
                    &with_body(&binding, serde_json::to_vec(&changed_body).unwrap()),
                    &deadline
                ),
                Err(NativeError::InvalidInput),
                "{field}"
            );
        }
        for changed in [
            json!({"Parameters":{"readonly":false}}),
            json!({"Options":body["properties"]["Options"],"Parameters":{}}),
        ] {
            let mut changed_body = body.clone();
            changed_body["properties"] = changed;
            assert!(
                validate_request(
                    &binding,
                    &with_body(&binding, serde_json::to_vec(&changed_body).unwrap()),
                    &deadline
                )
                .is_err()
            );
        }
    }

    #[test]
    fn duplicate_fields_and_missing_timeout_are_not_production_policy() {
        let binding = binding();
        let deadline = NativeDeadline::new(Duration::from_secs(30)).unwrap();
        let valid = String::from_utf8(request(&binding).bytes().unwrap().to_vec()).unwrap();
        for body in [
            valid.replace(
                "\"request_readonly\":true",
                "\"request_readonly\":false,\"request_readonly\":true",
            ),
            valid.replace(
                "\"db\":\"exact-db\"",
                "\"db\":\"other\",\"db\":\"exact-db\"",
            ),
            valid.replace(",\"servertimeout\":\"00:00:01.000\"", ""),
        ] {
            assert_eq!(
                validate_request(&binding, &with_body(&binding, body.into_bytes()), &deadline),
                Err(NativeError::InvalidInput)
            );
        }
    }

    #[test]
    fn timeout_parser_requires_positive_exact_milliseconds_and_one_hour_bound() {
        for invalid in [
            "",
            "00:00:00.000",
            "00:00:01",
            "0:00:01.000",
            "00:60:00.000",
            "00:00:60.000",
            "01:00:00.001",
            "00:00:01.0000",
            "00:00:01.0\u{03bb}",
        ] {
            assert_eq!(parse_timeout(invalid), Err(NativeError::InvalidInput));
        }
        for timeout in [
            Duration::from_millis(1),
            Duration::from_millis(1234),
            Duration::from_secs(3600),
        ] {
            assert_eq!(parse_timeout(&format_timeout(timeout)), Ok(timeout));
        }
    }
}
