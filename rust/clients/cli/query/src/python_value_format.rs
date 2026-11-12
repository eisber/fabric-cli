// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde_json::Value;
use std::fmt::Write;

pub(crate) fn encode_json(value: &Value) -> String {
    match value {
        Value::Number(value) => format_number(value),
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(encode_json).collect::<Vec<_>>().join(",")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!(
                    "{}:{}",
                    encode_json(&Value::String(key.clone())),
                    encode_json(value)
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => {
            let mut output = String::new();
            for c in value.to_string().chars() {
                if c < '\u{7f}' {
                    output.push(c);
                } else {
                    for unit in c.encode_utf16(&mut [0; 2]) {
                        write!(output, "\\u{unit:04x}").unwrap();
                    }
                }
            }
            output
        }
    }
}

// JMESPath's compact JSON layout differs from the shared CLI envelope encoder.
// Keep Python's shortest-roundtrip/exponent policy for scalar str(float) here.
pub(crate) fn format_number(value: &serde_json::Number) -> String {
    if !value.is_f64() {
        return value.to_string();
    }
    // arbitrary_precision retains source float lexemes too. Python parses them
    // as binary64; only integer lexemes should bypass finite-float normalization.
    let text = serde_json::Number::from_f64(value.as_f64().expect("finite float"))
        .expect("finite float")
        .to_string();
    let (sign, magnitude) = text
        .strip_prefix('-')
        .map_or(("", text.as_str()), |magnitude| ("-", magnitude));
    let (mantissa, exponent) = magnitude
        .split_once('e')
        .map_or((magnitude, 0), |(m, e)| (m, e.parse::<i32>().unwrap()));
    let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let digits: String = mantissa.chars().filter(|&c| c != '.').collect();
    let Some(first) = digits.find(|c| c != '0') else {
        return format!("{sign}0.0");
    };
    let exponent = exponent + point - first as i32 - 1;
    let digits = digits[first..].trim_end_matches('0');
    let mut output = sign.to_owned();
    if !(-4..16).contains(&exponent) {
        output.push_str(&digits[..1]);
        if digits.len() > 1 {
            output.push('.');
            output.push_str(&digits[1..]);
        }
        write!(output, "e{exponent:+03}").unwrap();
    } else if exponent < 0 {
        output.push_str("0.");
        output.extend(std::iter::repeat_n('0', (-exponent - 1) as usize));
        output.push_str(digits);
    } else {
        let point = exponent as usize + 1;
        if point >= digits.len() {
            output.push_str(digits);
            output.extend(std::iter::repeat_n('0', point - digits.len()));
            output.push_str(".0");
        } else {
            output.push_str(&digits[..point]);
            output.push('.');
            output.push_str(&digits[point..]);
        }
    }
    output
}
