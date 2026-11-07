// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public options consumer for Python definition metadata UTF-8 byte compatibility.

use fabric_core_json::{
    Format, Indent, Options, encode, encode_unicode, encode_with_options, json_dumps,
};
use serde_json::{Value, json};

#[test]
fn ensure_ascii_false_matches_handwritten_source_bytes_in_every_layout() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../tests/compatibility/output/unicode-json-cases.json"
    ))
    .unwrap();
    for case in cases {
        let format = match case.get("indent") {
            Some(Value::Null) => Format::Default,
            Some(value) if value == 0 => Format::Pretty(Indent::Zero),
            Some(value) if value == 2 => Format::Pretty(Indent::Two),
            Some(value) if value == 4 => Format::Pretty(Indent::Four),
            None => Format::Pretty(Indent::Four),
            _ => panic!("unsupported fixture indentation"),
        };
        let output = encode_unicode(&case["data"], format);
        assert_eq!(
            output,
            encode_with_options(
                &case["data"],
                Options {
                    format,
                    ensure_ascii: false
                }
            )
        );
        assert_eq!(
            output.as_bytes(),
            case["expected"].as_str().unwrap().as_bytes(),
            "{}",
            case["name"]
        );
        assert!(!output.ends_with('\n'));
        assert!(!output.contains('\r'));
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            case["data"]
        );
        assert_eq!(
            encode_with_options(
                &case["data"],
                Options {
                    format,
                    ensure_ascii: true
                }
            ),
            encode(&case["data"], format)
        );
    }
}

#[test]
fn defaults_and_legacy_ascii_apis_are_unchanged() {
    let value = json!({"name":"caf\u{e9} \u{1f680} \u{7f}"});
    assert_eq!(
        Options::default(),
        Options {
            format: Format::Default,
            ensure_ascii: true
        }
    );
    assert_eq!(
        encode_with_options(&value, Options::default()),
        json_dumps(&value)
    );
    assert_eq!(
        json_dumps(&value),
        "{\"name\": \"caf\\u00e9 \\ud83d\\ude80 \\u007f\"}"
    );
    assert_eq!(
        encode(&value, Format::Pretty(Indent::Four)),
        "{\n    \"name\": \"caf\\u00e9 \\ud83d\\ude80 \\u007f\"\n}"
    );
}

#[test]
fn unsupported_values_are_not_coerced_by_the_new_option() {
    for text in [
        r#""\ud800""#,
        r#""\udfff""#,
        "NaN",
        "Infinity",
        "-Infinity",
        "1e999",
    ] {
        assert!(serde_json::from_str::<Value>(text).is_err(), "{text}");
    }
    for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(serde_json::Number::from_f64(number).is_none());
    }
    // Value is already finite JSON. Arbitrary integer fidelity and upstream
    // lossy conversions remain outside this formatting-only option.
    assert_eq!(
        encode_with_options(
            &Value::Null,
            Options {
                format: Format::Pretty(Indent::Four),
                ensure_ascii: false
            }
        ),
        "null"
    );
}
