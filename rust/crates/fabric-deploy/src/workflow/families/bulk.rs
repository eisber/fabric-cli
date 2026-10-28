// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

pub(super) async fn publish<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    publication: Publication<'_>,
    ids: &mut HashMap<String, String>,
) -> Result<(), WorkflowFailure> {
    let Publication {
        config,
        context,
        plan,
        base,
        deployed,
        ..
    } = publication;
    let batches = batches(config, context, plan, deployed)?;
    let environment_names: Vec<_> = plan
        .order
        .iter()
        .map(|index| &plan.repository.items[*index])
        .filter(|item| item.item_type == "Environment")
        .map(|item| item.name.clone())
        .collect();
    if !environment_names.is_empty() {
        environment::check(io, base, &environment_names, true).await?;
    }
    let count = batches.len();
    for (stage, batch) in batches.into_iter().enumerate() {
        let mut parts = Vec::new();
        let mut item_options = Vec::new();
        for index in &batch {
            let mut item = plan.repository.items[*index].clone();
            io.current = Some(format!("{}.{}", item.name, item.item_type));
            let processor = Processor::for_type(&item.item_type)?;
            // Bulk source hooks only expose Environment's func_process_file.
            if processor == Processor::Environment {
                preprocess(io, publication, &mut item, ids, true).await?;
            } else {
                let snapshot = item.clone();
                let mut remaining = context.definition_limits.max_bytes.get();
                let mut budget = TransformBudget::new(
                    TransformLimits {
                        max_bytes: remaining.min(TransformLimits::default().max_bytes),
                        ..TransformLimits::default()
                    },
                    &|| false,
                );
                for file in &mut item.files {
                    if !file.text
                        || file.path.ends_with(".platform")
                        || crate::processors::excludes_with_context(
                            processor,
                            &item.item_type,
                            &file.path,
                            context,
                            true,
                        )?
                    {
                        remaining = remaining
                            .checked_sub(file.bytes.len())
                            .ok_or(ConfigurationError::Limit)?;
                        continue;
                    }
                    file.bytes = super::super::parameters::apply(
                        io,
                        publication,
                        &snapshot,
                        &file.path,
                        file.content()?,
                        ids,
                        &mut budget,
                    )
                    .await?
                    .into_bytes();
                    remaining = remaining
                        .checked_sub(file.bytes.len())
                        .ok_or(ConfigurationError::Limit)?;
                }
            }
            io.retain_prepared(&item)?;
            let directory = item
                .path
                .rsplit('/')
                .next()
                .ok_or_else(|| invalid("Missing item directory name"))?;
            let prefix = format!("{}/{directory}", item.folder_path);
            for file in &item.files {
                if crate::processors::excludes_with_context(
                    processor,
                    &item.item_type,
                    &file.path,
                    context,
                    true,
                )? {
                    continue;
                }
                parts.push(json!({"path": format!("{prefix}/{}", file.path), "payload": STANDARD.encode(&file.bytes), "payloadType": "InlineBase64"}));
            }
            if processor == Processor::SemanticModel && context.enabled("enable_purge_data") {
                item_options.push(
                    json!({"logicalId": item.logical_id, "options": {"allowPurgeData": true}}),
                );
            }
        }
        let mut options = json!({"allowPairingByName": true});
        if !item_options.is_empty() {
            options["itemOptionsByLogicalId"] = item_options.into();
        }
        let response = io
            .request(
                ResourceMethod::Post,
                &format!("{base}/items/bulkImportDefinitions"),
                Some(json!({
                    "definitionParts": parts, "options": options,
                })),
            )
            .await?;
        let details = response
            .get("importItemDefinitionsDetails")
            .and_then(Value::as_array)
            .ok_or_else(|| io.error(OperationErrorKind::InvalidResponse))?;
        for index in &batch {
            let item = &plan.repository.items[*index];
            io.current = Some(format!("{}.{}", item.name, item.item_type));
            let detail = details
                .iter()
                .find(|detail| {
                    detail.get("itemType").and_then(Value::as_str) == Some(&item.item_type)
                        && detail.get("itemDisplayName").and_then(Value::as_str) == Some(&item.name)
                })
                .ok_or_else(|| io.error(OperationErrorKind::InvalidResponse))?;
            let id = detail
                .get("itemId")
                .and_then(Value::as_str)
                .filter(|id| valid_guid(id))
                .ok_or_else(|| io.error(OperationErrorKind::InvalidResponse))?;
            ids.insert(identity_key(item), id.into());
            if let Some(mut response) = io.last_response.clone() {
                response["body"] = detail.clone();
                io.collect("publish", &item.item_type, &item.name, Some(response));
            }
            io.result.published.push(PublishedItem {
                name: item.name.clone(),
                logical_id: item.logical_id.clone(),
                outcome: DeploymentOutcome {
                    action: if detail.get("operationType").and_then(Value::as_str) == Some("Create")
                    {
                        DeploymentAction::Created
                    } else {
                        DeploymentAction::Updated
                    },
                    item_id: id.into(),
                },
            });
        }
        if stage + 1 < count {
            for index in &batch {
                let item = &plan.repository.items[*index];
                for attribute in required_attributes(context, config, item) {
                    wait_attribute(io, item, &attribute, base, ids, context).await?;
                }
            }
            let _ = deployed_items(io.list(&format!("{base}/items")).await?)?;
        }
    }
    for index in &plan.order {
        let item = &plan.repository.items[*index];
        let prepared = io
            .processed_items
            .remove(&identity_key(item))
            .unwrap_or_else(|| item.clone());
        let id = ids
            .get(&identity_key(item))
            .ok_or_else(|| invalid("Bulk result item ID was not resolved"))?;
        match item.item_type.as_str() {
            "VariableLibrary" => activate_value_set(io, config, &prepared, base, id).await?,
            "Environment" => {
                io.request_policy(
                    ResourceMethod::Post,
                    &format!("{base}/environments/{id}/staging/publish?beta=False"),
                    None,
                    CompletionPolicy::ReturnAccepted,
                )
                .await?;
            }
            "Lakehouse" if context.enabled("enable_shortcut_publish") => {
                lakehouse::shortcuts(io, publication, &prepared, ids).await?
            }
            _ => {}
        }
    }
    semanticmodel::bind(io, publication, ids).await?;
    if !environment_names.is_empty() {
        environment::check(io, base, &environment_names, false).await?;
    }
    Ok(())
}

fn required_attributes(
    context: &EngineContext,
    config: &DeploymentConfig,
    item: &RepositoryItem,
) -> Vec<String> {
    let mut result = Vec::new();
    for section in ["find_replace", "key_value_replace"] {
        for rule in context
            .parameter_document
            .as_ref()
            .and_then(|document| document.get(section))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some((kind, name, attribute)) = super::super::parameters::environment_value(
                rule.get("replace_value"),
                &config.environment,
            )
            .and_then(Value::as_str)
            .and_then(super::super::parameters::item_variable)
                && kind == item.item_type
                && name == item.name
                && context.listed(
                    "ASYNC_PROVISIONED_ATTRIBUTES",
                    &[
                        "sqlendpoint",
                        "sqlendpointfqdn",
                        "sqlendpointid",
                        "queryserviceuri",
                    ],
                    attribute,
                )
                && !result.iter().any(|value| value == attribute)
            {
                result.push(attribute.into());
            }
        }
    }
    result.sort();
    result
}

async fn wait_attribute<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    item: &RepositoryItem,
    attribute: &str,
    base: &str,
    ids: &HashMap<String, String>,
    context: &EngineContext,
) -> Result<(), WorkflowFailure> {
    let mut attempt = 1u32;
    let Some(path) = context.attribute_pointer(&item.item_type, attribute) else {
        return Ok(());
    };
    let id = ids
        .get(&identity_key(item))
        .ok_or_else(|| invalid("Provisioned item ID is missing"))?;
    loop {
        let metadata = io
            .request(
                ResourceMethod::Get,
                &format!("{base}/{}s/{id}", item.item_type.to_lowercase()),
                None,
            )
            .await?;
        if metadata
            .pointer(&path)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
        {
            return Ok(());
        }
        if metadata
            .pointer("/properties/sqlEndpointProperties/provisioningStatus")
            .and_then(Value::as_str)
            == Some("Failed")
        {
            return Err(io
                .error(OperationErrorKind::OperationFailed {
                    status: "Failed".into(),
                    error: None,
                })
                .into());
        }
        io.wait(source_delay(attempt, 30)).await?;
        attempt = attempt.saturating_add(1);
    }
}

fn batches(
    config: &DeploymentConfig,
    context: &EngineContext,
    plan: &DeploymentPlan,
    deployed: &[DeployedItem],
) -> Result<Vec<Vec<usize>>, ConfigurationError> {
    if plan
        .repository
        .items
        .iter()
        .any(|item| item.logical_id == context.default_guid())
    {
        return Err(invalid(
            "Bulk import requires unique, non-placeholder logical IDs",
        ));
    }
    if plan.order.is_empty() {
        return Ok(Vec::new());
    }
    let mut edges = Vec::new();
    for section in ["find_replace", "key_value_replace"] {
        for rule in context
            .parameter_document
            .as_ref()
            .and_then(|document| document.get(section))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some((kind, name, _)) = super::super::parameters::environment_value(
                rule.get("replace_value"),
                &config.environment,
            )
            .and_then(Value::as_str)
            .and_then(super::super::parameters::item_variable) else {
                continue;
            };
            if deployed
                .iter()
                .any(|item| item.item_type == kind && item.name == name)
            {
                continue;
            }
            let dependency = plan
                .repository
                .items
                .iter()
                .position(|item| item.item_type == kind && item.name == name);
            for index in &plan.order {
                if Some(*index) == dependency {
                    continue;
                }
                let item = &plan.repository.items[*index];
                if !item
                    .files
                    .iter()
                    .any(|file| super::super::parameters::rule_applies(rule, item, &file.path))
                {
                    continue;
                }
                let dependency = dependency
                    .filter(|dependency| plan.order.contains(dependency))
                    .ok_or_else(|| {
                        invalid(
                            "Required new dynamic-variable item is excluded from bulk deployment",
                        )
                    })?;
                if !edges.contains(&(*index, dependency)) {
                    edges.push((*index, dependency));
                }
            }
        }
    }
    let mut units: HashMap<_, _> = plan.order.iter().map(|index| (*index, *index)).collect();
    if !edges.is_empty() {
        for left in &plan.order {
            let item = &plan.repository.items[*left];
            for right in &plan.order {
                if left == right {
                    continue;
                }
                let logical = &plan.repository.items[*right].logical_id;
                if item.files.iter().any(|file| {
                    file.text
                        && file.name() != ".platform"
                        && file.content().is_ok_and(|content| {
                            content.to_lowercase().contains(&logical.to_lowercase())
                        })
                }) {
                    let a = representative(&units, *left);
                    let b = representative(&units, *right);
                    let representative = if plan.order.iter().position(|index| *index == a)
                        < plan.order.iter().position(|index| *index == b)
                    {
                        a
                    } else {
                        b
                    };
                    units.insert(a, representative);
                    units.insert(b, representative);
                }
            }
        }
    }
    for edge in &mut edges {
        edge.0 = representative(&units, edge.0);
        edge.1 = representative(&units, edge.1);
        if edge.0 == edge.1 {
            return Err(invalid(
                "Logical-ID same-batch requirement conflicts with dynamic-variable sequential dependency",
            ));
        }
    }
    let mut remaining = plan.order.clone();
    let mut batches = Vec::new();
    let maximum = context
        .constants
        .get("BULK_ITEM_COUNT_LIMIT")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1000);
    while !remaining.is_empty() {
        let stage: Vec<_> = remaining
            .iter()
            .copied()
            .filter(|index| {
                !edges.iter().any(|(dependent, prerequisite)| {
                    *dependent == representative(&units, *index)
                        && remaining
                            .iter()
                            .any(|index| representative(&units, *index) == *prerequisite)
                })
            })
            .collect();
        if stage.is_empty() {
            return Err(invalid("Circular dynamic-variable dependency"));
        }
        if stage.len() > maximum {
            return Err(invalid(
                "Bulk publish batch exceeds the API item count limit",
            ));
        }
        remaining.retain(|index| !stage.contains(index));
        batches.push(stage);
    }
    Ok(batches)
}

fn representative(units: &HashMap<usize, usize>, mut index: usize) -> usize {
    while units[&index] != index {
        index = units[&index];
    }
    index
}
