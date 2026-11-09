// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    Owned, WarehouseDefinition, WarehouseDefinitionFormat, WarehouseError, WarehouseRequests,
    WarehouseTarget,
};
use fabric_sdk_sql_definition::{
    DefinitionError, DefinitionLimits, warehouse::parse_warehouse_definition_response,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod, ResourceOperation};

/// Documented supported identities, not verified claims or identity selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WarehouseDefinitionIdentity {
    /// Delegated user with applicable permissions and label usage rights.
    User,
    /// Explicit caller-selected application service principal.
    ServicePrincipal,
    /// Explicit caller-selected managed identity.
    ManagedIdentity,
}

/// One native definition POST, not an ordinary metadata GET or a deployment.
///
/// The request retains the selected owner. Native definition responses do not
/// echo owner IDs; content admission does not independently verify remote identity.
/// Raw execution belongs to the existing SDK facade/shared operation session.
#[derive(Debug)]
pub struct WarehouseDefinitionRequest {
    operation: ResourceOperation,
    owner: WarehouseTarget,
    format: WarehouseDefinitionFormat,
    limits: DefinitionLimits,
}

impl WarehouseDefinitionRequest {
    /// Fixed native POST with no body and nonpolling acceptance policy.
    pub fn operation(&self) -> &ResourceOperation {
        &self.operation
    }

    /// Explicit native Warehouse coordinates, never inferred from filenames.
    pub const fn owner(&self) -> WarehouseTarget {
        self.owner
    }

    /// Effective format, resolving omission to Warehouse's sqlproj default.
    pub const fn format(&self) -> WarehouseDefinitionFormat {
        self.format
    }

    /// All six caller-selected inclusive content bounds.
    pub const fn limits(&self) -> DefinitionLimits {
        self.limits
    }

    /// Read/write delegated alternatives; ordinary read scopes are insufficient.
    ///
    /// Actual permissions remain authoritative at the service. Encrypted labels
    /// can require additional usage rights; labels are excluded from definitions.
    pub const fn delegated_scopes(&self) -> &'static [&'static str] {
        &["Warehouse.ReadWrite.All", "Item.ReadWrite.All"]
    }

    /// Documented identity support; never requests a different credential.
    pub const fn supported_identities(&self) -> &'static [WarehouseDefinitionIdentity] {
        &[
            WarehouseDefinitionIdentity::User,
            WarehouseDefinitionIdentity::ServicePrincipal,
            WarehouseDefinitionIdentity::ManagedIdentity,
        ]
    }

    /// Admit the authoritative completed result without I/O or observation.
    ///
    /// The caller must establish successful result readiness with the existing
    /// shared observer using the same endpoint/identity and actual accepted ID.
    /// A monitor, metadata body or denial is not a definition.
    ///
    /// # Errors
    /// Returns protected shared codec classification; the input remains available
    /// to the caller as partial evidence. No part is dropped or truncated.
    pub fn decode_completed_result(
        &self,
        body: &[u8],
    ) -> Result<Owned<WarehouseDefinition>, DefinitionError> {
        Ok(Owned {
            owner: self.owner,
            data: parse_warehouse_definition_response(body, self.format, self.limits)?,
        })
    }
}

impl WarehouseRequests {
    /// Describe POST of one native Warehouse definition, with no request body.
    ///
    /// None omits the query and uses the service's sqlproj default; Some sends
    /// exactly the selected native format. This is separate from the five GET
    /// management-read inventory and never inherits ordinary read permissions.
    ///
    /// # Errors
    /// Rejects invalid fixed descriptor construction before credential work.
    pub fn get_definition(
        target: &WarehouseTarget,
        format: Option<WarehouseDefinitionFormat>,
        limits: DefinitionLimits,
    ) -> Result<WarehouseDefinitionRequest, WarehouseError> {
        let mut operation = ResourceOperation::new(
            ResourceMethod::Post,
            &[
                "workspaces",
                &target.workspace_id().to_string(),
                "warehouses",
                &target.warehouse_id().to_string(),
                "getDefinition",
            ],
            None,
            CompletionPolicy::ReturnAccepted,
        )
        .map_err(|_| WarehouseError::InvalidInput)?;
        if let Some(format) = format {
            operation = operation
                .with_query(&[("format", format.as_str())])
                .map_err(|_| WarehouseError::InvalidInput)?;
        }
        Ok(WarehouseDefinitionRequest {
            operation,
            owner: *target,
            format: format.unwrap_or_default(),
            limits,
        })
    }
}
