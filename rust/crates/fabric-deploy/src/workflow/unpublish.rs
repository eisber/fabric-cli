// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::planner::{orphan_selected, pipeline_references, topological_order};
use base64::{Engine, engine::general_purpose::STANDARD};

pub(super) async fn run<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    config: &DeploymentConfig,
    context: &EngineContext,
    plan: &DeploymentPlan,
    base: &str,
) -> Result<(), WorkflowFailure> {
    let deployed = deployed_items(io.list(&format!("{base}/items")).await?)?;
    for item_type in crate::deployment_config::ITEM_TYPES.iter().rev() {
        if !config.item_types.iter().any(|scope| scope == item_type)
            || !context.can_unpublish(item_type)
        {
            continue;
        }
        let mut orphans = Vec::new();
        for (index, item) in deployed.iter().enumerate() {
            if item.item_type != *item_type
                || plan
                    .repository
                    .items
                    .iter()
                    .any(|source| source.item_type == item.item_type && source.name == item.name)
            {
                continue;
            }
            if orphan_selected(config, item_type, &item.name)? {
                orphans.push(index);
            }
        }
        if *item_type == "DataPipeline" {
            let mut edges = Vec::new();
            for index in &orphans {
                let id = &deployed[*index].id;
                let definition = io
                    .request(
                        ResourceMethod::Post,
                        &format!("{base}/items/{id}/getDefinition"),
                        None,
                    )
                    .await?;
                let parts = definition
                    .pointer("/definition/parts")
                    .and_then(Value::as_array)
                    .ok_or_else(|| invalid("Missing deployed pipeline definition parts"))?;
                let Some(part) = parts.iter().find(|part| {
                    part.get("path").and_then(Value::as_str) == Some("pipeline-content.json")
                }) else {
                    continue;
                };
                let encoded = part
                    .get("payload")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("Missing deployed pipeline payload"))?;
                let bytes = STANDARD
                    .decode(encoded)
                    .map_err(|_| invalid("Invalid deployed pipeline base64"))?;
                let content = serde_json::from_slice(&bytes)
                    .map_err(|_| invalid("Invalid deployed pipeline JSON"))?;
                for reference in pipeline_references(&content) {
                    if let Some(dependency) = deployed
                        .iter()
                        .position(|item| item.item_type == "DataPipeline" && item.id == reference)
                    {
                        edges.push((*index, dependency));
                    }
                }
            }
            orphans = topological_order(&orphans, &edges)?;
            orphans.reverse();
        }
        for index in orphans {
            let item = &deployed[index];
            io.current = Some(format!("{}.{}", item.name, item.item_type));
            let query = if context.enabled("enable_hard_delete") {
                "?hardDelete=true"
            } else {
                ""
            };
            if let Err(error) = io
                .request(
                    ResourceMethod::Delete,
                    &format!("{base}/items/{}{query}", item.id),
                    None,
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
    let remaining = deployed_items(io.list(&format!("{base}/items")).await?)?;
    let folders = folder_hierarchy(&io.list(&format!("{base}/folders")).await?)?;
    if !context.enabled("disable_workspace_folder_publish") {
        let mut protected: HashSet<_> = remaining
            .iter()
            .filter(|item| !item.folder_id.is_empty())
            .map(|item| item.folder_id.clone())
            .collect();
        for (path, id) in &folders {
            if !protected.contains(id) {
                continue;
            }
            let mut parent = path.as_str();
            while let Some((ancestor, _)) = parent.rsplit_once('/') {
                if let Some(id) = folders.get(ancestor) {
                    protected.insert(id.clone());
                }
                parent = ancestor;
            }
        }
        let mut order: Vec<_> = folders.iter().collect();
        order.sort_by_key(|(path, _)| {
            std::cmp::Reverse(path.bytes().filter(|ch| *ch == b'/').count())
        });
        for (_, id) in order {
            if protected.contains(id) {
                continue;
            }
            if let Err(error) = io
                .request(
                    ResourceMethod::Delete,
                    &format!("{base}/folders/{id}"),
                    None,
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
    io.current = None;
    Ok(())
}
