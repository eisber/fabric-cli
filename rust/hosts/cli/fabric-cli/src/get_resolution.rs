// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded name resolution for the explicit get head.

use crate::get_target::{GetRepresentation, GetTarget, is_id};
use fabric_sdk::{
    Client,
    items::{ItemExecutor, ItemListError, ItemListLimits, ItemListOptions},
    workspaces::{WorkspaceExecutor, WorkspaceListError, WorkspaceListLimits},
};
use std::{future::Future, num::NonZeroUsize, pin::pin};

/// A resolved retrieval target; names never become request-path identifiers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedGetTarget {
    /// One primary workspace.
    Workspace(String),
    /// One explicit item and representation.
    Item {
        /// Canonical owning workspace.
        workspace_id: String,
        /// Canonical item.
        item_id: String,
        /// Requested representation.
        representation: GetRepresentation,
    },
    /// One existing global operation handle.
    Operation {
        /// Canonical operation handle.
        operation_id: String,
        /// Explicit result child.
        result: bool,
    },
}

/// Name-resolution failure, without echoing names or provider diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum GetResolutionError {
    /// No exact typed candidate exists in the authorized bounded listing.
    #[error("the addressed typed target was not found")]
    NotFound,
    /// More than one authorized candidate has the selected name and type.
    #[error("the addressed typed target is ambiguous; use its explicit identifier")]
    AmbiguousTarget,
    /// Discovery failed, was cancelled or exceeded its configured finite bounds.
    #[error("bounded target discovery did not complete")]
    Discovery,
    /// Original typed workspace discovery failure, including denial/cancellation.
    #[error("workspace discovery did not complete")]
    Workspace(#[source] WorkspaceListError),
    /// Original typed item discovery failure, including denial/cancellation.
    #[error("item discovery did not complete")]
    Item(#[source] ItemListError),
    /// Resolved discovery data exceeded the invocation allowance.
    #[error("target discovery exceeded its byte budget")]
    BudgetExceeded,
}

/// Resolve one parsed target over the existing SDK, with no identity fallback.
///
/// Explicit GUIDs need no enumeration. Name discovery reads complete authorized
/// workspace/item listings under finite page/row/response bounds; a partial
/// listing never establishes uniqueness. The caller supplies one cancellation
/// future for its whole invocation, including the total deadline.
///
/// # Errors
/// Returns missing/ambiguous targets separately from failed/bounded discovery.
pub async fn resolve(
    target: GetTarget,
    client: &Client,
    workspaces: &dyn WorkspaceExecutor,
    items: &dyn ItemExecutor,
    max_bytes: NonZeroUsize,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<ResolvedGetTarget, GetResolutionError> {
    let mut cancelled = pin!(cancelled);
    let (workspace, item) = match target {
        GetTarget::Operation {
            operation_id,
            result,
        } => {
            return Ok(ResolvedGetTarget::Operation {
                operation_id,
                result,
            });
        }
        GetTarget::Workspace { workspace } => (workspace, None),
        GetTarget::Item {
            workspace,
            item,
            item_type,
            representation,
        } => (workspace, Some((item, item_type, representation))),
    };
    let mut remaining = max_bytes.get();
    let workspace_id = if is_id(&workspace) {
        workspace
    } else {
        let listing = client
            .list_workspaces(
                workspaces,
                WorkspaceListLimits {
                    max_pages: NonZeroUsize::new(10).unwrap(),
                    max_items: NonZeroUsize::new(1_000).unwrap(),
                    max_response_bytes: max_bytes,
                },
                cancelled.as_mut(),
            )
            .await
            .map_err(GetResolutionError::Workspace)?;
        charge(
            serde_json::to_vec(&listing.value)
                .map_err(|_| GetResolutionError::Discovery)?
                .len(),
            &mut remaining,
        )?;
        unique(
            listing
                .value
                .iter()
                .filter(|candidate| {
                    candidate.display_name.eq_ignore_ascii_case(&workspace)
                        && candidate.workspace_type.eq_ignore_ascii_case("Workspace")
                })
                .map(|candidate| candidate.id.clone()),
        )?
    };
    let Some((item, item_type, representation)) = item else {
        return Ok(ResolvedGetTarget::Workspace(workspace_id));
    };
    let item_id = if is_id(&item) {
        item
    } else {
        let response_bound =
            NonZeroUsize::new(remaining).ok_or(GetResolutionError::BudgetExceeded)?;
        let listing = client
            .list_items(
                items,
                &workspace_id,
                ItemListOptions {
                    query: Vec::new(),
                    limits: ItemListLimits {
                        max_pages: NonZeroUsize::new(10).unwrap(),
                        max_items: NonZeroUsize::new(1_000).unwrap(),
                        max_response_bytes: response_bound,
                    },
                },
                cancelled.as_mut(),
            )
            .await
            .map_err(GetResolutionError::Item)?;
        charge(
            serde_json::to_vec(&listing.value)
                .map_err(|_| GetResolutionError::Discovery)?
                .len(),
            &mut remaining,
        )?;
        unique(
            listing
                .value
                .iter()
                .filter(|candidate| {
                    candidate.display_name.eq_ignore_ascii_case(&item)
                        && item_type
                            .as_ref()
                            .is_none_or(|kind| candidate.item_type() == Some(kind.as_str()))
                })
                .map(|candidate| candidate.id.clone()),
        )?
    };
    Ok(ResolvedGetTarget::Item {
        workspace_id,
        item_id,
        representation,
    })
}

fn unique(mut candidates: impl Iterator<Item = String>) -> Result<String, GetResolutionError> {
    let id = candidates.next().ok_or(GetResolutionError::NotFound)?;
    if candidates.next().is_some() {
        return Err(GetResolutionError::AmbiguousTarget);
    }
    Ok(id)
}

fn charge(bytes: usize, remaining: &mut usize) -> Result<(), GetResolutionError> {
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or(GetResolutionError::BudgetExceeded)?;
    Ok(())
}
