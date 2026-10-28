// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    MutationError, MutationHost, MutationResult, NoticeKind, message, require_id, workspace_id,
};
use fabric_cli_context::{Element, NodeKind};
use fabric_sdk_items::mutations::ItemMutations;
use fabric_sdk_workspaces::mutations::{ResourceOutcome, WorkspaceMutations};
use std::sync::Arc;

struct Action {
    label: &'static str,
    progress: &'static str,
    past: &'static str,
}

const START: Action = Action {
    label: "start",
    progress: "Starting",
    past: "started",
};
const STOP: Action = Action {
    label: "stop",
    progress: "Stopping",
    past: "stopped",
};
const ASSIGN: Action = Action {
    label: "assignment",
    progress: "Assigning",
    past: "assigned",
};
const UNASSIGN: Action = Action {
    label: "unassignment",
    progress: "Unassigning",
    past: "unassigned",
};

impl MutationHost<'_> {
    /// Execute `start` for an existing mirrored database.
    ///
    /// # Errors
    /// State validation precedes confirmation, including when force is supplied.
    /// Capacity ARM lifecycle is a separate handler and is not silently redirected.
    pub async fn start(
        &mut self,
        element: &Arc<Element>,
        force: bool,
    ) -> Result<MutationResult, MutationError> {
        self.mirroring(element, force, true).await
    }

    /// Execute `stop` for an existing mirrored database.
    ///
    /// # Errors
    /// Unsupported types, invalid state and operation failures remain explicit.
    /// The pinned Python substring-membership state quirk is preserved.
    pub async fn stop(
        &mut self,
        element: &Arc<Element>,
        force: bool,
    ) -> Result<MutationResult, MutationError> {
        self.mirroring(element, force, false).await
    }

    async fn mirroring(
        &mut self,
        element: &Arc<Element>,
        force: bool,
        start: bool,
    ) -> Result<MutationResult, MutationError> {
        if !matches!(element.kind(), NodeKind::Item(kind) if kind.as_str() == "MirroredDatabase") {
            return Err(MutationError::new(
                "UnsupportedCommand",
                "This handler requires a MirroredDatabase",
            ));
        }
        let workspace = workspace_id(element)?;
        let id = require_id(element)?;
        let status = ItemMutations::mirroring_status(self.operations, workspace, id).await?;
        if let ResourceOutcome::Completed { status: 200, data } = status {
            let state = data
                .as_ref()
                .and_then(|value| value.get("status"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    MutationError::new(
                        "InvalidResponse",
                        "Mirroring status returned invalid metadata",
                    )
                })?;
            let allowed = if start {
                matches!(state, "Stopped" | "Initialized")
            } else {
                "Running".contains(state)
            };
            if !allowed {
                return Err(MutationError::new(
                    if start {
                        "AlreadyRunning"
                    } else {
                        "NotRunning"
                    },
                    format!(
                        "'{}' is not in a valid state to {}. State: {state}",
                        element.name(),
                        if start { "start" } else { "stop" }
                    ),
                ));
            }
        }
        let action = if start { &START } else { &STOP };
        if !self.confirm_action(&element.name(), force, action).await {
            return Ok(declined());
        }
        let outcome = if start {
            ItemMutations::start_mirroring(self.operations, workspace, id).await?
        } else {
            ItemMutations::stop_mirroring(self.operations, workspace, id).await?
        };
        Ok(action_result(outcome, &element.name(), action))
    }

    /// Execute the capacity branch of `assign`.
    ///
    /// The existing context host supplies the selected capacity ID and full name;
    /// no hidden collection, config or credential is discovered here.
    ///
    /// # Errors
    /// Non-workspace targets and service/policy failures are explicit.
    pub async fn assign_capacity(
        &mut self,
        workspace: &Arc<Element>,
        capacity_id: &str,
        capacity_name: &str,
        force: bool,
    ) -> Result<MutationResult, MutationError> {
        if !matches!(workspace.kind(), NodeKind::Workspace(_)) {
            return Err(MutationError::new(
                "NotSupported",
                "Capacity can only be assigned to a workspace",
            ));
        }
        if !self.confirm_action(capacity_name, force, &ASSIGN).await {
            return Ok(declined());
        }
        let outcome = WorkspaceMutations::assign_capacity(
            self.operations,
            require_id(workspace)?,
            capacity_id,
        )
        .await?;
        Ok(action_result(outcome, capacity_name, &ASSIGN))
    }

    /// Execute capacity `unassign`, checking the actual linked capacity first.
    ///
    /// # Errors
    /// An unrelated capacity, malformed primary metadata, non-workspace target or
    /// service failure never becomes a successful unassignment.
    pub async fn unassign_capacity(
        &mut self,
        workspace: &Arc<Element>,
        capacity_id: &str,
        capacity_name: &str,
        force: bool,
    ) -> Result<MutationResult, MutationError> {
        if !matches!(workspace.kind(), NodeKind::Workspace(_)) {
            return Err(MutationError::new(
                "NotSupported",
                "Capacity can only be unassigned from a workspace",
            ));
        }
        let outcome = WorkspaceMutations::metadata(self.operations, require_id(workspace)?).await?;
        let ResourceOutcome::Completed {
            data: Some(data), ..
        } = outcome
        else {
            return Err(MutationError::new(
                "InvalidResponse",
                "Workspace metadata is not complete",
            ));
        };
        if !data.is_object() {
            return Err(MutationError::new(
                "InvalidResponse",
                "Workspace metadata is not an object",
            ));
        }
        if data.get("capacityId").and_then(serde_json::Value::as_str) != Some(capacity_id) {
            return Err(MutationError::new(
                "InvalidInput",
                "Cannot unassign capacity: Workspace not linked to it",
            ));
        }
        if !self.confirm_action(capacity_name, force, &UNASSIGN).await {
            return Ok(declined());
        }
        let outcome =
            WorkspaceMutations::unassign_capacity(self.operations, require_id(workspace)?).await?;
        Ok(action_result(outcome, capacity_name, &UNASSIGN))
    }

    async fn confirm_action(&mut self, name: &str, force: bool, action: &Action) -> bool {
        if !force && !self.ui.confirm("Are you sure?").await {
            self.ui.notice(
                NoticeKind::Warning,
                &format!("Resource {} cancelled", action.label),
            );
            return false;
        }
        if force {
            self.ui.notice(
                NoticeKind::Warning,
                &format!("Executing force {}...", action.label),
            );
        }
        self.ui.notice(
            NoticeKind::Progress,
            &format!("{} '{name}'...", action.progress),
        );
        true
    }
}

fn declined() -> MutationResult {
    MutationResult {
        declined: true,
        ..MutationResult::default()
    }
}

fn action_result(outcome: ResourceOutcome, name: &str, action: &Action) -> MutationResult {
    let text = match &outcome {
        ResourceOutcome::Completed {
            status: 200 | 201, ..
        } => Some(format!("'{name}' {}", action.past)),
        ResourceOutcome::Accepted { .. } => Some(format!("'{name}' is being {}", action.past)),
        _ => None,
    };
    MutationResult {
        outputs: text.map(|text| vec![message(text)]).unwrap_or_default(),
        outcome: Some(outcome),
        ..MutationResult::default()
    }
}
