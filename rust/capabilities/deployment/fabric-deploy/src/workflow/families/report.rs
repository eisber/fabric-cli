// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use fabric_core_json::{Format, Indent, encode};

pub(super) fn preprocess(
    repository: &DeploymentRepository,
    item: &RepositoryItem,
    path: &str,
    content: &str,
) -> Result<String, ConfigurationError> {
    if path.rsplit('/').next() != Some("definition.pbir") {
        return Ok(content.into());
    }
    let mut definition: Value =
        serde_json::from_str(content).map_err(|_| invalid("Invalid report definition.pbir"))?;
    let Some(relative) = definition
        .pointer("/datasetReference/byPath/path")
        .and_then(Value::as_str)
    else {
        return Ok(content.into());
    };
    let mut components: Vec<_> = item.path.split('/').map(str::to_owned).collect();
    for component in relative.replace('\\', "/").split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components
                    .pop()
                    .ok_or_else(|| invalid("Report semantic-model path escapes repository"))?;
            }
            component if component.contains(':') => {
                return Err(invalid(
                    "Report semantic-model path is not repository-relative",
                ));
            }
            component => components.push(component.into()),
        }
    }
    let target = components.join("/");
    let model = repository
        .items
        .iter()
        .find(|source| source.item_type == "SemanticModel" && source.path == target)
        .ok_or_else(|| {
            invalid("Semantic model not found in repository for report byPath reference")
        })?;
    definition["$schema"] = "https://developer.microsoft.com/json-schemas/fabric/item/report/definitionProperties/1.0.0/schema.json".into();
    definition["datasetReference"] = json!({"byConnection": {
        "connectionString": null, "pbiServiceModelId": null,
        "pbiModelVirtualServerName": "sobe_wowvirtualserver",
        "pbiModelDatabaseName": model.logical_id, "name": "EntityDataSource",
        "connectionType": "pbiServiceXmlaStyleLive",
    }});
    Ok(encode(&definition, Format::Pretty(Indent::Four)))
}
