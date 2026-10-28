// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{TransformBudget, TransformError, context::truthy};
use serde_json::{Map, Value};

fn invalid(message: &str) -> TransformError {
    TransformError::Invalid(message.into())
}

fn names(value: &Value) -> bool {
    value.is_string()
        || value
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
}

fn connection(value: &Value, mapping: bool, environment: &str) -> Result<(), TransformError> {
    let guid = |value: &Value| {
        value
            .as_str()
            .is_some_and(crate::deployment_config::valid_guid)
    };
    if !mapping {
        return if guid(value) {
            Ok(())
        } else {
            Err(invalid("Legacy connection_id must be a string GUID"))
        };
    }
    let values = value
        .as_object()
        .filter(|values| !values.is_empty())
        .ok_or_else(|| invalid("connection_id must be a nonempty environment dictionary"))?;
    for value in values.values() {
        if !(guid(value)
            || value
                .as_array()
                .is_some_and(|values| !values.is_empty() && values.iter().all(guid)))
        {
            return Err(invalid(
                "connection_id environment values must be GUIDs or nonempty GUID lists",
            ));
        }
    }
    if environment != "N/A"
        && values.len() != 1
        && values.keys().any(|key| key.to_lowercase() == "_all_")
    {
        return Err(invalid(
            "The reserved _ALL_ connection environment must be the sole key",
        ));
    }
    Ok(())
}

fn semantic_model(value: &Value, environment: &str) -> Result<(), TransformError> {
    if let Some(section) = value.as_object() {
        if section.is_empty()
            || section
                .keys()
                .any(|key| !["default", "models"].contains(&key.as_str()))
        {
            return Err(invalid(
                "semantic_model_binding requires default and/or models",
            ));
        }
        if let Some(default) = section.get("default") {
            let default = default
                .as_object()
                .ok_or_else(|| invalid("semantic_model_binding.default must be a dictionary"))?;
            connection(
                default
                    .get("connection_id")
                    .ok_or_else(|| invalid("Missing default connection_id"))?,
                true,
                environment,
            )?;
        }
        if let Some(models) = section.get("models") {
            let models = models
                .as_array()
                .filter(|models| !models.is_empty())
                .ok_or_else(|| invalid("semantic_model_binding.models must be a nonempty list"))?;
            for entry in models {
                let entry = entry
                    .as_object()
                    .ok_or_else(|| invalid("Semantic model entry must be a dictionary"))?;
                if !entry.get("semantic_model_name").is_some_and(names) {
                    return Err(invalid("Missing or invalid semantic_model_name"));
                }
                connection(
                    entry
                        .get("connection_id")
                        .ok_or_else(|| invalid("Missing model connection_id"))?,
                    true,
                    environment,
                )?;
            }
        }
    } else {
        let entries = value
            .as_array()
            .ok_or_else(|| invalid("semantic_model_binding must be a list or dictionary"))?;
        for entry in entries {
            let entry = entry
                .as_object()
                .ok_or_else(|| invalid("Semantic model entry must be a dictionary"))?;
            if entry.len() != 2
                || !entry
                    .get("semantic_model_name")
                    .is_some_and(|value| truthy(value) && names(value))
                || !entry.contains_key("connection_id")
            {
                return Err(invalid(
                    "Invalid legacy semantic_model_binding keys or required values",
                ));
            }
            connection(&entry["connection_id"], false, environment)?;
        }
    }
    Ok(())
}

pub(super) fn item_settings(
    document: &Map<String, Value>,
    environment: &str,
    budget: &mut TransformBudget<'_>,
) -> Result<Map<String, Value>, TransformError> {
    let mut result = Map::new();
    if let Some(value) = document.get("spark_pool_replace_value") {
        result.insert("spark_pool_replace_value".into(), value.clone());
    }
    if let Some(pool) = document.get("spark_pool") {
        let rules = pool
            .as_array()
            .ok_or_else(|| invalid("spark_pool must be a list"))?;
        for rule in rules {
            budget.checkpoint()?;
            let rule = rule
                .as_object()
                .ok_or_else(|| invalid("spark_pool rule must be a dictionary"))?;
            if rule.keys().any(|key| {
                !["instance_pool_id", "replace_value", "item_name"].contains(&key.as_str())
            }) || !rule
                .get("instance_pool_id")
                .is_some_and(|value| value.as_str().is_some_and(|value| !value.is_empty()))
            {
                return Err(invalid("Invalid spark_pool keys or instance_pool_id"));
            }
            let values = rule
                .get("replace_value")
                .and_then(Value::as_object)
                .filter(|values| !values.is_empty())
                .ok_or_else(|| invalid("spark_pool replace_value must be a nonempty dictionary"))?;
            for value in values.values() {
                let value = value
                    .as_object()
                    .ok_or_else(|| invalid("spark_pool replacement must be a dictionary"))?;
                if value.len() != 2
                    || !value.get("type").is_some_and(|value| {
                        matches!(value.as_str(), Some("Capacity" | "Workspace"))
                    })
                    || !value
                        .get("name")
                        .is_some_and(|value| value.as_str().is_some_and(|value| !value.is_empty()))
                {
                    return Err(invalid(
                        "spark_pool replacement requires type Capacity/Workspace and a name",
                    ));
                }
            }
            if environment != "N/A"
                && values.len() != 1
                && values.keys().any(|key| key.to_lowercase() == "_all_")
            {
                return Err(invalid(
                    "The reserved _ALL_ pool environment must be the sole key",
                ));
            }
            super::context::Filters::parse(rule)?;
        }
        result.insert("spark_pool".into(), pool.clone());
    }
    let mut semantic = document.get("semantic_model_binding").cloned();
    if let Some(gateways) = document.get("gateway_binding") {
        let gateways = gateways
            .as_array()
            .ok_or_else(|| invalid("gateway_binding must be a list"))?;
        result.insert("gateway_binding".into(), Value::Array(gateways.clone()));
        if !gateways.is_empty() && semantic.as_ref().is_none_or(Value::is_array) {
            let mut entries = semantic
                .take()
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            for entry in gateways {
                let entry = entry
                    .as_object()
                    .ok_or_else(|| invalid("Gateway binding entry must be a dictionary"))?;
                entries.push(Value::Object(Map::from_iter([
                    (
                        "connection_id".into(),
                        entry.get("gateway_id").cloned().unwrap_or(Value::Null),
                    ),
                    (
                        "semantic_model_name".into(),
                        entry
                            .get("dataset_name")
                            .cloned()
                            .unwrap_or_else(|| Value::Array(Vec::new())),
                    ),
                ])));
            }
            semantic = Some(Value::Array(entries));
        }
    }
    if let Some(semantic) = semantic {
        semantic_model(&semantic, environment)?;
        result.insert("semantic_model_binding".into(), semantic);
    }
    Ok(result)
}
