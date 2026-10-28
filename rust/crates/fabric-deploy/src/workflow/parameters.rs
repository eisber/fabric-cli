// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::families::Publication;
use super::*;
use crate::{
    model::RepositoryItem,
    parameter_transforms::{ParameterProgram, TransformBudget},
};
use serde_json::Map;

pub(super) fn environment_value<'a>(
    value: Option<&'a Value>,
    environment: &str,
) -> Option<&'a Value> {
    let value = value?.as_object()?;
    if value.len() == 1 && value.keys().next()?.eq_ignore_ascii_case("_ALL_") {
        value.values().next()
    } else {
        value.get(environment)
    }
}

pub(super) fn matches_filter(filter: Option<&Value>, value: &str) -> bool {
    match filter {
        None | Some(Value::Null) => true,
        Some(Value::String(filter)) => filter == value,
        Some(Value::Array(filters)) => filters.iter().any(|filter| filter.as_str() == Some(value)),
        _ => false,
    }
}

fn glob(pattern: &str, path: &str) -> bool {
    fn component(pattern: &[u8], text: &[u8]) -> bool {
        let (mut p, mut t, mut star, mut restart) = (0, 0, None, 0);
        while t < text.len() {
            if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
                p += 1;
                t += 1;
            } else if p < pattern.len() && pattern[p] == b'*' {
                star = Some(p);
                p += 1;
                restart = t;
            } else if let Some(position) = star {
                restart += 1;
                t = restart;
                p = position + 1;
            } else {
                return false;
            }
        }
        while p < pattern.len() && pattern[p] == b'*' {
            p += 1;
        }
        p == pattern.len()
    }
    fn segments(pattern: &[&str], path: &[&str]) -> bool {
        if pattern.is_empty() {
            return path.is_empty();
        }
        if pattern[0] == "**" {
            return (0..=path.len()).any(|index| segments(&pattern[1..], &path[index..]));
        }
        !path.is_empty()
            && component(pattern[0].as_bytes(), path[0].as_bytes())
            && segments(&pattern[1..], &path[1..])
    }
    segments(
        &pattern.split('/').collect::<Vec<_>>(),
        &path.split('/').collect::<Vec<_>>(),
    )
}

pub(super) fn rule_applies(rule: &Value, item: &RepositoryItem, path: &str) -> bool {
    if !matches_filter(rule.get("item_type"), &item.item_type)
        || !matches_filter(rule.get("item_name"), &item.name)
    {
        return false;
    }
    let Some(filter) = rule.get("file_path").filter(|value| !value.is_null()) else {
        return true;
    };
    let full = format!("{}/{}", item.path, path);
    let matches = |filter: &str| {
        let filter = filter.replace('\\', "/");
        glob(filter.trim_start_matches("./"), &full)
    };
    match filter {
        Value::String(filter) => matches(filter),
        Value::Array(filters) => filters.iter().filter_map(Value::as_str).any(matches),
        _ => false,
    }
}

pub(super) fn validate(
    config: &DeploymentConfig,
    context: &EngineContext,
) -> Result<(), ConfigurationError> {
    if config.workspace.contains_key("parameter_file_path")
        && config.parameters().is_none()
        && context.parameter_document.is_none()
    {
        return Err(invalid(
            "Configured parameter file must be explicitly read before deployment",
        ));
    }
    let Some(document) = &context.parameter_document else {
        return Ok(());
    };
    let document = document
        .as_object()
        .ok_or_else(|| invalid("Parameter file must be a dictionary"))?;
    if document.is_empty()
        || document.keys().any(|key| {
            ![
                "find_replace",
                "key_value_replace",
                "spark_pool",
                "semantic_model_binding",
            ]
            .contains(&key.as_str())
        })
    {
        return Err(invalid("Invalid parameter section"));
    }
    let mut sanitized = Map::new();
    for section in ["key_value_replace", "find_replace"] {
        let Some(rules) = document.get(section) else {
            continue;
        };
        let rules = rules
            .as_array()
            .ok_or_else(|| invalid("Parameter section must be a list"))?;
        let mut selected = Vec::new();
        for rule in rules {
            let mut rule = rule
                .as_object()
                .cloned()
                .ok_or_else(|| invalid("Parameter rule must be a dictionary"))?;
            for key in ["item_type", "item_name", "file_path"] {
                if let Some(value) = rule.remove(key)
                    && !value.is_null()
                    && !(value.is_string()
                        || value
                            .as_array()
                            .is_some_and(|values| values.iter().all(Value::is_string)))
                {
                    return Err(invalid(
                        "Parameter filter must be a string or list of strings",
                    ));
                }
            }
            let values = rule
                .get_mut("replace_value")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| invalid("replace_value must be a dictionary"))?;
            for value in values.values_mut() {
                if value.as_str().is_some_and(|value| value.starts_with('$')) {
                    *value = "deferred-dynamic-value".into();
                }
            }
            if environment_value(Some(&Value::Object(values.clone())), &config.environment)
                .is_none()
            {
                continue;
            }
            if let Some(find) = rule.get_mut("find_value")
                && find.as_str().is_some_and(|value| value.starts_with('$'))
            {
                *find = "deferred-dynamic-find".into();
            }
            selected.push(Value::Object(rule));
        }
        if !selected.is_empty() {
            sanitized.insert(section.into(), Value::Array(selected));
        }
    }
    if !sanitized.is_empty() {
        ParameterProgram::with_default_limits(&Value::Object(sanitized), &config.environment)
            .map_err(|error| ConfigurationError::Transform(Box::new(error)))?;
    }
    Ok(())
}

pub(super) fn item_variable(value: &str) -> Option<(&str, &str, &str)> {
    let value = value.strip_prefix("$items.")?;
    let (item_type, value) = value.split_once('.')?;
    let (name, attribute) = value.rsplit_once('.')?;
    let attribute = attribute.strip_prefix('$').unwrap_or(attribute);
    (!name.is_empty()
        && crate::deployment_config::ITEM_TYPES.contains(&item_type)
        && !attribute.is_empty())
    .then_some((item_type, name, attribute))
}

pub(super) fn has_unfiltered_item_variable(context: &EngineContext, environment: &str) -> bool {
    let filters = context
        .constants
        .get("PARAMETER_FILE_FILTERS")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_else(|| vec!["item_type", "item_name", "file_path"]);
    for section in ["find_replace", "key_value_replace"] {
        for rule in context
            .parameter_document
            .as_ref()
            .and_then(|document| document.get(section))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if environment_value(rule.get("replace_value"), environment)
                .and_then(Value::as_str)
                .and_then(item_variable)
                .is_some()
                && !filters.iter().any(|key| rule.get(*key).is_some_and(truthy))
            {
                return true;
            }
        }
    }
    false
}

pub(super) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

pub(super) async fn apply<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    publication: Publication<'_>,
    item: &RepositoryItem,
    path: &str,
    content: &str,
    ids: &HashMap<String, String>,
    budget: &mut TransformBudget<'_>,
) -> Result<String, WorkflowFailure> {
    let Publication {
        config, context, ..
    } = publication;
    let Some(document) = &context.parameter_document else {
        return config.parameters().map_or_else(
            || Ok(content.into()),
            |program| {
                program
                    .apply(content, path, budget)
                    .map_err(|error| ConfigurationError::Transform(Box::new(error)).into())
            },
        );
    };
    let mut selected = Map::new();
    for section in ["key_value_replace", "find_replace"] {
        let mut rules = Vec::new();
        for rule in document
            .get(section)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if !rule_applies(rule, item, path) {
                continue;
            }
            let Some(replacement) =
                environment_value(rule.get("replace_value"), &config.environment)
            else {
                continue;
            };
            let replacement = resolve(io, replacement, publication, ids).await?;
            let mut rule = rule
                .as_object()
                .cloned()
                .ok_or_else(|| invalid("Invalid parameter rule"))?;
            for key in ["item_type", "item_name", "file_path"] {
                rule.remove(key);
            }
            rule.insert(
                "replace_value".into(),
                json!({(config.environment.clone()): replacement}),
            );
            if let Some(find) = rule
                .get("find_value")
                .cloned()
                .filter(|value| value.as_str().is_some_and(|value| value.starts_with('$')))
            {
                rule.insert(
                    "find_value".into(),
                    resolve(io, &find, publication, ids).await?,
                );
            }
            rules.push(Value::Object(rule));
        }
        if !rules.is_empty() {
            selected.insert(section.into(), Value::Array(rules));
        }
    }
    if selected.is_empty() {
        return Ok(content.into());
    }
    let program =
        ParameterProgram::from_document(&Value::Object(selected), &config.environment, budget)
            .map_err(|error| ConfigurationError::Transform(Box::new(error)))?;
    program
        .apply(content, path, budget)
        .map_err(|error| ConfigurationError::Transform(Box::new(error)).into())
}

pub(super) async fn resolve<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    value: &Value,
    publication: Publication<'_>,
    ids: &HashMap<String, String>,
) -> Result<Value, WorkflowFailure> {
    let Publication {
        context,
        plan,
        base,
        workspace,
        ..
    } = publication;
    let repository = &plan.repository;
    let Some(value) = value.as_str().filter(|value| value.starts_with('$')) else {
        return Ok(value.clone());
    };
    if matches!(value, "$workspace.id" | "$workspace.$id") {
        return Ok(workspace.into());
    }
    if matches!(value, "$workspace.$name" | "$workspace.$name_encoded") {
        let metadata = io.request(ResourceMethod::Get, base, None).await?;
        let name = metadata
            .get("displayName")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Workspace name could not be resolved"))?;
        let name = if value.ends_with("_encoded") {
            percent_encode(name)
        } else {
            name.into()
        };
        return Ok(name.into());
    }
    let (item_type, name, attribute, target_workspace) =
        if let Some((kind, name, attribute)) = item_variable(value) {
            (
                kind.to_owned(),
                name.to_owned(),
                attribute.to_lowercase(),
                workspace.to_owned(),
            )
        } else if let Some(value) = value.strip_prefix("$workspace.") {
            let (name, remainder) = value
                .split_once(".$items.")
                .map(|(name, rest)| (name, Some(rest)))
                .unwrap_or_else(|| (value.strip_suffix(".$id").unwrap_or(value), None));
            let workspaces = io.list("/v1/workspaces").await?;
            let id = workspaces
                .iter()
                .find(|value| value.get("displayName").and_then(Value::as_str) == Some(name))
                .and_then(|value| value.get("id"))
                .and_then(Value::as_str)
                .filter(|id| valid_guid(id))
                .ok_or_else(|| invalid("Referenced workspace name could not be resolved"))?;
            let Some(remainder) = remainder else {
                return Ok(id.into());
            };
            let variable = format!("$items.{remainder}");
            let (kind, name, attribute) = item_variable(&variable)
                .ok_or_else(|| invalid("Invalid cross-workspace item variable"))?;
            (
                kind.into(),
                name.into(),
                attribute.to_lowercase(),
                id.into(),
            )
        } else if let Some(name) = value.strip_prefix("$ENV:") {
            if !context.enabled("enable_environment_variable_replacement") {
                return Err(invalid("Environment replacement feature is not enabled").into());
            }
            return context
                .environment_variables
                .get(name)
                .cloned()
                .map(Value::String)
                .ok_or_else(|| {
                    invalid("Referenced environment variable was not supplied by the caller").into()
                });
        } else {
            return Err(invalid("Invalid dynamic replacement variable").into());
        };
    if !context.listed(
        "ITEM_ATTR_LOOKUP",
        &[
            "id",
            "sqlendpoint",
            "sqlendpointfqdn",
            "sqlendpointid",
            "queryserviceuri",
        ],
        &attribute,
    ) {
        return Err(invalid("Invalid dynamic item attribute").into());
    }
    let target_base = format!("/v1/workspaces/{target_workspace}");
    let deployed = deployed_items(io.list(&format!("{target_base}/items")).await?)?;
    let id = deployed
        .iter()
        .find(|item| item.item_type == item_type && item.name == name)
        .map(|item| item.id.clone())
        .or_else(|| {
            if target_workspace == workspace {
                repository
                    .items
                    .iter()
                    .find(|item| item.item_type == item_type && item.name == name)
                    .and_then(|item| ids.get(&identity_key(item)))
                    .cloned()
            } else {
                None
            }
        })
        .ok_or_else(|| invalid("Dynamic referenced item is not deployed"))?;
    if attribute == "id" {
        return Ok(id.into());
    }
    let Some(path) = context.attribute_pointer(&item_type, &attribute) else {
        return Ok(Value::String(String::new()));
    };
    let metadata = io
        .request(
            ResourceMethod::Get,
            &format!("{target_base}/{}s/{id}", item_type.to_lowercase()),
            None,
        )
        .await?;
    let value = metadata
        .pointer(&path)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("Dynamic item attribute is not available"))?;
    Ok(
        if item_type == "SQLDatabase" && attribute == "sqlendpointfqdn" {
            value.split(',').next().unwrap_or(value).into()
        } else {
            value.into()
        },
    )
}

fn percent_encode(value: &str) -> String {
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            result.push(char::from(byte));
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}
