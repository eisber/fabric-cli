// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Projected GET result storage over the existing Python storage encoder.

use super::{DefinitionError, DefinitionErrorKind, DefinitionLimits, ExportStorage, source_codec};
use fabric_core_json::PythonValue;
use serde_json::Value;

/// Caller-selected text-mode newline translation, never an ambient setting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GetOutputNewline {
    /// Unix/macOS text files.
    #[default]
    Lf,
    /// Windows text files; every LF becomes CRLF, including an existing CRLF.
    CrLf,
}

/// Serialized projected result, without any definition parts or output envelope.
pub struct GetOutputValue {
    /// Exact bytes for the caller's existing file capability.
    pub bytes: Vec<u8>,
    /// Source `_validate_json` decision, also controlling filename suffixes.
    pub is_json: bool,
}

impl std::fmt::Debug for GetOutputValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GetOutputValue")
            .field("bytes", &"<projected GET result>")
            .field("is_json", &self.is_json)
            .finish()
    }
}

/// Serialize a projected GET result using the existing source storage codec.
///
/// Objects/lists and valid JSON strings use pretty Unicode local JSON or default
/// ASCII Lakehouse JSON. Ordinary local scalars use Python spelling. Lakehouse
/// non-string scalars are unsupported, matching its source `len(data)` failure.
/// This API grants no filesystem authority and performs no I/O.
///
/// # Errors
/// Retains finite native JSON, encoded-byte and newline-expansion bounds.
/// Unsupported values fail, never become a stringified envelope or empty file.
pub fn serialize_get_projected_result(
    value: &Value,
    storage: ExportStorage,
    newline: GetOutputNewline,
    limits: DefinitionLimits,
) -> Result<GetOutputValue, DefinitionError> {
    let native = PythonValue::try_from_json(value).map_err(|_| DefinitionErrorKind::Limit)?;
    let (mut bytes, is_json) = source_codec::store_projected(&native, storage)?;
    if bytes.len() > limits.max_bytes.get() {
        return Err(DefinitionErrorKind::Limit.into());
    }
    if storage == ExportStorage::Local && newline == GetOutputNewline::CrLf {
        let length = bytes
            .len()
            .checked_add(bytes.iter().filter(|byte| **byte == b'\n').count())
            .filter(|length| *length <= limits.max_bytes.get())
            .ok_or(DefinitionErrorKind::Limit)?;
        let mut translated = Vec::with_capacity(length);
        for byte in bytes {
            if byte == b'\n' {
                translated.push(b'\r');
            }
            translated.push(byte);
        }
        bytes = translated;
    }
    Ok(GetOutputValue { bytes, is_json })
}
