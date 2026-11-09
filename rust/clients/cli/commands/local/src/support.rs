// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::collections::BTreeMap;

use fabric_cli_context::{CommandPolicy, NodeKind, PathElement, PathKind, WorkspaceType};
use serde::Deserialize;

use crate::LocalError;

/// Pinned Python command capability snapshot, not an authorization decision.
#[derive(Clone, Debug, Deserialize)]
pub struct SupportMatrix {
    /// Original Python source commit.
    pub source_commit: String,
    /// Recognized suffixes in source enum order, excluding `.Personal`.
    pub elements: Vec<String>,
    /// Source policies including enum spelling quirks.
    pub policies: Vec<SupportEntry>,
    /// Source browser route segments keyed by exact item suffix.
    pub uris: BTreeMap<String, String>,
}

/// One source command and the arguments to the shared context capability checker.
#[derive(Clone, Debug, Deserialize)]
pub struct SupportEntry {
    /// Linux source command spelling; not a claim that the Rust leaf is ported.
    pub command: String,
    /// Accepted hierarchy types.
    pub supported_elements: Vec<String>,
    /// Accepted item suffixes.
    pub supported_items: Vec<String>,
    /// Denied suffixes, overriding positive matches.
    pub unsupported_items: Vec<String>,
}

impl SupportEntry {
    fn policy(&self) -> CommandPolicy {
        CommandPolicy {
            supported_elements: self.supported_elements.clone(),
            supported_items: self.supported_items.clone(),
            unsupported_items: self.unsupported_items.clone(),
        }
    }
}

impl SupportMatrix {
    /// Read the immutable development-time extraction; no Python runtime needed.
    pub fn source() -> Result<Self, LocalError> {
        serde_json::from_str(include_str!("support.json")).map_err(|_| {
            LocalError::command(
                "InvalidSupportMatrix",
                "The embedded source support matrix is invalid",
            )
        })
    }

    /// Apply existing context command-support precedence to a real resolved node.
    pub fn check(&self, element: &PathElement, command: &str) -> Result<(), LocalError> {
        self.policies
            .iter()
            .find(|entry| entry.command == command)
            .ok_or_else(|| LocalError::command("UnsupportedCommand", "Unknown source command"))?
            .policy()
            .check(element, command)?;
        Ok(())
    }

    /// List source-supported commands for a resolved node, in sorted order.
    pub fn commands_for(&self, element: &PathElement) -> Vec<String> {
        let mut commands = self
            .policies
            .iter()
            .filter(|entry| entry.policy().check(element, &entry.command).is_ok())
            .map(|entry| entry.command.clone())
            .collect::<Vec<_>>();
        commands.sort();
        commands
    }

    /// Match a recognized dot suffix case-insensitively, without resolving a path.
    pub fn canonical_suffix(&self, value: &str) -> Option<&str> {
        self.elements
            .iter()
            .find(|suffix| suffix.eq_ignore_ascii_case(value))
            .map(String::as_str)
    }

    /// Build a typed, ID-less source model for `desc .Notebook` and peers.
    pub fn model(&self, suffix: &str) -> Result<std::sync::Arc<PathElement>, LocalError> {
        use fabric_cli_context::{Element, ItemType, VirtualItemKind, VirtualWorkspaceKind};
        use std::sync::Arc;
        let suffix = self.canonical_suffix(suffix).ok_or_else(|| {
            LocalError::command("InvalidElementType", "Unknown Fabric element suffix")
        })?;
        let item_suffix = suffix
            .strip_prefix('.')
            .filter(|suffix| !suffix.is_empty())
            .ok_or_else(|| {
                LocalError::command("InvalidElementType", "Invalid Fabric element suffix")
            })?;
        let tenant = Element::tenant("fab", "synthetic");
        let workspace = Element::child(
            "fab",
            None,
            NodeKind::Workspace(WorkspaceType::Workspace),
            Arc::clone(&tenant),
        )?;
        if suffix == ".Workspace" {
            return Ok(PathElement::from_fabric(workspace));
        }
        if suffix == ".Folder" {
            return Ok(PathElement::from_fabric(Element::child(
                "fab",
                None,
                NodeKind::Folder,
                workspace,
            )?));
        }
        for kind in VirtualWorkspaceKind::ALL {
            if item_suffix == kind.suffix() {
                let container = PathElement::child(
                    kind.collection(),
                    None,
                    PathKind::VirtualWorkspace(kind),
                    PathElement::from_fabric(tenant),
                )?;
                return Ok(PathElement::child(
                    "fab",
                    None,
                    PathKind::VirtualWorkspaceItem(kind),
                    container,
                )?);
            }
        }
        for kind in VirtualItemKind::ALL {
            if item_suffix == kind.suffix() {
                let container = PathElement::child(
                    kind.collection(),
                    None,
                    PathKind::VirtualItemContainer(kind),
                    PathElement::from_fabric(workspace),
                )?;
                return Ok(PathElement::child(
                    "fab",
                    None,
                    PathKind::VirtualItem(kind),
                    container,
                )?);
            }
        }
        let kind = ItemType::parse(item_suffix)?;
        Ok(PathElement::from_fabric(Element::child(
            "fab",
            None,
            NodeKind::Item(kind),
            workspace,
        )?))
    }

    /// Source report for a label and resolved model, retaining source blank lines.
    pub fn describe(&self, label: &str, element: &PathElement) -> String {
        format!(
            "Commands for '{label}'. Type '<command> -h' for help.\n\nAvailable commands:\n  {}\n",
            self.commands_for(element).join("\n  ")
        )
    }
}
