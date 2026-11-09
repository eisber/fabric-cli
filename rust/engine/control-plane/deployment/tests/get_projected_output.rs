// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Projected-result storage does not decode definition parts or wrap envelopes.

#![deny(unnameable_test_items)]

use fabric_deploy::definitions::{
    DefinitionErrorKind, DefinitionLimits, ExportStorage, GetOutputNewline,
    serialize_get_projected_result,
};
use serde_json::{Value, json};
use std::num::NonZeroUsize;

#[test]
fn local_json_is_pretty_unicode_and_remote_is_ascii_default() {
    let data = json!({"name":"café 🚀","value":[1,false]});
    let local = serialize_get_projected_result(
        &data,
        ExportStorage::Local,
        GetOutputNewline::Lf,
        DefinitionLimits::default(),
    )
    .unwrap();
    assert!(local.is_json);
    assert_eq!(
        String::from_utf8(local.bytes).unwrap(),
        "{\n    \"name\": \"café 🚀\",\n    \"value\": [\n        1,\n        false\n    ]\n}"
    );
    let remote = serialize_get_projected_result(
        &data,
        ExportStorage::Lakehouse,
        GetOutputNewline::CrLf,
        DefinitionLimits::default(),
    )
    .unwrap();
    assert!(remote.is_json);
    assert_eq!(
        remote.bytes,
        br#"{"name": "caf\u00e9 \ud83d\ude80", "value": [1, false]}"#
    );
}

#[test]
fn source_validate_json_controls_string_parsing_and_local_scalar_spelling() {
    for (value, expected, is_json) in [
        (json!("false"), "false", true),
        (json!("null"), "null", true),
        (json!("42"), "42", true),
        (json!("\"café\""), "\"café\"", true),
        (json!("NaN"), "NaN", true),
        (json!("18446744073709551616"), "18446744073709551616", true),
        (json!("not JSON"), "not JSON", false),
        (json!(""), "", false),
        (Value::Null, "None", false),
        (json!(false), "False", false),
        (json!(true), "True", false),
        (json!(0), "0", false),
        (json!([]), "[]", true),
        (json!({}), "{}", true),
    ] {
        let output = serialize_get_projected_result(
            &value,
            ExportStorage::Local,
            GetOutputNewline::Lf,
            DefinitionLimits::default(),
        )
        .unwrap();
        assert_eq!(output.bytes, expected.as_bytes(), "{value}");
        assert_eq!(output.is_json, is_json, "{value}");
    }
}

#[test]
fn remote_scalars_fail_instead_of_emitting_success_shaped_empty_json() {
    for value in [Value::Null, json!(false), json!(0), json!(1.25)] {
        let error = serialize_get_projected_result(
            &value,
            ExportStorage::Lakehouse,
            GetOutputNewline::Lf,
            DefinitionLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(error.kind, DefinitionErrorKind::InvalidDefinition));
        assert!(error.completed_paths.is_empty());
        assert!(error.incomplete_path.is_none());
    }
}

#[test]
fn explicit_text_newlines_include_existing_crlf_and_expansion_is_bounded() {
    let value = json!("one\r\ntwo\n");
    let local = serialize_get_projected_result(
        &value,
        ExportStorage::Local,
        GetOutputNewline::CrLf,
        DefinitionLimits::default(),
    )
    .unwrap();
    assert_eq!(local.bytes, b"one\r\r\ntwo\r\n");
    let remote = serialize_get_projected_result(
        &value,
        ExportStorage::Lakehouse,
        GetOutputNewline::CrLf,
        DefinitionLimits::default(),
    )
    .unwrap();
    assert_eq!(remote.bytes, b"one\r\ntwo\n");
    let limits = DefinitionLimits {
        max_bytes: NonZeroUsize::new(9).unwrap(),
        ..DefinitionLimits::default()
    };
    assert!(matches!(
        serialize_get_projected_result(
            &value,
            ExportStorage::Local,
            GetOutputNewline::CrLf,
            limits
        )
        .unwrap_err()
        .kind,
        DefinitionErrorKind::Limit
    ));
}

#[test]
fn unsupported_local_surrogate_is_not_repaired_and_remote_preserves_ascii_escape() {
    let value = json!(r#""\ud800""#);
    assert!(
        serialize_get_projected_result(
            &value,
            ExportStorage::Local,
            GetOutputNewline::Lf,
            DefinitionLimits::default()
        )
        .is_err()
    );
    let remote = serialize_get_projected_result(
        &value,
        ExportStorage::Lakehouse,
        GetOutputNewline::Lf,
        DefinitionLimits::default(),
    )
    .unwrap();
    assert_eq!(remote.bytes, br#""\ud800""#);
}
