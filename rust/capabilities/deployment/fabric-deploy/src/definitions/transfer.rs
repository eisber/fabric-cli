// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{DefinitionError, DefinitionErrorKind as Kind, DefinitionLimits, DefinitionRead};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_core_json::json_dumps;
use serde_json::{Value, json};

/// Build Python's copy/create-or-update payload without decoding definition parts.
///
/// `folderId` is retained even when null, description is appended only when
/// truthy, and the service definition remains byte-for-byte base64 rather than
/// using the cleaned Notebook export representation. Context/discovery,
/// same-workspace `_copy` naming, collision/overwrite prompts, mutation, deletion
/// and cache reconciliation belong to the resource command owner.
///
/// # Errors
/// Missing definition/invalid destination metadata fails before a mutation.
pub fn copy_payload(
    source: &DefinitionRead,
    item_type: &str,
    display_name: &str,
    folder_id: Option<&str>,
) -> Result<Value, DefinitionError> {
    if item_type.is_empty() || display_name.is_empty() {
        return Err(Kind::InvalidInput.into());
    }
    if let Some(status) = source.definition_access_denied {
        return Err(Kind::DefinitionAccessDenied(status).into());
    }
    let definition = source
        .item
        .get("definition")
        .ok_or(Kind::InvalidDefinition)?;
    if !definition.is_object() {
        return Err(Kind::InvalidDefinition.into());
    }
    let mut payload = json!({
        "type": item_type,
        "displayName": display_name,
        "definition": definition,
        "folderId": folder_id,
    });
    if let Some(description) = source.item.get("description").filter(|value| truthy(value)) {
        payload["description"] = description.clone();
    }
    Ok(payload)
}

/// Decode the pinned SET codec, not the export decoder.
///
/// SET calls `json.loads` for every nonempty decoded part regardless of suffix;
/// it never clears Notebook cell outputs or converts bytes to local file text.
/// The caller then applies its existing restricted explicit-path replacement
/// policy, including source friendly-name aliases, before calling
/// [`encode_set_definition`]. JMESPath projection and deployment JSONPath are not
/// interchangeable with that mutation policy.
///
/// # Errors
/// Rejects malformed/unsupported base64/JSON and bounded numeric/part domains.
/// No raw text fallback or export cleanup is substituted for source SET errors.
pub fn decode_set_definition(
    mut item: Value,
    limits: DefinitionLimits,
) -> Result<Value, DefinitionError> {
    let Some(parts) = parts_mut(&mut item)? else {
        return Ok(item);
    };
    if parts.len() > limits.max_files.get() {
        return Err(Kind::Limit.into());
    }
    let mut remaining = limits.max_bytes.get();
    for part in parts {
        if part.get("nested_parts").is_some() {
            return Err(Kind::Unsupported("nested SET definition parts").into());
        }
        let Some(payload) = part.get("payload").filter(|value| truthy(value)) else {
            continue;
        };
        let encoded = payload.as_str().ok_or(Kind::InvalidDefinition)?;
        if encoded.len() / 4 * 3 > remaining.saturating_add(2) {
            return Err(Kind::Limit.into());
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| Kind::Unsupported("Python permissive base64 SET branch"))?;
        if STANDARD.encode(&bytes) != encoded {
            return Err(Kind::Unsupported("noncanonical source base64 SET branch").into());
        }
        remaining = remaining.checked_sub(bytes.len()).ok_or(Kind::Limit)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| Kind::InvalidDefinition)?;
        let value = super::codec::parse_json(text)?.ok_or(Kind::InvalidDefinition)?;
        part["payload"] = value;
        part["payloadType"] = json!("DecodeBase64");
    }
    Ok(item)
}

/// Apply only the source Notebook dependency-container initialization.
///
/// Call this after decoding and before the resource owner's explicit-path
/// replacement. Existing unrelated metadata and insertion order are retained.
///
/// # Errors
/// Rejects malformed part/metadata/dependency shapes exactly where source SET
/// would fail instead of silently discarding data.
pub fn ensure_notebook_dependency(item: &mut Value, query: &str) -> Result<(), DefinitionError> {
    for dependency in ["lakehouse", "warehouse", "environment"] {
        if !query.contains(&format!("dependencies.{dependency}")) {
            continue;
        }
        let payload = item
            .pointer_mut("/definition/parts/0/payload")
            .and_then(Value::as_object_mut)
            .ok_or(Kind::InvalidDefinition)?;
        let metadata = payload.entry("metadata").or_insert_with(|| json!({}));
        let dependencies = metadata
            .as_object_mut()
            .ok_or(Kind::InvalidDefinition)?
            .entry("dependencies")
            .or_insert_with(|| json!({}));
        dependencies
            .as_object_mut()
            .ok_or(Kind::InvalidDefinition)?
            .entry(dependency)
            .or_insert_with(|| json!({}));
    }
    Ok(())
}

/// Encode the actual source SET part rules and return its exact request bytes.
///
/// Object payloads use the shared default Python ASCII/spacing encoder; strings
/// are encoded directly. Arrays/scalars remain unchanged, including their prior
/// payloadType, as in the pinned source (not an invented API repair). An ipynb
/// part sets definition.format even when its edited payload is a scalar.
///
/// # Errors
/// Rejects unsupported nested parts and explicit part/request byte bounds.
pub fn encode_set_definition(
    mut item: Value,
    limits: DefinitionLimits,
) -> Result<Vec<u8>, DefinitionError> {
    let mut ipynb = false;
    if let Some(parts) = parts_mut(&mut item)? {
        if parts.len() > limits.max_files.get() {
            return Err(Kind::Limit.into());
        }
        for part in parts {
            if part.get("nested_parts").is_some() {
                return Err(Kind::Unsupported("nested SET definition parts").into());
            }
            let Some(payload) = part.get("payload") else {
                continue;
            };
            ipynb |= part
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(|path| path.ends_with(".ipynb"));
            let bytes = match payload {
                Value::Object(_) => Some(json_dumps(payload).into_bytes()),
                Value::String(text) => Some(text.as_bytes().to_vec()),
                _ => None,
            };
            if let Some(bytes) = bytes {
                if bytes.len() > limits.max_bytes.get() {
                    return Err(Kind::Limit.into());
                }
                part["payload"] = json!(STANDARD.encode(bytes));
                part["payloadType"] = json!("InlineBase64");
            }
        }
    }
    if ipynb {
        item["definition"]["format"] = json!("ipynb");
    }
    let bytes = json_dumps(&item).into_bytes();
    if bytes.len() > limits.max_bytes.get() {
        return Err(Kind::Limit.into());
    }
    Ok(bytes)
}

fn parts_mut(item: &mut Value) -> Result<Option<&mut Vec<Value>>, Kind> {
    let item = item.as_object_mut().ok_or(Kind::InvalidDefinition)?;
    let Some(definition) = item.get_mut("definition") else {
        return Ok(None);
    };
    let definition = definition.as_object_mut().ok_or(Kind::InvalidDefinition)?;
    definition
        .get_mut("parts")
        .map(|parts| parts.as_array_mut().ok_or(Kind::InvalidDefinition))
        .transpose()
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}
