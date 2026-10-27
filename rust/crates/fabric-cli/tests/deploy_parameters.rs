// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public native parameter parsing against actual immutable Python wrapper input.

use fabric_cli::deploy::parameters;
use serde_json::Value;

#[test]
fn actual_python_depth_one_and_json_typing_receipts_match() {
    let fixture: Value = serde_json::from_str(include_str!("deploy_wrapper_source.json")).unwrap();
    assert_eq!(
        fixture["source"],
        "microsoft/fabric-cli@cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    let cases = fixture["parameters"].as_array().unwrap();
    assert_eq!(cases.len(), 8);
    for case in cases {
        let tokens: Vec<String> = serde_json::from_value(case["tokens"].clone()).unwrap();
        assert_eq!(
            Value::Object(parameters(&tokens).unwrap()),
            case["expected"],
            "{}",
            case["id"]
        );
    }
}

#[test]
fn unsupported_or_invalid_lexical_inputs_fail_explicitly() {
    for value in [
        "",
        "not-an-assignment",
        "key=",
        "space key=value",
        "a=line\nbreak",
        "ключ=value",
    ] {
        assert!(parameters(&[value.into()]).is_err(), "{value}");
    }
}
