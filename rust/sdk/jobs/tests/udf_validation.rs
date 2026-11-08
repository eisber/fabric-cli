// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic pure UDF consumers, not captured service or production schema proof.

#![deny(unnameable_test_items)]

use fabric_sdk_jobs::udf::{
    UdfInputSchema, UdfJsonLimits, UdfProtocolIssue, UdfResponse, UdfResponseKind,
    UdfValidationError, validate_arguments,
};
use serde_json::{Value, json};
use std::{cell::Cell, num::NonZeroUsize};

fn limits(bytes: usize, depth: usize, nodes: usize) -> UdfJsonLimits {
    UdfJsonLimits {
        max_bytes: NonZeroUsize::new(bytes).unwrap(),
        max_depth: NonZeroUsize::new(depth).unwrap(),
        max_nodes: NonZeroUsize::new(nodes).unwrap(),
    }
}

fn fixture_limits() -> UdfJsonLimits {
    limits(4096, 16, 256)
}

fn fixture_schema() -> Value {
    json!({
        "type":"object",
        "properties":{
            "count":{"type":"integer"},
            "ratio":{"type":"number"},
            "enabled":{"type":"boolean"},
            "tags":{"type":"array","items":{"type":"string"}},
            "context":{
                "type":"object",
                "properties":{"label":{"type":"string"}},
                "required":["label"],
                "additionalProperties":false
            },
            "note":{"type":["string","null"],"default":"server-default"}
        },
        "required":["count","ratio","enabled","tags","context"],
        "additionalProperties":false
    })
}

struct FixtureSchema {
    document: Value,
    preflights: Cell<usize>,
    validations: Cell<usize>,
}

impl FixtureSchema {
    fn new() -> Self {
        Self {
            document: fixture_schema(),
            preflights: Cell::new(0),
            validations: Cell::new(0),
        }
    }
}

impl UdfInputSchema for FixtureSchema {
    fn preflight(&self) -> Result<(), UdfValidationError> {
        self.preflights.set(self.preflights.get() + 1);
        // This fixture implements one declared schema, not a schema dialect.
        if self.document != fixture_schema() {
            return Err(UdfValidationError::UnsupportedSchema);
        }
        Ok(())
    }

    fn validate(&self, arguments: &Value) -> Result<(), UdfValidationError> {
        self.validations.set(self.validations.get() + 1);
        let object = arguments
            .as_object()
            .ok_or(UdfValidationError::SchemaViolation)?;
        if !object
            .get("count")
            .is_some_and(|v| v.is_i64() || v.is_u64())
            || !object.get("ratio").is_some_and(Value::is_number)
            || !object.get("enabled").is_some_and(Value::is_boolean)
            || !object
                .get("tags")
                .and_then(Value::as_array)
                .is_some_and(|tags| tags.iter().all(Value::is_string))
            || !object
                .get("context")
                .and_then(Value::as_object)
                .is_some_and(|context| {
                    context.len() == 1 && context.get("label").is_some_and(Value::is_string)
                })
            || object
                .get("note")
                .is_some_and(|note| !note.is_null() && !note.is_string())
            || object.keys().any(|key| {
                !["count", "ratio", "enabled", "tags", "context", "note"].contains(&key.as_str())
            })
        {
            return Err(UdfValidationError::SchemaViolation);
        }
        Ok(())
    }
}

struct AnyObject;

impl UdfInputSchema for AnyObject {
    fn preflight(&self) -> Result<(), UdfValidationError> {
        Ok(())
    }

    fn validate(&self, _: &Value) -> Result<(), UdfValidationError> {
        Ok(())
    }
}

fn arguments() -> Value {
    json!({"count":2,"ratio":1.5,"enabled":false,"tags":["a"],"context":{"label":"x"}})
}

fn native(output: Value) -> Value {
    json!({
        "functionName":"calculate",
        "invocationId":"offline-001",
        "status":"Succeeded",
        "output":output,
        "errors":[]
    })
}

fn received(status: u16, value: &Value) -> UdfResponse {
    UdfResponse::from_received(
        "calculate",
        status,
        &serde_json::to_vec(value).unwrap(),
        fixture_limits(),
    )
}

#[test]
fn exact_direct_payload_and_all_native_argument_types_are_preserved() {
    let bytes =
        br#" { "count":2,"ratio":1.5,"enabled":false,"tags":["a"],"context":{"label":"x"} } "#;
    let schema = FixtureSchema::new();
    let validated = validate_arguments(bytes, &schema, fixture_limits()).unwrap();
    assert_eq!(validated.bytes(), bytes);
    assert_eq!(validated.value(), &arguments());
    assert!(validated.value()["count"].is_i64());
    assert!(validated.value()["ratio"].is_f64());
    assert_eq!(validated.value()["enabled"], false);
    assert!(validated.value().get("parameters").is_none());
    assert!(validated.value().get("executionData").is_none());
    assert_eq!(schema.preflights.get(), 1);
    assert_eq!(schema.validations.get(), 1);
    assert!(!format!("{validated:?}").contains("label"));
}

#[test]
fn optional_omit_null_empty_and_declared_default_are_distinct() {
    for note in [
        None,
        Some(Value::Null),
        Some(json!("")),
        Some(json!("server-default")),
    ] {
        let mut value = arguments();
        if let Some(note) = note.clone() {
            value["note"] = note;
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        let validated =
            validate_arguments(&bytes, &FixtureSchema::new(), fixture_limits()).unwrap();
        assert_eq!(validated.value().get("note"), note.as_ref());
        assert_eq!(validated.bytes(), bytes);
    }
}

#[test]
fn actual_schema_rejects_missing_case_wrong_types_and_unknown_arguments() {
    let mut negatives = Vec::new();
    for key in ["count", "ratio", "enabled", "tags", "context"] {
        let mut value = arguments();
        value.as_object_mut().unwrap().remove(key);
        negatives.push(value);
    }
    for (key, wrong) in [
        ("count", json!(2.5)),
        ("ratio", json!("1.5")),
        ("enabled", json!(0)),
        ("tags", json!(["a", 2])),
        ("context", json!({"label":"x","extra":false})),
        ("context", json!({"label":false})),
        ("note", json!(0)),
        ("Count", json!(2)),
        ("unknown", Value::Null),
    ] {
        let mut value = arguments();
        value[key] = wrong;
        negatives.push(value);
    }
    for value in negatives {
        let schema = FixtureSchema::new();
        assert_eq!(
            validate_arguments(
                &serde_json::to_vec(&value).unwrap(),
                &schema,
                fixture_limits()
            )
            .unwrap_err(),
            UdfValidationError::SchemaViolation
        );
        assert_eq!(schema.preflights.get(), 1);
        assert_eq!(schema.validations.get(), 1);
    }
}

#[test]
fn entire_schema_preflight_rejects_unvisited_unsupported_constructs_first() {
    for (keyword, value) in [
        ("$ref", json!("https://invalid.example/schema")),
        ("oneOf", json!([{"type":"string"}])),
        ("format", json!("not-implemented")),
        ("unknownKeyword", json!(true)),
    ] {
        let mut schema = FixtureSchema::new();
        schema.document["properties"]["note"][keyword] = value;
        assert_eq!(
            validate_arguments(b"not even JSON", &schema, fixture_limits()).unwrap_err(),
            UdfValidationError::UnsupportedSchema
        );
        assert_eq!(schema.preflights.get(), 1);
        assert_eq!(schema.validations.get(), 0);
    }
}

#[test]
fn duplicate_keys_are_rejected_recursively_after_json_unescaping() {
    for bytes in [
        br#"{"x":1,"x":2}"#.as_slice(),
        br#"{"x":1,"\u0078":2}"#,
        br#"{"nested":[{"x":0,"x":false}]}"#,
        br#"{"a":{"x":null,"\u0078":""}}"#,
    ] {
        assert_eq!(
            validate_arguments(bytes, &AnyObject, fixture_limits()).unwrap_err(),
            UdfValidationError::DuplicateKey
        );
    }
    assert!(
        validate_arguments(
            br#"{"x":1,"X":2,"a":{"x":3},"b":{"x":4}}"#,
            &AnyObject,
            fixture_limits()
        )
        .is_ok()
    );
}

#[test]
fn syntax_non_object_and_nonfinite_fail_without_success_shaped_fallback() {
    for bytes in [
        b"".as_slice(),
        b"{} {}",
        br#"{"x":true,}"#,
        br#"{"x":[1,]}"#,
        br#"{"x":"unterminated}"#,
        br#"{"x":"\q"}"#,
        br#"{"x":NaN}"#,
        br#"{"x":1e400}"#,
        br#"{"x":true false}"#,
        br#"{"x":01}"#,
        br#"{"x":[}"#,
        &[0xff],
    ] {
        assert!(
            validate_arguments(bytes, &AnyObject, fixture_limits()).is_err(),
            "{bytes:?}"
        );
    }
    for bytes in [b"null".as_slice(), b"false", b"0", b"[]", br#""text""#] {
        assert_eq!(
            validate_arguments(bytes, &AnyObject, fixture_limits()).unwrap_err(),
            UdfValidationError::ArgumentsNotObject
        );
    }
}

#[test]
fn representable_integers_are_not_rounded_and_overflow_is_explicit() {
    let bytes = br#"{"min":-9223372036854775808,"max":18446744073709551615,"precise":9007199254740993,"decimal":1.0,"exponent":1e0}"#;
    let validated = validate_arguments(bytes, &AnyObject, fixture_limits()).unwrap();
    assert_eq!(validated.bytes(), bytes);
    assert_eq!(validated.value()["min"].as_i64(), Some(i64::MIN));
    assert_eq!(validated.value()["max"].as_u64(), Some(u64::MAX));
    assert_eq!(
        validated.value()["precise"].as_u64(),
        Some(9_007_199_254_740_993)
    );
    assert!(validated.value()["decimal"].is_f64());
    assert!(validated.value()["exponent"].is_f64());
    for bytes in [
        br#"{"x":18446744073709551616}"#.as_slice(),
        br#"{"x":-9223372036854775809}"#,
    ] {
        assert_eq!(
            validate_arguments(bytes, &AnyObject, fixture_limits()).unwrap_err(),
            UdfValidationError::IntegerOutOfRange
        );
    }
}

#[test]
fn exact_caller_byte_depth_and_node_limits_cover_n_minus_one_n_n_plus_one() {
    let bytes = br#"{"x":[0]}"#;
    for (bound, accepted) in [
        (bytes.len() - 1, false),
        (bytes.len(), true),
        (bytes.len() + 1, true),
    ] {
        assert_eq!(
            validate_arguments(bytes, &AnyObject, limits(bound, 8, 16)).is_ok(),
            accepted
        );
    }
    for (depth, accepted) in [(1, false), (2, true), (3, true)] {
        assert_eq!(
            validate_arguments(bytes, &AnyObject, limits(64, depth, 16)).is_ok(),
            accepted
        );
    }
    // Root object, member name, array and integer: exactly four charged nodes.
    for (nodes, accepted) in [(3, false), (4, true), (5, true)] {
        assert_eq!(
            validate_arguments(bytes, &AnyObject, limits(64, 8, nodes)).is_ok(),
            accepted
        );
    }
}

#[test]
fn deeply_nested_input_is_iteratively_bounded_before_value_allocation() {
    let text = format!(r#"{{"x":{}{}}}"#, "[".repeat(1000), "]".repeat(1000));
    assert_eq!(
        validate_arguments(text.as_bytes(), &AnyObject, limits(4096, 16, 4096)).unwrap_err(),
        UdfValidationError::JsonLimit
    );
}

#[test]
fn complete_matching_success_preserves_every_output_shape_and_extra_fields() {
    for output in [
        json!(false),
        json!(0),
        Value::Null,
        json!([]),
        json!(""),
        json!("text"),
        json!([false, 0, null]),
        json!({"value":3}),
    ] {
        let mut body = native(output.clone());
        body["extra"] = json!({"native":true});
        let response = received(200, &body);
        assert!(response.succeeded());
        assert_eq!(response.kind(), UdfResponseKind::Succeeded);
        assert_eq!(response.output(), Some(&output));
        assert_eq!(response.invocation_id(), Some("offline-001"));
        assert_eq!(response.native_status(), Some("Succeeded"));
        assert_eq!(response.native(), Some(&body));
        assert_eq!(response.http_status(), 200);
        let debug = format!("{response:?}");
        assert!(!debug.contains("offline-001"));
        assert!(!debug.contains("extra"));
    }
}

#[test]
fn documented_native_failures_and_http_statuses_remain_distinct() {
    for native_status in ["Failed", "BadRequest", "Timeout", "ResponseTooLarge"] {
        for status in [200, 400, 401, 403, 408, 409, 413, 422, 429, 500, 503] {
            let mut body = native(Value::Null);
            body["status"] = json!(native_status);
            body["errors"] = json!([{"code":"offline-error","detail":"untrusted"}]);
            let response = received(status, &body);
            assert!(!response.succeeded());
            assert_eq!(response.kind(), UdfResponseKind::NativeFailure);
            assert_eq!(response.http_status(), status);
            assert_eq!(response.native_status(), Some(native_status));
            assert_eq!(response.errors(), body.get("errors"));
            assert_eq!(response.invocation_id(), Some("offline-001"));
        }
    }
    let mut too_large = native(Value::Null);
    too_large["status"] = json!("ResponseTooLarge");
    assert_eq!(
        received(403, &too_large).native_status(),
        Some("ResponseTooLarge")
    );
}

#[test]
fn non_200_success_and_nonempty_errors_are_contradictory_never_success() {
    for status in [201, 204, 301, 307, 400, 401, 403, 429, 500, 503] {
        let response = received(status, &native(json!(0)));
        assert_eq!(
            response.kind(),
            UdfResponseKind::ProtocolFailure(UdfProtocolIssue::ContradictorySuccess)
        );
        assert_eq!(response.native_status(), Some("Succeeded"));
        assert_eq!(response.output(), Some(&json!(0)));
        assert!(!response.succeeded());
    }
    let mut body = native(json!(false));
    body["errors"] = json!([{"code":"failure"}]);
    assert_eq!(
        received(200, &body).kind(),
        UdfResponseKind::ProtocolFailure(UdfProtocolIssue::ContradictorySuccess)
    );
}

#[test]
fn unexpected_202_never_becomes_accepted_success_or_a_monitor() {
    let mut body = native(json!("result"));
    body["monitorUrl"] = json!("https://invalid.example/not-an-approved-monitor");
    let response = received(202, &body);
    assert_eq!(
        response.kind(),
        UdfResponseKind::ProtocolFailure(UdfProtocolIssue::UnsupportedAsync)
    );
    assert_eq!(response.invocation_id(), Some("offline-001"));
    assert!(!response.succeeded());
}

#[test]
fn missing_wrong_type_unknown_and_wrong_function_data_are_not_success() {
    for (key, issue) in [
        ("functionName", UdfProtocolIssue::FunctionMismatch),
        ("invocationId", UdfProtocolIssue::MissingCorrelation),
        ("status", UdfProtocolIssue::MissingStatus),
        ("errors", UdfProtocolIssue::InvalidErrors),
        ("output", UdfProtocolIssue::MissingOutput),
    ] {
        let mut body = native(Value::Null);
        body.as_object_mut().unwrap().remove(key);
        assert_eq!(
            received(200, &body).kind(),
            UdfResponseKind::ProtocolFailure(issue)
        );
        if key != "output" {
            body[key] = json!(0);
            assert_eq!(
                received(200, &body).kind(),
                UdfResponseKind::ProtocolFailure(issue)
            );
        }
    }
    for (key, wrong, issue) in [
        (
            "functionName",
            json!("Calculate"),
            UdfProtocolIssue::FunctionMismatch,
        ),
        (
            "invocationId",
            json!(""),
            UdfProtocolIssue::MissingCorrelation,
        ),
        ("status", json!(""), UdfProtocolIssue::MissingStatus),
        ("status", json!("Running"), UdfProtocolIssue::UnknownStatus),
        ("errors", Value::Null, UdfProtocolIssue::InvalidErrors),
    ] {
        let mut body = native(Value::Null);
        body[key] = wrong;
        assert_eq!(
            received(200, &body).kind(),
            UdfResponseKind::ProtocolFailure(issue)
        );
    }
    assert_eq!(
        received(200, &json!(false)).kind(),
        UdfResponseKind::ProtocolFailure(UdfProtocolIssue::NotEnvelope)
    );
}

#[test]
fn malformed_or_truncated_body_keeps_http_and_raw_evidence_not_success() {
    for body in [
        b"not json".as_slice(),
        br#"{"functionName":"calculate","invocationId":"offline-001""#,
        br#"{"status":"Succeeded","status":"Failed"}"#,
        br#"{"x":18446744073709551616}"#,
    ] {
        let response = UdfResponse::from_received("calculate", 403, body, fixture_limits());
        assert!(!response.succeeded());
        assert_eq!(response.http_status(), 403);
        assert_eq!(response.body(), Some(body));
        assert!(response.native().is_none());
    }
}

#[test]
fn parseable_partial_body_retains_native_success_evidence_without_claiming_success() {
    let body = native(json!(false));
    let bytes = serde_json::to_vec(&body).unwrap();
    let response = UdfResponse::from_partial("calculate", 200, &bytes, fixture_limits());
    assert!(!response.succeeded());
    assert_eq!(
        response.kind(),
        UdfResponseKind::ProtocolFailure(UdfProtocolIssue::IncompleteBody)
    );
    assert_eq!(response.native_status(), Some("Succeeded"));
    assert_eq!(response.invocation_id(), Some("offline-001"));
    assert_eq!(response.output(), Some(&json!(false)));
    assert_eq!(response.body(), Some(bytes.as_slice()));
    let oversized = UdfResponse::from_partial("calculate", 403, &bytes, limits(1, 16, 256));
    assert_eq!(oversized.http_status(), 403);
    assert!(oversized.body().is_none());
    assert_eq!(
        oversized.kind(),
        UdfResponseKind::ProtocolFailure(UdfProtocolIssue::ResponseLimit)
    );
}

#[test]
fn response_limits_never_clip_an_envelope_into_valid_success() {
    let bytes = serde_json::to_vec(&native(json!(0))).unwrap();
    for (bound, accepted) in [
        (bytes.len() - 1, false),
        (bytes.len(), true),
        (bytes.len() + 1, true),
    ] {
        let response = UdfResponse::from_received("calculate", 200, &bytes, limits(bound, 16, 256));
        assert_eq!(response.succeeded(), accepted);
        assert_eq!(response.http_status(), 200);
        if !accepted {
            assert!(response.body().is_none());
            assert!(response.native().is_none());
            assert_eq!(
                response.kind(),
                UdfResponseKind::ProtocolFailure(UdfProtocolIssue::ResponseLimit)
            );
        }
    }
    for bounded in [limits(4096, 1, 256), limits(4096, 16, 1)] {
        let response = UdfResponse::from_received("calculate", 200, &bytes, bounded);
        // This fixture has no nested container except the errors array.
        if bounded.max_depth.get() == 1 || bounded.max_nodes.get() == 1 {
            assert!(!response.succeeded());
            assert_eq!(response.body(), Some(bytes.as_slice()));
        }
    }
}

#[test]
fn diagnostics_never_echo_argument_source_or_provider_details() {
    for error in [
        UdfValidationError::UnsupportedSchema,
        UdfValidationError::SchemaViolation,
        UdfValidationError::InvalidJson,
        UdfValidationError::DuplicateKey,
        UdfValidationError::IntegerOutOfRange,
        UdfValidationError::JsonLimit,
        UdfValidationError::ArgumentsNotObject,
    ] {
        let text = format!("{error}: {error:?}");
        assert!(!text.contains("sensitive-fixture"));
        assert!(text.len() < 160);
    }
    let mut body = native(json!("sensitive-fixture"));
    body["errors"] = json!([{"detail":"sensitive-fixture"}]);
    assert!(!format!("{:?}", received(200, &body)).contains("sensitive-fixture"));
}
