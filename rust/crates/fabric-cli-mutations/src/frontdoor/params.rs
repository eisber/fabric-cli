// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::parser::invalid;
use crate::MutationError;
use serde_json::{Map, Value};

/// Parse the bounded source mkdir parameter format, retaining string values.
///
/// Source comma boundaries are a following `key=` even inside JSON/quotes.
/// Two-level dotted keys retain the entire raw remainder as a string.
/// Malformed/Unicode keys are explicitly rejected in this preview rather than
/// reproducing Python regex substring recovery or silently dropping arguments.
pub fn creation_parameters(tokens: &[String]) -> Result<Map<String, Value>, MutationError> {
    let joined = tokens.join(" ").replace('\\', "");
    let mut remaining = joined.trim();
    if !tokens.is_empty() && remaining.is_empty() {
        return Err(invalid("Invalid parameter format"));
    }
    let mut result = Map::new();
    while !remaining.is_empty() {
        let equal = key_end(remaining).ok_or_else(|| invalid("Invalid parameter format"))?;
        let key = &remaining[..equal];
        let value_start = equal + 1;
        let end = remaining[value_start..]
            .char_indices()
            .find_map(|(offset, ch)| {
                (ch == ',' && key_end(remaining[value_start + offset + 1..].trim_start()).is_some())
                    .then_some(value_start + offset)
            })
            .unwrap_or(remaining.len());
        let value = &remaining[value_start..end];
        if value.is_empty() || value.contains('\n') {
            return Err(invalid("Invalid parameter format"));
        }
        if let Some((parent, child)) = key.split_once('.') {
            let nested = result
                .entry(parent.to_owned())
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or_else(|| invalid("Conflicting nested creation parameters"))?;
            nested.insert(child.into(), Value::String(value.into()));
        } else {
            let cleaned = if value.trim().starts_with("[{") && value.trim().ends_with("}]") {
                match fabric_cli_output::parse_data(value) {
                    Ok(value) => value,
                    Err(_) if serde_json::from_str::<Value>(value).is_ok() => {
                        return Err(invalid(
                            "Creation parameter is outside the supported JSON domain",
                        ));
                    }
                    Err(_) => Value::String(value.replace(['\'', '"'], "")),
                }
            } else {
                Value::String(value.replace(['\'', '"'], ""))
            };
            result.insert(key.into(), cleaned);
        }
        remaining = remaining.get(end + 1..).unwrap_or("").trim_start();
    }
    Ok(result)
}

fn key_end(value: &str) -> Option<usize> {
    let end = value.find('=')?;
    let key = value[..end].strip_suffix('!').unwrap_or(&value[..end]);
    (!key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.')))
    .then_some(end)
}
