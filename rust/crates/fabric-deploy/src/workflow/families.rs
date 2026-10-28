// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{
    model::RepositoryItem,
    parameter_transforms::{TransformBudget, TransformLimits},
    transforms::{NotebookTransforms, replace_workspace_ids},
};

mod bulk;
mod dataflow;
mod environment;
mod kql;
mod lakehouse;
mod report;
mod semanticmodel;

pub(super) async fn publish<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    config: &DeploymentConfig,
    context: &EngineContext,
    plan: &mut DeploymentPlan,
    base: &str,
    workspace: &str,
    folders: &HashMap<String, String>,
    deployed: &[DeployedItem],
    ids: &mut HashMap<String, String>,
) -> Result<(), WorkflowFailure> {
    let bulk = context.bulk_publish
        && !config
            .item_types
            .iter()
            .any(|item_type| ["Warehouse", "DataBuildToolJob"].contains(&item_type.as_str()));
    if bulk && !super::parameters::has_unfiltered_item_variable(context) {
        return bulk::publish(io, config, context, plan, base, workspace, deployed, ids).await;
    }
    let mut environments = Vec::new();
    for item_type in crate::deployment_config::ITEM_TYPES {
        let mut indices: Vec<_> = plan
            .order
            .iter()
            .copied()
            .filter(|index| plan.repository.items[*index].item_type == *item_type)
            .collect();
        if indices.is_empty() {
            continue;
        }
        if *item_type == "Dataflow" {
            indices = dataflow::order(config, context, &plan.repository, &indices)?;
        }
        if *item_type == "Environment" {
            environments = indices
                .iter()
                .map(|index| plan.repository.items[*index].name.clone())
                .collect();
            environment::check(io, base, &environments, true).await?;
        }
        if matches!(*item_type, "DataPipeline" | "KQLQueryset" | "KQLDashboard") {
            // These source pre-hooks refresh deployed metadata at their type boundary.
            let _ = deployed_items(io.list(&format!("{base}/items")).await?)?;
        }
        let mut failure = None;
        let mut failure_item = None;
        for index in indices {
            let item = &plan.repository.items[index];
            io.current = Some(format!("{}.{}", item.name, item.item_type));
            let result = publish_one(
                io,
                config,
                context,
                &plan.repository,
                index,
                base,
                workspace,
                folders,
                deployed,
                ids,
            )
            .await;
            match result {
                Ok(()) => {}
                Err(error) => {
                    if matches!(&error, WorkflowFailure::Operation(error) if matches!(error.kind, OperationErrorKind::Cancelled))
                    {
                        return Err(error);
                    }
                    if failure.is_none() {
                        failure_item = io.current.clone();
                        failure = Some(error.clone());
                    }
                    io.result.item_failures.push(error);
                }
            }
        }
        if *item_type == "Lakehouse" && context.enabled("enable_shortcut_publish") {
            for index in &plan.order {
                let item = &plan.repository.items[*index];
                if item.item_type == "Lakehouse" && ids.contains_key(&item.logical_id) {
                    lakehouse::shortcuts(
                        io,
                        config,
                        context,
                        &plan.repository,
                        item,
                        base,
                        workspace,
                        ids,
                    )
                    .await?;
                }
            }
        }
        if *item_type == "SemanticModel" {
            semanticmodel::bind(
                io,
                config,
                context,
                &plan.repository,
                &plan.order,
                base,
                ids,
            )
            .await?;
        }
        if let Some(error) = failure {
            io.current = failure_item;
            return Err(error);
        }
    }
    if !environments.is_empty() {
        environment::check(io, base, &environments, false).await?;
    }
    io.current = None;
    Ok(())
}

async fn publish_one<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    config: &DeploymentConfig,
    context: &EngineContext,
    repository: &DeploymentRepository,
    index: usize,
    base: &str,
    workspace: &str,
    folders: &HashMap<String, String>,
    deployed: &[DeployedItem],
    ids: &mut HashMap<String, String>,
) -> Result<(), WorkflowFailure> {
    let source = &repository.items[index];
    let processor = Processor::for_type(&source.item_type)?;
    let existing = ids.get(&source.logical_id).cloned().or_else(|| {
        deployed
            .iter()
            .find(|item| item.item_type == source.item_type && item.name == source.name)
            .map(|item| item.id.clone())
    });
    let mut item = source.clone();
    if !processor.shell_only() {
        preprocess(
            io, config, context, repository, &mut item, base, workspace, ids, false,
        )
        .await?;
    }
    let folder_id = folders
        .get(&item.folder_path)
        .map(String::as_str)
        .unwrap_or("");
    let body = mutation_body(
        processor,
        &item,
        existing.is_some(),
        folder_id,
        context.enabled("enable_purge_data"),
    )?;
    let (method, path) = match &existing {
        None => (ResourceMethod::Post, format!("{base}/items")),
        Some(id) if processor.shell_only() => (ResourceMethod::Patch, format!("{base}/items/{id}")),
        Some(id) => (
            ResourceMethod::Post,
            format!("{base}/items/{id}/updateDefinition?updateMetadata=True"),
        ),
    };
    let response = io.request(method, &path, Some(body)).await?;
    let id = existing.clone().map(Ok).unwrap_or_else(|| {
        response
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| valid_guid(id))
            .map(str::to_owned)
            .ok_or_else(|| io.error(OperationErrorKind::InvalidResponse))
    })?;
    ids.insert(item.logical_id.clone(), id.clone());
    io.result.published.push(PublishedItem {
        name: item.name.clone(),
        logical_id: item.logical_id.clone(),
        outcome: DeploymentOutcome {
            action: if existing.is_some() {
                DeploymentAction::Updated
            } else {
                DeploymentAction::Created
            },
            item_id: id.clone(),
        },
    });
    if existing.is_some()
        && !context.enabled("disable_workspace_folder_publish")
        && item.item_type != "OrgAppAudience"
        && deployed
            .iter()
            .find(|target| target.id == id)
            .is_some_and(|target| target.folder_id != folder_id)
    {
        io.request(
            ResourceMethod::Post,
            &format!("{base}/items/{id}/move"),
            Some(json!({"targetFolderId": folder_id})),
        )
        .await?;
    }
    match processor {
        Processor::VariableLibrary => activate_value_set(io, config, source, base, &id).await?,
        Processor::Lakehouse => lakehouse::readiness(io, base, &id).await?,
        Processor::Environment => {
            io.request_policy(
                ResourceMethod::Post,
                &format!("{base}/environments/{id}/staging/publish?beta=False"),
                None,
                CompletionPolicy::ReturnAccepted,
            )
            .await?;
        }
        _ => {}
    }
    Ok(())
}

async fn activate_value_set<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    config: &DeploymentConfig,
    item: &RepositoryItem,
    base: &str,
    id: &str,
) -> Result<(), WorkflowFailure> {
    let Some(file) = item
        .files
        .iter()
        .find(|file| file.name() == "settings.json")
    else {
        io.result.notices.push(format!(
            "settings.json file not found for item {}. Active value set not changed.",
            item.name
        ));
        return Ok(());
    };
    let settings: Value = serde_json::from_slice(&file.bytes)
        .map_err(|_| invalid("Invalid VariableLibrary settings.json"))?;
    let values = settings
        .get("valueSetsOrder")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("Missing VariableLibrary valueSetsOrder"))?;
    let value_set = if values
        .iter()
        .any(|value| value.as_str() == Some(&config.environment))
    {
        config.environment.as_str()
    } else {
        "Default value set"
    };
    io.request(
        ResourceMethod::Patch,
        &format!("{base}/VariableLibraries/{id}"),
        Some(json!({"properties": {"activeValueSetName": value_set}})),
    )
    .await?;
    Ok(())
}

async fn preprocess<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    config: &DeploymentConfig,
    context: &EngineContext,
    repository: &DeploymentRepository,
    item: &mut RepositoryItem,
    base: &str,
    workspace: &str,
    ids: &HashMap<String, String>,
    bulk: bool,
) -> Result<(), WorkflowFailure> {
    let processor = Processor::for_type(&item.item_type)?;
    let transforms = NotebookTransforms {
        logical_ids: repository
            .items
            .iter()
            .map(|source| {
                (
                    source.logical_id.clone(),
                    ids.get(&source.logical_id).cloned().unwrap_or_default(),
                )
            })
            .collect(),
        literals: Vec::new(),
    };
    let snapshot = item.clone();
    let mut budget = TransformBudget::new(TransformLimits::default(), &|| false);
    for file in &mut item.files {
        if !file.text || file.path.ends_with(".platform") || processor.excludes(&file.path) {
            continue;
        }
        let mut content = file.content()?.to_owned();
        match processor {
            Processor::Report => {
                content = report::preprocess(repository, &snapshot, &file.path, &content)?
            }
            Processor::Dataflow => {
                content = dataflow::preprocess(
                    config, context, repository, &snapshot, &file.path, &content,
                )?;
            }
            Processor::KqlQueryset | Processor::KqlDashboard => {
                content = kql::preprocess(io, repository, processor, &content, base, ids).await?
            }
            Processor::Environment => {
                content = environment::preprocess(
                    io, config, context, &snapshot, &file.path, &content, base,
                )
                .await?
            }
            _ => {}
        }
        if !bulk && processor != Processor::VariableLibrary {
            content = transforms
                .logical_content(&content, TransformLimits::default().max_bytes)
                .map_err(|_| {
                    invalid("Referenced logical ID is not yet deployed or output bound exceeded")
                })?;
        }
        content = super::parameters::apply(
            io,
            config,
            context,
            repository,
            &snapshot,
            &file.path,
            &content,
            base,
            workspace,
            ids,
            &mut budget,
        )
        .await?;
        if !bulk && processor != Processor::VariableLibrary {
            content =
                replace_workspace_ids(&content, workspace, TransformLimits::default().max_bytes)
                    .map_err(|_| invalid("Workspace replacement exceeded its output bound"))?;
            if item.item_type == "Reflex" {
                content = content.replace(
                    &format!("\\\"name\\\":\\\"workspaceId\\\",\\\"type\\\":\\\"string\\\",\\\"value\\\":\\\"{}", crate::transforms::DEFAULT_GUID),
                    &format!("\\\"name\\\":\\\"workspaceId\\\",\\\"type\\\":\\\"string\\\",\\\"value\\\":\\\"{workspace}"),
                );
            }
        }
        file.bytes = content.into_bytes();
    }
    Ok(())
}
