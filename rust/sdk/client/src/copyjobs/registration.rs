// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::v2::{
    Availability, Binding, ContractError, Effect, Generation, OperationDescriptor, Prerequisite,
    Registry, TargetKind, Verb,
};
use serde_json::json;

/// Adds only native CopyJob read bindings to the existing neutral registry.
///
/// Root and facade resolution return the same descriptor. Registration performs
/// no I/O, declares no execution/reset/transfer capability and does not establish
/// caller authorization. The common heads retain grammar, output and paging policy.
///
/// # Errors
/// Preserves duplicate/conflicting registration errors; never replaces bindings.
pub fn register(registry: &mut Registry) -> Result<(), ContractError> {
    let mut candidate = registry.clone();
    register_bindings(&mut candidate)?;
    *registry = candidate;
    Ok(())
}

fn register_bindings(registry: &mut Registry) -> Result<(), ContractError> {
    registry.alias("copy-job", "CopyJob")?;
    let ids = json!({
        "type":"object",
        "required":["workspaceId","itemId"],
        "properties":{
            "workspaceId":{"type":"string","format":"uuid"},
            "itemId":{"type":"string","format":"uuid"}
        },
        "additionalProperties":false
    });
    let metadata = json!({
        "type":"object",
        "required":["id","workspaceId","displayName","type"],
        "properties":{
            "id":{"type":"string","format":"uuid"},
            "workspaceId":{"type":"string","format":"uuid"},
            "displayName":{"type":"string"},
            "type":{"const":"CopyJob"}
        },
        "additionalProperties":true
    });
    for (id, facet, representation, verb, input, output, permission, source) in [
        (
            "fabric.copyjob.get-metadata.v1",
            "item",
            "metadata",
            Verb::Get,
            ids.clone(),
            metadata.clone(),
            "CopyJob item read permission; CopyJob.Read.All/ReadWrite.All or Item.Read.All/ReadWrite.All.",
            "https://learn.microsoft.com/en-us/rest/api/fabric/copyjob/items/get-copy-job",
        ),
        (
            "fabric.copyjob.get-definition.v1",
            "src",
            "definition",
            Verb::Get,
            json!({
                "type":"object",
                "required":["workspaceId","itemId"],
                "properties":{
                    "workspaceId":{"type":"string","format":"uuid"},
                    "itemId":{"type":"string","format":"uuid"},
                    "nativeFormat":{"type":"string","maxLength":128,"pattern":"^[A-Za-z0-9][A-Za-z0-9._-]*$"}
                },
                "additionalProperties":false
            }),
            json!({
                "type":"object",
                "description":"Completed exact native multipart envelope or accepted shared operation, never metadata fallback.",
                "oneOf":[
                    {"required":["definition"],"properties":{"definition":{
                        "type":"object","required":["parts"],
                        "properties":{"format":{"type":"string"},"parts":{
                            "type":"array",
                            "items":{"type":"object","required":["path","payload","payloadType"],
                                "properties":{"path":{"type":"string"},"payload":{"type":"string"},"payloadType":{"type":"string"}}},
                            "contains":{"type":"object","required":["path"],"properties":{"path":{"const":"copyjob-content.json"}}},
                            "minContains":1,"maxContains":1
                        }},
                        "additionalProperties":true
                    }}},
                    {"required":["operationId"],"properties":{"operationId":{"type":"string","format":"uuid"}}}
                ]
            }),
            "CopyJob item read and write permissions; encrypted labels require usage rights to remove the label. Definition excludes the sensitivity label.",
            "https://learn.microsoft.com/en-us/rest/api/fabric/copyjob/items/get-copy-job-definition",
        ),
        (
            "fabric.copyjob.list-metadata-page.v1",
            "items",
            "metadata",
            Verb::List,
            json!({
                "type":"object",
                "required":["workspaceId","recursive"],
                "properties":{
                    "workspaceId":{"type":"string","format":"uuid"},
                    "recursive":{"type":"boolean"},
                    "rootFolderId":{"type":"string","format":"uuid"},
                    "continuationToken":{"type":"string","minLength":1}
                },
                "additionalProperties":false
            }),
            json!({
                "type":"object","required":["value"],
                "properties":{
                    "value":{"type":"array","items":metadata},
                    "continuationToken":{"type":["string","null"]},
                    "continuationUri":{"type":["string","null"]}
                },
                "additionalProperties":true,
                "description":"One complete native metadata page. A continuation leaves enumeration incomplete; not run history."
            }),
            "Workspace Viewer role; Workspace.Read.All or Workspace.ReadWrite.All. Caller owns bounded continuation traversal.",
            "https://learn.microsoft.com/en-us/rest/api/fabric/copyjob/items/list-copy-jobs",
        ),
    ] {
        registry.register(OperationDescriptor::new(
            id,
            verb,
            input,
            output,
            vec![Effect::RemoteRead],
        )?)?;
        let binding = Binding::new(
            "CopyJob",
            facet,
            representation,
            Generation::Any,
            id,
            Availability::Conditional(vec![Prerequisite {
                id: "native-read-authorization".into(),
                description: format!(
                    "{permission} Source: {source} No live capability or authorization proof; caller-injected Fabric credentials only."
                ),
            }]),
        )?;
        registry.bind(if verb == Verb::List {
            binding.with_target_kind(TargetKind::WorkspaceCollection)
        } else {
            binding
        })?;
    }
    Ok(())
}
