// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

pub(super) async fn bind<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    publication: Publication<'_>,
    ids: &HashMap<String, String>,
) -> Result<(), WorkflowFailure> {
    let Publication {
        config,
        context,
        plan,
        base,
        ..
    } = publication;
    let repository = &plan.repository;
    let selected = &plan.order;
    let Some(bindings) = context
        .program
        .as_ref()
        .and_then(|program| program.item_settings().get("semantic_model_binding"))
        .or_else(|| {
            context
                .parameter_document
                .as_ref()
                .and_then(|document| document.get("semantic_model_binding"))
        })
    else {
        return Ok(());
    };
    let models: Vec<_> = repository
        .items
        .iter()
        .filter(|item| item.item_type == "SemanticModel")
        .collect();
    let mut mapping: HashMap<String, Vec<String>> = HashMap::new();
    let normalize = |value: &Value| -> Vec<String> {
        match value {
            Value::String(value) => vec![value.clone()],
            Value::Array(values) => values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        }
    };
    let names = |value: Option<&Value>| -> Vec<String> { value.map(normalize).unwrap_or_default() };
    if let Some(entries) = bindings.as_array() {
        for entry in entries {
            let Some(connection) = entry
                .get("connection_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            for name in names(entry.get("semantic_model_name")) {
                if models.iter().any(|model| model.name == name) {
                    mapping.insert(name, vec![connection.into()]);
                }
            }
        }
    } else if let Some(bindings) = bindings.as_object() {
        let mut explicit = HashSet::new();
        for entry in bindings
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(value) = super::super::parameters::environment_value(
                entry.get("connection_id"),
                &config.environment,
            ) else {
                continue;
            };
            let connections = normalize(value);
            if connections.is_empty() {
                continue;
            }
            for name in names(entry.get("semantic_model_name")) {
                explicit.insert(name.clone());
                if models.iter().any(|model| model.name == name) {
                    mapping.insert(name, connections.clone());
                }
            }
        }
        if let Some(default) = super::super::parameters::environment_value(
            bindings
                .get("default")
                .and_then(|value| value.get("connection_id")),
            &config.environment,
        ) {
            let connections = normalize(default);
            for model in &models {
                if !explicit.contains(&model.name) && !connections.is_empty() {
                    mapping.insert(model.name.clone(), connections.clone());
                }
            }
        }
    } else {
        return Ok(());
    }
    if mapping.is_empty() {
        return Ok(());
    }
    let connections = match io.list_fabric("/v1/connections").await {
        Ok(connections) => connections,
        Err(WorkflowFailure::Operation(error))
            if !matches!(error.kind, OperationErrorKind::Cancelled) =>
        {
            io.result.warnings.push(*error);
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    for model in models {
        let Some(connection_ids) = mapping.get(&model.name) else {
            continue;
        };
        if !selected
            .iter()
            .any(|index| repository.items[*index].logical_id == model.logical_id)
        {
            continue;
        }
        let Some(id) = ids.get(&identity_key(model)) else {
            continue;
        };
        let valid: Vec<_> = connection_ids
            .iter()
            .filter_map(|id| {
                connections
                    .iter()
                    .find(|connection| connection.get("id").and_then(Value::as_str) == Some(id))
            })
            .collect();
        if valid.is_empty() {
            continue;
        }
        let item_connections = match io
            .list_fabric(&format!("{base}/items/{id}/connections"))
            .await
        {
            Ok(connections) => connections,
            Err(WorkflowFailure::Operation(error))
                if !matches!(error.kind, OperationErrorKind::Cancelled) =>
            {
                io.result.warnings.push(*error);
                continue;
            }
            Err(error) => return Err(error),
        };
        if item_connections.is_empty() {
            continue;
        }
        for connection in valid {
            let mut path = connection
                .pointer("/connectionDetails/path")
                .cloned()
                .unwrap_or(Value::Null);
            if connection
                .pointer("/connectionDetails/type")
                .and_then(Value::as_str)
                == Some("AzureDataLakeStorage")
                && let Some(value) = path
                    .as_str()
                    .filter(|value| !value.is_empty() && !value.ends_with('/'))
            {
                path = format!("{value}/").into();
            }
            let body = json!({"connectionBinding": {
                "id": connection.get("id").cloned().unwrap_or(Value::Null),
                "connectivityType": connection.get("connectivityType").cloned().unwrap_or(Value::Null),
                "connectionDetails": {
                    "type": connection.pointer("/connectionDetails/type").cloned().unwrap_or(Value::Null), "path": path,
                },
            }});
            if let Err(error) = io
                .request_fabric(
                    ResourceMethod::Post,
                    &format!("{base}/semanticModels/{id}/bindConnection"),
                    Some(body),
                )
                .await
            {
                match error {
                    WorkflowFailure::Operation(error)
                        if !matches!(error.kind, OperationErrorKind::Cancelled) =>
                    {
                        io.result.warnings.push(*error)
                    }
                    error => return Err(error),
                }
            }
        }
    }
    Ok(())
}
