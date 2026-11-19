// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{AclRead, CommandError, CommandValue, NamedResource, read_acls};
use fabric_cli_context::{Element, NodeKind};
use fabric_sdk_access::{AccessClient, Resource};
use fabric_sdk_api::Limits;
use serde_json::Value;
use std::{future::Future, sync::Arc};

pub(crate) fn acl_context(resource: &Resource) -> CommandValue {
    let warnings = match resource {
        Resource::Item { .. } => vec!["Requires Fabric administrator".into()],
        Resource::LakehouseDataRoles { .. } => vec!["Requires data access roles enabled".into()],
        _ => Vec::new(),
    };
    CommandValue {
        warnings,
        ..CommandValue::default()
    }
}

pub(crate) fn project_get(rows: Vec<Value>, query: Option<&str>) -> Result<Value, CommandError> {
    fabric_cli_query::search(&Value::Array(rows), query.unwrap_or("")).map_err(|_| {
        CommandError::new(
            "InvalidInput",
            "Invalid jmespath query (https://jmespath.org)",
        )
    })
}

/// Converts an existing resolved workspace/item context to the source read route.
/// Hidden and OneLake contexts remain explicit typed host inputs, not fake nodes.
/// The source SemanticModel/Environment/Dashboard exclusions precede all I/O.
pub fn acl_resource(context: &Arc<Element>) -> Result<Resource, CommandError> {
    let id = context.id().ok_or_else(|| {
        CommandError::new("NotFound", "The requested resource could not be found")
    })?;
    match context.kind() {
        NodeKind::Workspace(_) => Ok(Resource::Workspace(id.into())),
        NodeKind::Item(kind)
            if !matches!(kind.as_str(), "SemanticModel" | "Environment" | "Dashboard") =>
        {
            let workspace = context
                .workspace()
                .ok_or_else(|| CommandError::new("InvalidPath", "Item has no owning workspace"))?;
            let workspace = workspace.id().ok_or_else(|| {
                CommandError::new("NotFound", "The requested resource could not be found")
            })?;
            Ok(Resource::Item {
                workspace: workspace.into(),
                item: id.into(),
            })
        }
        _ => Err(CommandError::new(
            "UnsupportedCommand",
            "Resource does not support access control reads",
        )),
    }
}

/// Adds source OneLake security guidance using explicit display metadata.
/// This never opens a browser, changes security or executes its displayed command.
/// The host still owns final process exit semantics for the source handled error.
pub async fn read_named_acls(
    client: &AccessClient,
    target: &NamedResource,
    workspace_name: Option<&str>,
    options: AclRead,
    limits: Limits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<CommandValue, CommandError> {
    if matches!(target.resource, Resource::LakehouseDataRoles { .. })
        && workspace_name.is_none_or(str::is_empty)
    {
        return Err(CommandError::new(
            "NotConfigured",
            "OneLake ACL guidance requires the resolved workspace display name",
        ));
    }
    read_acls(client, &target.resource, options, limits, cancelled)
        .await
        .map_err(|mut error| {
            if matches!(target.resource, Resource::LakehouseDataRoles { .. })
                && error.code == "BadRequest"
            {
                error.code = "UniversalSecurityDisabled".into();
                error.handled = true;
                error.message = format!("Universal security is disabled for '{}'", target.name);
                if let Some(workspace) = workspace_name {
                    error.diagnostics.push(format!(
                        "\u{2192} Run 'open /{workspace}/{}' and enable it",
                        target.name
                    ));
                }
            }
            error
        })
}
