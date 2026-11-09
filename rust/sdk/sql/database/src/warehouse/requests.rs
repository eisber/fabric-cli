// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{ConnectionMetadata, Owned, RestorePoint, RestorePointId, RestorePointPage};
use super::{Warehouse, WarehousePage, WarehouseTarget};
use crate::{
    ContinuationToken, FolderScope, Guid, SqlResponseError, SqlResponseErrorKind, WorkspaceId,
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::Value;
use std::fmt;

/// Local or shared read failure; implicit diagnostics never disclose service data.
#[derive(Clone, PartialEq)]
pub enum WarehouseError {
    /// Invalid native ID, option or fixed descriptor.
    InvalidInput,
    /// Caller supplied an owner type other than exactly Warehouse.
    WrongOwner,
    /// Original shared HTTP/authentication/cancellation/limit evidence.
    Operation(ResourceError),
    /// Protected completed-response or unexpected-acceptance evidence.
    Response(SqlResponseError),
}

impl fmt::Debug for WarehouseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput => f.write_str("WarehouseError::InvalidInput"),
            Self::WrongOwner => f.write_str("WarehouseError::WrongOwner"),
            Self::Response(error) => fmt::Debug::fmt(error, f),
            Self::Operation(error) => f
                .debug_struct("WarehouseError::Operation")
                .field("status", &error.status)
                .field("has_operation_id", &error.operation_id.is_some())
                .finish(),
        }
    }
}

impl fmt::Display for WarehouseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Warehouse management read failed or is incomplete; inspect typed evidence")
    }
}

impl std::error::Error for WarehouseError {}

/// Documented resource permissions, not a local authorization check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WarehousePermission {
    /// Read item metadata or restore points.
    ItemRead,
    /// Read Warehouse and Viewer-or-higher workspace role for connection metadata.
    ItemReadAndWorkspaceViewer,
    /// Viewer-or-higher workspace role for Warehouse enumeration.
    WorkspaceViewer,
}

/// Only documented native management reads, not a global verb registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WarehouseRead {
    /// Native Warehouse metadata GET.
    Metadata,
    /// Native Warehouse LIST, one page.
    List,
    /// Native connection metadata GET, not SQL execution.
    Connection,
    /// Native restore-point LIST, one page.
    RestorePoints,
    /// One exact native restore-point GET.
    RestorePoint,
}

impl WarehouseRead {
    /// Complete operation inventory of this isolated read adapter.
    pub const ALL: [Self; 5] = [
        Self::Metadata,
        Self::List,
        Self::Connection,
        Self::RestorePoints,
        Self::RestorePoint,
    ];

    /// Native method; this inventory never contains POST or mutations.
    pub const fn method(self) -> ResourceMethod {
        ResourceMethod::Get
    }

    /// Documented permission; the provider remains authoritative.
    pub const fn permission(self) -> WarehousePermission {
        match self {
            Self::List => WarehousePermission::WorkspaceViewer,
            Self::Connection => WarehousePermission::ItemReadAndWorkspaceViewer,
            Self::Metadata | Self::RestorePoints | Self::RestorePoint => {
                WarehousePermission::ItemRead
            }
        }
    }

    /// Delegated alternatives, distinct from audience and actual effective grants.
    pub const fn delegated_scopes(self) -> &'static [&'static str] {
        match self {
            Self::List => &["Workspace.Read.All", "Workspace.ReadWrite.All"],
            _ => &[
                "Warehouse.Read.All",
                "Warehouse.ReadWrite.All",
                "Item.Read.All",
                "Item.ReadWrite.All",
            ],
        }
    }
}

/// Explicit folder/recursion choices; native defaults are never inferred.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListWarehouses {
    /// Always sent explicitly to the provider.
    pub recursive: bool,
    /// Workspace root or one concrete folder.
    pub root_folder: FolderScope,
    /// Actual decoded prior-page token; no automatic traversal.
    pub continuation_token: Option<ContinuationToken>,
}

/// Native connection endpoint selector, not permission or account selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivateLinkType {
    /// Public connection metadata.
    None,
    /// Workspace-private-link connection metadata.
    Workspace,
}

/// Optional explicit connection metadata selectors; no tenant/identity fallback.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConnectionOptions {
    /// Guest tenant query parameter, not a credential override.
    pub guest_tenant_id: Option<Guid>,
    /// Native network selector, preserving omitted versus explicit None.
    pub private_link_type: Option<PrivateLinkType>,
}

struct Expected {
    workspace: WorkspaceId,
    owner: Option<WarehouseTarget>,
    restore_point: Option<RestorePointId>,
}

/// Typed fixed-route descriptor consumed by the existing shared execution policy.
///
/// No HTTP, runtime, credentials, pagination or monitor is configured here.
pub struct WarehouseRequest<T> {
    operation: ResourceOperation,
    read: WarehouseRead,
    expected: Expected,
    decoder: fn(Value, &Expected) -> Result<T, SqlResponseErrorKind>,
}

impl<T> fmt::Debug for WarehouseRequest<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WarehouseRequest")
            .field("operation", &self.operation)
            .field("read", &self.read)
            .finish()
    }
}

impl<T> WarehouseRequest<T> {
    /// Validated fixed route and non-waiting shared completion policy.
    pub fn operation(&self) -> &ResourceOperation {
        &self.operation
    }

    /// Native read descriptor for host capability/permission discovery.
    pub const fn read(&self) -> WarehouseRead {
        self.read
    }

    /// Decode exactly one completed 200 without following any returned monitor.
    ///
    /// # Errors
    /// Malformed/foreign identity and undocumented statuses are non-success;
    /// unexpected acceptance preserves its handle rather than polling.
    pub fn decode(&self, outcome: ResourceOutcome) -> Result<T, WarehouseError> {
        crate::requests::decode_metadata_response(outcome, |data| {
            (self.decoder)(data, &self.expected)
        })
        .map_err(WarehouseError::Response)
    }

    /// Submit one GET through the caller's existing shared policy.
    ///
    /// # Errors
    /// Preserves shared authentication, service, cancellation and partial evidence.
    pub async fn execute(self, executor: &mut dyn ResourceOperations) -> Result<T, WarehouseError> {
        let outcome = executor
            .execute(self.operation.clone())
            .await
            .map_err(WarehouseError::Operation)?;
        self.decode(outcome)
    }
}

/// Warehouse-specific descriptors without mutation or SQL fallback.
pub struct WarehouseRequests;

impl WarehouseRequests {
    /// GET the exact Warehouse's metadata, without augmentations.
    ///
    /// # Errors
    /// Returns a sanitized descriptor error before execution.
    pub fn get(target: &WarehouseTarget) -> Result<WarehouseRequest<Warehouse>, WarehouseError> {
        item_request(
            target,
            &[],
            WarehouseRead::Metadata,
            None,
            |data, expected| {
                let warehouse: Warehouse = decode(data)?;
                check_owner(&warehouse, expected)?;
                Ok(warehouse)
            },
        )
    }

    /// GET one Warehouse collection page with explicit folder/recursion options.
    ///
    /// # Errors
    /// Rejects invalid descriptor options; response ownership is checked per row.
    pub fn list_page(
        workspace: &WorkspaceId,
        options: &ListWarehouses,
    ) -> Result<WarehouseRequest<WarehousePage>, WarehouseError> {
        let mut query = vec![("recursive", options.recursive.to_string())];
        if let FolderScope::Folder(folder) = options.root_folder {
            query.push(("rootFolderId", folder.to_string()));
        }
        if let Some(token) = &options.continuation_token {
            query.push(("continuationToken", token.as_str().to_owned()));
        }
        let operation = get_operation(&["workspaces", &workspace.to_string(), "warehouses"])?;
        Ok(WarehouseRequest {
            operation: query_operation(operation, &query)?,
            read: WarehouseRead::List,
            expected: Expected {
                workspace: *workspace,
                owner: None,
                restore_point: None,
            },
            decoder: |data, expected| {
                let page: WarehousePage = decode(data)?;
                for warehouse in &page.value {
                    check_owner(warehouse, expected)?;
                }
                Ok(page)
            },
        })
    }

    /// GET connection metadata tied to its selected owner, without connecting.
    ///
    /// # Errors
    /// Missing/blank connection strings and malformed responses fail explicitly.
    pub fn connection(
        target: &WarehouseTarget,
        options: ConnectionOptions,
    ) -> Result<WarehouseRequest<Owned<ConnectionMetadata>>, WarehouseError> {
        let mut request = item_request(
            target,
            &["connectionString"],
            WarehouseRead::Connection,
            None,
            |data, expected| {
                let connection: ConnectionMetadata = decode(data)?;
                if connection.connection_string.as_str().trim().is_empty() {
                    return Err(SqlResponseErrorKind::InvalidShape);
                }
                owned(connection, expected)
            },
        )?;
        let mut query = Vec::new();
        if let Some(tenant) = options.guest_tenant_id {
            query.push(("guestTenantId", tenant.to_string()));
        }
        if let Some(link) = options.private_link_type {
            query.push((
                "privateLinkType",
                match link {
                    PrivateLinkType::None => "None",
                    PrivateLinkType::Workspace => "Workspace",
                }
                .to_owned(),
            ));
        }
        if !query.is_empty() {
            request.operation = query_operation(request.operation, &query)?;
        }
        Ok(request)
    }

    /// GET one native restore-point page, with no ID/time inference.
    ///
    /// # Errors
    /// Invalid descriptors or row shapes fail; continuations stay inert.
    pub fn restore_points_page(
        target: &WarehouseTarget,
        continuation: Option<&ContinuationToken>,
    ) -> Result<WarehouseRequest<Owned<RestorePointPage>>, WarehouseError> {
        let mut request = item_request(
            target,
            &["restorePoints"],
            WarehouseRead::RestorePoints,
            None,
            |data, expected| owned(decode(data)?, expected),
        )?;
        if let Some(token) = continuation {
            request.operation = query_operation(
                request.operation,
                &[("continuationToken", token.as_str().to_owned())],
            )?;
        }
        Ok(request)
    }

    /// GET one exact restore point, never the newest or nearest timestamp.
    ///
    /// # Errors
    /// Rejects a returned point different from the requested native key.
    pub fn restore_point(
        target: &WarehouseTarget,
        point: &RestorePointId,
    ) -> Result<WarehouseRequest<Owned<RestorePoint>>, WarehouseError> {
        item_request(
            target,
            &["restorePoints", point.as_str()],
            WarehouseRead::RestorePoint,
            Some(point.clone()),
            |data, expected| {
                let point: RestorePoint = decode(data)?;
                if expected.restore_point.as_ref() != Some(&point.id) {
                    return Err(SqlResponseErrorKind::IdentityMismatch);
                }
                owned(point, expected)
            },
        )
    }
}

fn decode<T: serde::de::DeserializeOwned>(data: Value) -> Result<T, SqlResponseErrorKind> {
    serde_json::from_value(data).map_err(|_| SqlResponseErrorKind::InvalidShape)
}

fn owned<T>(data: T, expected: &Expected) -> Result<Owned<T>, SqlResponseErrorKind> {
    Ok(Owned {
        owner: expected
            .owner
            .ok_or(SqlResponseErrorKind::IdentityMismatch)?,
        data,
    })
}

fn check_owner(warehouse: &Warehouse, expected: &Expected) -> Result<(), SqlResponseErrorKind> {
    if warehouse.workspace_id != expected.workspace
        || expected
            .owner
            .is_some_and(|owner| warehouse.id != owner.warehouse_id())
    {
        return Err(SqlResponseErrorKind::IdentityMismatch);
    }
    Ok(())
}

fn item_request<T>(
    target: &WarehouseTarget,
    children: &[&str],
    read: WarehouseRead,
    restore_point: Option<RestorePointId>,
    decoder: fn(Value, &Expected) -> Result<T, SqlResponseErrorKind>,
) -> Result<WarehouseRequest<T>, WarehouseError> {
    let workspace = target.workspace_id().to_string();
    let warehouse = target.warehouse_id().to_string();
    let mut segments = vec!["workspaces", &workspace, "warehouses", &warehouse];
    segments.extend_from_slice(children);
    let operation = if restore_point.is_some() {
        ResourceOperation::encoded(
            ResourceMethod::Get,
            &segments,
            None,
            CompletionPolicy::ReturnAccepted,
        )
        .map_err(|_| WarehouseError::InvalidInput)?
    } else {
        get_operation(&segments)?
    };
    Ok(WarehouseRequest {
        operation,
        read,
        expected: Expected {
            workspace: target.workspace_id(),
            owner: Some(*target),
            restore_point,
        },
        decoder,
    })
}

fn get_operation(segments: &[&str]) -> Result<ResourceOperation, WarehouseError> {
    ResourceOperation::new(
        ResourceMethod::Get,
        segments,
        None,
        CompletionPolicy::ReturnAccepted,
    )
    .map_err(|_| WarehouseError::InvalidInput)
}

fn query_operation(
    operation: ResourceOperation,
    query: &[(&str, String)],
) -> Result<ResourceOperation, WarehouseError> {
    let refs: Vec<_> = query
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    operation
        .with_query(&refs)
        .map_err(|_| WarehouseError::InvalidInput)
}
