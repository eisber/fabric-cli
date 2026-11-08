// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::v2::{
    Availability, Binding, ContractError, Effect, Generation, OperationDescriptor, Registry,
    TargetKind, Verb,
};
use serde_json::json;

/// Add only implemented metadata bindings and their type-constrained alias.
///
/// No query, execute, ingestion, source/definition, shortcut or mutation binding
/// is advertised. Schema projections describe discovery, not effective access.
///
/// # Errors
/// Preserves registry duplicate/ambiguity errors rather than replacing entries.
pub fn register(registry: &mut Registry) -> Result<(), ContractError> {
    registry.register(OperationDescriptor::new(
        "fabric.kql-database.metadata.get.v1",
        Verb::Get,
        json!({"type":"object","properties":{},"additionalProperties":false}),
        json!({"type":"object","required":["id","workspaceId","type","displayName"],
            "properties":{"type":{"const":"KQLDatabase"}}}),
        vec![Effect::RemoteRead],
    )?)?;
    registry.register(OperationDescriptor::new(
        "fabric.kql-database.metadata.list-page.v1",
        Verb::List,
        json!({"type":"object","required":["recursive"],
            "properties":{"recursive":{"type":"boolean"},
                "rootFolderId":{"type":"string","format":"uuid"},
                "continuationToken":{"type":"string","minLength":1}},
            "additionalProperties":false}),
        json!({"type":"object","required":["value"],
            "properties":{"value":{"type":"array","items":{"type":"object"}}}}),
        vec![Effect::RemoteRead],
    )?)?;
    registry.alias("kql-database", "KQLDatabase")?;
    for (kind, facet, id) in [
        (
            TargetKind::Item,
            "item",
            "fabric.kql-database.metadata.get.v1",
        ),
        (
            TargetKind::WorkspaceCollection,
            "items",
            "fabric.kql-database.metadata.list-page.v1",
        ),
    ] {
        registry.bind(
            Binding::new(
                "KQLDatabase",
                facet,
                "metadata",
                Generation::Any,
                id,
                Availability::Available,
            )?
            .with_target_kind(kind),
        )?;
    }
    Ok(())
}
