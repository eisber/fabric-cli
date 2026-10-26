// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{PathElement, PathKind};
use crate::{ContextError, Element, HomeContext, ItemType, NodeKind, WorkspaceType};
use std::sync::Arc;

/// Caller-resolved source command capability rule, independent of runtime mode.
///
/// Supply canonical enum strings after source group/subcommand combination.
/// This mirrors `FabricElement`/`_BaseItem.check_command_support`, not argument
/// parsing or service authorization. A support rule grants no service authority.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommandPolicy {
    /// Canonical Python hierarchy type strings.
    pub supported_elements: Vec<String>,
    /// Canonical item/virtual-item suffixes.
    pub supported_items: Vec<String>,
    /// Denied suffixes, which override all positive matches.
    pub unsupported_items: Vec<String>,
}

impl CommandPolicy {
    /// Source-pinned `fs.ls` capability policy from `command_support.yaml`.
    pub fn source_list() -> Self {
        Self {
            supported_elements: [
                "Tenant",
                "Workspace",
                "Folder",
                "VirtualWorkspace",
                "VirtualItemContainer",
                "Onelake",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            supported_items: [
                "Lakehouse",
                "SparkJobDefinition",
                "Warehouse",
                "KQLDatabase",
                "MirroredDatabase",
                "SemanticModel",
                "Capacity",
                "Domain",
                "SparkPool",
                "ManagedIdentity",
                "ManagedPrivateEndpoint",
                "ExternalDataShare",
                "Connection",
                "Gateway",
                "SQLDatabase",
                "CosmosDBDatabase",
                "GraphQLApi",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            unsupported_items: Vec::new(),
        }
    }

    /// Apply the actual source precedence, retaining the caller's command alias.
    pub fn check(&self, element: &PathElement, command: &str) -> Result<(), ContextError> {
        let suffix = match element.kind() {
            PathKind::Fabric(NodeKind::Item(kind)) => Some(kind.as_str()),
            PathKind::VirtualWorkspaceItem(kind) => Some(kind.suffix()),
            PathKind::VirtualItem(kind) => Some(kind.suffix()),
            _ => None,
        };
        if let Some(suffix) = suffix {
            if self.unsupported_items.iter().any(|value| value == suffix) {
                return Err(unsupported_command(command));
            }
            if self.supported_items.iter().any(|value| value == suffix) {
                return Ok(());
            }
        }
        if self
            .supported_elements
            .iter()
            .any(|value| value == element.kind().as_str())
        {
            Ok(())
        } else {
            Err(unsupported_command(command))
        }
    }
}

/// Source item folders, retaining order and types even where `fs.ls` rejects them.
pub fn item_folders(kind: ItemType) -> &'static [&'static str] {
    match kind.as_str() {
        "Lakehouse" => &["Files", "Tables", "TableMaintenance"],
        "Warehouse" | "MirroredDatabase" | "MirroredWarehouse" => &["Files", "Tables"],
        "SemanticModel" => &["Tables"],
        "SparkJobDefinition" => &["Libs", "Main", "Snapshots"],
        "KQLDatabase" => &["Tables", "Shortcut"],
        "SQLDatabase" => &["Tables", "Files", "Code"],
        "CosmosDBDatabase" => &["Tables", "Files", "Code", "Audit"],
        _ => &[],
    }
}

fn unsupported_command(command: &str) -> ContextError {
    ContextError::python(
        "UnsupportedCommand",
        format!("not supported for command '{command}'"),
    )
}

/// Translate source personal-workspace selection over an injected identity flag
/// and ordered workspace snapshot, never querying authentication or a service.
pub fn select_home(user_identity: bool, workspaces: &[Arc<Element>]) -> HomeContext {
    if !user_identity {
        return HomeContext::Unavailable(ContextError::python(
            "InvalidOperation",
            "The personal workspace is only available with user authentication",
        ));
    }
    match workspaces
        .iter()
        .find(|workspace| workspace.kind() == NodeKind::Workspace(WorkspaceType::Personal))
    {
        Some(workspace) => HomeContext::Workspace(workspace.name()),
        None => HomeContext::Unavailable(ContextError::python(
            "NotFound",
            "The personal workspace could not be found",
        )),
    }
}
