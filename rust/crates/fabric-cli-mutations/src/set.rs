// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    CacheEffect, MutationError, MutationHost, MutationResult, NoticeKind, check_item_support,
    message, require_id, supported_element, workspace_id,
};
use fabric_cli_context::{Element, NodeKind};
use fabric_sdk_folders::mutations::FolderMutations;
use fabric_sdk_items::{ItemCollection, mutations::ItemMutations};
use fabric_sdk_workspaces::mutations::{ResourceOutcome, WorkspaceMutations};
use jpx_core::ast::Ast;
use serde_json::{Value, json};
use std::sync::Arc;

impl MutationHost<'_> {
    /// Execute metadata `set`, preserving source prompt/read/request ordering.
    ///
    /// # Errors
    /// Invalid/simple-path queries, blocked metadata and read/mutation failures
    /// remain explicit. Item-definition changes use the deployment-owned seam
    /// and are not silently treated as metadata updates.
    pub async fn set(
        &mut self,
        element: &Arc<Element>,
        query: &str,
        input: &str,
        force: bool,
    ) -> Result<MutationResult, MutationError> {
        supported_element(element)?;
        check_item_support(element, true)?;
        require_id(element)?;
        validate_query(element, query)?;
        if query.starts_with("definition") && matches!(element.kind(), NodeKind::Item(_)) {
            return Err(MutationError::new(
                "NotSupported",
                "Item definition updates require the deployment-owned definition handler",
            ));
        }
        let before_prompt = if matches!(element.kind(), NodeKind::Workspace(_)) {
            Some(self.metadata.read(element).await?)
        } else {
            None
        };
        self.ui.notice(
            NoticeKind::Warning,
            "Modifying properties may lead to unintended consequences",
        );
        if !force && !self.ui.confirm("Are you sure?").await {
            return Ok(MutationResult {
                declined: true,
                ..MutationResult::default()
            });
        }
        let metadata = match before_prompt {
            Some(metadata) => metadata,
            None => self.metadata.read(element).await?,
        };
        let mut payload = update_fabric_element(metadata, query, input)?;
        self.ui.notice(
            NoticeKind::Progress,
            &format!("Setting new property for '{}'...", element.name()),
        );
        let outcome = match element.kind() {
            NodeKind::Workspace(_) => {
                let spark = query.starts_with("sparkSettings");
                if spark {
                    payload = payload
                        .get("sparkSettings")
                        .cloned()
                        .ok_or_else(invalid_query)?;
                }
                WorkspaceMutations::update(
                    self.operations,
                    require_id(element)?,
                    payload.clone(),
                    spark,
                )
                .await?
            }
            NodeKind::Folder => {
                FolderMutations::update(
                    self.operations,
                    workspace_id(element)?,
                    require_id(element)?,
                    payload.clone(),
                )
                .await?
            }
            NodeKind::Item(item_type) => {
                let collection =
                    ItemCollection::from_item_type(item_type.as_str()).ok_or_else(|| {
                        MutationError::new("NotSupported", "Unmapped item collection")
                    })?;
                ItemMutations::update(
                    self.operations,
                    workspace_id(element)?,
                    collection,
                    require_id(element)?,
                    payload.clone(),
                )
                .await?
            }
            NodeKind::Tenant => unreachable!("validated above"),
        };
        let source_prints = matches!(element.kind(), NodeKind::Item(_))
            || matches!(outcome, ResourceOutcome::Completed { status: 200, .. });
        let mut result = MutationResult {
            outcome: Some(outcome),
            ..MutationResult::default()
        };
        if source_prints {
            result
                .outputs
                .push(message(format!("{} updated", element.kind().as_str())));
            if let Some(name) = payload.get("displayName") {
                result.effects.push(CacheEffect::Renamed {
                    target: Arc::clone(element),
                    name: name.clone(),
                });
            }
        }
        Ok(result)
    }
}

fn validate_query(element: &Element, query: &str) -> Result<(), MutationError> {
    let ast = jpx_core::parse(query).map_err(|_| invalid_query())?;
    path(&ast)?;
    if matches!(element.kind(), NodeKind::Item(_)) {
        if !["displayName", "description", "properties", "definition"]
            .iter()
            .any(|key| query == *key || query.starts_with(&format!("{key}.")))
        {
            return Err(MutationError::new(
                "InvalidQuery",
                format!(
                    "Invalid query '{query}'\n\nAvailable queries:\n  displayName\n  description\n  properties\n  definition"
                ),
            ));
        }
    } else {
        let mut blocked = vec!["id", "type", "workspaceId", "folderId"];
        if matches!(element.kind(), NodeKind::Workspace(_)) {
            blocked.extend([
                "capacityId",
                "capacityRegion",
                "apiEndpoint",
                "domainId",
                "oneLakeEndpoints",
                "workspaceIdentity",
                "managedPrivateEndpoints",
                "roleAssignments",
            ]);
        }
        if blocked
            .iter()
            .any(|key| query == *key || query.starts_with(&format!("{key}.")))
        {
            return Err(MutationError::new(
                "InvalidQuery",
                format!("Query '{query}' is not supported for set"),
            ));
        }
    }
    Ok(())
}

enum Step {
    Field(String),
    Index(i32),
}

fn path(ast: &Ast) -> Result<Vec<Step>, MutationError> {
    match ast {
        Ast::Field { name, .. } => Ok(vec![Step::Field(name.clone())]),
        Ast::Index { idx, .. } => Ok(vec![Step::Index(*idx)]),
        Ast::Subexpr { lhs, rhs, .. } => {
            let mut steps = path(lhs)?;
            steps.extend(path(rhs)?);
            Ok(steps)
        }
        _ => Err(invalid_query()),
    }
}

fn index(value: i32, length: usize) -> Result<usize, MutationError> {
    let length = i64::try_from(length).map_err(|_| invalid_query())?;
    let value = i64::from(value);
    let value = if value < 0 { length + value } else { value };
    if value < 0 || value >= length {
        return Err(invalid_query());
    }
    usize::try_from(value).map_err(|_| invalid_query())
}

/// Translate the source's raw final-key replacement and top-level projection.
///
/// # Errors
/// Missing/scalar parents, invalid indices or non-simple paths fail explicitly.
pub fn update_fabric_element(
    mut metadata: Value,
    query: &str,
    input: &str,
) -> Result<Value, MutationError> {
    let parts: Vec<_> = query.trim_matches('.').split('.').collect();
    let (key, parents) = parts.split_last().ok_or_else(invalid_query)?;
    let mut parent = &mut metadata;
    if !parents.is_empty() {
        let ast = jpx_core::parse(&parents.join(".")).map_err(|_| invalid_query())?;
        for step in path(&ast)? {
            parent = match step {
                Step::Field(key) => parent.get_mut(&key).ok_or_else(invalid_query)?,
                Step::Index(value) => {
                    let values = parent.as_array_mut().ok_or_else(invalid_query)?;
                    let position = index(value, values.len())?;
                    &mut values[position]
                }
            };
        }
    }
    let input = serde_json::from_str(input).unwrap_or_else(|_| Value::String(input.into()));
    if let Some((key, position)) = key.split_once('[') {
        let position = position
            .strip_suffix(']')
            .and_then(|value| value.parse::<i32>().ok())
            .ok_or_else(invalid_query)?;
        let parent = if key.is_empty() {
            parent
        } else {
            parent.get_mut(key).ok_or_else(invalid_query)?
        };
        let values = parent.as_array_mut().ok_or_else(invalid_query)?;
        let position = index(position, values.len())?;
        values[position] = input;
    } else {
        parent
            .as_object_mut()
            .ok_or_else(invalid_query)?
            .insert((*key).into(), input);
    }
    let top = query.split('.').next().ok_or_else(invalid_query)?;
    Ok(match metadata.get(top) {
        Some(value) => {
            let mut result = serde_json::Map::new();
            result.insert(top.into(), value.clone());
            Value::Object(result)
        }
        None => json!({}),
    })
}

fn invalid_query() -> MutationError {
    MutationError::new("InvalidQuery", "Invalid set query")
}
