// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! SDK-independent public core consumer and unchanged source byte fixture regression.

use fabric_core_json::{Format, Indent, encode, json_dumps};
use serde_json::{Value, json};

#[test]
fn shared_default_encoder_matches_every_handwritten_source_case() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/output/definition-cases.json"
    ))
    .unwrap();
    for case in cases {
        assert_eq!(
            json_dumps(&case["data"]),
            case["expected"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn shared_pretty_encoding_matches_original_literal_output_cases() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/output/cases.json"
    ))
    .unwrap();
    for case in fixtures["cases"].as_array().unwrap() {
        let Some(expected) = case["json"].as_str() else {
            continue;
        };
        let indent = match case["indent"].as_u64().unwrap_or(4) {
            0 => Indent::Zero,
            2 => Indent::Two,
            4 => Indent::Four,
            _ => panic!("unsupported fixture indentation"),
        };
        let value: Value = serde_json::from_str(expected).unwrap();
        assert_eq!(encode(&value, Format::Pretty(indent)), expected);
    }
    assert_eq!(encode(&json!({}), Format::Pretty(Indent::Zero)), "{}");
}
