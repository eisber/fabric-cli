// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded strict JSON decoding before a released typed SDK validates the body.

use serde_json::Value;
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
    let value = fabric_cli_output::input::strict_json_bounded(bytes, maximum_bytes.get())
        .map_err(|_| InputError::InvalidJson)?;
    if !value.is_object() {
        return Err(InputError::InvalidJson);
    }
    Ok(value)
}
