// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde_json::{Map, Number, Value};
use std::{collections::HashMap, error::Error, fmt};

/// Explicit constructor digit bound; not Python's configurable process-wide limit.
pub const MAX_DECIMAL_DIGITS: usize = 4300;
/// Explicit per-string codepoint bound (including surrogates).
pub const MAX_STRING_CODEPOINTS: usize = 1_048_576;
/// Native encode/conversion root depth is zero; children may reach this depth.
pub const MAX_NATIVE_DEPTH: usize = 128;
/// Maximum native value nodes visited per encode/conversion, including the root.
pub const MAX_NATIVE_NODES: usize = 100_000;
/// Maximum native encoder UTF-8 output length, checked before each append.
pub const MAX_NATIVE_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// Classification of explicit resource bounds, separate from invalid values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    /// Input decimal digit count, before removing leading zeros.
    DecimalDigits,
    /// Individual Python string codepoints.
    StringCodepoints,
    /// Dictionary input entries, before duplicate replacement.
    DictEntries,
    /// Nested child depth.
    Depth,
    /// Tree value nodes.
    Nodes,
    /// Encoded UTF-8 byte count.
    OutputBytes,
}

/// Typed native-value construction/encoding/conversion failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PythonValueError {
    /// Integer was not optional-sign followed by one or more ASCII digits.
    InvalidInteger,
    /// Input codepoint exceeded Python's inclusive U+10FFFF ceiling.
    InvalidCodepoint {
        /// Zero-based input index.
        index: usize,
        /// Invalid value, without replacement/repair.
        codepoint: u32,
    },
    /// A preserved surrogate cannot be returned as a UTF-8 scalar string.
    Surrogate {
        /// Zero-based string codepoint index.
        index: usize,
        /// Original surrogate.
        codepoint: u32,
    },
    /// A finite JSON `Number` cannot represent the original integer exactly.
    IntegerOutOfRange,
    /// NaN/Infinity cannot be converted to finite `serde_json::Value`.
    NonFiniteFloat,
    /// Explicit safety bound; no truncated/success-shaped value is returned.
    Limit {
        /// Which bound failed.
        kind: LimitKind,
        /// Inclusive maximum.
        maximum: usize,
    },
}

impl fmt::Display for PythonValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInteger => formatter
                .write_str("integer must contain an optional sign and ASCII decimal digits"),
            Self::InvalidCodepoint { index, codepoint } => write!(
                formatter,
                "invalid Python codepoint U+{codepoint:X} at index {index}"
            ),
            Self::Surrogate { index, codepoint } => write!(
                formatter,
                "surrogate U+{codepoint:04X} at index {index} cannot be represented as UTF-8"
            ),
            Self::IntegerOutOfRange => formatter
                .write_str("integer cannot be represented exactly as an i64/u64 JSON number"),
            Self::NonFiniteFloat => {
                formatter.write_str("nonfinite Python float cannot be represented as finite JSON")
            }
            Self::Limit { kind, maximum } => write!(
                formatter,
                "native value {kind:?} limit exceeded (maximum {maximum})"
            ),
        }
    }
}

impl Error for PythonValueError {}

/// Validated arbitrary-magnitude integer, stored as canonical ASCII decimal.
///
/// Grammar is `[+-]?[0-9]+`, not Python's general string-to-int parser: no
/// whitespace, separators, exponents, Unicode digits or JSON token injection.
/// Leading zeros and `+` are removed, and every signed zero becomes `"0"`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DecimalInteger(String);

impl DecimalInteger {
    /// Validate and canonicalize without numeric casts or arbitrary-precision deps.
    pub fn parse(value: &str) -> Result<Self, PythonValueError> {
        let (negative, digits) = if let Some(value) = value.strip_prefix('-') {
            (true, value)
        } else {
            (false, value.strip_prefix('+').unwrap_or(value))
        };
        if digits.len() > MAX_DECIMAL_DIGITS {
            return Err(PythonValueError::Limit {
                kind: LimitKind::DecimalDigits,
                maximum: MAX_DECIMAL_DIGITS,
            });
        }
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(PythonValueError::InvalidInteger);
        }
        let magnitude = digits.trim_start_matches('0');
        let canonical = if magnitude.is_empty() {
            "0".to_owned()
        } else if negative {
            format!("-{magnitude}")
        } else {
            magnitude.to_owned()
        };
        Ok(Self(canonical))
    }

    /// Borrow canonical decimal text, never an unvalidated raw JSON token.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Python string identity as codepoints, preserving lone and adjacent surrogates.
///
/// An adjacent high/low surrogate pair remains two Python characters and is not
/// silently combined into the corresponding single astral scalar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PythonString(Vec<u32>);

impl PythonString {
    /// Construct from validated Python codepoints (U+0000 through U+10FFFF).
    pub fn from_codepoints(
        points: impl IntoIterator<Item = u32>,
    ) -> Result<Self, PythonValueError> {
        let mut values = Vec::new();
        for (index, codepoint) in points.into_iter().enumerate() {
            if index == MAX_STRING_CODEPOINTS {
                return Err(PythonValueError::Limit {
                    kind: LimitKind::StringCodepoints,
                    maximum: MAX_STRING_CODEPOINTS,
                });
            }
            if codepoint > 0x10ffff {
                return Err(PythonValueError::InvalidCodepoint { index, codepoint });
            }
            values.push(codepoint);
        }
        Ok(Self(values))
    }

    /// Construct from UTF-8 scalar text; no surrogate conversion is performed.
    pub fn from_text(value: &str) -> Result<Self, PythonValueError> {
        Self::from_codepoints(value.chars().map(u32::from))
    }

    /// Exact Python codepoints, including any surrogate values.
    pub fn codepoints(&self) -> &[u32] {
        &self.0
    }

    /// Convert only if every preserved codepoint is a Unicode scalar.
    pub fn try_to_string(&self) -> Result<String, PythonValueError> {
        self.0
            .iter()
            .enumerate()
            .map(|(index, &codepoint)| {
                char::from_u32(codepoint).ok_or(PythonValueError::Surrogate { index, codepoint })
            })
            .collect()
    }
}

/// Ordered Python string-key dictionary, with last-value/first-position duplicates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PythonDict(Vec<(PythonString, PythonValue)>);

impl PythonDict {
    /// Construct without sorting: duplicate keys replace values in the first slot.
    ///
    /// Input entries (including duplicates) are bounded by [`MAX_NATIVE_NODES`].
    /// Keys compare exact codepoints, not normalized Unicode or surrogate pairs.
    pub fn from_entries(
        entries: impl IntoIterator<Item = (PythonString, PythonValue)>,
    ) -> Result<Self, PythonValueError> {
        let mut positions: HashMap<PythonString, usize> = HashMap::new();
        let mut values: Vec<(PythonString, PythonValue)> = Vec::new();
        for (index, (key, value)) in entries.into_iter().enumerate() {
            if index == MAX_NATIVE_NODES {
                return Err(PythonValueError::Limit {
                    kind: LimitKind::DictEntries,
                    maximum: MAX_NATIVE_NODES,
                });
            }
            if let Some(&position) = positions.get(&key) {
                values[position].1 = value;
            } else {
                positions.insert(key.clone(), values.len());
                values.push((key, value));
            }
        }
        Ok(Self(values))
    }

    /// Borrow unique keys and values in first-insertion order.
    pub fn entries(&self) -> &[(PythonString, PythonValue)] {
        &self.0
    }
}

/// Additive caller-owned Python value tree, not a replacement JSON/service parser.
///
/// No arbitrary objects, callbacks, bytes, cycles, ambient state or mutable global
/// policy. The float variant preserves f64 bits, including signed zero/NaN payload,
/// although Python JSON intentionally renders all NaNs as `NaN`.
#[derive(Clone, Debug, PartialEq)]
pub enum PythonValue {
    /// Python None.
    None,
    /// Python bool (not an integer).
    Bool(bool),
    /// Canonical validated arbitrary decimal integer.
    Integer(DecimalInteger),
    /// Python float, including infinities and NaNs.
    Float(f64),
    /// Exact Python Unicode codepoints.
    String(PythonString),
    /// Ordered list, including falsey children.
    List(Vec<PythonValue>),
    /// Ordered string-key dictionary.
    Dict(PythonDict),
}

impl PythonValue {
    /// Python truthiness of this supported value domain; NaN is truthy.
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::None => false,
            Self::Bool(value) => *value,
            Self::Integer(value) => value.as_str() != "0",
            Self::Float(value) => *value != 0.0,
            Self::String(value) => !value.codepoints().is_empty(),
            Self::List(value) => !value.is_empty(),
            Self::Dict(value) => !value.entries().is_empty(),
        }
    }

    /// Convert to finite JSON only when lossless: no bigint rounding, nonfinite
    /// null replacement, surrogate repair or reordering is allowed.
    ///
    /// Applies native depth/node safety bounds before recursive conversion.
    pub fn try_to_json(&self) -> Result<Value, PythonValueError> {
        to_json(self, &mut Conversion::default(), 0)
    }

    /// Convert finite JSON to explicit Python values, preserving numeric kinds,
    /// insertion order, negative float zero and Unicode scalar codepoints.
    pub fn try_from_json(value: &Value) -> Result<Self, PythonValueError> {
        from_json(value, &mut Conversion::default(), 0)
    }
}

impl TryFrom<&PythonValue> for Value {
    type Error = PythonValueError;
    fn try_from(value: &PythonValue) -> Result<Self, Self::Error> {
        value.try_to_json()
    }
}

impl TryFrom<&Value> for PythonValue {
    type Error = PythonValueError;
    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        Self::try_from_json(value)
    }
}

#[derive(Default)]
struct Conversion {
    nodes: usize,
}

impl Conversion {
    fn enter(&mut self, depth: usize) -> Result<(), PythonValueError> {
        if depth > MAX_NATIVE_DEPTH {
            return Err(PythonValueError::Limit {
                kind: LimitKind::Depth,
                maximum: MAX_NATIVE_DEPTH,
            });
        }
        if self.nodes == MAX_NATIVE_NODES {
            return Err(PythonValueError::Limit {
                kind: LimitKind::Nodes,
                maximum: MAX_NATIVE_NODES,
            });
        }
        self.nodes += 1;
        Ok(())
    }
}

fn to_json(
    value: &PythonValue,
    state: &mut Conversion,
    depth: usize,
) -> Result<Value, PythonValueError> {
    state.enter(depth)?;
    Ok(match value {
        PythonValue::None => Value::Null,
        PythonValue::Bool(value) => Value::Bool(*value),
        PythonValue::Integer(value) => {
            let number = if let Ok(value) = value.as_str().parse::<i64>() {
                Number::from(value)
            } else if let Ok(value) = value.as_str().parse::<u64>() {
                Number::from(value)
            } else {
                return Err(PythonValueError::IntegerOutOfRange);
            };
            Value::Number(number)
        }
        PythonValue::Float(value) => {
            Value::Number(Number::from_f64(*value).ok_or(PythonValueError::NonFiniteFloat)?)
        }
        PythonValue::String(value) => Value::String(value.try_to_string()?),
        PythonValue::List(values) => Value::Array(
            values
                .iter()
                .map(|value| to_json(value, state, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        PythonValue::Dict(values) => {
            let mut object = Map::new();
            for (key, value) in values.entries() {
                object.insert(key.try_to_string()?, to_json(value, state, depth + 1)?);
            }
            Value::Object(object)
        }
    })
}

fn from_json(
    value: &Value,
    state: &mut Conversion,
    depth: usize,
) -> Result<PythonValue, PythonValueError> {
    state.enter(depth)?;
    Ok(match value {
        Value::Null => PythonValue::None,
        Value::Bool(value) => PythonValue::Bool(*value),
        Value::Number(value) if value.is_f64() => {
            PythonValue::Float(value.as_f64().expect("finite JSON float"))
        }
        Value::Number(value) => PythonValue::Integer(DecimalInteger::parse(&value.to_string())?),
        Value::String(value) => PythonValue::String(PythonString::from_text(value)?),
        Value::Array(values) => PythonValue::List(
            values
                .iter()
                .map(|value| from_json(value, state, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(values) => {
            let entries = values
                .iter()
                .map(|(key, value)| {
                    Ok((
                        PythonString::from_text(key)?,
                        from_json(value, state, depth + 1)?,
                    ))
                })
                .collect::<Result<Vec<_>, _>>()?;
            PythonValue::Dict(PythonDict::from_entries(entries)?)
        }
    })
}
