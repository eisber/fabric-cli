// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Warehouse-only delegation from the existing explicit-budget get/list head.

use super::{ReadPage, ReadTarget};
use crate::warehouse_reads::{
    WarehouseCollection, WarehouseItemRead, WarehousePageLimits, WarehouseReadError,
    WarehouseReadHost, WarehouseRepresentation,
};
use fabric_sdk::sql_databases::{
    FolderScope,
    warehouse::{ConnectionOptions, RestorePointId, WarehouseTarget},
};
use fabric_sdk_api::workspace_reads::{WorkspaceReadError, WorkspaceReadResult};
use fabric_sdk_operations::{OperationErrorKind, OperationLimits};
use serde_json::{Value, json};
use std::{future::pending, num::NonZeroUsize};

/// One Warehouse-native delegate; the common head owns options and delivery.
#[derive(Clone)]
pub struct WarehouseCommandRead {
    selection: Selection,
    path: String,
    item_id: Option<String>,
    representation: &'static str,
    max_data_bytes: NonZeroUsize,
    max_records: Option<NonZeroUsize>,
}

impl std::fmt::Debug for WarehouseCommandRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WarehouseCommandRead")
            .field("representation", &self.representation)
            .field("max_data_bytes", &self.max_data_bytes)
            .field("max_records", &self.max_records)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
enum Selection {
    Get(WarehouseItemRead),
    List(WarehouseCollection),
}

impl WarehouseCommandRead {
    pub(super) fn target(&self) -> ReadTarget {
        ReadTarget {
            path: self.path.clone(),
            id: self.item_id.clone(),
            kind: "Warehouse",
            scope: if self.item_id.is_some() {
                "item"
            } else {
                "workspace"
            },
        }
    }

    pub(super) fn representation(&self) -> &'static str {
        self.representation
    }

    pub(super) async fn execute(
        &self,
        host: &WarehouseReadHost<'_>,
    ) -> Result<(WorkspaceReadResult, Value), (WorkspaceReadError, Value)> {
        let limits = OperationLimits::default();
        let payload = match &self.selection {
            Selection::Get(selection) => {
                host.get(selection, limits, self.max_data_bytes, pending())
                    .await
            }
            Selection::List(selection) => host
                .list_page(
                    selection,
                    None,
                    WarehousePageLimits {
                        max_records: self.max_records.expect("validated LIST bound"),
                        max_data_bytes: self.max_data_bytes,
                        operations: limits,
                    },
                    pending(),
                )
                .await
                .map(|page| page.payload),
        }
        .map_err(|error| {
            let evidence = error_evidence(&error);
            (read_error(&error), evidence)
        })?;
        let meta = payload.data["meta"].clone();
        let data = payload.data["data"].clone();
        // The native token is informative here. Only the typed in-process seam
        // retains the credential/options-bound continuation object.
        let mut evidence = meta;
        if !payload.complete {
            evidence["coverage"]["resumeKind"] = json!("nativeInformational");
        }
        Ok((
            WorkspaceReadResult {
                data,
                complete: payload.complete,
                truncated: !payload.complete,
                page: None,
            },
            evidence,
        ))
    }
}

pub(super) fn page(evidence: &Value) -> Option<ReadPage> {
    let coverage = evidence.get("coverage")?;
    Some(ReadPage {
        pages_received: usize::try_from(coverage["pagesReceived"].as_u64()?).ok()?,
        records_observed: usize::try_from(coverage["recordsReturned"].as_u64()?).ok()?,
        records_returned: usize::try_from(coverage["recordsReturned"].as_u64()?).ok()?,
        has_more: coverage["hasMore"].as_bool()?,
        output_truncated: false,
        native_continuation: None,
    })
}

pub(super) fn parse(
    verb: &str,
    path: &str,
    facade: Option<&str>,
    matches: &clap::ArgMatches,
    bytes: NonZeroUsize,
) -> Result<Option<WarehouseCommandRead>, WorkspaceReadError> {
    let warehouse_path =
        path.contains(".Warehouse") || path.ends_with("/warehouses") || facade == Some("warehouse");
    if !warehouse_path {
        return Ok(None);
    }
    if facade == Some("workspace") {
        return Err(WorkspaceReadError::UnsupportedCapability);
    }
    let (workspace, rest) = path
        .strip_prefix('/')
        .and_then(|path| path.split_once(".Workspace"))
        .ok_or(WorkspaceReadError::InvalidInput)?;
    let workspace_id = workspace
        .parse()
        .map_err(|_| WorkspaceReadError::InvalidInput)?;
    let collection = verb == "list"
        && (rest == "/warehouses" || (rest.is_empty() && facade == Some("warehouse")));
    let (selection, item_id, representation, canonical_suffix) = if collection {
        let recursive = matches
            .get_one::<bool>("recursive")
            .copied()
            .ok_or(WorkspaceReadError::InvalidInput)?;
        (
            Selection::List(WarehouseCollection::Warehouses {
                workspace: workspace_id,
                recursive,
                root_folder: FolderScope::Workspace,
            }),
            None,
            "warehouses",
            "/warehouses".to_owned(),
        )
    } else {
        let rest = rest
            .strip_prefix('/')
            .ok_or(WorkspaceReadError::InvalidInput)?;
        let (typed_item, facet) = rest.split_once('/').unwrap_or((rest, ""));
        let item = typed_item
            .strip_suffix(".Warehouse")
            .ok_or(WorkspaceReadError::UnsupportedCapability)?;
        let owner = WarehouseTarget::new(
            workspace_id,
            item.parse().map_err(|_| WorkspaceReadError::InvalidInput)?,
            "Warehouse",
        )
        .map_err(|_| WorkspaceReadError::InvalidInput)?;
        let item = owner.warehouse_id().to_string();
        let (selection, representation) = match (verb, facet) {
            ("get", "") => (
                Selection::Get(
                    WarehouseItemRead::new(
                        workspace,
                        &item,
                        "Warehouse",
                        WarehouseRepresentation::Metadata,
                    )
                    .map_err(|_| WorkspaceReadError::InvalidInput)?,
                ),
                "primary-metadata",
            ),
            ("get", "settings/connection") => (
                Selection::Get(
                    WarehouseItemRead::new(
                        workspace,
                        &item,
                        "Warehouse",
                        WarehouseRepresentation::Connection(ConnectionOptions::default()),
                    )
                    .map_err(|_| WorkspaceReadError::InvalidInput)?,
                ),
                "connection",
            ),
            ("get", facet) if facet.starts_with("restore-points/") => {
                let key = facet
                    .strip_prefix("restore-points/")
                    .expect("checked prefix");
                let key = RestorePointId::new(key).map_err(|_| WorkspaceReadError::InvalidInput)?;
                (
                    Selection::Get(
                        WarehouseItemRead::new(
                            workspace,
                            &item,
                            "Warehouse",
                            WarehouseRepresentation::RestorePoint(key),
                        )
                        .map_err(|_| WorkspaceReadError::InvalidInput)?,
                    ),
                    "restore-point",
                )
            }
            ("list", "restore-points") => {
                if matches.get_one::<bool>("recursive").is_some() {
                    return Err(WorkspaceReadError::UnsupportedCapability);
                }
                (
                    Selection::List(WarehouseCollection::RestorePoints(owner)),
                    "restore-points",
                )
            }
            _ => return Err(WorkspaceReadError::UnsupportedCapability),
        };
        (
            selection,
            Some(item.clone()),
            representation,
            format!(
                "/{item}.Warehouse{}",
                if facet.is_empty() {
                    String::new()
                } else {
                    format!("/{facet}")
                }
            ),
        )
    };
    Ok(Some(WarehouseCommandRead {
        selection,
        path: format!("/{workspace_id}.Workspace{canonical_suffix}"),
        item_id,
        representation,
        max_data_bytes: bytes,
        max_records: if verb == "list" {
            matches.get_one::<NonZeroUsize>("limit").copied()
        } else {
            None
        },
    }))
}

fn read_error(error: &WarehouseReadError) -> WorkspaceReadError {
    match error {
        WarehouseReadError::Operation(error) => match &error.kind {
            OperationErrorKind::Credential => WorkspaceReadError::Unauthenticated,
            OperationErrorKind::Http { status: 401, .. } => WorkspaceReadError::Unauthenticated,
            OperationErrorKind::Http { status: 403, .. } => WorkspaceReadError::AccessDenied,
            OperationErrorKind::Http { status: 404, .. } => WorkspaceReadError::NotFound,
            OperationErrorKind::Http { status, .. }
                if (200..300).contains(status) && *status != 202 =>
            {
                WorkspaceReadError::InvalidResponse
            }
            OperationErrorKind::Limit => WorkspaceReadError::BudgetExceeded,
            OperationErrorKind::Cancelled => WorkspaceReadError::Cancelled,
            _ => WorkspaceReadError::OperationFailed,
        },
        WarehouseReadError::BudgetExceeded => WorkspaceReadError::BudgetExceeded,
        WarehouseReadError::Paging(error)
            if matches!(
                error.kind,
                fabric_sdk_paging::PagingErrorKind::ItemLimit
                    | fabric_sdk_paging::PagingErrorKind::ResponseLimit
            ) =>
        {
            WorkspaceReadError::BudgetExceeded
        }
        _ => WorkspaceReadError::InvalidResponse,
    }
}

fn error_evidence(error: &WarehouseReadError) -> Value {
    match error {
        WarehouseReadError::Operation(error) => json!({
            "operationId":error.operation_id,
            "status":match &error.kind {
                OperationErrorKind::Http {status,..} => Some(*status),
                _ => None
            },
            "mutationMayHaveOccurred":error.mutation_may_have_occurred,
            "serviceCode":match &error.kind {
                OperationErrorKind::Http {code,..} => code.as_deref(),
                _ => None
            },
            "completed":false
        }),
        WarehouseReadError::Native(
            fabric_sdk::sql_databases::warehouse::WarehouseError::Response(error),
        ) => json!({
            "status":error.http_status, "operationId":error.operation_id, "completed":false
        }),
        _ => json!({"completed":false,"resumable":false}),
    }
}
