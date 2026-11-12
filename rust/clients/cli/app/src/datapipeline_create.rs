// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Metadata-only DataPipeline creation, not definition publication or execution.

use crate::{
    create_command::{CreateBinding, MAX_BYTES},
    get_target::is_id,
};
use fabric_sdk::items::{
    ItemCollection,
    create::{
        CreateAction, CreateCapabilities, CreateError, CreateInputValidator, CreatePlan,
        CreateSupport, CreateTarget, DuplicateSupport, InputDisposition, SchemaRef, SkipSupport,
        TargetScope,
    },
};
use fabric_sdk_operations::FabricOperation;
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod, ResourceOperation};
use serde_json::{Value, json};

const OWNER: &str = "DataPipeline";
const SCHEMA: &str = "fabric.datapipeline.create-metadata/v1";

/// Positively selected native Create Data Pipeline metadata contract.
///
/// The API documents metadata-only creation and `ItemDisplayNameAlreadyInUse`.
/// That service rejection, not the discovery snapshot, backs `error`/`skip`.
/// Definition, folder, sensitivity-label and job inputs are not in this slice.
pub struct DataPipelineCreateBinding;

impl CreateInputValidator for DataPipelineCreateBinding {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        if target.scope() != TargetScope::NamedLeaf
            || target.item_type() != OWNER
            || !is_id(target.parent_id().as_str())
            || schema.as_str() != SCHEMA
        {
            return Err(CreateError::InputRejected);
        }
        if let Some(supplied) = supplied {
            let body = supplied.as_object().ok_or(CreateError::InputRejected)?;
            if body
                .keys()
                .any(|key| !matches!(key.as_str(), "displayName" | "description"))
                || body.get("displayName").and_then(Value::as_str) != Some(target.name())
                || body.get("description").is_some_and(|value| {
                    !value
                        .as_str()
                        .is_some_and(|value| value.chars().count() <= 256)
                })
            {
                return Err(CreateError::InputRejected);
            }
        }
        Ok(InputDisposition::CreateOnly)
    }
}

impl CreateBinding for DataPipelineCreateBinding {
    fn item_type(&self) -> &'static str {
        OWNER
    }

    fn capabilities(&self) -> CreateCapabilities {
        CreateCapabilities {
            schema: SchemaRef::new(SCHEMA).expect("static DataPipeline schema"),
            create: CreateSupport::RejectExisting,
            replacement: None,
            skip: SkipSupport::ExactExistingUnchanged,
            duplicate: DuplicateSupport::Unsupported,
        }
    }

    fn operation(
        &self,
        plan: &CreatePlan,
        raw: Option<&[u8]>,
        wait: bool,
    ) -> Result<FabricOperation, CreateError> {
        let CreateAction::Create { input } = plan.action() else {
            return Err(CreateError::UnsupportedPolicy);
        };
        if input.target() != plan.target() || input.schema() != &self.capabilities().schema {
            return Err(CreateError::InputRejected);
        }
        self.validate(plan.target(), input.schema(), input.supplied())?;
        let payload = input
            .supplied()
            .cloned()
            .unwrap_or_else(|| json!({"displayName":plan.target().name()}));
        let bytes = match (raw, input.supplied()) {
            (Some(raw), Some(supplied))
                if fabric_cli_output::input::strict_json_bounded(raw, MAX_BYTES)
                    .is_ok_and(|value| &value == supplied) =>
            {
                raw.to_vec()
            }
            (None, None) => serde_json::to_vec(&payload).map_err(|_| CreateError::InputRejected)?,
            _ => return Err(CreateError::InputRejected),
        };
        let operation = ResourceOperation::new(
            ResourceMethod::Post,
            &[
                "workspaces",
                plan.target().parent_id().as_str(),
                ItemCollection::DataPipeline.as_str(),
            ],
            Some(payload),
            if wait {
                CompletionPolicy::WaitForResult
            } else {
                CompletionPolicy::ReturnAccepted
            },
        )
        .map_err(|_| CreateError::InputRejected)?;
        FabricOperation::new(
            operation.method(),
            operation.path(),
            bytes,
            operation.completion(),
        )
        .map_err(|_| CreateError::InputRejected)
    }
}
