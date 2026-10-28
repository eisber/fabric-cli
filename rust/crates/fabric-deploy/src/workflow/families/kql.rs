// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use fabric_core_json::{Format, Indent, encode};

pub(super) async fn preprocess<E: OperationExecutor>(
    io: &mut ServiceIo<'_, E>,
    repository: &DeploymentRepository,
    processor: Processor,
    content: &str,
    base: &str,
    ids: &HashMap<String, String>,
) -> Result<String, WorkflowFailure> {
    let mut definition: Value =
        serde_json::from_str(content).map_err(|_| invalid("Invalid KQL definition JSON"))?;
    let path = if processor == Processor::KqlQueryset {
        "/queryset/dataSources"
    } else {
        "/dataSources"
    };
    let Some(sources) = definition.pointer_mut(path).and_then(Value::as_array_mut) else {
        if processor == Processor::KqlQueryset {
            return Ok(content.into());
        }
        return Err(invalid("Missing KQL Dashboard dataSources").into());
    };
    if sources.is_empty() && processor == Processor::KqlQueryset {
        return Ok(content.into());
    }
    let deployed = if processor == Processor::KqlDashboard {
        Some(deployed_items(io.list(&format!("{base}/items")).await?)?)
    } else {
        None
    };
    for source in sources {
        if source.is_null() || source.as_object().is_some_and(|object| object.is_empty()) {
            return Err(invalid("No data sources found in KQL Dashboard").into());
        }
        if source.get("clusterUri").and_then(Value::as_str) != Some("") {
            continue;
        }
        let id = if processor == Processor::KqlQueryset {
            let logical = source
                .get("databaseItemId")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("Missing KQL databaseItemId"))?;
            let database = repository
                .items
                .iter()
                .find(|item| item.item_type == "KQLDatabase" && item.logical_id == logical)
                .ok_or_else(|| invalid("KQL database source not found in repository"))?;
            ids.get(&database.logical_id)
                .cloned()
                .ok_or_else(|| invalid("KQL database is not yet deployed"))?
        } else {
            let name = source
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("Missing KQL database name"))?;
            deployed
                .as_ref()
                .into_iter()
                .flatten()
                .find(|item| item.item_type == "KQLDatabase" && item.name == name)
                .map(|item| item.id.clone())
                .ok_or_else(|| invalid("KQL database source is not deployed"))?
        };
        let metadata = io
            .request(
                ResourceMethod::Get,
                &format!("{base}/kqlDatabases/{id}"),
                None,
            )
            .await?;
        let uri = metadata
            .pointer("/properties/queryServiceUri")
            .and_then(Value::as_str)
            .filter(|uri| !uri.is_empty())
            .ok_or_else(|| invalid("KQL database cluster URI was not found"))?;
        source["clusterUri"] = uri.into();
    }
    Ok(encode(&definition, Format::Pretty(Indent::Two)))
}
