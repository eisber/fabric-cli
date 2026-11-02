// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::parameter_transforms::TransformLimits;
use fancy_regex::Regex;

fn replace(content: &str, find: &str, replacement: &str) -> Result<String, ConfigurationError> {
    let count = content.matches(find).count();
    let length = count
        .checked_mul(find.len())
        .and_then(|removed| content.len().checked_sub(removed))
        .and_then(|length| {
            count
                .checked_mul(replacement.len())
                .and_then(|added| length.checked_add(added))
        })
        .filter(|length| *length <= TransformLimits::default().max_bytes)
        .ok_or(ConfigurationError::Limit)?;
    let result = content.replace(find, replacement);
    debug_assert_eq!(result.len(), length);
    Ok(result)
}

pub(super) fn logical(
    content: &str,
    repository: &DeploymentRepository,
    ids: &HashMap<String, String>,
    context: &EngineContext,
) -> Result<String, ConfigurationError> {
    let mut result = content.to_owned();
    for item in &repository.items {
        if item.logical_id == context.default_guid() || !result.contains(&item.logical_id) {
            continue;
        }
        let id = ids
            .get(&identity_key(item))
            .ok_or_else(|| invalid("Referenced logical item is not yet deployed"))?;
        result = replace(&result, &item.logical_id, id)?;
    }
    Ok(result)
}

pub(super) fn workspace(
    content: &str,
    item_type: &str,
    workspace: &str,
    context: &EngineContext,
) -> Result<String, ConfigurationError> {
    let standard = context.text_constant(
        "WORKSPACE_ID_REFERENCE_REGEX",
        r#""?(default_lakehouse_workspace_id|workspaceId|workspace)"?\s*[:=]\s*"(.*?)""#,
    );
    let mut result = substitute(content, standard, |captures| {
        let whole = captures
            .get(0)
            .ok_or_else(|| invalid("Missing workspace-reference match"))?
            .as_str();
        if captures.len() <= 2 {
            return Err(invalid(
                "Workspace-reference pattern must retain capture group 2",
            ));
        }
        let Some(value) = captures.get(2).map(|value| value.as_str()) else {
            return Ok(whole.into());
        };
        if value == context.default_guid() {
            replace(whole, context.default_guid(), workspace)
        } else {
            Ok(whole.into())
        }
    })?;
    if item_type == "Reflex" {
        let reflex = context.text_constant("REFLEX_WORKSPACE_ID_REFERENCE_REGEX",
            r#"(\\"name\\":\\"workspaceId\\",\\"type\\":\\"string\\",\\"value\\":\\")00000000-0000-0000-0000-000000000000(\\")"#);
        result = substitute(&result, reflex, |captures| {
            if captures.len() <= 2 {
                return Err(invalid(
                    "Reflex workspace pattern must retain capture groups 1 and 2",
                ));
            }
            let prefix = captures.get(1).map(|value| value.as_str()).unwrap_or("");
            let suffix = captures.get(2).map(|value| value.as_str()).unwrap_or("");
            Ok(format!("{prefix}{workspace}{suffix}"))
        })?;
    }
    Ok(result)
}

fn substitute(
    content: &str,
    pattern: &str,
    mut replacement: impl FnMut(&fancy_regex::Captures<'_>) -> Result<String, ConfigurationError>,
) -> Result<String, ConfigurationError> {
    if pattern.len() > TransformLimits::default().max_pattern_bytes {
        return Err(ConfigurationError::Limit);
    }
    let regex = Regex::new(pattern).map_err(|_| invalid("Invalid runtime reference regex"))?;
    let mut result = String::new();
    let mut previous = 0;
    for (count, captures) in regex.captures_iter(content).enumerate() {
        if count >= TransformLimits::default().max_matches {
            return Err(ConfigurationError::Limit);
        }
        let captures =
            captures.map_err(|_| invalid("Runtime reference regex exceeded its matching bound"))?;
        let whole = captures
            .get(0)
            .ok_or_else(|| invalid("Missing reference match"))?;
        result.push_str(&content[previous..whole.start()]);
        result.push_str(&replacement(&captures)?);
        if result.len() > TransformLimits::default().max_bytes {
            return Err(ConfigurationError::Limit);
        }
        previous = whole.end();
    }
    result.push_str(&content[previous..]);
    if result.len() > TransformLimits::default().max_bytes {
        return Err(ConfigurationError::Limit);
    }
    Ok(result)
}
