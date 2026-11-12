// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in metadata-only CopyJob binding for the shared v2 create host.

use crate::create_command::CreateBinding;
use fabric_sdk::{
    copyjobs::create::CopyJobCreate,
    items::create::{
        CreateCapabilities, CreateError, CreateInputValidator, CreatePlan, CreateTarget,
        InputDisposition, SchemaRef,
    },
};
use fabric_sdk_operations::FabricOperation;
use serde_json::Value;

/// Exact native CopyJob metadata creation; no definition or execution binding.
#[derive(Clone, Copy, Debug, Default)]
pub struct CopyJobCreateBinding;

impl CreateInputValidator for CopyJobCreateBinding {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        CopyJobCreate.validate(target, schema, supplied)
    }
}

impl CreateBinding for CopyJobCreateBinding {
    fn item_type(&self) -> &'static str {
        "CopyJob"
    }

    fn capabilities(&self) -> CreateCapabilities {
        CopyJobCreate.capabilities()
    }

    fn operation(
        &self,
        plan: &CreatePlan,
        raw: Option<&[u8]>,
        wait: bool,
    ) -> Result<FabricOperation, CreateError> {
        CopyJobCreate.operation(plan, raw, wait)
    }
}
