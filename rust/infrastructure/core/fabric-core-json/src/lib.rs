// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared Python-style JSON-domain encoding for SDK, deployment and CLI consumers.
//!
//! A single string/number implementation preserves ordered JSON, ASCII escaping,
//! and default/pretty layout. Inputs are already JSON-domain values: i64/u64 and
//! finite f64 numbers. Additive caller-owned [`PythonValue`] encoding also supports
//! validated wide integers, nonfinite f64 and surrogate-preserving codepoints.
//! Neither path handles arbitrary Python runtime objects. No I/O, runtime,
//! environment, authentication or CLI dependency is present.

use serde_json::Value;

mod native;
mod view;

pub use native::{
    DecimalInteger, LimitKind, MAX_DECIMAL_DIGITS, MAX_NATIVE_DEPTH, MAX_NATIVE_NODES,
    MAX_NATIVE_OUTPUT_BYTES, MAX_STRING_CODEPOINTS, PythonDict, PythonString, PythonValue,
    PythonValueError,
};
use view::{Node, Number, Text, ValueRef};

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
    encode_inner(ValueRef::Json(value), options, None)
        .expect("serde_json strings contain scalar values; legacy encoding has no native limits")
}

/// Encode caller-owned Python values with Python's ASCII/`allow_nan=True` defaults.
///
/// Arbitrary decimal integers, nonfinite f64 and individual surrogate codepoints
/// retain their identity. Nonfinite values emit `NaN`, `Infinity`, `-Infinity`.
/// This is deliberately additive: existing `serde_json::Value` APIs stay finite.
///
/// The same recursive string/number/layout engine serves both contracts.
/// Native limits are explicit safety exceptions, not claims about Python's
/// configurable integer limits or recursion/environment settings.
///
/// ```
/// use fabric_core_json::{DecimalInteger, Format, PythonValue, encode_python};
///
/// let value = PythonValue::List(vec![
///     PythonValue::Integer(DecimalInteger::parse("18446744073709551616")?),
///     PythonValue::Float(f64::INFINITY),
/// ]);
/// assert_eq!(encode_python(&value, Format::Default)?, "[18446744073709551616, Infinity]");
/// assert!(value.try_to_json().is_err()); // Never round or substitute null.
/// # Ok::<(), fabric_core_json::PythonValueError>(())
/// ```
pub fn encode_python(value: &PythonValue, format: Format) -> Result<String, PythonValueError> {
    encode_python_with_options(
        value,
        Options {
            format,
            ..Options::default()
        },
    )
}

/// Encode Python values with explicit layout and Unicode policy.
///
/// With `ensure_ascii=false`, any surrogate codepoint returns
/// [`PythonValueError::Surrogate`] instead of repairing/combining codepoints
/// or returning replacement UTF-8. Python may return such a non-UTF-8 string;
/// this Rust API returns only valid UTF-8 or a typed error.
///
/// Limits: [`MAX_NATIVE_DEPTH`], [`MAX_NATIVE_NODES`] and
/// [`MAX_NATIVE_OUTPUT_BYTES`]. Constructor limits additionally apply to strings,
/// dictionaries and integers. No service parser or ambient state is accessed.
pub fn encode_python_with_options(
    value: &PythonValue,
    options: Options,
) -> Result<String, PythonValueError> {
    encode_inner(ValueRef::Python(value), options, Some(Budget::default()))
}

#[derive(Default)]
struct Budget {
    nodes: usize,
}

struct Output {
    text: String,
    budget: Option<Budget>,
}

impl Output {
    fn push(&mut self, text: &str) -> Result<(), PythonValueError> {
        if self.budget.is_some()
            && text.len() > MAX_NATIVE_OUTPUT_BYTES.saturating_sub(self.text.len())
        {
            return Err(PythonValueError::Limit {
                kind: LimitKind::OutputBytes,
                maximum: MAX_NATIVE_OUTPUT_BYTES,
            });
        }
        self.text.push_str(text);
        Ok(())
    }

    fn character(&mut self, character: char) -> Result<(), PythonValueError> {
        self.push(character.encode_utf8(&mut [0; 4]))
    }

    fn enter(&mut self, depth: usize) -> Result<(), PythonValueError> {
        if let Some(budget) = &mut self.budget {
            if depth > MAX_NATIVE_DEPTH {
                return Err(PythonValueError::Limit {
                    kind: LimitKind::Depth,
                    maximum: MAX_NATIVE_DEPTH,
                });
            }
            if budget.nodes == MAX_NATIVE_NODES {
                return Err(PythonValueError::Limit {
                    kind: LimitKind::Nodes,
                    maximum: MAX_NATIVE_NODES,
                });
            }
            budget.nodes += 1;
        }
        Ok(())
    }
}

fn encode_inner(
    value: ValueRef<'_>,
    options: Options,
    budget: Option<Budget>,
) -> Result<String, PythonValueError> {
    let mut output = Output {
        text: String::new(),
        budget,
    };
    let width = match options.format {
        Format::Default => None,
        Format::Pretty(indent) => Some(indent.width()),
    };
    write_value(value, width, 0, options.ensure_ascii, &mut output)?;
    Ok(output.text)
}

/// Encode raw data like Python `json.dumps(data)` with default options.
///
/// There is no envelope, wrapping, omission, sorting or trailing newline.
pub fn json_dumps(value: &Value) -> String {
    encode(value, Format::Default)
}

fn padding(output: &mut Output, width: usize, depth: usize) -> Result<(), PythonValueError> {
    output.push(&" ".repeat(width * depth))
}

fn string(
    value: Text<'_>,
    ensure_ascii: bool,
    output: &mut Output,
) -> Result<(), PythonValueError> {
    output.character('"')?;
    for (index, point) in value.points().enumerate() {
        match point {
            0x22 => output.push("\\\"")?,
            0x5c => output.push("\\\\")?,
            0x08 => output.push("\\b")?,
            0x0c => output.push("\\f")?,
            0x0a => output.push("\\n")?,
            0x0d => output.push("\\r")?,
            0x09 => output.push("\\t")?,
            0x20..=0x7e => output.character(char::from_u32(point).expect("ASCII scalar"))?,
            _ if !ensure_ascii && point >= 0x20 => {
                let character = char::from_u32(point).ok_or(PythonValueError::Surrogate {
                    index,
                    codepoint: point,
                })?;
                output.character(character)?;
            }
            _ => {
                if point <= 0xffff {
                    output.push(&format!("\\u{point:04x}"))?;
                } else {
                    let adjusted = point - 0x10000;
                    output.push(&format!(
                        "\\u{:04x}\\u{:04x}",
                        0xd800 + (adjusted >> 10),
                        0xdc00 + (adjusted & 0x3ff)
                    ))?;
                }
            }
        }
    }
    output.character('"')
}

fn number(value: Number<'_>, output: &mut Output) -> Result<(), PythonValueError> {
    let text = match value {
        Number::Json(value) if !value.is_f64() => return output.push(&value.to_string()),
        Number::Json(value) => value.to_string(),
        Number::Integer(value) => return output.push(value.as_str()),
        Number::Float(value) if value.is_nan() => return output.push("NaN"),
        Number::Float(value) if value == f64::INFINITY => return output.push("Infinity"),
        Number::Float(value) if value == f64::NEG_INFINITY => return output.push("-Infinity"),
        Number::Float(value) => serde_json::Number::from_f64(value)
            .expect("nonfinite values handled explicitly")
            .to_string(),
    };
    // serde_json's locked shortest-roundtrip formatter uses ties-to-even.
    // Rust's standard float formatter differs at exact decimal midpoints.
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
    output.push(sign)?;
    let Some(first) = digits.find(|c| c != '0') else {
        return output.push("0.0");
    };
    let exponent = exponent + point - first as i32 - 1;
    let digits = digits[first..].trim_end_matches('0');
    if !(-4..16).contains(&exponent) {
        output.push(&digits[..1])?;
        if digits.len() > 1 {
            output.character('.')?;
            output.push(&digits[1..])?;
        }
        output.push(&format!("e{exponent:+03}"))?;
    } else if exponent < 0 {
        output.push("0.")?;
        output.push(&"0".repeat((-exponent - 1) as usize))?;
        output.push(digits)?;
    } else {
        let point = exponent as usize + 1;
        if point >= digits.len() {
            output.push(digits)?;
            output.push(&"0".repeat(point - digits.len()))?;
            output.push(".0")?;
        } else {
            output.push(&digits[..point])?;
            output.character('.')?;
            output.push(&digits[point..])?;
        }
    }
    Ok(())
}

fn write_value(
    value: ValueRef<'_>,
    width: Option<usize>,
    depth: usize,
    ensure_ascii: bool,
    output: &mut Output,
) -> Result<(), PythonValueError> {
    output.enter(depth)?;
    match value.node() {
        Node::None => output.push("null")?,
        Node::Bool(true) => output.push("true")?,
        Node::Bool(false) => output.push("false")?,
        Node::Number(value) => number(value, output)?,
        Node::String(value) => string(value, ensure_ascii, output)?,
        Node::List(values) => {
            output.character('[')?;
            let mut populated = false;
            for (index, value) in values.enumerate() {
                populated = true;
                if index > 0 {
                    output.character(',')?;
                    if width.is_none() {
                        output.character(' ')?;
                    }
                }
                if let Some(width) = width {
                    output.character('\n')?;
                    padding(output, width, depth + 1)?;
                }
                write_value(value, width, depth + 1, ensure_ascii, output)?;
            }
            if let Some(width) = width.filter(|_| populated) {
                output.character('\n')?;
                padding(output, width, depth)?;
            }
            output.character(']')?;
        }
        Node::Dict(values) => {
            output.character('{')?;
            let mut populated = false;
            for (index, (key, value)) in values.enumerate() {
                populated = true;
                if index > 0 {
                    output.character(',')?;
                    if width.is_none() {
                        output.character(' ')?;
                    }
                }
                if let Some(width) = width {
                    output.character('\n')?;
                    padding(output, width, depth + 1)?;
                }
                string(key, ensure_ascii, output)?;
                output.push(": ")?;
                write_value(value, width, depth + 1, ensure_ascii, output)?;
            }
            if let Some(width) = width.filter(|_| populated) {
                output.character('\n')?;
                padding(output, width, depth)?;
            }
            output.character('}')?;
        }
    }
    Ok(())
}
