// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Python-compatible JSON-domain encoding, narrowed from the pinned Rust donor.
//! Inputs are serde JSON values; arbitrary-size decimal integers retain exact digits.
//! There is no native Python or surrogate-value layer.
#![forbid(unsafe_code)]
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use serde_json::{Map, Number, Value, value::RawValue};
use std::fmt::{self, Write};

/// Invalid JSON or a floating-point value outside the supported finite domain.
///
/// Error text never includes input bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeError;

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Invalid JSON or non-finite floating-point value")
    }
}

impl std::error::Error for DecodeError {}

/// Decode JSON without rounding integers through a machine-sized or floating-point type.
///
/// Decimal integer digits are retained by serde's `arbitrary_precision` feature.
/// Float tokens are normalized to Python's finite binary64 domain; overflow is
/// rejected rather than being mistaken for an arbitrary-size integer.
/// Object keys, including serde's private marker names, are always ordinary keys.
/// Tree construction is bounded to 128 JSON levels.
pub fn decode(bytes: &[u8]) -> Result<Value, DecodeError> {
    let raw: &RawValue = serde_json::from_slice(bytes).map_err(|_| DecodeError)?;
    decode_raw(raw, 0)
}

// RawValue's syntax scan is not a recursive Value parse, so explicitly retain a
// bounded nesting depth while constructing the caller-owned JSON tree.
const MAX_DECODE_DEPTH: usize = 128;

fn decode_raw(raw: &RawValue, depth: usize) -> Result<Value, DecodeError> {
    if depth >= MAX_DECODE_DEPTH {
        return Err(DecodeError);
    }
    let text = raw.get().trim();
    match text.as_bytes().first() {
        Some(b'{') => {
            // Never send actual object syntax through ValueVisitor: with
            // arbitrary_precision its private Number key is otherwise special.
            let RawObject(entries) = serde_json::from_str(text).map_err(|_| DecodeError)?;
            let mut object = Map::new();
            for (key, value) in entries {
                object.insert(key, decode_raw(value, depth + 1)?);
            }
            Ok(Value::Object(object))
        }
        Some(b'[') => {
            let values: Vec<&RawValue> = serde_json::from_str(text).map_err(|_| DecodeError)?;
            values
                .into_iter()
                .map(|value| decode_raw(value, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        }
        Some(b'"') => serde_json::from_str::<String>(text)
            .map(Value::String)
            .map_err(|_| DecodeError),
        Some(b't' | b'f') => serde_json::from_str::<bool>(text)
            .map(Value::Bool)
            .map_err(|_| DecodeError),
        Some(b'n') => Ok(Value::Null), // RawValue already validated the entire token.
        Some(b'-' | b'0'..=b'9') => {
            // Only genuine numeric syntax reaches Number's synthetic-map
            // protocol. Decimal integers never pass through floating point.
            let mut number: Number = serde_json::from_str(text).map_err(|_| DecodeError)?;
            if text.contains(['.', 'e', 'E']) {
                let float = number.as_f64().ok_or(DecodeError)?;
                number = Number::from_f64(float).ok_or(DecodeError)?;
            }
            Ok(Value::Number(number))
        }
        _ => Err(DecodeError),
    }
}

// Keep input order and duplicate-key behavior without a new map dependency.
// Borrowed raw values defer all kind interpretation to decode_raw.
struct RawObject<'a>(Vec<(String, &'a RawValue)>);
struct RawObjectVisitor;

impl<'de> Visitor<'de> for RawObjectVisitor {
    type Value = RawObject<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut entries = Vec::new();
        while let Some(entry) = map.next_entry::<String, &'de RawValue>()? {
            entries.push(entry);
        }
        Ok(RawObject(entries))
    }
}

impl<'de> Deserialize<'de> for RawObject<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(RawObjectVisitor)
    }
}

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
    let text = value.to_string();
    if !text.contains(['.', 'e', 'E']) {
        output.push_str(&text);
        return;
    }
    // arbitrary_precision retains float lexemes too. Normalize finite floats
    // before applying Python's layout, without ever converting integer tokens.
    // Callers decoding bytes should use `decode` to reject float overflow.
    let Some(canonical) = value.as_f64().and_then(Number::from_f64) else {
        // `decode` rejects these unsupported float tokens. If a library caller
        // supplies one directly, retain its digits without unbounded expansion.
        output.push_str(&text);
        return;
    };
    let text = canonical.to_string();
    // Donor shortest-roundtrip number normalization retains Python's exponent
    // thresholds, explicit exponent sign/two digits, .0 and signed zero.
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
