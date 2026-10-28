// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Python-named resource command handlers with caller-owned UI and operations.
//!
//! No process entry point, identity discovery, ambient cache, runtime or transport
//! is installed. The additive [`frontdoor`] module provides family registration.
//! See the README for the exact source pin and the intentionally incomplete matrix.

mod create;
mod errors;
mod execution;
pub mod frontdoor;
mod lifecycle;
mod moves;
mod onelake_read;
mod onelake_write;
mod removal;
mod set;
mod shortcuts;
mod sql_database;
mod template_enumeration;
mod templates;

use fabric_cli_context::{Element, NodeKind};
use fabric_cli_output::{OutputInput, OutputStatus};
use fabric_sdk_folders::mutations::FolderMutations;
use fabric_sdk_items::mutations::ItemMutations;
use fabric_sdk_workspaces::mutations::{
    ResourceError, ResourceOperations, ResourceOutcome, WorkspaceMutations,
};
use serde_json::Value;
use std::{fmt, future::Future, pin::Pin, sync::Arc};

pub use create::{CapacitySelection, CreateOptions};
pub use errors::operation_error_output;
pub use execution::FabricMutationBackend;
pub use moves::FolderMoveResult;
pub use onelake_read::{OneLakeGetResult, OneLakeReadError, OneLakeReadHost, OneLakeReadResult};
pub use onelake_write::{
    OneLakeResourceKind, OneLakeWriteError, OneLakeWriteHost, OneLakeWriteResult,
    OneLakeWriteTarget, file_destination, validate_copy_destination,
};
pub use removal::{
    RemovalCatalog, RemovalError, RemovalHost, RemovalResult, RemovalSort, RemovalUi,
};
pub use set::update_fabric_element;
pub use shortcuts::{
    OneLakeShortcutHost, ShortcutCreateDestination, ShortcutCreateOptions, ShortcutUpdateError,
    ShortcutUpdateResult,
};
pub use template_enumeration::{TemplateDirectory, TemplateEnumeration};

/// A caller-runtime asynchronous operation.
pub type CommandFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Source UI events, independent of terminal/JSON rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoticeKind {
    /// Progress text (Python's grey output).
    Progress,
    /// A warning; the host chooses its text-mode destination.
    Warning,
    /// Parameter help, not an operation result.
    Help,
}

/// User interaction explicitly supplied by the CLI host.
pub trait MutationUi: Send {
    /// Display source text without interpreting it as a command.
    fn notice(&mut self, kind: NoticeKind, message: &str);

    /// Confirm a consequential operation; a declined prompt sends no mutation.
    fn confirm<'a>(&'a mut self, message: &'a str) -> CommandFuture<'a, bool>;
}

/// Enriched metadata provider composed by the existing read-command host.
pub trait MutationMetadata: Send {
    /// Read the source-compatible metadata, including workspace enrichment.
    fn read<'a>(
        &'a mut self,
        element: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Value, MutationError>>;
}

/// Explicit cache/context work for the main host; no mutable globals are used.
#[derive(Clone, Debug, PartialEq)]
pub enum CacheEffect {
    /// Upsert a newly created resource with service metadata.
    Created {
        /// Original immutable target.
        target: Arc<Element>,
        /// Exact service result.
        data: Value,
    },
    /// Creation returned no usable ID; invalidate the containing collection.
    Invalidate {
        /// Original immutable target.
        target: Arc<Element>,
    },
    /// Python evicts the resource after delete completion or acceptance.
    Deleted {
        /// Original immutable target.
        target: Arc<Element>,
    },
    /// Rename the resource and refresh any affected current context.
    Renamed {
        /// Original immutable target.
        target: Arc<Element>,
        /// New display name, retaining JSON type for source fidelity.
        name: Value,
    },
    /// Source-confirmed folder reparenting for the caller's cache/context host.
    MovedFolder {
        /// Original immutable folder metadata.
        target: Arc<Element>,
        /// Selected new parent, preserving the owning workspace.
        parent: Arc<Element>,
    },
}

/// Observable command outcome, with acceptance separate from completion.
#[derive(Clone, Debug, Default)]
pub struct MutationResult {
    /// Output inputs for the existing envelope; empty for declined/help-only calls.
    pub outputs: Vec<OutputInput>,
    /// Actual service completion/acceptance; absent if no operation was dispatched.
    pub outcome: Option<ResourceOutcome>,
    /// Cache/context changes that the main host must apply, in order.
    pub effects: Vec<CacheEffect>,
    /// A user declined a confirmation, not a service cancellation.
    pub declined: bool,
    /// Creation's source `show_headers=True` for the text-output host.
    pub show_headers: bool,
    /// Multi-item creation responses/cache effects, including accepted dependencies.
    pub creation: Option<Box<CreationEvidence>>,
}

/// Actual creation progress, never a rollback promise or invented batch success.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CreationEvidence {
    /// Responses in dispatch order, even when a dependent create only returned 202.
    pub responses: Vec<ResourceOutcome>,
    /// Confirmed source cache work before a later operation failed or was accepted.
    pub effects: Vec<CacheEffect>,
    /// Completed dependency/root metadata collected by Python's batch-output logic.
    pub batch_items: Vec<Value>,
    /// Matching full names, preserving source batch order.
    pub batch_names: Vec<String>,
}

/// Structured command error retaining operation reconciliation evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct MutationError {
    /// Python-compatible code or explicit safety classification.
    pub code: String,
    /// Public diagnostic.
    pub message: String,
    /// Underlying policy/service error, if execution was attempted.
    pub operation: Option<Box<ResourceError>>,
    /// Earlier dependency responses and cache work that must not be lost on failure.
    pub creation: Option<Box<CreationEvidence>>,
}

impl MutationError {
    /// Construct a local command validation failure.
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            code: if code.is_empty() {
                "UnknownError"
            } else {
                code
            }
            .into(),
            message: if message.is_empty() {
                "An error occurred while processing the operation".into()
            } else {
                message.trim_end_matches('.').to_owned()
            },
            operation: None,
            creation: None,
        }
    }

    /// Render through the existing output layer, preserving stream/exit selection.
    pub fn output(&self) -> OutputInput {
        OutputInput {
            status: OutputStatus::Failure,
            error_code: Some(self.code.clone()),
            message: Some(self.message.clone()),
            ..OutputInput::default()
        }
    }
}

impl From<ResourceError> for MutationError {
    fn from(error: ResourceError) -> Self {
        let mut result = match errors::source_message(&error) {
            Ok(message) => Self::new(&error.code, message),
            Err(()) => Self::new("InvalidJson", "Invalid JSON format"),
        };
        result.operation = Some(Box::new(error));
        result
    }
}

impl fmt::Display for MutationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MutationError {}

/// Thin command composition; the existing CLI front door owns dispatch and state.
pub struct MutationHost<'a> {
    /// Shared bounded execution and operation reconciliation.
    pub operations: &'a mut dyn ResourceOperations,
    /// Caller-owned confirmation and notices.
    pub ui: &'a mut dyn MutationUi,
    /// Existing source-compatible metadata reads for `set`.
    pub metadata: &'a mut dyn MutationMetadata,
}

impl MutationHost<'_> {
    /// Execute `exists` for resolved workspace/item/folder metadata without I/O.
    ///
    /// Python checks the context ID, not a separate GET or a swallowed failure.
    ///
    /// # Errors
    /// Tenant and unsupported hierarchy branches fail explicitly.
    pub fn exists(element: &Arc<Element>) -> Result<MutationResult, MutationError> {
        supported_element(element)?;
        Ok(MutationResult {
            outputs: vec![message(if element.id().is_some_and(|id| !id.is_empty()) {
                "true"
            } else {
                "false"
            })],
            ..MutationResult::default()
        })
    }

    /// Execute the single-resource `rm`/`del` operation.
    ///
    /// Workspace non-force item selection belongs to `remove_workspace_items`,
    /// not deletion of the workspace itself.
    ///
    /// # Errors
    /// Invalid/unsupported resources and incomplete/service failures are explicit.
    pub async fn remove(
        &mut self,
        element: &Arc<Element>,
        force: bool,
    ) -> Result<MutationResult, MutationError> {
        self.remove_with_options(element, force, false).await
    }

    /// Execute `rm`/`del`, including the item-only `--hard` option.
    ///
    /// # Errors
    /// Rejects unsupported resource/option combinations and propagates failures.
    pub async fn remove_with_options(
        &mut self,
        element: &Arc<Element>,
        force: bool,
        hard: bool,
    ) -> Result<MutationResult, MutationError> {
        supported_element(element)?;
        if hard && !matches!(element.kind(), NodeKind::Item(_)) {
            return Err(MutationError::new(
                "InvalidInput",
                "--hard requires an item",
            ));
        }
        if matches!(element.kind(), NodeKind::Workspace(_)) && !force {
            return Err(MutationError::new(
                "NotSupported",
                "Workspace item selection is required; this call does not delete a workspace without -f",
            ));
        }
        check_item_support(element, false)?;
        let id = require_id(element)?;
        let prompt = if hard {
            format!(
                "'{}' will be deleted forever. Are you sure you want to proceed?",
                element.name()
            )
        } else {
            "Are you sure?".into()
        };
        if !force && !self.ui.confirm(&prompt).await {
            self.ui
                .notice(NoticeKind::Warning, "Resource delete cancelled");
            return Ok(MutationResult {
                declined: true,
                ..MutationResult::default()
            });
        }
        if force {
            self.ui.notice(
                NoticeKind::Warning,
                if hard {
                    "! Executing force hard delete"
                } else {
                    "Executing force delete..."
                },
            );
        } else if hard {
            self.ui
                .notice(NoticeKind::Warning, "! Executing hard delete");
        }
        self.ui.notice(
            NoticeKind::Progress,
            &format!("Deleting '{}'...", element.name()),
        );
        let outcome = match element.kind() {
            NodeKind::Workspace(_) => WorkspaceMutations::delete(self.operations, id).await?,
            NodeKind::Folder => {
                FolderMutations::delete(self.operations, workspace_id(element)?, id).await?
            }
            NodeKind::Item(_) => {
                ItemMutations::delete(self.operations, workspace_id(element)?, id, hard).await?
            }
            NodeKind::Tenant => unreachable!("validated above"),
        };
        let text = match &outcome {
            ResourceOutcome::Completed {
                status: 200 | 201, ..
            } => Some(format!("'{}' deleted", element.name())),
            ResourceOutcome::Accepted { .. } => {
                Some(format!("'{}' is being deleted", element.name()))
            }
            ResourceOutcome::Completed { .. } => None,
        };
        let effects = if text.is_some() {
            vec![CacheEffect::Deleted {
                target: Arc::clone(element),
            }]
        } else {
            Vec::new()
        };
        Ok(MutationResult {
            outputs: text.map(|text| vec![message(text)]).unwrap_or_default(),
            outcome: Some(outcome),
            effects,
            declined: false,
            show_headers: false,
            creation: None,
        })
    }
}

fn message(value: impl Into<String>) -> OutputInput {
    OutputInput {
        message: Some(value.into()),
        ..OutputInput::default()
    }
}

fn supported_element(element: &Element) -> Result<(), MutationError> {
    if element.kind() == NodeKind::Tenant {
        return Err(MutationError::new(
            "NotSupported",
            "This handler requires a workspace, item or folder",
        ));
    }
    Ok(())
}

fn require_id(element: &Element) -> Result<&str, MutationError> {
    element
        .id()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| MutationError::new("NotFound", "The resource does not exist"))
}

fn workspace_id(element: &Arc<Element>) -> Result<&str, MutationError> {
    let mut node = element.as_ref();
    loop {
        if matches!(node.kind(), NodeKind::Workspace(_)) {
            return require_id(node);
        }
        node = node
            .parent()
            .ok_or_else(|| MutationError::new("InvalidContext", "Missing owning workspace"))?;
    }
}

fn check_item_support(element: &Element, set: bool) -> Result<(), MutationError> {
    if let NodeKind::Item(item_type) = element.kind() {
        let blocked = matches!(
            item_type.as_str(),
            "Dashboard"
                | "PaginatedReport"
                | "SQLEndpoint"
                | "MirroredWarehouse"
                | "Datamart"
                | "OrgApp"
                | "MetricSet"
                | "ApacheAirflowJob"
                | "Exploration"
                | "RetailDataManager"
                | "Healthcaredatasolution"
                | "SustainabilityDataSolution"
                | "AISkill"
        ) || (set && item_type.as_str() == "GraphQuerySet");
        if blocked {
            return Err(MutationError::new(
                "UnsupportedCommand",
                format!(
                    "not supported for command '{}'",
                    if set { "set" } else { "del" }
                ),
            ));
        }
    }
    Ok(())
}
