// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Independently specified Python JSON formatting examples.
#![deny(unnameable_test_items)]
#![forbid(unsafe_code)]
use fabric_core_json::{Format, Indent, encode, json_dumps};
use serde_json::json;

#[test]
fn four_spaces_ascii_surrogate_pairs_and_no_implicit_newline() {
    assert_eq!(
        encode(&json!({"é":"😀","empty":[]}), Format::Pretty(Indent::Four)),
        "{\n    \"\\u00e9\": \"\\ud83d\\ude00\",\n    \"empty\": []\n}"
    );
    assert_eq!(json_dumps(&json!([1, 2, "a\n"])), r#"[1, 2, "a\n"]"#);
}
#[test]
fn python_float_layout_thresholds_and_negative_zero() {
    assert_eq!(
        json_dumps(&json!([1e-5, 1e-4, 1e15, 1e16, -0.0])),
        "[1e-05, 0.0001, 1000000000000000.0, 1e+16, -0.0]"
    );
}
