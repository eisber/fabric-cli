// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::ExistsError;
use fabric_v2_contracts::{
    Availability, Binding, ContractError, Effect, Generation, OperationDescriptor,
    PreparedOperation, Registry, ResolvedTarget, Target, TargetKind, TenantIdentity, Verb,
    WorkspaceIdentity,
};
use serde::Serialize;
use serde_json::json;

/// One operation identity shared by metadata Exists consumers.
pub const EXISTS_OPERATION_ID: &str = "fabric.metadata.exists";

/// Explicit identity or bounded name selector, never an arbitrary route.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum MetadataSelector {
    /// A syntactically validated provider UUID; not proof of existence.
    Id(String),
    /// A display name interpreted only within the selected parent and type.
    Name(String),
}

impl MetadataSelector {
    /// Validate an explicit UUID without credentials or HTTP.
    pub fn id(value: &str) -> Result<Self, ExistsError> {
        WorkspaceIdentity::new(value).map_err(|_| ExistsError::InvalidTarget)?;
        Ok(Self::Id(value.into()))
    }

    /// Validate an exact bounded name without guessing a provider identifier.
    pub fn name(value: &str) -> Result<Self, ExistsError> {
        if value.is_empty()
            || value.len() > fabric_sdk_discovery::MAX_TARGET_BYTES
            || value.trim() != value
            || matches!(value, "." | "..")
            || value.contains(['/', '\\', '*', '?', '#', '%', ':', '[', ']', '{', '}'])
            || value.chars().any(char::is_control)
        {
            return Err(ExistsError::InvalidTarget);
        }
        Ok(Self::Name(value.into()))
    }

    pub(super) fn validate(&self) -> Result<(), ExistsError> {
        match self {
            Self::Id(value) => Self::id(value),
            Self::Name(value) => Self::name(value),
        }
        .map(|_| ())
    }
}

/// Supported metadata-only scope; definitions, files and folders are deferred.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExistsTarget {
    /// One workspace, not tenant-wide physical existence.
    Workspace {
        /// Explicit UUID or workspace display name.
        workspace: MetadataSelector,
    },
    /// One root item under an explicit workspace.
    Item {
        /// Owning workspace selector.
        workspace: MetadataSelector,
        /// Item UUID or display name.
        item: MetadataSelector,
        /// Exact declared service type; required for a name, optional for IDs.
        item_type: Option<String>,
    },
}

/// Immutable caller-bound request; construction has no identity-provider effects.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExistsRequest {
    tenant_id: String,
    target: ExistsTarget,
}

impl ExistsRequest {
    /// Validate all target input before any credential or HTTP access.
    pub fn new(tenant_id: &str, target: ExistsTarget) -> Result<Self, ExistsError> {
        TenantIdentity::new(tenant_id).map_err(|_| ExistsError::InvalidTarget)?;
        let workspace = match &target {
            ExistsTarget::Workspace { workspace } => workspace,
            ExistsTarget::Item {
                workspace,
                item,
                item_type,
            } => {
                item.validate()?;
                if item_type.as_ref().is_some_and(|kind| {
                    kind.is_empty()
                        || kind.len() > 128
                        || !kind.bytes().all(|byte| byte.is_ascii_alphanumeric())
                        || matches!(
                            kind.as_str(),
                            "Workspace"
                                | "Personal"
                                | "Folder"
                                | "Capacity"
                                | "Connection"
                                | "Domain"
                                | "Gateway"
                                | "ManagedIdentity"
                                | "ManagedPrivateEndpoint"
                                | "SparkPool"
                        )
                }) || matches!(item, MetadataSelector::Name(_)) && item_type.is_none()
                {
                    return Err(ExistsError::InvalidTarget);
                }
                workspace
            }
        };
        workspace.validate()?;
        Ok(Self {
            tenant_id: tenant_id.into(),
            target,
        })
    }

    /// Explicit tenant binding, not inferred from a token or workspace name.
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// Original typed selector, without resolution or normalization.
    pub fn target(&self) -> &ExistsTarget {
        &self.target
    }
}

/// Shared operation description; no universal item/backend support is implied.
pub fn exists_descriptor() -> Result<OperationDescriptor, ContractError> {
    let selector = json!({"oneOf":[
        {"type":"object","required":["kind","value"],"additionalProperties":false,
            "properties":{"kind":{"const":"id"},"value":{"type":"string","format":"uuid"}}},
        {"type":"object","required":["kind","value"],"additionalProperties":false,
            "properties":{"kind":{"const":"name"},"value":{"type":"string","minLength":1,
                "description":"Bounded parent-scoped display name; unsafe/local/path selectors are rejected"}}}
    ]});
    OperationDescriptor::new(
        EXISTS_OPERATION_ID,
        Verb::Exists,
        json!({"type":"object","required":["tenantId","target"],"additionalProperties":false,"properties":{
            "tenantId":{"type":"string","format":"uuid"},
            "target":{"oneOf":[
                {"type":"object","required":["kind","workspace"],"additionalProperties":false,
                    "properties":{"kind":{"const":"workspace"},"workspace":selector}},
                {"type":"object","required":["kind","workspace","item","itemType"],"additionalProperties":false,
                    "properties":{"kind":{"const":"item"},"workspace":selector,"item":selector,
                        "itemType":{"type":["string","null"],"description":"Required exact type for names; optional for IDs; non-item types are rejected"}}}
            ]}
        }}),
        json!({"type":"object","required":["operationId","request","observation","exists","evidence"],
        "properties":{
            "exists":{"type":["boolean","null"]},
            "observation":{"enum":["present","absent","denied","ambiguous","incomplete","unknown","failed"]}
        }}),
        vec![Effect::RemoteRead],
    )
}

/// Prepare read intent over the explicit tenant without fabricating resolved IDs.
///
/// The immutable input retains every unresolved selector. Provider-established
/// workspace/item identities are returned separately in the observation.
pub fn prepare_exists(request: &ExistsRequest) -> Result<PreparedOperation, ExistsError> {
    let selection = Target {
        kind: TargetKind::TenantCollection,
        owner_type: "Tenant".into(),
        facet: "metadata-existence".into(),
        representation: "metadata".into(),
        generation: None,
    };
    let resolved = ResolvedTarget::tenant_collection(
        "/",
        TenantIdentity::new(request.tenant_id())?,
        selection.clone(),
    )?;
    let mut registry = Registry::default();
    registry.register(exists_descriptor()?)?;
    registry.bind(
        Binding::new(
            "Tenant",
            "metadata-existence",
            "metadata",
            Generation::Any,
            EXISTS_OPERATION_ID,
            Availability::Available,
        )?
        .with_target_kind(TargetKind::TenantCollection),
    )?;
    Ok(PreparedOperation::new(
        registry.resolve(Verb::Exists, &selection, None)?,
        resolved,
        serde_json::to_value(request).map_err(|_| ExistsError::Encoding)?,
    )?)
}
