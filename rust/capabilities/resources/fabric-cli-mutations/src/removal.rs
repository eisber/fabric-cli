// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    CacheEffect, CommandFuture, MutationError, MutationUi, NoticeKind, check_item_support, message,
    require_id,
};
use fabric_cli_context::{Element, NodeKind};
use fabric_cli_output::OutputInput;
use fabric_sdk_items::mutations::ItemMutations;
use fabric_sdk_workspaces::mutations::{ResourceOperations, ResourceOutcome, WorkspaceMutations};
use std::{fmt, sync::Arc};

/// Existing main UI's explicit multi-select interaction; the SDK never prompts.
pub trait RemovalUi: MutationUi {
    /// Return the user's selected full names, preserving source selection order.
    ///
    /// Empty selection declines without a request. This library does not invent
    /// a noninteractive default selection or accept a name as executable input.
    fn select<'a>(
        &'a mut self,
        prompt: &'a str,
        names: &'a [String],
    ) -> CommandFuture<'a, Vec<String>>;

    /// Select with observable input, cancellation and prompt-delivery failures.
    ///
    /// Existing infallible adapters retain their behavior through this default.
    /// Fallible hosts must override this method rather than return an empty list
    /// on failure. Production consumers use this boundary to retain partial work.
    fn select_result<'a>(
        &'a mut self,
        prompt: &'a str,
        names: &'a [String],
    ) -> CommandFuture<'a, Result<Vec<String>, MutationError>> {
        Box::pin(async move { Ok(self.select(prompt, names).await) })
    }
}

/// Existing catalog/read host's complete resource snapshots for removal commands.
pub trait RemovalCatalog: Send {
    /// Complete tenant workspace snapshot; failed/incomplete discovery is an error.
    fn workspaces<'a>(
        &'a mut self,
        tenant: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Vec<Arc<Element>>, MutationError>>;
    /// Complete workspace item snapshot, including rejected source item types.
    ///
    /// Do not synthesize empty data on a permission or paging failure. The source
    /// force branch still counts every underlying item before deleting a workspace.
    fn items<'a>(
        &'a mut self,
        workspace: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Vec<Arc<Element>>, MutationError>>;
}

/// Source configured ordering, injected rather than discovered from mutable config.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RemovalSort {
    /// Case-insensitive name, then source hierarchy type name.
    #[default]
    ByName,
    /// Source hierarchy type name, then case-insensitive name.
    ByType,
}

/// Actual removal progress, independent of Python's accepted-as-counted summaries.
#[derive(Clone, Debug, Default)]
pub struct RemovalResult {
    /// Source outputs in order, including per-item and final summary messages.
    pub outputs: Vec<OutputInput>,
    /// Accepted/completed responses in dispatch order, never invented rollbacks.
    pub responses: Vec<ResourceOutcome>,
    /// Source-confirmed cache removals (including source eviction on acceptance).
    pub effects: Vec<CacheEffect>,
    /// Source count increments on 200/201/202 only, not proof of terminal deletion.
    pub source_deleted_count: usize,
    /// The user selected nothing or declined confirmation before any further write.
    pub declined: bool,
}

/// Failure retaining the exact already-dispatched operations and cache/output work.
#[derive(Clone, Debug)]
pub struct RemovalError {
    /// Failing source/catalog/operation error and its own uncertainty evidence.
    pub error: Box<MutationError>,
    /// Earlier actual results, outputs and source cache work; no rollback is implied.
    pub partial: Box<RemovalResult>,
}
impl fmt::Display for RemovalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}
impl std::error::Error for RemovalError {}

/// Workspace/tenant removal command orchestration over existing catalog/UI/SDK.
pub struct RemovalHost<'a> {
    /// Explicit injected Fabric execution and shared operation reconciliation.
    pub operations: &'a mut dyn ResourceOperations,
    /// Existing CLI prompt/selection/notices implementation.
    pub ui: &'a mut dyn RemovalUi,
    /// Existing source-compatible complete resource catalog.
    pub catalog: &'a mut dyn RemovalCatalog,
    /// Explicit source configured item ordering, not an ambient default lookup.
    pub sort: RemovalSort,
}

impl RemovalHost<'_> {
    /// Execute source workspace `rm` behavior, including non-force item selection.
    ///
    /// Without force this removes only selected supported ITEMS, never the
    /// workspace. Force still reads/counts underlying items first. Empty/rejected
    /// snapshots, selections, confirmations, hard-delete flags and output/cache
    /// ordering preserve source behavior.
    ///
    /// # Errors
    /// Failed/incomplete catalog reads and later mutations retain explicit partial
    /// evidence. Accepted deletes may increment source counters without completion.
    pub async fn workspace(
        &mut self,
        workspace: &Arc<Element>,
        force: bool,
        hard: bool,
    ) -> Result<RemovalResult, RemovalError> {
        let mut result = RemovalResult::default();
        let operation = async {
            if !matches!(workspace.kind(), NodeKind::Workspace(_)) {
                return Err(MutationError::new(
                    "InvalidInput",
                    "Expected a workspace removal target",
                ));
            }
            let items = self.catalog.items(workspace).await?;
            for item in &items {
                validate_catalog_item(workspace, item)?;
            }
            if items.is_empty() && !force {
                return Err(MutationError::new(
                    "NotSupported",
                    "Empty workspace. Use -f to remove it",
                ));
            }
            if force {
                self.ui.notice(
                    NoticeKind::Progress,
                    &format!("This will delete {} underlying items", items.len()),
                );
                self.ui
                    .notice(NoticeKind::Warning, "Executing force delete...");
                self.ui.notice(
                    NoticeKind::Progress,
                    &format!("Deleting '{}'...", workspace.name()),
                );
                let outcome =
                    WorkspaceMutations::delete(self.operations, require_id(workspace)?).await?;
                if source_deleted(&outcome) {
                    result.source_deleted_count += 1;
                    result
                        .outputs
                        .push(message(delete_text(&workspace.name(), &outcome)));
                    result.effects.push(CacheEffect::Deleted {
                        target: Arc::clone(workspace),
                    });
                }
                result.responses.push(outcome);
                return Ok(());
            }
            let mut supported = items
                .into_iter()
                .filter(|item| check_item_support(item, false).is_ok())
                .collect::<Vec<_>>();
            if supported.is_empty() {
                return Err(MutationError::new(
                    "NotSupported",
                    "Items can't be deleted. Use -f to remove the workspace",
                ));
            }
            supported.sort_by_key(|item| {
                let name = item.short_name().to_lowercase();
                let kind = source_sort_type(item);
                match self.sort {
                    RemovalSort::ByName => (name, kind),
                    RemovalSort::ByType => (kind, name),
                }
            });
            let names = supported.iter().map(|item| item.name()).collect::<Vec<_>>();
            let selected = self.ui.select_result("Select items:", &names).await?;
            if selected.is_empty() {
                result.declined = true;
                return Ok(());
            }
            for name in &selected {
                self.ui.notice(NoticeKind::Progress, name);
            }
            self.ui
                .notice(NoticeKind::Progress, "------------------------------");
            if !self.ui.confirm_result("Are you sure?").await? {
                result.declined = true;
                return Ok(());
            }
            for item in supported.iter().filter(|item| {
                selected.contains(&item.name()) && matches!(item.kind(), NodeKind::Item(_))
            }) {
                let outcome = ItemMutations::delete(
                    self.operations,
                    require_id(workspace)?,
                    require_id(item)?,
                    hard,
                )
                .await?;
                if source_deleted(&outcome) {
                    result.source_deleted_count += 1;
                    // The source bulk helper prints "deleted" even for 202. Actual
                    // response evidence remains independent in `responses`.
                    result
                        .outputs
                        .push(message(format!("'{}' deleted", item.name())));
                    result.effects.push(CacheEffect::Deleted {
                        target: Arc::clone(item),
                    });
                }
                result.responses.push(outcome);
            }
            self.ui.notice(NoticeKind::Progress, "");
            result.outputs.push(message(format!(
                "{} items deleted successfully",
                result.source_deleted_count
            )));
            Ok(())
        }
        .await;
        match operation {
            Ok(()) => Ok(result),
            Err(error) => Err(RemovalError {
                error: Box::new(error),
                partial: Box::new(result),
            }),
        }
    }

    /// Execute source tenant `rm` multi-workspace selection/confirmation.
    ///
    /// Source force does not skip these prompts. Workspaces are sorted by their
    /// exact case-sensitive full names before selection; each chosen workspace
    /// receives one source force-delete operation, never a reset/replayed request.
    ///
    /// # Errors
    /// Incomplete catalog or later service failure retains all earlier responses,
    /// per-workspace outputs and cache effects without a misleading final summary.
    pub async fn tenant(&mut self, tenant: &Arc<Element>) -> Result<RemovalResult, RemovalError> {
        let mut result = RemovalResult::default();
        let operation = async {
            if tenant.kind() != NodeKind::Tenant {
                return Err(MutationError::new(
                    "InvalidInput",
                    "Expected a tenant removal target",
                ));
            }
            let mut workspaces = self.catalog.workspaces(tenant).await?;
            if workspaces.iter().any(|workspace| {
                !matches!(workspace.kind(), NodeKind::Workspace(_))
                    || workspace.parent() != Some(tenant)
            }) {
                return Err(MutationError::new(
                    "InvalidContext",
                    "Catalog returned an unrelated workspace",
                ));
            }
            workspaces.sort_by_key(|workspace| workspace.name());
            let names = workspaces
                .iter()
                .map(|workspace| workspace.name())
                .collect::<Vec<_>>();
            let selected = self.ui.select_result("Select workspaces:", &names).await?;
            if selected.is_empty() {
                result.declined = true;
                return Ok(());
            }
            for name in &selected {
                self.ui.notice(NoticeKind::Progress, name);
            }
            self.ui
                .notice(NoticeKind::Progress, "------------------------------");
            if !self.ui.confirm_result("Are you sure?").await? {
                result.declined = true;
                return Ok(());
            }
            for workspace in workspaces
                .iter()
                .filter(|workspace| selected.contains(&workspace.name()))
            {
                self.ui
                    .notice(NoticeKind::Warning, "Executing force delete...");
                self.ui.notice(
                    NoticeKind::Progress,
                    &format!("Deleting '{}'...", workspace.name()),
                );
                let outcome =
                    WorkspaceMutations::delete(self.operations, require_id(workspace)?).await?;
                if source_deleted(&outcome) {
                    result.source_deleted_count += 1;
                    result
                        .outputs
                        .push(message(delete_text(&workspace.name(), &outcome)));
                    result.effects.push(CacheEffect::Deleted {
                        target: Arc::clone(workspace),
                    });
                }
                result.responses.push(outcome);
            }
            self.ui.notice(NoticeKind::Progress, "");
            result.outputs.push(message(format!(
                "{} workspaces deleted successfully",
                result.source_deleted_count
            )));
            Ok(())
        }
        .await;
        match operation {
            Ok(()) => Ok(result),
            Err(error) => Err(RemovalError {
                error: Box::new(error),
                partial: Box::new(result),
            }),
        }
    }
}

fn validate_catalog_item(
    workspace: &Arc<Element>,
    item: &Arc<Element>,
) -> Result<(), MutationError> {
    if !matches!(item.kind(), NodeKind::Item(_) | NodeKind::Folder)
        || item.workspace().as_ref() != Some(workspace)
    {
        return Err(MutationError::new(
            "InvalidContext",
            "Catalog returned an unrelated workspace item",
        ));
    }
    Ok(())
}
fn source_sort_type(item: &Element) -> String {
    match item.kind() {
        NodeKind::Item(item_type) => {
            if item_type.as_str() == "CopyJob" {
                return "COPYJOB".into();
            }
            if item_type.as_str() == "GraphQLApi" {
                return "GRAPHQLAPI".into();
            }
            // Python uses the source enum NAME, not display casing.
            let mut name = String::new();
            let chars = item_type.as_str().chars().collect::<Vec<_>>();
            for (index, &character) in chars.iter().enumerate() {
                if index > 0
                    && character.is_uppercase()
                    && (chars[index - 1].is_lowercase()
                        || chars.get(index + 1).is_some_and(|next| next.is_lowercase()))
                {
                    name.push('_');
                }
                name.extend(character.to_uppercase());
            }
            name
        }
        _ => "Folder".into(),
    }
}
fn source_deleted(outcome: &ResourceOutcome) -> bool {
    matches!(
        outcome,
        ResourceOutcome::Completed {
            status: 200 | 201,
            ..
        } | ResourceOutcome::Accepted { .. }
    )
}
fn delete_text(name: &str, outcome: &ResourceOutcome) -> String {
    if matches!(outcome, ResourceOutcome::Accepted { .. }) {
        format!("'{name}' is being deleted")
    } else {
        format!("'{name}' deleted")
    }
}
