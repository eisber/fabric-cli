// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::v2::{
    Availability, Binding, ContractError, Effect, Generation, OperationDescriptor, Prerequisite,
    Registry, Verb,
};
use serde_json::json;

/// Adds public read bindings and one type-constrained facade to an existing registry.
///
/// Paths and owner resolution remain caller-owned. Registration is local
/// discovery, not deployment, authorization, a CLI grammar or a new MCP tool.
///
/// # Errors
/// Rejects duplicate/conflicting registration. Changes are atomic on error.
pub fn register_reads(registry: &mut Registry) -> Result<(), ContractError> {
    let mut candidate = registry.clone();
    candidate.alias("data-agent", "DataAgent")?;
    for (facet, verb, permission, preview) in [
        ("metadata", Verb::Get, "item-read", false),
        ("definition", Verb::Get, "item-read-write", false),
        ("settings-staging", Verb::Get, "configuration-read", true),
        ("settings-published", Verb::Get, "configuration-read", true),
        (
            "datasources-staging",
            Verb::List,
            "configuration-read",
            true,
        ),
        (
            "datasources-published",
            Verb::List,
            "configuration-read",
            true,
        ),
    ] {
        let id = format!("dataagent-{facet}-read");
        let input = if facet == "definition" {
            json!({
                "type":"object", "additionalProperties":false,
                "properties":{"nativeFormat":{"enum":["JSON"]}}
            })
        } else if verb == Verb::List {
            json!({
                "type":"object", "additionalProperties":false,
                "properties":{"continuationToken":{"type":"string","minLength":1}}
            })
        } else {
            json!({"type":"object","additionalProperties":false})
        };
        let output = if facet == "metadata" {
            json!({
                "type":"object", "required":["id","workspaceId","type","displayName"],
                "properties":{
                    "id":{"type":"string","format":"uuid"},
                    "workspaceId":{"type":"string","format":"uuid"},
                    "type":{"const":"DataAgent"}, "displayName":{"type":"string"}
                }
            })
        } else if facet == "definition" {
            json!({
                "type":"object","required":["definition"],
                "properties":{"definition":{
                    "type":"object","required":["parts"],
                    "properties":{"format":{"type":"string"},"parts":{
                        "type":"array","items":{
                            "type":"object","required":["path","payload","payloadType"],
                            "properties":{"path":{"type":"string"},"payload":{"type":"string"},
                                "payloadType":{"type":"string"}}
                        }
                    }}
                }}
            })
        } else if verb == Verb::List {
            json!({
                "type":"object","required":["value"],
                "properties":{"value":{"type":"array","items":{
                    "type":"object","required":["id","displayName","type"],
                    "properties":{"id":{"type":"string","format":"uuid"},
                        "displayName":{"type":"string"},"type":{"type":"string"}}
                }},"continuationToken":{"type":["string","null"]},
                    "continuationUri":{"type":["string","null"]}}
            })
        } else {
            json!({
                "type":"object","required":["aiInstructions"],
                "properties":{"aiInstructions":{"type":"string"}}
            })
        };
        candidate.register(OperationDescriptor::new(
            &id,
            verb,
            input,
            output,
            vec![Effect::RemoteRead],
        )?)?;
        let mut requirements = vec![Prerequisite {
            id: permission.into(),
            description: "The service must authorize the caller on the resolved DataAgent.".into(),
        }];
        if preview {
            requirements.push(Prerequisite {
                id: "dataagent-configuration-preview".into(),
                description: "The selected host must support the public configuration Preview."
                    .into(),
            });
        }
        candidate.bind(Binding::new(
            "DataAgent",
            facet,
            "json",
            Generation::Any,
            &id,
            Availability::Conditional(requirements),
        )?)?;
    }
    *registry = candidate;
    Ok(())
}
