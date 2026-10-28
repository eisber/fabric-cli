// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use fancy_regex::Regex;

fn dependency(
    config: &DeploymentConfig,
    context: &EngineContext,
    repository: &DeploymentRepository,
    item: &RepositoryItem,
    path: &str,
    content: &str,
) -> Result<Option<(usize, String, String)>, ConfigurationError> {
    if !path.ends_with(".pq") {
        return Ok(None);
    }
    let pattern = Regex::new(r#"(PowerPlatform\.Dataflows)(?:\(\[\]\))?[\s\S]*?workspaceId\s*=\s*"(.*?)"[\s\S]*?dataflowId\s*=\s*"(.*?)""#)
        .map_err(|_| invalid("Invalid source Dataflow reference pattern"))?;
    let Some(captures) = pattern
        .captures(content)
        .map_err(|_| invalid("Dataflow reference pattern exceeded its bound"))?
    else {
        return Ok(None);
    };
    let workspace = captures
        .get(2)
        .map(|value| value.as_str())
        .filter(|value| valid_guid(value))
        .ok_or_else(|| invalid("Invalid Dataflow source workspace ID"))?;
    let id = captures
        .get(3)
        .map(|value| value.as_str())
        .filter(|value| valid_guid(value))
        .ok_or_else(|| invalid("Invalid Dataflow source ID"))?;
    for rule in context
        .parameter_document
        .as_ref()
        .and_then(|document| document.get("find_replace"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if rule.get("find_value").and_then(Value::as_str) != Some(id)
            || !super::super::parameters::rule_applies(rule, item, path)
        {
            continue;
        }
        // The source dependency helper does not expand _ALL_ at this boundary.
        let Some(variable) = rule
            .get("replace_value")
            .and_then(|value| value.get(&config.environment))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some((kind, name, _)) = super::super::parameters::item_variable(variable) else {
            continue;
        };
        if kind != "Dataflow" {
            continue;
        }
        if let Some(index) = repository
            .items
            .iter()
            .position(|item| item.item_type == "Dataflow" && item.name == name)
        {
            return Ok(Some((index, workspace.into(), id.into())));
        }
    }
    Ok(None)
}

pub(super) fn order(
    config: &DeploymentConfig,
    context: &EngineContext,
    repository: &DeploymentRepository,
    indices: &[usize],
) -> Result<Vec<usize>, ConfigurationError> {
    let mut dependencies = Vec::new();
    for index in indices {
        let item = &repository.items[*index];
        for file in &item.files {
            if file.text
                && let Some((dependency, _, _)) = dependency(
                    config,
                    context,
                    repository,
                    item,
                    &file.path,
                    file.content()?,
                )?
            {
                dependencies.push((*index, dependency));
            }
        }
    }
    fn visit(
        index: usize,
        dependencies: &[(usize, usize)],
        visited: &mut HashSet<usize>,
        temporary: &mut HashSet<usize>,
        result: &mut Vec<usize>,
    ) -> Result<(), ConfigurationError> {
        if visited.contains(&index) {
            return Ok(());
        }
        if !temporary.insert(index) {
            return Err(invalid("Circular Dataflow dependency"));
        }
        for (_, prerequisite) in dependencies
            .iter()
            .filter(|(dependent, _)| *dependent == index)
        {
            visit(*prerequisite, dependencies, visited, temporary, result)?;
        }
        temporary.remove(&index);
        visited.insert(index);
        result.push(index);
        Ok(())
    }
    let mut result = Vec::new();
    let mut visited = HashSet::new();
    let mut temporary = HashSet::new();
    for (index, _) in &dependencies {
        visit(
            *index,
            &dependencies,
            &mut visited,
            &mut temporary,
            &mut result,
        )?;
    }
    for index in indices {
        if !visited.contains(index) {
            result.push(*index);
        }
    }
    result.retain(|index| indices.contains(index));
    Ok(result)
}

pub(super) fn preprocess(
    config: &DeploymentConfig,
    context: &EngineContext,
    repository: &DeploymentRepository,
    item: &RepositoryItem,
    path: &str,
    content: &str,
) -> Result<String, ConfigurationError> {
    let Some((index, workspace, id)) =
        dependency(config, context, repository, item, path, content)?
    else {
        return Ok(content.into());
    };
    Ok(content
        .replace(&id, &repository.items[index].logical_id)
        .replace(&workspace, crate::transforms::DEFAULT_GUID))
}
