// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{MutationError, create::truthy};
use serde_json::{Map, Value, json};

pub(crate) fn creation_payload(params: &Map<String, Value>) -> Result<Value, MutationError> {
    let Some(mode) = params.get("creationmode").filter(|value| !value.is_null()) else {
        return Ok(json!({}));
    };
    let mode = mode
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| mode.to_string());
    match mode.to_lowercase().as_str() {
        "new" => {
            let mut payload = json!({"creationMode":"New"});
            if let Some(value) = params
                .get("backupretentiondays")
                .filter(|value| !value.is_null())
            {
                let days = value
                    .as_i64()
                    .map(serde_json::Number::from)
                    .or_else(|| value.as_u64().map(serde_json::Number::from))
                    .or_else(|| value.as_str().and_then(integer))
                    .ok_or_else(|| {
                        MutationError::new(
                            "InvalidInput",
                            format!(
                                "Invalid backupRetentionDays value '{}'. It must be an integer",
                                value
                                    .as_str()
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| value.to_string())
                            ),
                        )
                    })?;
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

fn integer(value: &str) -> Option<serde_json::Number> {
    let value = value.trim();
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
    let value = value.replace('_', "");
    value
        .parse::<i64>()
        .map(serde_json::Number::from)
        .ok()
        .or_else(|| value.parse::<u64>().map(serde_json::Number::from).ok())
}
