// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root resource registrations and runtime-neutral composition of released handlers.
//!
//! The caller resolves context from its command-local cache, injects metadata,
//! operations and UI, and applies returned cache effects even after acceptance.
//! No identity, HTTP, retry, polling, file discovery, or output engine is added.

mod params;
mod parser;

pub use params::creation_parameters;
pub use parser::{ResourceArgs, ResourceCommand, command_from_matches, resource_subcommands};

use crate::{
    CapacitySelection, CreateOptions, MutationError, MutationHost, MutationResult,
    OneLakeShortcutHost, ShortcutCreateDestination, ShortcutCreateOptions,
};
use fabric_cli_context::{NodeKind, PathElement, PathKind, ResolveOptions, VirtualWorkspaceKind};
use fabric_cli_query::join_query_args;
use std::sync::Arc;

/// The existing host's context boundary, not a second catalog or cache.
pub trait ResourceContext {
    /// Resolve original tokens using the source `raise_error` distinction.
    ///
    /// The caller owns any asynchronous metadata preloading and diagnostic
    /// presentation. Lookup failures must not become fabricated missing nodes.
    fn resolve(
        &mut self,
        tokens: &[String],
        options: ResolveOptions,
    ) -> Result<Arc<PathElement>, MutationError>;
}

/// Non-secret configuration snapshot explicitly supplied at the command boundary.
#[derive(Clone, Debug, Default)]
pub struct ResourceConfig {
    /// Source default capacity; no ambient settings file is read by this crate.
    pub default_capacity_id: Option<String>,
}

/// Root-family composition over the already released mutation handlers.
pub struct ResourceHost<'a> {
    /// Existing immutable-context resolver and caller-owned metadata snapshot.
    pub context: &'a mut dyn ResourceContext,
    /// Existing shared operations, UI and enriched metadata read composition.
    pub mutations: MutationHost<'a>,
    /// Caller-captured command configuration.
    pub config: ResourceConfig,
}

impl ResourceHost<'_> {
    /// Execute a typed leaf and return the shared output/cache/operation evidence.
    ///
    /// The same injected operations object is borrowed throughout the command.
    /// Cancellation and uncertain-mutation state stay with its operation session.
    /// Errors never trigger replay, credential fallback, or implicit rollback.
    ///
    /// # Errors
    /// Unsupported resource/option branches fail explicitly before mutation.
    pub async fn execute(
        &mut self,
        command: &ResourceCommand,
    ) -> Result<MutationResult, MutationError> {
        if command.args().skill.is_some() {
            return Err(gap(
                command,
                "--skill requires shared resource request-header plumbing",
            ));
        }
        let creating = matches!(
            command,
            ResourceCommand::Exists(_) | ResourceCommand::Mkdir { .. } | ResourceCommand::Link { .. }
        );
        let target = self.context.resolve(
            &command.args().path,
            ResolveOptions {
                raise_error: !creating,
                supports_local_path: false,
            },
        )?;
        let mut result = match command {
            ResourceCommand::Exists(_) => {
                MutationHost::exists(fabric_target(command, &target)?)?
            }
            ResourceCommand::Mkdir { params, .. } => {
                let element = fabric_target(command, &target)?;
                let params = creation_parameters(params)?;
                let capacity = if matches!(element.kind(), NodeKind::Workspace(_))
                    && element.id().is_none()
                    && !params.is_empty()
                {
                    self.capacity_selection(&params)?
                } else {
                    CapacitySelection::Default
                };
                self.mutations
                    .create(
                        element,
                        &CreateOptions {
                            params,
                            capacity,
                            default_capacity_id: self.config.default_capacity_id.clone(),
                        },
                    )
                    .await?
            }
            ResourceCommand::Remove { force, hard, .. } => {
                let element = fabric_target(command, &target)?;
                if matches!(element.kind(), NodeKind::Workspace(_) | NodeKind::Tenant) {
                    return Err(gap(command, "workspace removal requires the catalog/selection host"));
                }
                self.mutations.remove_with_options(element, *force, *hard).await?
            }
            ResourceCommand::Set {
                query, input, force, ..
            } => {
                self.mutations
                    .set(
                        fabric_target(command, &target)?,
                        query,
                        &join_query_args(input),
                        *force,
                    )
                    .await?
            }
            ResourceCommand::Link {
                shortcut_type,
                target: source,
                input,
                force,
                ..
            } => {
                if source.is_some() {
                    return Err(gap(
                        command,
                        "--target resolution must preserve shortcut prompt/validation ordering; use inline --input",
                    ));
                }
                let destination = shortcut_destination(command, &target)?;
                OneLakeShortcutHost {
                    operations: self.mutations.operations,
                    ui: self.mutations.ui,
                }
                .create(
                    &destination,
                    &ShortcutCreateOptions {
                        target: None,
                        input: input.clone(),
                        shortcut_type: shortcut_type.clone(),
                        force: *force,
                    },
                )
                .await?
            }
            ResourceCommand::Start { force, .. } => {
                self.mutations
                    .start(fabric_target(command, &target)?, *force)
                    .await?
            }
            ResourceCommand::Stop { force, .. } => {
                self.mutations
                    .stop(fabric_target(command, &target)?, *force)
                    .await?
            }
            ResourceCommand::Assign { workspace, force, .. }
            | ResourceCommand::Unassign { workspace, force, .. } => {
                if target.kind()
                    != PathKind::VirtualWorkspaceItem(VirtualWorkspaceKind::Capacity)
                {
                    return Err(gap(command, "only the selected Capacity branch is implemented"));
                }
                let workspace = self.context.resolve(workspace, ResolveOptions::default())?;
                let workspace = fabric_target(command, &workspace)?;
                let id = target.id().filter(|id| !id.is_empty())
                    .ok_or_else(|| MutationError::new("NotFound", "The capacity does not exist"))?;
                if matches!(command, ResourceCommand::Assign { .. }) {
                    self.mutations.assign_capacity(workspace, id, &target.name(), *force).await?
                } else {
                    self.mutations.unassign_capacity(workspace, id, &target.name(), *force).await?
                }
            }
        };
        for output in &mut result.outputs {
            output.command = Some(command.name().into());
        }
        Ok(result)
    }

    fn capacity_selection(
        &mut self,
        params: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<CapacitySelection, MutationError> {
        let value = params
            .iter()
            .rev()
            .find(|(key, _)| key.eq_ignore_ascii_case("capacityname"))
            .map(|(_, value)| value);
        let Some(value) = value else {
            return Ok(CapacitySelection::Default);
        };
        let name = value
            .as_str()
            .ok_or_else(|| parser::invalid("capacityName must be a string"))?;
        if name.is_empty() {
            return Ok(CapacitySelection::Default);
        }
        if name == "none" {
            return Ok(CapacitySelection::Unassigned);
        }
        let name = name.replace(".Capacity", "").replace(".capacity", "");
        let selected = self.context.resolve(
            &[format!("/.capacities/{name}.Capacity")],
            ResolveOptions::default(),
        )?;
        if selected.kind() != PathKind::VirtualWorkspaceItem(VirtualWorkspaceKind::Capacity) {
            return Err(parser::invalid("capacityName did not resolve to a Capacity"));
        }
        Ok(CapacitySelection::Resolved(
            selected
                .id()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| parser::invalid("The selected capacity has no ID"))?
                .into(),
        ))
    }
}

fn fabric_target<'a>(
    command: &ResourceCommand,
    target: &'a PathElement,
) -> Result<&'a Arc<fabric_cli_context::Element>, MutationError> {
    target
        .fabric()
        .ok_or_else(|| gap(command, &format!("{} resource branch", target.kind().as_str())))
}

fn shortcut_destination(
    command: &ResourceCommand,
    target: &Arc<PathElement>,
) -> Result<ShortcutCreateDestination, MutationError> {
    let local_path = target
        .local_path()
        .ok_or_else(|| gap(command, "shortcut destination must be a OneLake path"))?;
    let mut item = target.as_ref();
    while matches!(item.kind(), PathKind::OneLake(_)) {
        item = item
            .parent()
            .ok_or_else(|| parser::invalid("Missing shortcut owning item"))?;
    }
    let fabric = fabric_target(command, item)?;
    let NodeKind::Item(item_type) = fabric.kind() else {
        return Err(parser::invalid("Missing shortcut owning item"));
    };
    Ok(ShortcutCreateDestination {
        workspace: crate::workspace_id(fabric)?.into(),
        item: crate::require_id(fabric)?.into(),
        local_path,
        display_path: target.path(),
        item_type,
    })
}

fn gap(command: &ResourceCommand, branch: &str) -> MutationError {
    MutationError::new(
        "NotSupported",
        format!("{}: {branch} is not supported by this resource adapter", command.name()),
    )
}
