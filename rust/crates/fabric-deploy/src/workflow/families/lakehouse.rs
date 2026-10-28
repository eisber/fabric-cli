// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use fancy_regex::Regex;

pub(super) async fn readiness<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    base: &str,
    id: &str,
) -> Result<(), WorkflowFailure> {
    let mut attempt = 1u32;
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
            _ => {
                io.wait(source_delay(attempt, 30)).await?;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

pub(super) async fn shortcuts<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    publication: Publication<'_>,
    item: &RepositoryItem,
    ids: &HashMap<String, String>,
) -> Result<(), WorkflowFailure> {
    let Publication {
        config,
        context,
        plan,
        base,
        workspace,
        ..
    } = publication;
    let repository = &plan.repository;
    let id = ids
        .get(&identity_key(item))
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
            publication,
            item,
            &file.path,
            file.content()?,
            ids,
            &mut budget,
        )
        .await?;
        let content = super::super::text::logical(&content, repository, ids, context)?;
        let content = super::super::text::workspace(&content, &item.item_type, workspace, context)?;
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
            let segments: Vec<_> = base
                .trim_start_matches('/')
                .split('/')
                .chain(["items", id.as_str(), "shortcuts"])
                .chain(key.split('/'))
                .collect();
            io.request_segments(ResourceMethod::Delete, &segments)
                .await?;
        }
    }
    for mut shortcut in shortcuts {
        if shortcut
            .pointer("/target/oneLake/itemId")
            .and_then(Value::as_str)
            == Some(context.default_guid())
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
