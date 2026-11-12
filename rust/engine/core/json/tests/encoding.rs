// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Independently specified Python JSON formatting examples.
#![deny(unnameable_test_items)]
#![forbid(unsafe_code)]
use fabric_core_json::{Format, Indent, decode, encode, json_dumps};
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

#[test]
fn arbitrary_size_python_integers_never_round_through_f64() {
    // Independent Python json.loads/json.dumps decimal integer expectations.
    let source = r#"{"u64":18446744073709551615,"above":18446744073709551617,"i64":-9223372036854775808,"below":-9223372036854775809,"huge":123456789012345678901234567890123456789012345678901234567890,"negative":-123456789012345678901234567890123456789012345678901234567890}"#;
    let expected = r#"{"u64": 18446744073709551615, "above": 18446744073709551617, "i64": -9223372036854775808, "below": -9223372036854775809, "huge": 123456789012345678901234567890123456789012345678901234567890, "negative": -123456789012345678901234567890123456789012345678901234567890}"#;
    let value = decode(source.as_bytes()).unwrap();
    assert_eq!(json_dumps(&value), expected);
    assert_eq!(decode(expected.as_bytes()).unwrap(), value);
    // Exercise the standalone encoder too, not only the normalized decoder.
    assert_eq!(json_dumps(&serde_json::from_str(source).unwrap()), expected);
    assert_eq!(
        encode(
            &decode(b"[18446744073709551617,-9223372036854775809]").unwrap(),
            Format::Pretty(Indent::Four),
        ),
        "[\n    18446744073709551617,\n    -9223372036854775809\n]",
    );
}

#[test]
fn arbitrary_precision_does_not_change_python_finite_float_semantics() {
    let value = decode(b"[0.10000000000000001,1.0000,1e2,1e-9999,-1e-9999]").unwrap();
    assert_eq!(json_dumps(&value), "[0.1, 1.0, 100.0, 0.0, -0.0]");
    assert_eq!(
        json_dumps(&serde_json::from_str("[0.10000000000000001,1.0000]").unwrap()),
        "[0.1, 1.0]",
    );
    for source in ["1e9999", "-1e9999", r#"{"nested":[1e9999]}"#] {
        let error = decode(source.as_bytes()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Invalid JSON or non-finite floating-point value"
        );
        assert!(!format!("{error:?}").contains(source));
    }

    #[test]
    fn genuine_private_number_keys_remain_ordered_objects_not_synthetic_numbers() {
        // Independent Python json.loads/json.dumps expectations, including escaped
        // spellings of the same legal key. Never deserialize expected objects into Value.
        for (source, expected) in [
            (
                r#"{"$serde_json::private::Number":"123"}"#,
                r#"{"$serde_json::private::Number": "123"}"#,
            ),
            (
                r#"{"$serde_json::private::Number":"not a number"}"#,
                r#"{"$serde_json::private::Number": "not a number"}"#,
            ),
            (
                r#"{"$serde_json::private::Number":18446744073709551617}"#,
                r#"{"$serde_json::private::Number": 18446744073709551617}"#,
            ),
            (
                r#"{"metadata":{"$serde_json::private::Number":"123"},"wide":18446744073709551617,"negative":-9223372036854775809}"#,
                r#"{"metadata": {"$serde_json::private::Number": "123"}, "wide": 18446744073709551617, "negative": -9223372036854775809}"#,
            ),
            (
                r#"{"\u0024serde_json::private::Number":"not a number"}"#,
                r#"{"$serde_json::private::Number": "not a number"}"#,
            ),
            (
                r#"{"metadata":{"$serde_json::private::\u004eumber":"123"},"array":[{"$serde_json::private::Number":"not a number"}]}"#,
                r#"{"metadata": {"$serde_json::private::Number": "123"}, "array": [{"$serde_json::private::Number": "not a number"}]}"#,
            ),
            (
                r#"{"before":1,"$serde_json::private::Number":"old","after":2,"$serde_json::private::Number":"new"}"#,
                r#"{"before": 1, "$serde_json::private::Number": "new", "after": 2}"#,
            ),
            (
                r#"{"$serde_json::private::Number":{"nested":[true,null,-9223372036854775809]},"$serde_json::private::RawValue":"literal"}"#,
                r#"{"$serde_json::private::Number": {"nested": [true, null, -9223372036854775809]}, "$serde_json::private::RawValue": "literal"}"#,
            ),
        ] {
            let value = decode(source.as_bytes()).unwrap();
            assert!(value.is_object(), "{source}");
            assert_eq!(json_dumps(&value), expected, "{source}");
            assert_eq!(decode(expected.as_bytes()).unwrap(), value);
        }
    }

    #[test]
    fn raw_kind_decomposition_keeps_scalars_syntax_and_nesting_bounded() {
        for (source, expected) in [
            (" \n true \t", "true"),
            ("false", "false"),
            ("null", "null"),
            (
                r#""$serde_json::private::Number""#,
                r#""$serde_json::private::Number""#,
            ),
            (
                "[{},[],1.000,18446744073709551617]",
                "[{}, [], 1.0, 18446744073709551617]",
            ),
        ] {
            assert_eq!(json_dumps(&decode(source.as_bytes()).unwrap()), expected);
        }
        let shallow = format!("{}0{}", "[".repeat(32), "]".repeat(32));
        assert!(decode(shallow.as_bytes()).is_ok());
        let too_deep = format!("{}0{}", "[".repeat(128), "]".repeat(128));
        assert!(decode(too_deep.as_bytes()).is_err());
        for malformed in [
            r#"{"$serde_json::private::Number":"valid",}"#,
            "{}{}",
            "null false",
            "[1,]",
            "",
        ] {
            assert!(decode(malformed.as_bytes()).is_err());
        }
    }
}
