// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Resource-specific ACL requests from Python cf67fd52, using shared execution.

use azure_core::http::Method;
use fabric_sdk_api::{ApiClient, ApiError, Collection, CollectionError, Limits, Paging, Request};
use std::future::Future;

mod mutations;
pub use mutations::{AclMutation, LabelItem, LabelMutation, PrincipalType};
/// Resource-specific ACL read routes. Item access is an administrator API;
/// Lakehouse data roles are Fabric APIs, not storage-token requests.
#[derive(Clone, Debug, PartialEq)]
pub enum Resource {
    /// Workspace role assignments.
    Workspace(String),
    /// Administrator item access details.
    Item {
        /// Workspace identifier.
        workspace: String,
        /// Item identifier.
        item: String,
    },
    /// Tenant-level gateway role assignments.
    Gateway(String),
    /// Tenant-level connection role assignments.
    Connection(String),
    /// Lakehouse-only OneLake security data roles.
    LakehouseDataRoles {
        /// Workspace identifier.
        workspace: String,
        /// Lakehouse identifier.
        item: String,
    },
}

impl Resource {
    /// Builds the exact source collection GET; never substitutes generic item ACLs.
    pub fn read_request(&self) -> Result<Request, ApiError> {
        let segments = match self {
            Self::Workspace(id) => vec!["workspaces", id, "roleAssignments"],
            Self::Item { workspace, item } => {
                vec!["admin", "workspaces", workspace, "items", item, "users"]
            }
            Self::Gateway(id) => vec!["gateways", id, "roleAssignments"],
            Self::Connection(id) => vec!["connections", id, "roleAssignments"],
            Self::LakehouseDataRoles { workspace, item } => {
                vec!["workspaces", workspace, "items", item, "dataAccessRoles"]
            }
        };
        Request::fabric(Method::Get, &segments)
    }

    /// Source payload array, which is not `value` for administrator item reads.
    pub const fn collection_field(&self) -> &'static str {
        if matches!(self, Self::Item { .. }) {
            "accessDetails"
        } else {
            "value"
        }
    }
}

/// Read operations over an explicitly composed API client.
pub struct AccessClient {
    api: ApiClient,
}

impl AccessClient {
    /// Reuses caller-supplied credentials, selection, HTTP policies and runtime.
    pub fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// Enumerates the complete source ACL collection for both `acl ls` and `acl get`.
    /// Errors retain successfully decoded partial rows, never report partial success.
    pub async fn list(
        &self,
        resource: &Resource,
        limits: Limits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Collection, CollectionError> {
        let request = resource.read_request().map_err(|error| CollectionError {
            error,
            partial: Box::default(),
        })?;
        self.api
            .collect(
                request,
                resource.collection_field(),
                Paging::Query,
                limits,
                cancelled,
            )
            .await
    }
}
