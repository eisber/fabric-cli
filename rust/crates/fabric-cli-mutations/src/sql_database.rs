// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{MutationError, create::truthy};
use serde_json::{Map, Value, json};

pub(crate) fn creation_payload(params: &Map<String, Value>) -> Result<Value, MutationError> {
    let Some(mode) = params.get("creationmode").filter(|value| !value.is_null()) else {
        return Ok(json!({}));
    };
    let mode = parameter_string(mode)?;
    match mode.to_lowercase().as_str() {
        "new" => {
            let mut payload = json!({"creationMode":"New"});
            if let Some(value) = params
                .get("backupretentiondays")
                .filter(|value| !value.is_null())
            {
                let Some(days) = parameter_integer(value) else {
                    return Err(MutationError::new(
                        "InvalidInput",
                        format!(
                            "Invalid backupRetentionDays value '{}'. It must be an integer",
                            parameter_string(value)?,
                        ),
                    ));
                };
                payload["backupRetentionDays"] = days.into();
            }
            if let Some(collation) = params.get("collation").filter(|value| !value.is_null()) {
                payload["collation"] = collation.clone();
            }
            Ok(payload)
        }
        "restore" => {
            required(
                params,
                &["restorepointintime", "itemid", "workspaceid"],
                "Missing required parameter(s) for restore mode. Required: restorePointInTime, itemId, workspaceId. Example: -P creationMode=Restore,restorePointInTime=2024-01-15T10:30:00Z,itemId=<item-id>,workspaceId=<workspace-id>",
            )?;
            Ok(json!({
                "creationMode":"Restore",
                "restorePointInTime": params["restorepointintime"],
                "sourceDatabaseReference": {
                    "itemId":params["itemid"],
                    "referenceType":params.get("referencetype").cloned().unwrap_or(json!("ById")),
                    "workspaceId":params["workspaceid"],
                },
            }))
        }
        "restoredeleteddatabase" => {
            required(
                params,
                &["restorabledeleteddatabasename", "restorepointintime"],
                "Missing required parameter(s) for RestoreDeletedDatabase mode. Required: restorableDeletedDatabaseName, restorePointInTime. Example: -P creationMode=RestoreDeletedDatabase,restorableDeletedDatabaseName=<name>,restorePointInTime=2024-01-15T10:30:00Z",
            )?;
            Ok(json!({
                "creationMode":"RestoreDeletedDatabase",
                "restorableDeletedDatabaseName":params["restorabledeleteddatabasename"],
                "restorePointInTime":params["restorepointintime"],
            }))
        }
        _ => Err(MutationError::new(
            "InvalidInput",
            format!(
                "Unsupported creation mode '{mode}'. Supported modes: New, Restore, RestoreDeletedDatabase"
            ),
        )),
    }
}

fn parameter_string(value: &Value) -> Result<String, MutationError> {
    match value {
        Value::String(value) => Ok(value.clone()),
        _ => crate::errors::python_repr(value)
            .map_err(|()| MutationError::new("InvalidJson", "Invalid JSON format")),
    }
}

fn parameter_integer(value: &Value) -> Option<serde_json::Number> {
    match value {
        Value::Bool(value) => Some(u8::from(*value).into()),
        Value::String(value) => integer(value),
        Value::Number(value) => value
            .as_i64()
            .map(Into::into)
            .or_else(|| value.as_u64().map(Into::into))
            .or_else(|| integer(&format!("{:.0}", value.as_f64()?.trunc()))),
        _ => None,
    }
}

fn required(
    params: &Map<String, Value>,
    keys: &[&str],
    message: &str,
) -> Result<(), MutationError> {
    if keys.iter().any(|key| !params.get(*key).is_some_and(truthy)) {
        return Err(MutationError::new("InvalidInput", message));
    }
    Ok(())
}

pub(crate) fn integer(value: &str) -> Option<serde_json::Number> {
    let normalized = value
        .chars()
        .map(|character| decimal_digit(character).unwrap_or(character))
        .collect::<String>();
    let value = normalized.trim();
    let digits = value
        .strip_prefix('+')
        .or_else(|| value.strip_prefix('-'))
        .unwrap_or(value);
    if digits.is_empty()
        || !digits.as_bytes()[0].is_ascii_digit()
        || !digits.as_bytes()[digits.len() - 1].is_ascii_digit()
        || digits.contains("__")
        || !digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'_')
    {
        return None;
    }

    fn decimal_digit(character: char) -> Option<char> {
        // Unicode 15.1 Nd zero blocks from the pinned Python 3.13 source oracle.
        const ZEROS: &[u32] = &[
            0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6,
            0xd66, 0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0,
            0x1a80, 0x1a90, 0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0,
            0xaa50, 0xabf0, 0xff10, 0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0,
            0x11450, 0x114d0, 0x11650, 0x116c0, 0x11730, 0x118e0, 0x11950, 0x11c50, 0x11d50,
            0x11da0, 0x11f50, 0x16a60, 0x16ac0, 0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec,
            0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e950, 0x1fbf0,
        ];
        let code = u32::from(character);
        let zero = ZEROS
            .iter()
            .find(|&&zero| code >= zero && code - zero < 10)?;
        char::from_u32(u32::from(b'0') + code - zero)
    }
    let value = value.replace('_', "");
    value
        .parse::<i64>()
        .map(serde_json::Number::from)
        .ok()
        .or_else(|| value.parse::<u64>().map(serde_json::Number::from).ok())
}
