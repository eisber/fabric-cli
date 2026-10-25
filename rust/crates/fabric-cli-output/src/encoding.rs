// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::Indent;
use serde_json::Value;
use std::fmt::Write;

pub(crate) fn encode(value: &Value, indent: Indent) -> String {
    let mut output = String::new();
    write_value(value, indent.width(), 0, &mut output);
    output
}

fn padding(output: &mut String, width: usize, depth: usize) {
    output.extend(std::iter::repeat_n(' ', width * depth));
}

fn string(value: &str, output: &mut String) {
    output.push('"');
    for c in value.chars() {
        match c {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            ' '..='~' => output.push(c),
            _ => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    write!(output, "\\u{unit:04x}").expect("writing to String cannot fail");
                }
            }
        }
    }
    output.push('"');
}

fn number(value: &serde_json::Number, output: &mut String) {
    if !value.is_f64() {
        write!(output, "{value}").expect("writing to String cannot fail");
        return;
    }
    // serde_json's locked shortest-roundtrip formatter uses ties-to-even.
    // Rust's standard float formatter differs at exact decimal midpoints.
    let text = value.to_string();
    let (sign, magnitude) = text
        .strip_prefix('-')
        .map_or(("", text.as_str()), |magnitude| ("-", magnitude));
    let (mantissa, exponent) = magnitude.split_once('e').map_or((magnitude, 0), |(m, e)| {
        (
            m,
            e.parse::<i32>().expect("JSON float exponent is an integer"),
        )
    });
    let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let digits: String = mantissa.chars().filter(|&c| c != '.').collect();
    output.push_str(sign);
    let Some(first) = digits.find(|c| c != '0') else {
        output.push_str("0.0");
        return;
    };
    let exponent = exponent + point - first as i32 - 1;
    let digits = digits[first..].trim_end_matches('0');
    if !(-4..16).contains(&exponent) {
        output.push_str(&digits[..1]);
        if digits.len() > 1 {
            output.push('.');
            output.push_str(&digits[1..]);
        }
        write!(output, "e{exponent:+03}").expect("writing to String cannot fail");
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
}

fn write_value(value: &Value, width: usize, depth: usize, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(true) => output.push_str("true"),
        Value::Bool(false) => output.push_str("false"),
        Value::Number(value) => number(value, output),
        Value::String(value) => string(value, output),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push('\n');
                padding(output, width, depth + 1);
                write_value(value, width, depth + 1, output);
            }
            if !values.is_empty() {
                output.push('\n');
                padding(output, width, depth);
            }
            output.push(']');
        }
        Value::Object(values) => {
            output.push('{');
            for (index, (key, value)) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push('\n');
                padding(output, width, depth + 1);
                string(key, output);
                output.push_str(": ");
                write_value(value, width, depth + 1, output);
            }
            if !values.is_empty() {
                output.push('\n');
                padding(output, width, depth);
            }
            output.push('}');
        }
    }
}
