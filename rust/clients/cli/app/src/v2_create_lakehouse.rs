// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Lakehouse-only binding for the released shared v2 create host.

use crate::create_command::CreateBinding;
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
        LakehouseCreateSchema.validate(target, schema, supplied)
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
        let operation = request
            .operation(if wait {
                CompletionPolicy::WaitForResult
            } else {
                CompletionPolicy::ReturnAccepted
            })
            .map_err(|_| CreateError::InputRejected)?;
        FabricOperation::from_resource(&operation).map_err(|_| CreateError::InputRejected)
    }
}
