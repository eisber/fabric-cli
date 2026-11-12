// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Lakehouse-only binding for the released shared v2 create host.

use crate::create_command::{AnticipatedCreateEffect, CreateBinding, CreateEffectKind};
use fabric_sdk::items::{
    create::{
        CreateAction, CreateCapabilities, CreateError, CreateInputValidator, CreatePlan,
        CreateTarget, InputDisposition, SchemaRef,
    },
    lakehouse_create::{LakehouseCreateInput, LakehouseCreateRequest, LakehouseCreateSchema},
};
use fabric_sdk_operations::FabricOperation;
use fabric_sdk_workspaces::mutations::CompletionPolicy;
use serde_json::Value;

/// Positively selected native Lakehouse leaf, without host or identity discovery.
pub struct LakehouseCreateBinding {
    capabilities: CreateCapabilities,
}

impl LakehouseCreateBinding {
    /// Capture the typed SDK's fixed provider-backed capabilities without I/O.
    ///
    /// # Errors
    /// Preserves an invalid SDK schema-reference error.
    pub fn new() -> Result<Self, CreateError> {
        Ok(Self {
            capabilities: LakehouseCreateSchema::capabilities()?,
        })
    }
}

impl CreateInputValidator for LakehouseCreateBinding {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        let disposition = LakehouseCreateSchema.validate(target, schema, supplied)?;
        require_workspace_root(supplied)?;
        Ok(disposition)
    }
}

impl CreateBinding for LakehouseCreateBinding {
    fn item_type(&self) -> &'static str {
        "Lakehouse"
    }

    fn capabilities(&self) -> CreateCapabilities {
        self.capabilities.clone()
    }

    fn operation(
        &self,
        plan: &CreatePlan,
        raw: Option<&[u8]>,
        wait: bool,
    ) -> Result<FabricOperation, CreateError> {
        let CreateAction::Create { input } = plan.action() else {
            return Err(CreateError::InputRejected);
        };
        let decoded = raw
            .map(LakehouseCreateInput::from_bytes)
            .transpose()
            .map_err(|_| CreateError::InputRejected)?;
        if decoded.as_ref().map(LakehouseCreateInput::value) != input.supplied() {
            return Err(CreateError::InputRejected);
        }
        let request =
            LakehouseCreateRequest::from_plan(plan).map_err(|_| CreateError::InputRejected)?;
        require_workspace_root(Some(request.body()))?;
        let operation = request
            .operation(if wait {
                CompletionPolicy::WaitForResult
            } else {
                CompletionPolicy::ReturnAccepted
            })
            .map_err(|_| CreateError::InputRejected)?;
        FabricOperation::from_resource(&operation).map_err(|_| CreateError::InputRejected)
    }

    fn anticipated_effects(
        &self,
        plan: &CreatePlan,
    ) -> Result<Vec<AnticipatedCreateEffect>, CreateError> {
        let request =
            LakehouseCreateRequest::from_plan(plan).map_err(|_| CreateError::InputRejected)?;
        require_workspace_root(Some(request.body()))?;
        Ok(vec![AnticipatedCreateEffect::new(
            CreateEffectKind::ChildResourceCreation,
            Some("SQLEndpoint"),
            "Fabric automatically provisions a SQL analytics endpoint alongside the Lakehouse. \
             This is an anticipated service-owned effect, not another CLI request or evidence \
             that the endpoint exists or is ready.",
            "https://learn.microsoft.com/en-us/fabric/data-engineering/lakehouse-api \
             @3893b03d0246df2a1978058f5542399274d1b243",
        )?])
    }
}

fn require_workspace_root(body: Option<&Value>) -> Result<(), CreateError> {
    if body
        .and_then(|body| body.get("folderId"))
        .is_some_and(|folder| !folder.is_null())
    {
        return Err(CreateError::InputRejected);
    }
    Ok(())
}
