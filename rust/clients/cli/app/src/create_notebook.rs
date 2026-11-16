// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The one positively mounted Notebook create binding.

use crate::create_command::CreateBinding;
use fabric_sdk::{
    items::create::{
        CreateCapabilities, CreateError, CreateInputValidator, CreatePlan, CreateTarget,
        InputDisposition, SchemaRef,
    },
    notebook_create,
};
use fabric_sdk_operations::FabricOperation;
use serde_json::Value;

/// Native Notebook schema/endpoint; no other artifact owner is selected.
pub struct NotebookBinding;

impl CreateInputValidator for NotebookBinding {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        notebook_create::NotebookCreateValidator.validate(target, schema, supplied)
    }
}

impl CreateBinding for NotebookBinding {
    fn item_type(&self) -> &'static str {
        "Notebook"
    }
    fn capabilities(&self) -> CreateCapabilities {
        notebook_create::capabilities()
    }
    fn operation(
        &self,
        plan: &CreatePlan,
        raw: Option<&[u8]>,
        wait: bool,
    ) -> Result<FabricOperation, CreateError> {
        notebook_create::operation(plan, raw, wait)
    }
}
