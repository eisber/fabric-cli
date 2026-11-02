// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure pinned GET value translation, deliberately separate from file export.

use super::{DefinitionError, DefinitionErrorKind as Kind, DefinitionLimits, source_codec};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_core_json::{
    MAX_NATIVE_DEPTH, MAX_NATIVE_NODES, PythonDict, PythonString, PythonValue, PythonValueError,
};
use serde_json::Value;
use std::fmt;

/// Whether the caller selected the source `decode=True` preprocessing branch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GetDefinitionPreprocess {
    /// Decode parts, then clear existing first-part Notebook outputs.
    #[default]
    Decode,
    /// Preserve acquired parts exactly, including payload type and Notebook outputs.
    Preserve,
}

/// Ordered GET value, including Python JSON domains and non-UTF-8 binary parts.
///
/// Containers use `Object`/`List`; `Scalar` returned by the decoder contains only
/// a non-container [`PythonValue`]. Keys retain native Python string identity.
/// Bytes are not base64 strings, file-export bytes, or invented JSON envelopes.
/// Debug intentionally does not disclose definition content.
#[derive(Clone, PartialEq)]
pub enum GetDefinitionValue {
    /// None, bool, integer, float or exact Python Unicode string.
    Scalar(PythonValue),
    /// Source `bytes` after base64 decoding fails UTF-8 conversion.
    Bytes(Vec<u8>),
    /// Ordered Python list.
    List(Vec<Self>),
    /// Ordered dictionary; overwrites retain the key's original position.
    Object(Vec<(PythonString, Self)>),
}

impl fmt::Debug for GetDefinitionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GetDefinitionValue(<definition content>)")
    }
}

/// Typed existing definition failure plus its logical GET value location.
///
/// Locations are JSON pointers with indices, never filesystem write evidence.
/// No completed/incomplete file paths are manufactured for this pure operation.
#[derive(Debug)]
pub struct GetDefinitionValueError {
    /// Existing classification, including explicit unsupported domains and limits.
    pub error: DefinitionError,
    /// Logical location such as `/definition/parts/2/payload`.
    pub location: String,
}

impl fmt::Display for GetDefinitionValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "GET definition value translation failed at {}",
            self.location
        )
    }
}

impl std::error::Error for GetDefinitionValueError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

fn failure(kind: Kind, location: &str) -> GetDefinitionValueError {
    GetDefinitionValueError {
        error: kind.into(),
        location: location.to_owned(),
    }
}

fn native_error(error: PythonValueError) -> Kind {
    match error {
        PythonValueError::Limit { .. } => Kind::Limit,
        _ => Kind::Unsupported("GET value is outside the requested native/serde JSON domain"),
    }
}

fn key_is(key: &PythonString, expected: &str) -> bool {
    key.codepoints()
        .iter()
        .copied()
        .eq(expected.chars().map(u32::from))
}

fn pointer(parent: &str, key: &str) -> String {
    format!("{parent}/{}", key.replace('~', "~0").replace('/', "~1"))
}

impl GetDefinitionValue {
    fn from_native(value: PythonValue) -> Self {
        Self::from_native_ref(&value)
    }

    fn from_native_ref(value: &PythonValue) -> Self {
        match value {
            PythonValue::List(values) => {
                Self::List(values.iter().map(Self::from_native_ref).collect())
            }
            PythonValue::Dict(dict) => Self::Object(
                dict.entries()
                    .iter()
                    .map(|(key, value)| (key.clone(), Self::from_native_ref(value)))
                    .collect(),
            ),
            scalar => Self::Scalar(scalar.clone()),
        }
    }

    fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(entries) => entries
                .iter()
                .find(|(candidate, _)| key_is(candidate, key))
                .map(|(_, value)| value),
            _ => None,
        }
    }

    fn get_mut(&mut self, key: &str) -> Option<&mut Self> {
        match self {
            Self::Object(entries) => entries
                .iter_mut()
                .find(|(candidate, _)| key_is(candidate, key))
                .map(|(_, value)| value),
            _ => None,
        }
    }

    fn set(&mut self, key: &str, value: Self) -> Result<(), Kind> {
        let Self::Object(entries) = self else {
            return Err(Kind::InvalidDefinition);
        };
        if let Some((_, current)) = entries.iter_mut().find(|(name, _)| key_is(name, key)) {
            *current = value;
        } else {
            entries.push((PythonString::from_text(key).map_err(native_error)?, value));
        }
        Ok(())
    }

    fn truthy(&self) -> bool {
        match self {
            Self::Scalar(PythonValue::None | PythonValue::Bool(false)) => false,
            Self::Scalar(PythonValue::Integer(value)) => value.as_str() != "0",
            Self::Scalar(PythonValue::Float(value)) => *value != 0.0,
            Self::Scalar(PythonValue::String(value)) => !value.codepoints().is_empty(),
            Self::List(values) => !values.is_empty(),
            Self::Object(values) => !values.is_empty(),
            Self::Bytes(values) => !values.is_empty(),
            _ => true,
        }
    }

    fn native_at(&self, location: &str) -> Result<PythonValue, GetDefinitionValueError> {
        match self {
            Self::Scalar(value) => Ok(value.clone()),
            Self::Bytes(_) => Err(failure(
                Kind::Unsupported("Python bytes are not JSON; caller must select a binary policy"),
                location,
            )),
            Self::List(values) => values
                .iter()
                .enumerate()
                .map(|(index, value)| value.native_at(&pointer(location, &index.to_string())))
                .collect::<Result<Vec<_>, _>>()
                .map(PythonValue::List),
            Self::Object(entries) => {
                let values = entries
                    .iter()
                    .map(|(key, value)| {
                        let name = key
                            .try_to_string()
                            .unwrap_or_else(|_| "<python-key>".into());
                        Ok((key.clone(), value.native_at(&pointer(location, &name))?))
                    })
                    .collect::<Result<Vec<_>, GetDefinitionValueError>>()?;
                PythonDict::from_entries(values)
                    .map(PythonValue::Dict)
                    .map_err(|error| failure(native_error(error), location))
            }
        }
    }

    /// Convert without losing wide integers, nonfinite floats or Unicode identity.
    ///
    /// # Errors
    /// Python bytes have no representation in the shared native JSON value domain.
    /// Such parts fail explicitly at their logical location, never become strings.
    /// Also enforces shared node/depth limits and rejects caller-constructed
    /// containers placed in `Scalar` instead of `Object`/`List`.
    pub fn try_to_python(&self) -> Result<PythonValue, GetDefinitionValueError> {
        self.validate_conversion()?;
        self.native_at("")
    }

    /// Convert only when every original value is representable by finite serde JSON.
    ///
    /// # Errors
    /// Rejects binary, wide integer, nonfinite and lone-surrogate values rather than
    /// rounding, stringifying, dropping or replacing them.
    /// Also enforces shared node/depth limits and the scalar/container invariant.
    pub fn try_to_json(&self) -> Result<Value, GetDefinitionValueError> {
        self.validate_conversion()?;
        self.json_at("")
    }

    fn validate_conversion(&self) -> Result<(), GetDefinitionValueError> {
        Budget {
            remaining: usize::MAX,
            nodes: 0,
        }
        .value(self, "", 0)
    }

    fn json_at(&self, location: &str) -> Result<Value, GetDefinitionValueError> {
        match self {
            Self::List(values) => values
                .iter()
                .enumerate()
                .map(|(index, value)| value.json_at(&pointer(location, &index.to_string())))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            Self::Object(entries) => {
                let mut values = serde_json::Map::new();
                for (key, value) in entries {
                    let name = key
                        .try_to_string()
                        .map_err(|error| failure(native_error(error), location))?;
                    let value = value.json_at(&pointer(location, &name))?;
                    values.insert(name, value);
                }
                Ok(Value::Object(values))
            }
            _ => self
                .native_at(location)?
                .try_to_json()
                .map_err(|error| failure(native_error(error), location)),
        }
    }
}

/// Merge already-acquired specialized metadata and definition, then decode GET values.
///
/// Translates the complete pure value flow in pinned Python
/// `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`: `dict.update`,
/// `decode_payload`, then first-part Notebook output cleanup. All item types use
/// the same decoder; only exact `Notebook` selects cleanup. Caller-owned support
/// checks, definition format selection, specialized endpoint acquisition, access
/// denial, connections/schedules and Environment/mirroring enrichments remain
/// outside this pure API. No generic metadata is fetched or substituted.
///
/// `definition=None` means the caller deliberately acquired metadata only (for
/// example a source access-denied fallback); it is not inferred from a failed
/// acquisition. `Preserve` corresponds to source `decode=False`.
///
/// `DefinitionLimits` bounds aggregate input/decoded/result storage units (UTF-8
/// bytes for input/decoded bytes, four bytes per result codepoint, one per node),
/// visited parts, decoded text paths and recursive part depth. Shared native JSON
/// node/string/decimal/depth bounds also apply. Paths are value data, not portable
/// file paths: leading slashes, duplicates and service order are retained.
///
/// # Errors
/// Returns existing typed errors with logical context on malformed structure,
/// allocation/domain bounds and noncanonical/permissive base64 (explicit
/// `Unsupported`, not silent alternative bytes). Source-caught decoding failures
/// instead retain the original payload/type and set the exact per-part `error`.
pub fn decode_get_definition_value(
    original_metadata: &Value,
    definition: Option<&Value>,
    item_type: &str,
    preprocess: GetDefinitionPreprocess,
    limits: DefinitionLimits,
) -> Result<GetDefinitionValue, GetDefinitionValueError> {
    let mut budget = Budget {
        remaining: limits.max_bytes.get(),
        nodes: 0,
    };
    budget.json(original_metadata, "", 0)?;
    if let Some(definition) = definition {
        budget.json(definition, "/acquired-definition", 0)?;
    }
    let native = PythonValue::try_from_json(original_metadata)
        .map_err(|error| failure(native_error(error), ""))?;
    let mut item = GetDefinitionValue::from_native(native);
    let GetDefinitionValue::Object(entries) = &mut item else {
        return Err(failure(Kind::InvalidDefinition, ""));
    };
    if let Some(definition) = definition {
        let native = PythonValue::try_from_json(definition)
            .map_err(|error| failure(native_error(error), "/acquired-definition"))?;
        let GetDefinitionValue::Object(definition) = GetDefinitionValue::from_native(native) else {
            return Err(failure(Kind::InvalidDefinition, "/acquired-definition"));
        };
        for (key, value) in definition {
            if let Some((_, current)) = entries.iter_mut().find(|(candidate, _)| *candidate == key)
            {
                *current = value;
            } else {
                entries.push((key, value));
            }
        }
    }
    if preprocess == GetDefinitionPreprocess::Decode {
        let mut parts = 0;
        decode_parts(&mut item, "", 0, limits, &mut parts, &mut budget)?;
        if item_type == "Notebook" {
            clean_notebook(&mut item)?;
        }
    }
    // Include retained encoded failures, error strings and newly parsed containers.
    budget.nodes = 0;
    budget.value(&item, "", 0)?;
    Ok(item)
}

struct Budget {
    remaining: usize,
    nodes: usize,
}

impl Budget {
    fn charge(&mut self, bytes: usize, location: &str) -> Result<(), GetDefinitionValueError> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| failure(Kind::Limit, location))?;
        Ok(())
    }

    fn node(&mut self, depth: usize, location: &str) -> Result<(), GetDefinitionValueError> {
        self.nodes += 1;
        if depth > MAX_NATIVE_DEPTH || self.nodes > MAX_NATIVE_NODES {
            return Err(failure(Kind::Limit, location));
        }
        self.charge(1, location)
    }

    fn json(
        &mut self,
        value: &Value,
        location: &str,
        depth: usize,
    ) -> Result<(), GetDefinitionValueError> {
        self.node(depth, location)?;
        match value {
            Value::String(value) => self.charge(value.len(), location)?,
            Value::Number(value) => self.charge(value.to_string().len(), location)?,
            Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    self.json(value, &pointer(location, &index.to_string()), depth + 1)?;
                }
            }
            Value::Object(entries) => {
                for (key, value) in entries {
                    self.charge(key.len(), location)?;
                    self.json(value, &pointer(location, key), depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn value(
        &mut self,
        value: &GetDefinitionValue,
        location: &str,
        depth: usize,
    ) -> Result<(), GetDefinitionValueError> {
        self.node(depth, location)?;
        match value {
            GetDefinitionValue::Object(entries) => {
                for (key, value) in entries {
                    self.charge(key.codepoints().len().saturating_mul(4), location)?;
                    let name = key
                        .try_to_string()
                        .unwrap_or_else(|_| "<python-key>".into());
                    self.value(value, &pointer(location, &name), depth + 1)?;
                }
            }
            GetDefinitionValue::List(values) => {
                for (index, value) in values.iter().enumerate() {
                    self.value(value, &pointer(location, &index.to_string()), depth + 1)?;
                }
            }
            GetDefinitionValue::Bytes(bytes) => self.charge(bytes.len(), location)?,
            GetDefinitionValue::Scalar(PythonValue::String(value)) => {
                self.charge(value.codepoints().len().saturating_mul(4), location)?;
            }
            GetDefinitionValue::Scalar(PythonValue::Integer(value)) => {
                self.charge(value.as_str().len(), location)?
            }
            GetDefinitionValue::Scalar(PythonValue::Dict(_) | PythonValue::List(_)) => {
                return Err(failure(Kind::InvalidInput, location));
            }
            _ => {}
        }
        Ok(())
    }
}

fn decode_parts(
    item: &mut GetDefinitionValue,
    location: &str,
    depth: usize,
    limits: DefinitionLimits,
    count: &mut usize,
    budget: &mut Budget,
) -> Result<(), GetDefinitionValueError> {
    if depth > limits.max_depth.get() {
        return Err(failure(Kind::Limit, location));
    }
    let has_definition_parts = match item.get("definition") {
        Some(GetDefinitionValue::Object(entries)) => {
            entries.iter().any(|(key, _)| key_is(key, "parts"))
        }
        Some(_) => {
            return Err(failure(
                Kind::InvalidDefinition,
                &pointer(location, "definition"),
            ));
        }
        None => false,
    };
    let (parts, parts_location) = if has_definition_parts {
        (
            item.get_mut("definition")
                .and_then(|value| value.get_mut("parts")),
            format!("{location}/definition/parts"),
        )
    } else {
        (
            item.get_mut("definitionParts"),
            pointer(location, "definitionParts"),
        )
    };
    let Some(parts) = parts else { return Ok(()) };
    let GetDefinitionValue::List(parts) = parts else {
        return Err(failure(Kind::InvalidDefinition, &parts_location));
    };
    for (index, part) in parts.iter_mut().enumerate() {
        let part_location = pointer(&parts_location, &index.to_string());
        *count += 1;
        if *count > limits.max_files.get() {
            return Err(failure(Kind::Limit, &part_location));
        }
        if !matches!(part, GetDefinitionValue::Object(_)) {
            return Err(failure(Kind::InvalidDefinition, &part_location));
        }
        decode_part(part, &part_location, limits, budget)?;
        // Pinned recursion tests only key presence; it does not visit that array.
        if part.get("nested_parts").is_some() {
            decode_parts(part, &part_location, depth + 1, limits, count, budget)?;
        }
    }
    Ok(())
}

fn type_name(value: &GetDefinitionValue) -> &'static str {
    match value {
        GetDefinitionValue::Scalar(PythonValue::None) => "NoneType",
        GetDefinitionValue::Scalar(PythonValue::Bool(_)) => "bool",
        GetDefinitionValue::Scalar(PythonValue::Integer(_)) => "int",
        GetDefinitionValue::Scalar(PythonValue::Float(_)) => "float",
        GetDefinitionValue::Scalar(PythonValue::String(_)) => "str",
        GetDefinitionValue::List(_) => "list",
        GetDefinitionValue::Object(_) => "dict",
        GetDefinitionValue::Bytes(_) => "bytes",
        _ => "object",
    }
}

fn text(value: &str) -> Result<GetDefinitionValue, Kind> {
    PythonString::from_text(value)
        .map(PythonValue::String)
        .map(GetDefinitionValue::Scalar)
        .map_err(native_error)
}

fn decode_part(
    part: &mut GetDefinitionValue,
    location: &str,
    limits: DefinitionLimits,
    budget: &mut Budget,
) -> Result<(), GetDefinitionValueError> {
    let payload_location = pointer(location, "payload");
    let Some(payload) = part.get("payload").filter(|value| value.truthy()) else {
        return Ok(());
    };
    let decoded = match payload {
        GetDefinitionValue::Scalar(PythonValue::String(encoded)) => {
            let encoded = encoded
                .try_to_string()
                .map_err(|error| failure(native_error(error), &payload_location))?;
            if !encoded.is_ascii() {
                Err("string argument should contain only ASCII characters".into())
            } else {
                if encoded.len() / 4 * 3 > budget.remaining.saturating_add(2) {
                    return Err(failure(Kind::Limit, &payload_location));
                }
                match STANDARD.decode(&encoded) {
                    Ok(bytes) if STANDARD.encode(&bytes) == encoded => Ok(bytes),
                    Err(_)
                        if encoded.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/')
                        }) =>
                    {
                        Err(if encoded.len() % 4 == 1 {
                            format!(
                                "Invalid base64-encoded string: number of data characters ({}) cannot be 1 more than a multiple of 4",
                                encoded.len()
                            )
                        } else {
                            "Incorrect padding".into()
                        })
                    }
                    _ => {
                        return Err(failure(
                            Kind::Unsupported("noncanonical/permissive GET base64"),
                            &payload_location,
                        ));
                    }
                }
            }
        }
        value => Err(format!(
            "argument should be a bytes-like object or ASCII string, not '{}'",
            type_name(value)
        )),
    };
    let decoded = match decoded {
        Ok(bytes) => {
            budget.charge(bytes.len(), &payload_location)?;
            match std::str::from_utf8(&bytes) {
                Err(_) => Ok(GetDefinitionValue::Bytes(bytes)),
                Ok(decoded) => {
                    let path = part.get("path");
                    let path = match path {
                        None => Ok(String::new()),
                        Some(GetDefinitionValue::Scalar(PythonValue::String(path))) => {
                            path.try_to_string().map_err(|error| {
                                failure(native_error(error), &pointer(location, "path"))
                            })
                        }
                        Some(path) => {
                            let message =
                                format!("'{}' object has no attribute 'endswith'", type_name(path));
                            return set_part_error(part, &message, location);
                        }
                    }?;
                    if path.len() > limits.max_path_bytes.get() {
                        return Err(failure(Kind::Limit, &pointer(location, "path")));
                    }
                    if [".json", ".ipynb", ".pbir", ".platform", ".pbism"]
                        .iter()
                        .any(|suffix| path.ends_with(suffix))
                    {
                        source_codec::parse_get(decoded)
                            .map_err(|kind| failure(kind, &payload_location))?
                            .map(GetDefinitionValue::from_native)
                    } else {
                        Ok(text(decoded).map_err(|kind| failure(kind, &payload_location))?)
                    }
                }
            }
        }
        Err(message) => Err(message),
    };
    match decoded {
        Ok(value) => {
            part.set("payload", value)
                .map_err(|kind| failure(kind, &payload_location))?;
            part.set(
                "payloadType",
                text("DecodeBase64").map_err(|kind| failure(kind, location))?,
            )
            .map_err(|kind| failure(kind, location))
        }
        Err(message) => set_part_error(part, &message, location),
    }
}

fn set_part_error(
    part: &mut GetDefinitionValue,
    message: &str,
    location: &str,
) -> Result<(), GetDefinitionValueError> {
    part.set(
        "error",
        text(&format!("Decoding error: {message}")).map_err(|kind| failure(kind, location))?,
    )
    .map_err(|kind| failure(kind, location))
}

fn clean_notebook(item: &mut GetDefinitionValue) -> Result<(), GetDefinitionValueError> {
    let Some(GetDefinitionValue::List(parts)) = item
        .get_mut("definition")
        .and_then(|definition| definition.get_mut("parts"))
    else {
        return Ok(());
    };
    let Some(payload) = parts.first_mut().and_then(|part| part.get_mut("payload")) else {
        return Ok(());
    };
    let location = "/definition/parts/0/payload";
    if !contains(payload, "cells").map_err(|kind| failure(kind, location))? {
        return Ok(());
    }
    let cells = payload
        .get_mut("cells")
        .ok_or_else(|| failure(Kind::InvalidDefinition, location))?;
    let location = pointer(location, "cells");
    match cells {
        GetDefinitionValue::List(cells) => {
            for (index, cell) in cells.iter_mut().enumerate() {
                let location = pointer(&location, &index.to_string());
                if contains(cell, "outputs").map_err(|kind| failure(kind, &location))? {
                    cell.set("outputs", GetDefinitionValue::List(Vec::new()))
                        .map_err(|kind| failure(kind, &location))?;
                }
            }
        }
        // Python iterates a dictionary's keys and a string's characters here.
        GetDefinitionValue::Object(entries) => {
            for (key, _) in entries {
                if text_contains(key, "outputs") {
                    return Err(failure(Kind::InvalidDefinition, &location));
                }
            }
        }
        GetDefinitionValue::Scalar(PythonValue::String(_)) => {}
        _ => return Err(failure(Kind::InvalidDefinition, &location)),
    }
    Ok(())
}

fn text_contains(value: &PythonString, key: &str) -> bool {
    let key: Vec<_> = key.chars().map(u32::from).collect();
    value
        .codepoints()
        .windows(key.len())
        .any(|window| window == key.as_slice())
}

fn contains(value: &GetDefinitionValue, key: &str) -> Result<bool, Kind> {
    match value {
        GetDefinitionValue::Object(_) => Ok(value.get(key).is_some()),
        GetDefinitionValue::List(values) => Ok(values.iter().any(|value| {
            matches!(value, GetDefinitionValue::Scalar(PythonValue::String(value)) if key_is(value, key))
        })),
        GetDefinitionValue::Scalar(PythonValue::String(value)) => Ok(text_contains(value, key)),
        _ => Err(Kind::InvalidDefinition),
    }
}
