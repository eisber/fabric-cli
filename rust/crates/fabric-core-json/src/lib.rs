// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared Python-style JSON-domain encoding for SDK, deployment and CLI consumers.
//!
//! A single string/number implementation preserves ordered JSON, ASCII escaping,
//! and default/pretty layout. Inputs are already JSON-domain values: i64/u64 and
//! finite f64 numbers, not arbitrary Python runtime objects or integers. No I/O,
//! runtime, environment, authentication or CLI dependency is present.

use serde_json::Value;
use std::fmt::Write;

/// Layout corresponding to Python `json.dumps` options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    /// Python's default no-indent layout, with comma-space and colon-space.
    #[default]
    Default,
    /// Pretty layout; zero still emits container newlines.
    Pretty(Indent),
}

/// Supported Python integer-indent modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Indent {
    /// No spaces, with container newlines (`indent=0`).
    Zero,
    /// Two spaces.
    Two,
    /// Four spaces.
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

/// Explicit Python JSON formatting options, separate from the legacy entry points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Default separators or a supported integer-indent layout.
    pub format: Format,
    /// Escape non-ASCII Unicode when true (Python's default).
    ///
    /// False retains Unicode scalar values as UTF-8, including DEL and Unicode
    /// separators. JSON syntax, quotes, backslashes and U+0000-U+001F are still
    /// escaped. Rust strings cannot contain lone surrogates; this option does
    /// not substitute replacement characters for unrepresentable Python values.
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

/// Encode an already-validated JSON value without adding a trailing newline.
///
/// Unknown object keys retain insertion order. Unicode uses Python's default
/// ASCII escaping, including lowercase UTF-16 surrogate pairs. This does not
/// coerce arbitrary `Serialize` inputs, redact bytes, emit NaN/Infinity or
/// accept integers outside serde_json's i64/u64 domain. Callers must reject
/// unsupported numeric inputs before a lossy upstream conversion to `Value`.
pub fn encode(value: &Value, format: Format) -> String {
    encode_with_options(
        value,
        Options {
            format,
            ..Options::default()
        },
    )
}

/// Encode a JSON-domain value like Python `json.dumps(..., ensure_ascii=False)`.
///
/// Uses the same recursive engine as [`encode`], retaining valid Unicode scalars
/// as UTF-8 while still escaping JSON syntax and controls below U+0020. Layout,
/// ordering, numeric policy and absence of a trailing newline are unchanged.
/// `Format::Pretty(Indent::Four)` matches the definition metadata/export layout.
/// Lone surrogates, arbitrary-width integers and nonfinite Python values remain
/// outside this `serde_json::Value` contract; no replacement or coercion occurs.
///
/// ```
/// use fabric_core_json::{Format, Indent, encode_unicode};
/// use serde_json::json;
///
/// let encoded = encode_unicode(&json!({"name": "caf\u{e9}"}), Format::Pretty(Indent::Four));
/// assert_eq!(encoded, "{\n    \"name\": \"caf\u{e9}\"\n}");
/// ```
pub fn encode_unicode(value: &Value, format: Format) -> String {
    encode_with_options(
        value,
        Options {
            format,
            ensure_ascii: false,
        },
    )
}

/// Encode a JSON-domain value with an explicit Python `ensure_ascii` policy.
///
/// This shares the legacy encoder's string, number and container implementation.
/// No envelope, sorting, newline or lossy coercion is added. For example, native
/// definition metadata uses Python `json.dumps(data, ensure_ascii=False, indent=4)`:
///
/// ```
/// use fabric_core_json::{Format, Indent, Options, encode_with_options};
/// use serde_json::json;
///
/// let value = json!({"name": "caf\u{00e9} \u{1f680}"});
/// let encoded = encode_with_options(&value, Options {
///     format: Format::Pretty(Indent::Four),
///     ensure_ascii: false,
/// });
/// assert_eq!(encoded, "{\n    \"name\": \"caf\u{00e9} \u{1f680}\"\n}");
/// ```
///
/// As with [`encode`], the input domain remains i64/u64, finite f64 and valid
/// Unicode scalar strings. Arbitrary integers, nonfinite Python values and lone
/// surrogates require a separate lossless native-value contract, not this API.
pub fn encode_with_options(value: &Value, options: Options) -> String {
    let mut output = String::new();
    let width = match options.format {
        Format::Default => None,
        Format::Pretty(indent) => Some(indent.width()),
    };
    write_value(value, width, 0, options.ensure_ascii, &mut output);
    output
}

/// Encode raw data like Python `json.dumps(data)` with default options.
///
/// There is no envelope, wrapping, omission, sorting or trailing newline.
pub fn json_dumps(value: &Value) -> String {
    encode(value, Format::Default)
}

fn padding(output: &mut String, width: usize, depth: usize) {
    output.extend(std::iter::repeat_n(' ', width * depth));
}

fn string(value: &str, ensure_ascii: bool, output: &mut String) {
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
            _ if !ensure_ascii && c >= '\u{20}' => output.push(c),
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

fn write_value(
    value: &Value,
    width: Option<usize>,
    depth: usize,
    ensure_ascii: bool,
    output: &mut String,
) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(true) => output.push_str("true"),
        Value::Bool(false) => output.push_str("false"),
        Value::Number(value) => number(value, output),
        Value::String(value) => string(value, ensure_ascii, output),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                    if width.is_none() {
                        output.push(' ');
                    }
                }
                if let Some(width) = width {
                    output.push('\n');
                    padding(output, width, depth + 1);
                }
                write_value(value, width, depth + 1, ensure_ascii, output);
            }
            if let Some(width) = width.filter(|_| !values.is_empty()) {
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
                    if width.is_none() {
                        output.push(' ');
                    }
                }
                if let Some(width) = width {
                    output.push('\n');
                    padding(output, width, depth + 1);
                }
                string(key, ensure_ascii, output);
                output.push_str(": ");
                write_value(value, width, depth + 1, ensure_ascii, output);
            }
            if let Some(width) = width.filter(|_| !values.is_empty()) {
                output.push('\n');
                padding(output, width, depth);
            }
            output.push('}');
        }
    }
}
