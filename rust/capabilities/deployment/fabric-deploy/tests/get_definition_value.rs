// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

//! Independent pinned source value receipts, never Rust-generated expectations.

use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_core_json::{Options, PythonValue, encode_python_with_options};
use fabric_deploy::definitions::{
    DefinitionErrorKind, DefinitionLimits, ExportStorage, GetDefinitionPreprocess,
    GetDefinitionValue, GetDefinitionValueError, decode_export_files_for_storage,
    decode_get_definition_value,
};
use serde_json::{Value, json};
use std::num::NonZeroUsize;

const PIN: &str = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4";

fn receipt() -> Value {
    serde_json::from_str(include_str!("get_definition_value_reference.json")).unwrap()
}

fn decode(case: &Value) -> Result<GetDefinitionValue, GetDefinitionValueError> {
    decode_get_definition_value(
        &case["metadata"],
        (!case["definition"].is_null()).then_some(&case["definition"]),
        case["item_type"].as_str().unwrap(),
        if case["decode"].as_bool().unwrap() {
            GetDefinitionPreprocess::Decode
        } else {
            GetDefinitionPreprocess::Preserve
        },
        DefinitionLimits::default(),
    )
}

fn python_tag(value: &PythonValue) -> Value {
    json!({"$python": encode_python_with_options(value, Options::default()).unwrap()})
}

fn normalized(value: &GetDefinitionValue) -> Value {
    match value {
        GetDefinitionValue::Bytes(bytes) => json!({"$bytes": bytes}),
        GetDefinitionValue::Scalar(value) => {
            value.try_to_json().unwrap_or_else(|_| python_tag(value))
        }
        GetDefinitionValue::List(values) => Value::Array(values.iter().map(normalized).collect()),
        GetDefinitionValue::Object(entries) => {
            if entries.iter().any(|(key, _)| key.try_to_string().is_err()) {
                return python_tag(&value.try_to_python().unwrap());
            }
            let mut result = serde_json::Map::new();
            for (key, value) in entries {
                result.insert(key.try_to_string().unwrap(), normalized(value));
            }
            Value::Object(result)
        }
    }
}

fn field<'a>(value: &'a GetDefinitionValue, name: &str) -> &'a GetDefinitionValue {
    let GetDefinitionValue::Object(entries) = value else {
        panic!("expected dictionary")
    };
    &entries
        .iter()
        .find(|(key, _)| key.try_to_string().as_deref() == Ok(name))
        .unwrap()
        .1
}

fn parts(value: &GetDefinitionValue) -> &[GetDefinitionValue] {
    let GetDefinitionValue::List(parts) = field(field(value, "definition"), "parts") else {
        panic!("expected parts")
    };
    parts
}

fn source_case(name: &str) -> Value {
    receipt()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap()
        .clone()
}

fn definition(parts: Value) -> Value {
    json!({"definition": {"parts": parts}})
}

fn input_part(path: &str, content: &[u8]) -> Value {
    json!({"path": path, "payload": STANDARD.encode(content), "payloadType": "InlineBase64"})
}

fn decode_definition(
    definition: &Value,
    item_type: &str,
    limits: DefinitionLimits,
) -> Result<GetDefinitionValue, GetDefinitionValueError> {
    decode_get_definition_value(
        &json!({"properties":{"specialized":true}}),
        Some(definition),
        item_type,
        GetDefinitionPreprocess::Decode,
        limits,
    )
}

#[test]
fn complete_pinned_source_receipts_match_values_errors_and_order() {
    let receipt = receipt();
    assert_eq!(receipt["source"], PIN);
    assert_eq!(receipt["cases"].as_array().unwrap().len(), 130);
    for case in receipt["cases"].as_array().unwrap() {
        let result = decode(case);
        if case["strict_base64_exception"] == true {
            let error = result.unwrap_err();
            assert!(
                matches!(error.error.kind, DefinitionErrorKind::Unsupported(_)),
                "{case}"
            );
            assert_eq!(error.location, "/definition/parts/0/payload");
        } else if case.get("source_error").is_some() {
            let error = result.unwrap_err();
            assert!(
                matches!(error.error.kind, DefinitionErrorKind::InvalidDefinition),
                "{case}"
            );
        } else {
            let result = result.unwrap_or_else(|error| panic!("{}: {error:?}", case["name"]));
            assert_eq!(normalized(&result), case["expected"], "{}", case["name"]);
            assert_eq!(
                serde_json::to_string(&normalized(&result)).unwrap(),
                serde_json::to_string(&case["expected"]).unwrap(),
                "nested dictionary order: {}",
                case["name"]
            );
            let GetDefinitionValue::Object(entries) = &result else {
                panic!("expected item")
            };
            let keys: Vec<_> = entries
                .iter()
                .map(|(key, _)| Value::String(key.try_to_string().unwrap()))
                .collect();
            assert_eq!(
                Value::Array(keys),
                case["expected_key_order"],
                "{}",
                case["name"]
            );
        }
    }
}

#[test]
fn independently_asserted_get_is_not_file_export_or_json_stringification() {
    let acquired = definition(json!([
        input_part("/not/a/local/path.json", b"false"),
        input_part("model.tmdl", b"false"),
        input_part("text.json", br#""false""#),
        input_part("binary.json", &[255, 0]),
    ]));
    let decoded =
        decode_definition(&acquired, "SemanticModel", DefinitionLimits::default()).unwrap();
    let parts = parts(&decoded);
    assert_eq!(
        field(&parts[0], "payload"),
        &GetDefinitionValue::Scalar(PythonValue::Bool(false))
    );
    assert_eq!(field(&parts[1], "payload").try_to_json().unwrap(), "false");
    assert_eq!(field(&parts[2], "payload").try_to_json().unwrap(), "false");
    assert_eq!(
        field(&parts[3], "payload"),
        &GetDefinitionValue::Bytes(vec![255, 0])
    );
    assert_eq!(
        field(&parts[0], "path").try_to_json().unwrap(),
        "/not/a/local/path.json"
    );
    assert_eq!(
        field(&parts[3], "payloadType").try_to_json().unwrap(),
        "DecodeBase64"
    );
    let error = decoded.try_to_json().unwrap_err();
    assert_eq!(error.location, "/definition/parts/3/payload");
    assert!(matches!(
        error.error.kind,
        DefinitionErrorKind::Unsupported(_)
    ));
    assert!(decoded.try_to_python().is_err());
}

#[test]
fn caught_decode_failures_keep_payload_type_and_prior_properties_without_aborting_later_parts() {
    let acquired = definition(json!([
        {"path":"bad.json","payload":STANDARD.encode(b"{broken"),"payloadType":"Original","error":"old"},
        {"path":"bad.txt","payload":"ab","payloadType":"Original"},
        input_part("ok.json", b"true"),
    ]));
    let result = decode_definition(&acquired, "Report", DefinitionLimits::default())
        .unwrap()
        .try_to_json()
        .unwrap();
    assert_eq!(
        result["definition"]["parts"][0],
        json!({
            "path":"bad.json","payload":STANDARD.encode(b"{broken"),"payloadType":"Original",
            "error":"Decoding error: Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"
        })
    );
    assert_eq!(
        result["definition"]["parts"][1]["error"],
        "Decoding error: Incorrect padding"
    );
    assert_eq!(result["definition"]["parts"][1]["payloadType"], "Original");
    assert_eq!(result["definition"]["parts"][2]["payload"], true);
    assert_eq!(result["properties"], json!({"specialized":true}));
}

#[test]
fn notebook_cleanup_is_exact_case_first_part_and_existing_outputs_only() {
    let case = source_case("notebook-first-existing-outputs-only");
    let result = decode(&case).unwrap().try_to_json().unwrap();
    assert_eq!(
        result["definition"]["parts"][0]["payload"]["cells"],
        json!([
            {"outputs":[],"execution_count":7},{"source":["x"]}
        ])
    );
    assert_eq!(
        result["definition"]["parts"][1]["payload"]["cells"][0]["outputs"],
        json!([2])
    );
    let mut lower = case.clone();
    lower["item_type"] = json!("notebook");
    assert_eq!(
        decode(&lower).unwrap().try_to_json().unwrap()["definition"]["parts"][0]["payload"]["cells"]
            [0]["outputs"],
        json!([1])
    );
    let preserved = decode(&source_case("notebook-preserve"))
        .unwrap()
        .try_to_json()
        .unwrap();
    assert!(preserved["definition"]["parts"][0]["payload"].is_string());
    assert_eq!(
        preserved["definition"]["parts"][0]["payloadType"],
        "InlineBase64"
    );
}

#[test]
fn nested_parts_are_a_source_presence_marker_not_an_invented_export_traversal() {
    let result = decode(&source_case("nested-source-selector"))
        .unwrap()
        .try_to_json()
        .unwrap();
    assert_eq!(
        result["definition"]["parts"][0]["definitionParts"][0]["payload"],
        json!({"inner":true})
    );
    let result = decode(&source_case("nested-key-does-not-traverse-array"))
        .unwrap()
        .try_to_json()
        .unwrap();
    assert_eq!(
        result["definition"]["parts"][0]["nested_parts"][0]["payload"],
        "dHJ1ZQ=="
    );
    let result = decode(&source_case("nested-without-marker-not-decoded"))
        .unwrap()
        .try_to_json()
        .unwrap();
    assert_eq!(
        result["definition"]["parts"][0]["definitionParts"][0]["payload"],
        "dHJ1ZQ=="
    );
}

#[test]
fn python_numeric_and_unicode_domains_are_preserved_with_explicit_serde_conversion_failures() {
    for content in [
        "184467440737095516160123456789",
        "NaN",
        "Infinity",
        "-Infinity",
        "1e999",
        r#""\ud800""#,
        r#"{"\udfff": 1}"#,
    ] {
        let acquired = definition(json!([input_part("v.json", content.as_bytes())]));
        let decoded =
            decode_definition(&acquired, "Environment", DefinitionLimits::default()).unwrap();
        assert!(decoded.try_to_python().is_ok(), "{content}");
        let error = decoded.try_to_json().unwrap_err();
        assert!(
            matches!(error.error.kind, DefinitionErrorKind::Unsupported(_)),
            "{content}"
        );
        assert!(
            error.location.starts_with("/definition/parts/0/payload"),
            "{content}"
        );
    }
    let acquired = definition(json!([input_part("v.json", b"-0.0")]));
    let decoded = decode_definition(&acquired, "Report", DefinitionLimits::default()).unwrap();
    let GetDefinitionValue::Scalar(PythonValue::Float(value)) =
        field(&parts(&decoded)[0], "payload")
    else {
        panic!("expected float")
    };
    assert_eq!(value.to_bits(), (-0.0_f64).to_bits());
}

#[test]
fn finite_part_depth_path_byte_node_and_decimal_bounds_fail_without_empty_success() {
    let mut limits = DefinitionLimits {
        max_files: NonZeroUsize::new(1).unwrap(),
        ..DefinitionLimits::default()
    };
    let acquired = definition(json!([
        input_part("a.txt", b"a"),
        input_part("b.txt", b"b")
    ]));
    let error = decode_definition(&acquired, "Report", limits).unwrap_err();
    assert!(matches!(error.error.kind, DefinitionErrorKind::Limit));
    assert_eq!(error.location, "/definition/parts/1");

    limits = DefinitionLimits::default();
    limits.max_path_bytes = NonZeroUsize::new(2).unwrap();
    let error = decode_definition(
        &definition(json!([input_part("long.txt", b"x")])),
        "Report",
        limits,
    )
    .unwrap_err();
    assert!(matches!(error.error.kind, DefinitionErrorKind::Limit));
    assert_eq!(error.location, "/definition/parts/0/path");

    limits = DefinitionLimits::default();
    limits.max_bytes = NonZeroUsize::new(1).unwrap();
    assert!(matches!(
        decode_definition(&json!({}), "Report", limits)
            .unwrap_err()
            .error
            .kind,
        DefinitionErrorKind::Limit
    ));

    limits = DefinitionLimits::default();
    limits.max_depth = NonZeroUsize::new(1).unwrap();
    let error = decode(&source_case("nested-recursive-definition")).unwrap();
    assert!(error.try_to_json().is_ok());
    let error = decode_definition(
        &source_case("nested-recursive-definition")["definition"],
        "Report",
        limits,
    )
    .unwrap_err();
    assert!(matches!(error.error.kind, DefinitionErrorKind::Limit));

    for content in [
        "1".repeat(fabric_core_json::MAX_DECIMAL_DIGITS + 1),
        format!("{}0{}", "[".repeat(130), "]".repeat(130)),
        format!("[{}]", "0,".repeat(fabric_core_json::MAX_NATIVE_NODES)),
        format!(
            "\"{}\"",
            "x".repeat(fabric_core_json::MAX_STRING_CODEPOINTS + 1)
        ),
    ] {
        let error = decode_definition(
            &definition(json!([input_part("v.json", content.as_bytes())])),
            "Report",
            DefinitionLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(error.error.kind, DefinitionErrorKind::Limit));
        assert!(error.error.completed_paths.is_empty());
        assert!(error.error.incomplete_path.is_none());
    }
}

#[test]
fn malformed_structure_is_typed_but_missing_definition_is_valid_metadata_only() {
    for value in [Value::Null, json!([]), json!("not metadata")] {
        let error = decode_get_definition_value(
            &value,
            None,
            "Report",
            GetDefinitionPreprocess::Decode,
            DefinitionLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(
            error.error.kind,
            DefinitionErrorKind::InvalidDefinition
        ));
        assert_eq!(error.location, "");
    }
    for (acquired, location) in [
        (json!([]), "/acquired-definition"),
        (definition(json!(null)), "/definition/parts"),
        (definition(json!([null])), "/definition/parts/0"),
        (json!({"definition":null}), "/definition"),
    ] {
        let error =
            decode_definition(&acquired, "Report", DefinitionLimits::default()).unwrap_err();
        assert!(matches!(
            error.error.kind,
            DefinitionErrorKind::InvalidDefinition
        ));
        assert_eq!(error.location, location);
        assert!(error.error.completed_paths.is_empty());
        assert!(error.error.incomplete_path.is_none());
    }
    let metadata = json!({"id":"synthetic","properties":{"specialized":{"a":1}}});
    assert_eq!(
        decode_get_definition_value(
            &metadata,
            None,
            "Report",
            GetDefinitionPreprocess::Decode,
            DefinitionLimits::default()
        )
        .unwrap()
        .try_to_json()
        .unwrap(),
        metadata
    );
}

#[test]
fn decoder_does_not_mutate_acquired_inputs_or_expose_content_in_debug() {
    let acquired = definition(json!([input_part("v.txt", b"sensitive-synthetic-content")]));
    let before = acquired.clone();
    let decoded = decode_definition(&acquired, "Report", DefinitionLimits::default()).unwrap();
    assert_eq!(acquired, before);
    assert!(!format!("{decoded:?}").contains("sensitive-synthetic-content"));
}

#[test]
fn get_json_diagnostics_do_not_change_existing_export_fallback_order() {
    for suffix in [".", "e+"] {
        let content = format!(
            "{}{suffix}",
            "1".repeat(fabric_core_json::MAX_DECIMAL_DIGITS + 1)
        );
        let acquired = definition(json!([input_part("v.txt", content.as_bytes())]));
        let files = decode_export_files_for_storage(
            &acquired,
            false,
            DefinitionLimits::default(),
            ExportStorage::Local,
        )
        .unwrap();
        assert_eq!(files[0].bytes, content.as_bytes());
    }
}
