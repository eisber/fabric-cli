// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::families::Publication;
use super::*;
use crate::{
    model::RepositoryItem,
    parameter_transforms::{
        DynamicVariable, DynamicVariableKind, ParameterItem, ParameterResolution,
        ParameterResolver, TransformBudget, TransformError, TransformLimits, encode_dynamic_name,
        resolve_parameter_paths,
    },
};
struct Resolver<'io, 'context, 'session, E> {
    io: &'io mut ServiceIo<'session, E>,
    publication: Publication<'context>,
    paths: HashMap<Vec<String>, Vec<String>>,
    failure: Option<WorkflowFailure>,
}

impl<E: OperationExecutor> ParameterResolver for Resolver<'_, '_, '_, E> {
    fn resolve<'a>(&'a mut self, variable: &'a DynamicVariable) -> ParameterResolution<'a> {
        Box::pin(async move {
            match resolve_value(self.io, self.publication, variable).await {
                Ok(value) => Ok(Value::String(value)),
                Err(error) => {
                    self.failure = Some(error);
                    Err(TransformError::Invalid("Metadata resolution failed".into()))
                }
            }
        })
    }

    fn resolve_paths(&self, filters: &[String]) -> Result<Vec<String>, TransformError> {
        self.paths
            .get(filters)
            .cloned()
            .ok_or_else(|| TransformError::Invalid("Unprepared path-filter capability".into()))
    }
}

pub(super) async fn apply<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    publication: Publication<'_>,
    item: &RepositoryItem,
    path: &str,
    content: &str,
    _ids: &HashMap<String, String>,
    budget: &mut TransformBudget<'_>,
) -> Result<String, WorkflowFailure> {
    let Some(program) = publication.context.program.as_ref() else {
        return Ok(content.into());
    };
    let mut paths = HashMap::new();
    for section in ["key_value_replace", "find_replace"] {
        for rule in program
            .source_document()
            .get(section)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(filters) = super::parameters::path_filters(rule) else {
                continue;
            };
            if paths.contains_key(&filters) {
                continue;
            }
            let resolved = resolve_parameter_paths(
                publication
                    .context
                    .repository_root
                    .as_deref()
                    .ok_or_else(|| invalid("Path filters require the caller's repository root"))?,
                &filters,
                &super::parameters::inventory(&publication.plan.repository),
                publication.context.path_filter_options,
                budget,
            )
            .map_err(|error| ConfigurationError::Transform(Box::new(error)))?;
            for skipped in resolved.skipped {
                io.result
                    .notices
                    .push(format!("Parameter path filter was skipped: {skipped}"));
            }
            paths.insert(filters, resolved.paths);
        }
    }
    let full = if item.path.is_empty() {
        path.into()
    } else {
        format!("{}/{}", item.path, path)
    };
    let item = ParameterItem {
        item_type: &item.item_type,
        item_name: &item.name,
        file_path: &full,
    };
    let mut resolver = Resolver {
        io,
        publication,
        paths,
        failure: None,
    };
    let result = program
        .apply_scoped(content, item, &mut resolver, budget)
        .await;
    if let Some(error) = resolver.failure {
        Err(error)
    } else {
        result.map_err(|error| ConfigurationError::Transform(Box::new(error)).into())
    }
}

async fn resolve_value<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    publication: Publication<'_>,
    variable: &DynamicVariable,
) -> Result<String, WorkflowFailure> {
    let workspace = if let Some(name) = &variable.workspace_name {
        io.list("/v1/workspaces")
            .await?
            .iter()
            .find(|workspace| workspace.get("displayName").and_then(Value::as_str) == Some(name))
            .and_then(|workspace| workspace.get("id"))
            .and_then(Value::as_str)
            .filter(|id| valid_guid(id))
            .map(str::to_owned)
            .ok_or_else(|| invalid("Referenced workspace could not be resolved"))?
    } else {
        publication.workspace.into()
    };
    let base = format!("/v1/workspaces/{workspace}");
    if variable.kind == DynamicVariableKind::Workspace {
        if variable.attribute == "id" {
            return Ok(workspace);
        }
        let metadata = io.request(ResourceMethod::Get, &base, None).await?;
        let name = metadata
            .get("displayName")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Workspace display name was not found"))?;
        return if variable.attribute == "name_encoded" {
            encode_dynamic_name(
                name,
                &mut TransformBudget::new(TransformLimits::default(), &|| false),
            )
            .map_err(|error| ConfigurationError::Transform(Box::new(error)).into())
        } else {
            Ok(name.into())
        };
    }
    let kind = variable
        .item_type
        .as_deref()
        .ok_or_else(|| invalid("Missing dynamic item type"))?;
    let name = variable
        .item_name
        .as_deref()
        .ok_or_else(|| invalid("Missing dynamic item name"))?;
    let items = deployed_items(io.list(&format!("{base}/items")).await?)?;
    let id = items
        .iter()
        .find(|item| item.item_type == kind && item.name == name)
        .map(|item| item.id.as_str())
        .ok_or_else(|| invalid("Dynamic referenced item is not deployed"))?;
    if variable.attribute == "id" {
        return Ok(id.into());
    }
    let Some(pointer) = publication
        .context
        .attribute_pointer(kind, &variable.attribute)
    else {
        return Ok(String::new());
    };
    let key = (
        workspace.clone(),
        kind.to_owned(),
        id.to_owned(),
        name.to_owned(),
        variable.attribute.clone(),
    );
    if let Some(value) = io.attribute_cache.get(&key) {
        return Ok(value.clone());
    }
    let metadata = io
        .request(
            ResourceMethod::Get,
            &format!("{base}/{}s/{id}", kind.to_lowercase()),
            None,
        )
        .await?;
    let value = metadata
        .pointer(&pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("Dynamic item attribute is not available"))?
        .to_owned();
    io.attribute_cache.insert(key, value.clone());
    if kind == "SQLDatabase"
        && matches!(
            variable.attribute.as_str(),
            "sqlendpoint" | "sqlendpointfqdn"
        )
    {
        let fqdn = value.split(',').next().unwrap_or(&value).to_owned();
        for (attribute, value) in [
            ("sqlendpoint", value.clone()),
            ("sqlendpointfqdn", fqdn.clone()),
        ] {
            io.attribute_cache.insert(
                (
                    workspace.clone(),
                    kind.into(),
                    id.into(),
                    name.into(),
                    attribute.into(),
                ),
                value,
            );
        }
        return Ok(if variable.attribute == "sqlendpointfqdn" {
            fqdn
        } else {
            value
        });
    }
    Ok(value)
}
