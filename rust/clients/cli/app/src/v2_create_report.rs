// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Typed Report selection for the shared opt-in create host.

use crate::create_command::{CreateBinding, MAX_BYTES};
use fabric_sdk::{
    items::create::{
        ConflictPolicy, CreateAction, CreateCapabilities, CreateError, CreateInputValidator,
        CreatePlan, CreateTarget, InputDisposition, SchemaRef,
    },
    reports::create::{ReportCreateValidator, capabilities},
};
use fabric_sdk_operations::FabricOperation;
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::Value;
use std::num::NonZeroUsize;

/// One native Report definition-create binding; no related model is resolved.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReportCreateBinding;

impl CreateInputValidator for ReportCreateBinding {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        let disposition = validator().validate(target, schema, supplied)?;
        if supplied
            .and_then(|body| body.get("folderId"))
            .is_some_and(|folder| !folder.is_null())
        {
            return Err(CreateError::InputRejected);
        }
        Ok(disposition)
    }
}

impl CreateBinding for ReportCreateBinding {
    fn item_type(&self) -> &'static str {
        "Report"
    }

    fn capabilities(&self) -> CreateCapabilities {
        capabilities()
    }

    fn operation(
        &self,
        plan: &CreatePlan,
        raw: Option<&[u8]>,
        wait: bool,
    ) -> Result<FabricOperation, CreateError> {
        if !matches!(plan.policy(), ConflictPolicy::Error | ConflictPolicy::Skip) {
            return Err(CreateError::UnsupportedPolicy);
        }
        let CreateAction::Create { input } = plan.action() else {
            return Err(CreateError::InputRejected);
        };
        self.validate(input.target(), input.schema(), input.supplied())?;
        let raw = raw
            .filter(|raw| raw.len() <= MAX_BYTES)
            .ok_or(CreateError::InputRejected)?;
        let parsed = fabric_cli_output::input::strict_json_bounded(raw, MAX_BYTES)
            .map_err(|_| CreateError::InputRejected)?;
        if input.supplied() != Some(&parsed) {
            return Err(CreateError::InputRejected);
        }
        FabricOperation::new(
            ResourceMethod::Post,
            &format!(
                "/v1/workspaces/{}/reports",
                input.target().parent_id().as_str()
            ),
            raw.to_vec(),
            if wait {
                CompletionPolicy::WaitForResult
            } else {
                CompletionPolicy::ReturnAccepted
            },
        )
        .map_err(|_| CreateError::InputRejected)
    }
}

fn validator() -> ReportCreateValidator {
    ReportCreateValidator::new(NonZeroUsize::new(MAX_BYTES).expect("positive shared create bound"))
}
