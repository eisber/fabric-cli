// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded strict JSON decoding before a released typed SDK validates the body.

use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use std::{fmt, num::NonZeroUsize};

/// Local input decoding failure; source content never appears in diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputError {
    /// Omitted input differs from an empty supplied document.
    Missing,
    /// Supplied input contains no JSON document.
    Empty,
    /// Raw input bytes exceed the caller's explicit finite bound.
    TooLarge,
    /// The caller selected media not advertised by this decoder.
    UnsupportedMedia,
    /// Text is not strict UTF-8.
    InvalidUtf8,
    /// JSON syntax, duplicate keys, depth, trailing input or object root is invalid.
    InvalidJson,
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "An explicit input document is required.",
            Self::Empty => "The supplied input document is empty.",
            Self::TooLarge => "The input document exceeds its explicit byte bound.",
            Self::UnsupportedMedia => "This input decoder requires application/json.",
            Self::InvalidUtf8 => "The input document must be strict UTF-8.",
            Self::InvalidJson => "The input must be one JSON object without duplicate keys.",
        })
    }
}

impl std::error::Error for InputError {}

/// Decodes exactly one bounded UTF-8 JSON object, rejecting duplicate keys at
/// every depth, including escape-equivalent keys.
///
/// The returned JSON is only decoded input, **not** a schema-validated operation.
/// A released representation-specific SDK schema must reject unknown/read-only
/// fields and check body/path identity before approval or dispatch. Native bytes,
/// archives, URI downloads and arbitrary media are not accepted here.
///
/// # Errors
/// Returns a sanitized local classification without reflecting input content.
pub fn decode_json_input(
    bytes: Option<&[u8]>,
    media_type: &str,
    maximum_bytes: NonZeroUsize,
) -> Result<Value, InputError> {
    if media_type != "application/json" {
        return Err(InputError::UnsupportedMedia);
    }
    let bytes = bytes.ok_or(InputError::Missing)?;
    if bytes.len() > maximum_bytes.get() {
        return Err(InputError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| InputError::InvalidUtf8)?;
    if text.trim().is_empty() {
        return Err(InputError::Empty);
    }
    let mut decoder = serde_json::Deserializer::from_str(text);
    let value = StrictValue::deserialize(&mut decoder)
        .map_err(|_| InputError::InvalidJson)?
        .0;
    decoder.end().map_err(|_| InputError::InvalidJson)?;
    if !value.is_object() {
        return Err(InputError::InvalidJson);
    }
    Ok(value)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON without duplicate keys")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Number::from_f64(value)
            .map(|number| StrictValue(Value::Number(number)))
            .ok_or_else(|| E::custom("nonfinite JSON number"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.into())))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Self::Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom("duplicate JSON key"));
            }
            values.insert(key, object.next_value::<StrictValue>()?.0);
        }
        Ok(StrictValue(Value::Object(values)))
    }
}
