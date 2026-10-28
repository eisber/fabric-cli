// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{AccessClient, Resource};
use azure_core::http::Method;
use fabric_sdk_api::{ApiError, Request, Response, require_status};
use serde_json::json;
use std::{future::Future, num::NonZeroUsize};

/// Source principal probing order; each variant produces a distinct request body.
#[derive(Clone, Copy, Debug)]
pub enum PrincipalType {
    /// Entra user.
    User,
    /// Entra application service principal.
    ServicePrincipal,
    /// Entra group.
    Group,
    /// Service-principal profile.
    ServicePrincipalProfile,
}

impl PrincipalType {
    /// Pinned source order, not an ambient principal lookup.
    pub const SOURCE_ORDER: [Self; 4] = [
        Self::User,
        Self::ServicePrincipal,
        Self::Group,
        Self::ServicePrincipalProfile,
    ];

    /// Case-sensitive wire value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::ServicePrincipal => "ServicePrincipal",
            Self::Group => "Group",
            Self::ServicePrincipalProfile => "ServicePrincipalProfile",
        }
    }
}

/// A single ACL write. Orchestration/confirmation belongs to the CLI consumer.
#[derive(Clone, Debug)]
pub enum AclMutation<'a> {
    /// Add one explicit principal type. No executor retries a mutation.
    Add {
        /// Principal object identifier supplied by the user.
        identity: &'a str,
        /// Explicit source principal kind.
        principal_type: PrincipalType,
        /// Validated role, preserving the supplied case.
        role: &'a str,
    },
    /// Update the source-selected assignment route.
    Update {
        /// Source uses the supplied identity here, not the lookup assignment ID.
        identity: &'a str,
        /// Validated source role.
        role: &'a str,
    },
    /// Delete the actual matched assignment, not its principal object ID.
    Remove {
        /// Matched role-assignment identifier.
        assignment: &'a str,
    },
}

/// An explicitly resolved item for the Fabric administrator label APIs.
#[derive(Clone, Debug)]
pub struct LabelItem {
    /// Fabric item identifier.
    pub id: String,
    /// Canonical Fabric item type; SDK does not apply CLI support restrictions.
    pub item_type: String,
}

/// Sensitivity-label assignment or removal.
#[derive(Clone, Debug)]
pub enum LabelMutation<'a> {
    /// Apply a locally resolved label ID using assignmentMethod Standard.
    Set(&'a str),
    /// Remove the sensitivity label.
    Remove,
}

impl Resource {
    /// Source roles accepted case-insensitively for ACL writes.
    /// Items and OneLake data roles are read-only in the source ACL commands.
    pub const fn roles(&self) -> Option<&'static [&'static str]> {
        match self {
            Self::Workspace(_) => Some(&["Admin", "Member", "Contributor", "Viewer"]),
            Self::Connection(_) => Some(&["Owner", "UserWithReshare", "User"]),
            Self::Gateway(_) => Some(&[
                "Admin",
                "ConnectionCreator",
                "ConnectionCreatorWithResharing",
            ]),
            _ => None,
        }
    }

    /// Builds one resource-specific write; rejects unsupported resources before I/O.
    pub fn mutation_request(&self, mutation: AclMutation<'_>) -> Result<Request, ApiError> {
        let mut segments: Vec<&str> = match self {
            Self::Workspace(id) => vec!["workspaces", id, "roleAssignments"],
            Self::Connection(id) => vec!["connections", id, "roleAssignments"],
            Self::Gateway(id) => vec!["gateways", id, "roleAssignments"],
            _ => return Err(ApiError::InvalidInput),
        };
        let role = match &mutation {
            AclMutation::Add { role, .. } | AclMutation::Update { role, .. } => Some(*role),
            _ => None,
        };
        if role.is_some_and(|role| {
            !self
                .roles()
                .expect("write resource")
                .iter()
                .any(|valid| valid.eq_ignore_ascii_case(role))
        }) {
            return Err(ApiError::InvalidInput);
        }
        match mutation {
            AclMutation::Add {
                identity,
                principal_type,
                role,
            } => {
                if identity.is_empty() || identity.chars().any(char::is_control) {
                    return Err(ApiError::InvalidInput);
                }
                Request::fabric(Method::Post,&segments)?.with_json(&json!({"principal":{"id":identity,"type":principal_type.as_str()},"role":role}))
            }
            AclMutation::Update { identity, role } => {
                segments.push(identity);
                Request::fabric(Method::Patch, &segments)?.with_json(&json!({"role":role}))
            }
            AclMutation::Remove { assignment } => {
                segments.push(assignment);
                Request::fabric(Method::Delete, &segments)
            }
        }
    }
}

impl AccessClient {
    /// Executes one confirmed ACL mutation through the shared API executor.
    /// Uses the shared Python synchronous operation policy without write replay.
    pub async fn mutate_acl(
        &self,
        resource: &Resource,
        mutation: AclMutation<'_>,
        limit: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Response, ApiError> {
        let request = resource.mutation_request(mutation)?;
        let response = self.api.send_completed(&request, limit, cancelled).await?;
        require_status(&response, &[200, 201, 202, 204])?;
        Ok(response)
    }

    /// Applies/removes a label using the Fabric administrator bulk endpoint.
    /// No Power BI route, storage token or write replay is used.
    pub async fn mutate_label(
        &self,
        item: &LabelItem,
        mutation: LabelMutation<'_>,
        limit: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Response, ApiError> {
        if item.id.is_empty()
            || item.item_type.is_empty()
            || item
                .id
                .chars()
                .chain(item.item_type.chars())
                .any(char::is_control)
        {
            return Err(ApiError::InvalidInput);
        }
        let items = json!([{"id":item.id,"type":item.item_type}]);
        let (route, body) = match mutation {
            LabelMutation::Set(label) if !label.is_empty() => (
                "bulkSetLabels",
                json!({"items":items,"labelId":label,"assignmentMethod":"Standard"}),
            ),
            LabelMutation::Set(_) => return Err(ApiError::InvalidInput),
            LabelMutation::Remove => ("bulkRemoveLabels", json!({"items":items})),
        };
        let request =
            Request::fabric(Method::Post, &["admin", "items", route])?.with_json(&body)?;
        let response = self.api.send_completed(&request, limit, cancelled).await?;
        require_status(&response, &[200, 201, 202, 204])?;
        Ok(response)
    }
}
