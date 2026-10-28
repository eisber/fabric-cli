// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use fancy_regex::Regex;

pub(super) async fn readiness<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    base: &str,
    id: &str,
) -> Result<(), WorkflowFailure> {
    loop {
        let response = io
            .request(
                ResourceMethod::Get,
                &format!("{base}/lakehouses/{id}"),
                None,
            )
            .await?;
        match response
            .pointer("/properties/sqlEndpointProperties/provisioningStatus")
            .and_then(Value::as_str)
        {
            Some("Success") => return Ok(()),
            Some("Failed") => {
                return Err(io
                    .error(OperationErrorKind::OperationFailed {
                        status: "Failed".into(),
                        error: None,
                    })
                    .into());
            }
            _ => io.wait(30).await?,
        }
    }
}

pub(super) async fn shortcuts<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    config: &DeploymentConfig,
    context: &EngineContext,
    repository: &DeploymentRepository,
    item: &RepositoryItem,
    base: &str,
    workspace: &str,
    ids: &HashMap<String, String>,
) -> Result<(), WorkflowFailure> {
    let id = ids
        .get(&item.logical_id)
        .ok_or_else(|| invalid("Lakehouse is not deployed"))?;
    let deployed = io.list(&format!("{base}/items/{id}/shortcuts")).await?;
    let mut shortcuts = if let Some(file) = item
        .files
        .iter()
        .find(|file| file.name() == "shortcuts.metadata.json")
    {
        let mut budget = TransformBudget::new(TransformLimits::default(), &|| false);
        let content = super::super::parameters::apply(
            io,
            config,
            context,
            repository,
            item,
            &file.path,
            file.content()?,
            base,
            workspace,
            ids,
            &mut budget,
        )
        .await?;
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
        let content = transforms
            .apply(&content, workspace, TransformLimits::default().max_bytes)
            .map_err(|_| invalid("Shortcut logical/workspace reference could not be resolved"))?;
        let document: Value = serde_json::from_str(&content)
            .map_err(|_| invalid("Invalid shortcuts.metadata.json"))?;
        if document.is_null() {
            Vec::new()
        } else {
            document
                .as_array()
                .cloned()
                .ok_or_else(|| invalid("Shortcut metadata must be a list"))?
        }
    } else {
        Vec::new()
    };
    if let Some(pattern) = config
        .publish
        .get("shortcut_exclude_regex")
        .and_then(Value::as_str)
    {
        let regex = Regex::new(pattern).map_err(|_| invalid("Invalid shortcut exclusion regex"))?;
        let mut selected = Vec::new();
        for shortcut in shortcuts {
            if let Some(name) = shortcut.get("name").and_then(Value::as_str) {
                let excluded = regex
                    .find(name)
                    .map_err(|_| invalid("Shortcut regex exceeded its matching bound"))?
                    .is_some_and(|matched| matched.start() == 0);
                if !excluded {
                    selected.push(shortcut);
                }
            }
        }
        shortcuts = selected;
    }
    if shortcuts.is_empty() {
        return Ok(());
    }
    let key = |shortcut: &Value| -> Result<String, ConfigurationError> {
        let path = shortcut
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Shortcut path is required"))?;
        let name = shortcut
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Shortcut name is required"))?;
        if name.is_empty()
            || name.contains(['/', '\\', '\0'])
            || path.split('/').any(|segment| matches!(segment, "." | ".."))
        {
            return Err(invalid("Unsafe shortcut path or name"));
        }
        Ok(format!("{path}/{name}"))
    };
    let selected: HashSet<_> = shortcuts.iter().map(key).collect::<Result<_, _>>()?;
    for shortcut in deployed {
        let key = key(&shortcut)?;
        if !selected.contains(&key) {
            io.request(
                ResourceMethod::Delete,
                &format!("{base}/items/{id}/shortcuts/{key}"),
                None,
            )
            .await?;
        }
    }
    for mut shortcut in shortcuts {
        if shortcut
            .pointer("/target/oneLake/itemId")
            .and_then(Value::as_str)
            == Some(crate::transforms::DEFAULT_GUID)
        {
            shortcut["target"]["oneLake"]["itemId"] = id.clone().into();
        }
        if let Err(error) = io
            .request(
                ResourceMethod::Post,
                &format!("{base}/items/{id}/shortcuts?shortcutConflictPolicy=CreateOrOverwrite"),
                Some(shortcut),
            )
            .await
        {
            match error {
                WorkflowFailure::Operation(error)
                    if context.enabled("continue_on_shortcut_failure")
                        && !matches!(error.kind, OperationErrorKind::Cancelled) =>
                {
                    io.result.warnings.push(*error)
                }
                error => return Err(error),
            }
        }
    }
    Ok(())
}
