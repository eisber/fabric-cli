// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Definition encoding uses the public data API, not the command envelope.

use fabric_cli_output::{Indent, OutputEnvelope, OutputInput, Timestamp, json_dumps};
use serde_json::{Value, json};

#[test]
fn default_python_json_dumps_keeps_raw_data_and_exact_handwritten_bytes() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../tests/compatibility/output/definition-cases.json"
    ))
    .unwrap();
    for case in cases {
        assert_eq!(
            json_dumps(&case["data"]).unwrap(),
            case["expected"],
            "{}",
            case["name"]
        );
    }
    let raw = json!({"definition": {"parts": []}});
    let encoded = json_dumps(&raw).unwrap();
    assert!(!encoded.contains('\n'));
    let envelope = OutputEnvelope::new(
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        OutputInput {
            data: Some(raw),
            ..OutputInput::default()
        },
    );
    assert_ne!(encoded, envelope.to_json(Indent::Zero).unwrap());
}
