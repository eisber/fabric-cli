// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{DefinitionError, DefinitionErrorKind as Kind, DefinitionLimits};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_core_json::{Format, Indent, encode_unicode, json_dumps};
use serde_json::Value;
use std::{collections::HashMap, fmt};

/// A portable relative path and exact local bytes, in original service order.
#[derive(PartialEq, Eq)]
pub struct DefinitionFile {
    /// Forward-slash relative path, never absolute/traversing.
    pub path: String,
    /// Bytes ready for a binary write, including native text-mode translation.
    pub bytes: Vec<u8>,
}

/// Source storage serialization policy, independent of transport and credentials.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExportStorage {
    /// Pretty Unicode JSON and native text-mode newlines.
    #[default]
    Local,
    /// Default Python JSON bytes and unchanged ordinary text/binary bytes.
    Lakehouse,
}

impl fmt::Debug for DefinitionFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DefinitionFile")
            .field("path", &self.path)
            .field("bytes", &"<definition content>")
            .finish()
    }
}

/// Validate a portable destination using the released definition path policy.
///
/// # Errors
/// Rejects traversal, absolute paths, controls, Windows device/ADS aliases,
/// nonportable punctuation and explicit path/depth bounds on every platform.
pub fn validate_part_path(path: &str, limits: DefinitionLimits) -> Result<(), Kind> {
    if path.is_empty() || path.len() > limits.max_path_bytes.get() {
        return Err(Kind::UnsafePath);
    }
    if path.split('/').count() > limits.max_depth.get() {
        return Err(Kind::Limit);
    }
    for part in path.split('/') {
        let stem = part
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        let reserved = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                    || matches!(suffix, "\u{b9}" | "\u{b2}" | "\u{b3}")
            })
        });
        if matches!(part, "" | "." | "..")
            || reserved
            || part.ends_with(['.', ' '])
            || part
                .chars()
                .any(|ch| ch.is_control() || "\\:*?\"<>|".contains(ch))
        {
            return Err(Kind::UnsafePath);
        }
    }
    Ok(())
}

/// Decode and format actual source definition parts without changing their order.
///
/// Reuses the released definition-file algorithm with shared Python JSON
/// formatting rather than a second encoder. JSON suffixes parse before storage;
/// other UTF-8 text parses only at the storage boundary. That distinction retains
/// Python's `None`/`False` spelling for suffix-decoded scalar JSON. Binary payloads
/// bypass text newline translation. Only existing first-part Notebook outputs
/// are cleared, never added. Missing/empty payloads retain source behavior.
///
/// # Errors
/// This original bounded API rejects malformed base64/JSON and native numeric
/// domains rather than applying per-part fallback. Full source consumers use
/// [`decode_export_files_for_storage`]. Unsafe/colliding paths and aggregate
/// decoded/formatted bounds fail before any filesystem write.
pub fn decode_export_files(
    item: &Value,
    notebook: bool,
    limits: DefinitionLimits,
) -> Result<Vec<DefinitionFile>, DefinitionError> {
    decode_files(item, notebook, limits, ExportStorage::Local, true)
}

/// Decode source-ordered parts for local or Lakehouse storage.
///
/// The Lakehouse policy has no native newline conversion and uses source default
/// ASCII JSON rather than the local pretty Unicode serializer. This API performs
/// no remote I/O and does not discover credentials. Source per-part decode errors
/// retain the encoded payload; wide integers/nonfinite values use the existing
/// shared native Python-value encoder without lossy `serde_json` conversion.
///
/// # Errors
/// Retains the same path/collision and finite allocation policy as
/// [`decode_export_files`], plus shared native-value bounds. Permissive base64 is
/// deliberately rejected, never decoded or silently written as another value.
pub fn decode_export_files_for_storage(
    item: &Value,
    notebook: bool,
    limits: DefinitionLimits,
    destination: ExportStorage,
) -> Result<Vec<DefinitionFile>, DefinitionError> {
    decode_files(item, notebook, limits, destination, false)
}

fn decode_files(
    item: &Value,
    notebook: bool,
    limits: DefinitionLimits,
    destination: ExportStorage,
    bounded_legacy: bool,
) -> Result<Vec<DefinitionFile>, DefinitionError> {
    let definition = item
        .get("definition")
        .and_then(Value::as_object)
        .ok_or(Kind::InvalidDefinition)?;
    let empty = Vec::new();
    let parts = match definition.get("parts") {
        Some(parts) => parts.as_array().ok_or(Kind::InvalidDefinition)?,
        None => &empty,
    };
    if parts.len() > limits.max_files.get() {
        return Err(Kind::Limit.into());
    }
    let mut files = Vec::with_capacity(parts.len());
    let mut remaining = limits.max_bytes.get();
    let mut decoded_remaining = limits.max_bytes.get();
    let mut paths = HashMap::new();
    let mut prefixes = HashMap::new();
    for (index, part) in parts.iter().enumerate() {
        if bounded_legacy && part.get("nested_parts").is_some() {
            return Err(Kind::Unsupported("nested export definition parts").into());
        }
        let path = part
            .get("path")
            .and_then(Value::as_str)
            .ok_or(Kind::InvalidDefinition)?
            .trim_start_matches('/');
        validate_part_path(path, limits)?;
        if let Some(previous) = paths.insert(path.to_lowercase(), path)
            && previous != path
        {
            return Err(Kind::Collision.into());
        }
        let mut prefix = String::new();
        for component in path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            if let Some(previous) = prefixes.insert(prefix.to_lowercase(), prefix.clone())
                && previous != prefix
            {
                return Err(Kind::Collision.into());
            }
        }
        part.as_object().ok_or(Kind::InvalidDefinition)?;
        let payload = part
            .get("payload")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let (mut bytes, text) = if !bounded_legacy {
            super::source_codec::payload(
                &payload,
                path,
                notebook && index == 0,
                destination,
                &mut decoded_remaining,
            )?
        } else {
            match payload {
                Value::String(payload) if !payload.is_empty() => {
                    if payload.len() / 4 * 3 > decoded_remaining.saturating_add(2) {
                        return Err(Kind::Limit.into());
                    }
                    let decoded = match STANDARD.decode(&payload) {
                        Ok(decoded) => decoded,
                        Err(_) if bounded_legacy => {
                            return Err(Kind::Unsupported(
                                "Python permissive/error-preserving base64 branch",
                            )
                            .into());
                        }
                        Err(_) => {
                            // Python retains the original part after a decode error.
                            // Do not turn this into permissive base64 decoding.
                            let mut bytes = storage(&Value::String(payload), destination)?;
                            if destination == ExportStorage::Local {
                                bytes = native_text(bytes, remaining)?;
                            }
                            remaining = remaining.checked_sub(bytes.len()).ok_or(Kind::Limit)?;
                            files.push(DefinitionFile {
                                path: path.to_owned(),
                                bytes,
                            });
                            continue;
                        }
                    };
                    if STANDARD.encode(&decoded) != payload {
                        return Err(if bounded_legacy {
                            Kind::Unsupported("noncanonical source base64")
                        } else {
                            Kind::InvalidDefinition
                        }
                        .into());
                    }
                    decoded_remaining = decoded_remaining
                        .checked_sub(decoded.len())
                        .ok_or(Kind::Limit)?;
                    match std::str::from_utf8(&decoded) {
                        Ok(text) => {
                            let suffix_json = [".json", ".ipynb", ".pbir", ".platform", ".pbism"]
                                .iter()
                                .any(|suffix| path.ends_with(suffix));
                            let value = if suffix_json {
                                let parsed = parse_json(text)?;
                                if bounded_legacy && parsed.is_none() {
                                    return Err(Kind::Unsupported(
                                        "Python error-preserving malformed suffix JSON",
                                    )
                                    .into());
                                }
                                let mut value =
                                    parsed.unwrap_or_else(|| Value::String(payload.clone()));
                                if notebook && index == 0 {
                                    clean_notebook(&mut value)?;
                                }
                                value
                            } else {
                                Value::String(text.to_owned())
                            };
                            (storage(&value, destination)?, true)
                        }
                        Err(_) => (decoded, false),
                    }
                }
                value => {
                    if bounded_legacy
                        && part.get("payload").is_some()
                        && (value.is_array() || value.is_object())
                    {
                        return Err(Kind::Unsupported("non-string encoded payload").into());
                    }
                    (storage(&value, destination)?, true)
                }
            }
        };
        if text && destination == ExportStorage::Local {
            bytes = native_text(bytes, remaining)?;
        }
        remaining = remaining.checked_sub(bytes.len()).ok_or(Kind::Limit)?;
        files.push(DefinitionFile {
            path: path.to_owned(),
            bytes,
        });
    }
    for file in &files {
        let mut parent = file.path.as_str();
        while let Some((prefix, _)) = parent.rsplit_once('/') {
            if paths.contains_key(&prefix.to_lowercase()) {
                return Err(Kind::Collision.into());
            }
            parent = prefix;
        }
    }
    Ok(files)
}

fn clean_notebook(value: &mut Value) -> Result<(), Kind> {
    if let Some(cells) = value.get_mut("cells") {
        for cell in cells.as_array_mut().ok_or(Kind::InvalidDefinition)? {
            let cell = cell.as_object_mut().ok_or(Kind::InvalidDefinition)?;
            if let Some(outputs) = cell.get_mut("outputs") {
                *outputs = Value::Array(Vec::new());
            }
        }
    }
    Ok(())
}

fn storage(value: &Value, destination: ExportStorage) -> Result<Vec<u8>, Kind> {
    if destination == ExportStorage::Lakehouse
        && matches!(value, Value::Null | Value::Bool(_) | Value::Number(_))
    {
        // Source cannot take len() of these decoded scalar payloads.
        return Err(Kind::InvalidDefinition);
    }
    let encode = |value: &Value| match destination {
        ExportStorage::Local => encode_unicode(value, Format::Pretty(Indent::Four)),
        ExportStorage::Lakehouse => json_dumps(value),
    };
    let text = match value {
        Value::String(text) => match parse_json(text)? {
            Some(value) => encode(&value),
            None => text.clone(),
        },
        Value::Object(_) | Value::Array(_) => encode(value),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(_) => json_dumps(value),
    };
    Ok(text.into_bytes())
}

pub(super) fn parse_json(text: &str) -> Result<Option<Value>, Kind> {
    let value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) if error.is_syntax() || error.is_eof() => return Ok(None),
        Err(_) => {
            return Err(Kind::Unsupported(
                "JSON numeric/depth domain outside the shared encoder",
            ));
        }
    };
    // serde_json otherwise accepts out-of-domain Python integers as lossy f64.
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut string = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if string => index += 2,
            b'"' => {
                string = !string;
                index += 1;
            }
            b'-' | b'0'..=b'9' if !string => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && matches!(bytes[index], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    index += 1;
                }
                let number = &text[start..index];
                if !number.contains(['.', 'e', 'E'])
                    && number.parse::<i64>().is_err()
                    && number.parse::<u64>().is_err()
                {
                    return Err(Kind::Unsupported(
                        "Python integer outside the shared JSON domain",
                    ));
                }
            }
            _ => index += 1,
        }
    }
    Ok(Some(value))
}

fn native_text(bytes: Vec<u8>, limit: usize) -> Result<Vec<u8>, Kind> {
    #[cfg(windows)]
    {
        let mut output = Vec::new();
        for byte in bytes {
            let needed = if byte == b'\n' { 2 } else { 1 };
            if needed > limit.saturating_sub(output.len()) {
                return Err(Kind::Limit);
            }
            if byte == b'\n' {
                output.push(b'\r');
            }
            output.push(byte);
        }
        Ok(output)
    }
    #[cfg(not(windows))]
    {
        if bytes.len() > limit {
            Err(Kind::Limit)
        } else {
            Ok(bytes)
        }
    }
}
