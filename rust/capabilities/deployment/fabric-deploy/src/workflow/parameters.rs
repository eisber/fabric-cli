// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::families::Publication;
use super::*;
use crate::{
    model::RepositoryItem,
    parameter_transforms::{
        DynamicVariable, DynamicVariableKind, ParameterItem, TransformBudget, TransformLimits,
        matches_parameter_filters, resolve_parameter_paths,
    },
};

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
    matches_parameter_filters(
        filter.filter(|value| truthy(value)),
        None,
        None,
        ParameterItem {
            item_type: value,
            item_name: "",
            file_path: "",
        },
    )
}

pub(super) fn inventory(repository: &DeploymentRepository) -> Vec<String> {
    let mut files = Vec::new();
    for item in &repository.items {
        for file in &item.files {
            let path = if item.path.is_empty() {
                file.path.clone()
            } else {
                format!("{}/{}", item.path, file.path)
            };
            if !files.contains(&path) {
                files.push(path);
            }
        }
    }
    files
}

pub(super) fn path_filters(rule: &Value) -> Option<Vec<String>> {
    let value = rule.get("file_path").filter(|value| truthy(value))?;
    Some(match value {
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Value::String(value) => vec![value.clone()],
        _ => return None,
    })
}

pub(super) fn rule_applies(
    rule: &Value,
    item: &RepositoryItem,
    path: &str,
    context: &EngineContext,
    repository: &DeploymentRepository,
) -> Result<bool, ConfigurationError> {
    let paths = path_filters(rule)
        .map(|filters| {
            resolve_parameter_paths(
                context
                    .repository_root
                    .as_deref()
                    .ok_or_else(|| invalid("Path filters require the caller's repository root"))?,
                &filters,
                &inventory(repository),
                context.path_filter_options,
                &mut TransformBudget::new(TransformLimits::default(), &|| false),
            )
            .map(|result| result.paths)
            .map_err(|error| ConfigurationError::Transform(Box::new(error)))
        })
        .transpose()?;
    let full = if item.path.is_empty() {
        path.into()
    } else {
        format!("{}/{}", item.path, path)
    };
    Ok(matches_parameter_filters(
        rule.get("item_type"),
        rule.get("item_name"),
        paths.as_deref(),
        ParameterItem {
            item_type: &item.item_type,
            item_name: &item.name,
            file_path: &full,
        },
    ))
}

pub(super) fn validate(
    config: &DeploymentConfig,
    context: &EngineContext,
) -> Result<(), ConfigurationError> {
    if config.workspace.contains_key("parameter_file_path") && context.program.is_none() {
        return Err(invalid(
            "Configured parameter file must be explicitly prepared before deployment",
        ));
    }
    if let Some(program) = &context.program {
        program
            .require_item_settings_consumed(&["spark_pool", "semantic_model_binding"])
            .map_err(|error| ConfigurationError::Transform(Box::new(error)))?;
    }
    Ok(())
}

pub(super) fn item_variable(value: &str) -> Option<(String, String, String)> {
    let variable = DynamicVariable::parse(value).ok()?;
    if variable.kind != DynamicVariableKind::Item || variable.workspace_name.is_some() {
        return None;
    }
    Some((variable.item_type?, variable.item_name?, variable.attribute))
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
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
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
    super::parameter_context::apply(io, publication, item, path, content, ids, budget).await
}
