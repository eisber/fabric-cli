// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Python-compatible JSON-domain encoding, narrowed from the pinned Rust donor.
//! Inputs are serde JSON values only; no native Python, surrogate, or wide-int layer.
#![forbid(unsafe_code)]
use serde_json::{Number, Value};
use std::fmt::Write;

/// Python `json.dumps` layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    /// Comma-space and colon-space separators.
    #[default]
    Default,
    /// Container newlines and selected indentation.
    Pretty(Indent),
}
/// Supported indentation widths.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Indent {
    /// Newlines without indentation.
    Zero,
    /// Two-space indentation.
    Two,
    /// Four-space indentation.
    #[default]
    Four,
}
impl Indent {
    fn width(self) -> usize {
        match self {
            Self::Zero => 0,
            Self::Two => 2,
            Self::Four => 4,
        }
    }
}
/// Encoding options within the finite JSON domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Compact or indented layout.
    pub format: Format,
    /// Escape non-ASCII using lowercase UTF-16 escapes (Python default).
    pub ensure_ascii: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            format: Format::Default,
            ensure_ascii: true,
        }
    }
}
/// Encode without a trailing newline; preserves ordered object keys.
pub fn encode(value: &Value, format: Format) -> String {
    encode_with_options(
        value,
        Options {
            format,
            ..Options::default()
        },
    )
}
/// Encode with Unicode scalar strings retained as UTF-8.
pub fn encode_unicode(value: &Value, format: Format) -> String {
    encode_with_options(
        value,
        Options {
            format,
            ensure_ascii: false,
        },
    )
}
/// Encode finite JSON-domain values with explicit options.
pub fn encode_with_options(value: &Value, options: Options) -> String {
    let width = match options.format {
        Format::Default => None,
        Format::Pretty(indent) => Some(indent.width()),
    };
    let mut output = String::new();
    write_value(value, width, 0, options.ensure_ascii, &mut output);
    output
}
/// Encode raw data like Python `json.dumps`, without an envelope or newline.
pub fn json_dumps(value: &Value) -> String {
    encode(value, Format::Default)
}

fn string(value: &str, ensure_ascii: bool, output: &mut String) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            ' '..='~' => output.push(character),
            c if !ensure_ascii && c >= ' ' => output.push(c),
            c => {
                for point in c.encode_utf16(&mut [0; 2]) {
                    write!(output, "\\u{point:04x}").expect("String writes cannot fail");
                }
            }
        }
    }
    output.push('"');
}
fn number(value: &Number, output: &mut String) {
    if !value.is_f64() {
        output.push_str(&value.to_string());
        return;
    }
    // Donor shortest-roundtrip number normalization retains Python's exponent
    // thresholds, explicit exponent sign/two digits, .0 and signed zero.
    let text = value.to_string();
    let (sign, magnitude) = text
        .strip_prefix('-')
        .map_or(("", text.as_str()), |m| ("-", m));
    let (mantissa, exponent) = magnitude.split_once('e').map_or((magnitude, 0), |(m, e)| {
        (m, e.parse::<i32>().expect("valid JSON exponent"))
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
        write!(output, "e{exponent:+03}").expect("String writes cannot fail");
    } else if exponent < 0 {
        output.push_str("0.");
        output.push_str(&"0".repeat((-exponent - 1) as usize));
        output.push_str(digits);
    } else {
        let point = exponent as usize + 1;
        if point >= digits.len() {
            output.push_str(digits);
            output.push_str(&"0".repeat(point - digits.len()));
            output.push_str(".0");
        } else {
            output.push_str(&digits[..point]);
            output.push('.');
            output.push_str(&digits[point..]);
        }
    }
}
fn separator(output: &mut String, index: usize, width: Option<usize>, depth: usize) {
    if index > 0 {
        output.push(',');
        if width.is_none() {
            output.push(' ');
        }
    }
    if let Some(width) = width {
        output.push('\n');
        output.push_str(&" ".repeat(width * depth));
    }
}
fn close(
    output: &mut String,
    populated: bool,
    width: Option<usize>,
    depth: usize,
    delimiter: char,
) {
    if let Some(width) = width.filter(|_| populated) {
        output.push('\n');
        output.push_str(&" ".repeat(width * depth));
    }
    output.push(delimiter);
}
fn write_value(
    value: &Value,
    width: Option<usize>,
    depth: usize,
    ascii: bool,
    output: &mut String,
) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(v) => output.push_str(if *v { "true" } else { "false" }),
        Value::Number(v) => number(v, output),
        Value::String(v) => string(v, ascii, output),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                separator(output, index, width, depth + 1);
                write_value(value, width, depth + 1, ascii, output);
            }
            close(output, !values.is_empty(), width, depth, ']');
        }
        Value::Object(values) => {
            output.push('{');
            for (index, (key, value)) in values.iter().enumerate() {
                separator(output, index, width, depth + 1);
                string(key, ascii, output);
                output.push_str(": ");
                write_value(value, width, depth + 1, ascii, output);
            }
            close(output, !values.is_empty(), width, depth, '}');
        }
    }
}
