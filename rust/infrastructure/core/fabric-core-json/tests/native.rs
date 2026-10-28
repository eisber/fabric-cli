// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public native-value encoding, input validation, and lossless conversion consumers.

#[path = "support/native.rs"]
mod support;

use fabric_core_json::{
    DecimalInteger, Format, Indent, LimitKind, MAX_DECIMAL_DIGITS, MAX_NATIVE_DEPTH,
    MAX_NATIVE_NODES, MAX_NATIVE_OUTPUT_BYTES, MAX_STRING_CODEPOINTS, Options, PythonDict,
    PythonString, PythonValue, PythonValueError, encode, encode_python, encode_python_with_options,
};
use serde_json::{Value, json};

fn integer(text: &str) -> PythonValue {
    PythonValue::Integer(DecimalInteger::parse(text).unwrap())
}

fn text(value: &str) -> PythonString {
    PythonString::from_text(value).unwrap()
}

#[test]
fn native_values_match_handwritten_python_bytes_and_truth_without_loss() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/output/native-cases.json"
    ))
    .unwrap();
    for case in cases {
        let observed = support::observe(&case).unwrap();
        assert_eq!(observed["json"], case["json"], "{}", case["name"]);
        assert_eq!(observed["truth"], case["truth"], "{}", case["name"]);
        assert_eq!(observed.as_object().unwrap().len(), 7);
    }
}

#[test]
fn decimal_grammar_is_validated_and_canonical_without_token_injection() {
    for (raw, expected) in [
        ("0", "0"),
        ("-0", "0"),
        ("+000", "0"),
        ("000123", "123"),
        ("-000123", "-123"),
    ] {
        assert_eq!(DecimalInteger::parse(raw).unwrap().as_str(), expected);
    }
    for raw in [
        "", "+", "-", " 1", "1 ", "1_000", "1.0", "1e4", "0x10", "--1", "+-1", "[1]", "1, 2",
        "1\n", "\u{0661}", "NaN",
    ] {
        assert_eq!(
            DecimalInteger::parse(raw),
            Err(PythonValueError::InvalidInteger),
            "{raw:?}"
        );
    }
    let maximum = "9".repeat(MAX_DECIMAL_DIGITS);
    let wide = DecimalInteger::parse(&maximum).unwrap();
    assert_eq!(
        encode_python(&PythonValue::Integer(wide), Format::Default).unwrap(),
        maximum
    );
    assert_eq!(
        DecimalInteger::parse(&"9".repeat(MAX_DECIMAL_DIGITS + 1)),
        Err(PythonValueError::Limit {
            kind: LimitKind::DecimalDigits,
            maximum: MAX_DECIMAL_DIGITS
        })
    );
}

#[test]
fn exact_python_strings_reject_only_invalid_range_and_never_repair_surrogates() {
    let paired = PythonString::from_codepoints([0xd83d, 0xde80]).unwrap();
    let scalar = text("\u{1f680}");
    assert_ne!(paired, scalar);
    assert_eq!(paired.codepoints(), [0xd83d, 0xde80]);
    assert_eq!(scalar.codepoints(), [0x1f680]);
    assert_eq!(
        paired.try_to_string(),
        Err(PythonValueError::Surrogate {
            index: 0,
            codepoint: 0xd83d
        })
    );
    assert_eq!(scalar.try_to_string().unwrap(), "\u{1f680}");
    assert_eq!(
        PythonString::from_codepoints([0, 0x10ffff])
            .unwrap()
            .codepoints(),
        [0, 0x10ffff]
    );
    assert_eq!(
        PythonString::from_codepoints([1, 0x110000]),
        Err(PythonValueError::InvalidCodepoint {
            index: 1,
            codepoint: 0x110000
        })
    );
    let value = PythonValue::String(paired);
    assert_eq!(
        encode_python(&value, Format::Default).unwrap(),
        "\"\\ud83d\\ude80\""
    );
    assert_eq!(
        encode_python_with_options(
            &value,
            Options {
                format: Format::Default,
                ensure_ascii: false
            }
        ),
        Err(PythonValueError::Surrogate {
            index: 0,
            codepoint: 0xd83d
        })
    );
    let maximum =
        PythonString::from_codepoints(std::iter::repeat_n(0, MAX_STRING_CODEPOINTS)).unwrap();
    assert_eq!(maximum.codepoints().len(), MAX_STRING_CODEPOINTS);
    assert_eq!(
        PythonString::from_codepoints(std::iter::repeat_n(0, MAX_STRING_CODEPOINTS + 1)),
        Err(PythonValueError::Limit {
            kind: LimitKind::StringCodepoints,
            maximum: MAX_STRING_CODEPOINTS
        })
    );
}

#[test]
fn dictionary_duplicates_replace_last_value_without_moving_first_position() {
    let dictionary = PythonDict::from_entries([
        (text("z"), integer("1")),
        (text("a"), integer("2")),
        (text("z"), integer("3")),
    ])
    .unwrap();
    assert_eq!(
        dictionary
            .entries()
            .iter()
            .map(|(k, _)| k.try_to_string().unwrap())
            .collect::<Vec<_>>(),
        ["z", "a"]
    );
    assert_eq!(
        encode_python(&PythonValue::Dict(dictionary), Format::Default).unwrap(),
        "{\"z\": 3, \"a\": 2}"
    );
    let maximum =
        PythonDict::from_entries((0..MAX_NATIVE_NODES).map(|_| (text("same"), PythonValue::None)))
            .unwrap();
    assert_eq!(maximum.entries().len(), 1);
    let limit =
        PythonDict::from_entries((0..=MAX_NATIVE_NODES).map(|_| (text("same"), PythonValue::None)));
    assert_eq!(
        limit,
        Err(PythonValueError::Limit {
            kind: LimitKind::DictEntries,
            maximum: MAX_NATIVE_NODES
        })
    );
}

#[test]
fn finite_json_conversion_preserves_kind_order_and_negative_zero_or_fails_typed() {
    let json = json!({"z":[1,-0.0,1.0,null,false,"\u{1f680}"],"a":18446744073709551615u64});
    let python = PythonValue::try_from(&json).unwrap();
    let converted = Value::try_from(&python).unwrap();
    assert_eq!(
        encode(&converted, Format::Default),
        encode(&json, Format::Default)
    );
    assert_eq!(
        converted["z"][1].as_f64().unwrap().to_bits(),
        (-0.0f64).to_bits()
    );
    assert!(converted["z"][0].as_number().unwrap().is_i64());
    assert!(converted["z"][2].as_number().unwrap().is_f64());
    for raw in ["-9223372036854775808", "18446744073709551615"] {
        let native = integer(raw);
        let finite = native.try_to_json().unwrap();
        assert_eq!(encode(&finite, Format::Default), raw);
        assert_eq!(PythonValue::try_from_json(&finite).unwrap(), native);
    }
    for bits in [0x7ff8000000001234_u64, 0xfff8000000001234] {
        let native = PythonValue::Float(f64::from_bits(bits));
        let PythonValue::Float(value) = native.clone() else {
            panic!("float variant must not change");
        };
        assert_eq!(value.to_bits(), bits);
        assert!(native.is_truthy());
        assert_eq!(encode_python(&native, Format::Default).unwrap(), "NaN");
    }
    for raw in ["18446744073709551616", "-9223372036854775809"] {
        assert_eq!(
            integer(raw).try_to_json(),
            Err(PythonValueError::IntegerOutOfRange)
        );
    }
    for value in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        assert_eq!(
            PythonValue::Float(value).try_to_json(),
            Err(PythonValueError::NonFiniteFloat)
        );
    }
    let nested = PythonValue::Dict(
        PythonDict::from_entries([(
            text("x"),
            PythonValue::List(vec![integer("18446744073709551616")]),
        )])
        .unwrap(),
    );
    assert_eq!(
        nested.try_to_json(),
        Err(PythonValueError::IntegerOutOfRange)
    );
    let surrogate = PythonValue::Dict(
        PythonDict::from_entries([(
            PythonString::from_codepoints([0xd800]).unwrap(),
            PythonValue::None,
        )])
        .unwrap(),
    );
    assert_eq!(
        surrogate.try_to_json(),
        Err(PythonValueError::Surrogate {
            index: 0,
            codepoint: 0xd800
        })
    );
}

#[test]
fn explicit_safety_limits_fail_at_boundaries_without_truncation_or_success_default() {
    let mut value = PythonValue::None;
    for _ in 0..MAX_NATIVE_DEPTH {
        value = PythonValue::List(vec![value]);
    }
    assert!(encode_python(&value, Format::Default).is_ok());
    let finite = value.try_to_json().unwrap();
    assert!(PythonValue::try_from_json(&finite).is_ok());
    let too_deep_json = Value::Array(vec![finite]);
    assert_eq!(
        PythonValue::try_from_json(&too_deep_json),
        Err(PythonValueError::Limit {
            kind: LimitKind::Depth,
            maximum: MAX_NATIVE_DEPTH
        })
    );
    value = PythonValue::List(vec![value]);
    assert_eq!(
        encode_python(&value, Format::Default),
        Err(PythonValueError::Limit {
            kind: LimitKind::Depth,
            maximum: MAX_NATIVE_DEPTH
        })
    );
    assert_eq!(
        value.try_to_json(),
        Err(PythonValueError::Limit {
            kind: LimitKind::Depth,
            maximum: MAX_NATIVE_DEPTH
        })
    );

    let nodes = PythonValue::List(vec![PythonValue::None; MAX_NATIVE_NODES - 1]);
    assert!(encode_python(&nodes, Format::Default).is_ok());
    assert!(nodes.try_to_json().is_ok());
    assert!(
        PythonValue::try_from_json(&Value::Array(vec![Value::Null; MAX_NATIVE_NODES - 1])).is_ok()
    );
    let nodes = PythonValue::List(vec![PythonValue::None; MAX_NATIVE_NODES]);
    assert_eq!(
        encode_python(&nodes, Format::Default),
        Err(PythonValueError::Limit {
            kind: LimitKind::Nodes,
            maximum: MAX_NATIVE_NODES
        })
    );
    assert_eq!(
        nodes.try_to_json(),
        Err(PythonValueError::Limit {
            kind: LimitKind::Nodes,
            maximum: MAX_NATIVE_NODES
        })
    );
    assert_eq!(
        PythonValue::try_from_json(&Value::Array(vec![Value::Null; MAX_NATIVE_NODES])),
        Err(PythonValueError::Limit {
            kind: LimitKind::Nodes,
            maximum: MAX_NATIVE_NODES
        })
    );

    let string =
        PythonString::from_codepoints(std::iter::repeat_n(0x10000, MAX_STRING_CODEPOINTS)).unwrap();
    let values = PythonValue::List(vec![
        PythonValue::String(string.clone()),
        PythonValue::String(string),
    ]);
    assert_eq!(
        encode_python(&values, Format::Pretty(Indent::Four)),
        Err(PythonValueError::Limit {
            kind: LimitKind::OutputBytes,
            maximum: MAX_NATIVE_OUTPUT_BYTES
        })
    );
}

#[test]
fn encoded_output_limit_is_exact_in_utf8_bytes_not_codepoints() {
    let first =
        PythonString::from_codepoints(std::iter::repeat_n(0x10000, MAX_STRING_CODEPOINTS)).unwrap();
    let remainder = MAX_NATIVE_OUTPUT_BYTES - 8 - 12 * MAX_STRING_CODEPOINTS;
    let second = |extra| {
        PythonString::from_codepoints(
            std::iter::repeat_n(0x10000, remainder / 12)
                .chain(std::iter::repeat_n(u32::from('x'), remainder % 12 + extra)),
        )
        .unwrap()
    };
    let value = PythonValue::List(vec![
        PythonValue::String(first.clone()),
        PythonValue::String(second(0)),
    ]);
    let encoded = encode_python(&value, Format::Default).unwrap();
    assert_eq!(encoded.len(), MAX_NATIVE_OUTPUT_BYTES);
    assert!(encoded.starts_with("[\"\\ud800\\udc00"));
    assert!(encoded.ends_with("\"]"));
    let beyond = PythonValue::List(vec![
        PythonValue::String(first),
        PythonValue::String(second(1)),
    ]);
    assert_eq!(
        encode_python(&beyond, Format::Default),
        Err(PythonValueError::Limit {
            kind: LimitKind::OutputBytes,
            maximum: MAX_NATIVE_OUTPUT_BYTES
        })
    );
    assert!(
        encode_python_with_options(
            &beyond,
            Options {
                format: Format::Default,
                ensure_ascii: false
            }
        )
        .unwrap()
        .len()
            < MAX_NATIVE_OUTPUT_BYTES
    );
}
