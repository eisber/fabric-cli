// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Empty SparkJobDefinition item creation, not job configuration or execution.

use crate::{
    create_command::{CreateBinding, MAX_BYTES},
    get_target::{GetRepresentation, GetTarget, is_id},
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

const OWNER: &str = "SparkJobDefinition";
const SCHEMA: &str = "fabric.sparkJobDefinition.empty-create/v1";

/// Explicit opt-in leaf over the shared SDK policy and native create host.
pub struct EmptySparkJobDefinition;

impl CreateInputValidator for EmptySparkJobDefinition {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        let path = format!(
            "/{}.Workspace/{}.{}",
            target.parent_id().as_str(),
            target.name(),
            OWNER,
        );
        if target.scope() != TargetScope::NamedLeaf
            || target.item_type() != OWNER
            || schema.as_str() != SCHEMA
            || !is_id(target.parent_id().as_str())
            || !matches!(
                GetTarget::parse(&path),
                Ok(GetTarget::Item {
                    workspace,
                    item,
                    item_type: Some(owner),
                    representation: GetRepresentation::Metadata,
                }) if workspace == target.parent_id().as_str()
                    && item == target.name() && owner == OWNER
            )
        {
            return Err(CreateError::InputRejected);
        }
        if let Some(supplied) = supplied {
            let object = supplied.as_object().ok_or(CreateError::InputRejected)?;
            for (key, value) in object {
                let valid = match key.as_str() {
                    "displayName" => value.as_str() == Some(target.name()),
                    "type" => value.as_str() == Some(OWNER),
                    "folderId" => value.is_null(),
                    "description" => value
                        .as_str()
                        .is_some_and(|text| text.chars().count() <= 256),
                    _ => false,
                };
                if !valid {
                    return Err(CreateError::InputRejected);
                }
            }
        }
        Ok(InputDisposition::CreateOnly)
    }
}

impl CreateBinding for EmptySparkJobDefinition {
    fn item_type(&self) -> &'static str {
        OWNER
    }

    fn capabilities(&self) -> CreateCapabilities {
        CreateCapabilities {
            schema: SchemaRef::new(SCHEMA).expect("fixed empty SparkJobDefinition schema"),
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
        if input.target() != plan.target() {
            return Err(CreateError::InputRejected);
        }
        self.validate(plan.target(), input.schema(), input.supplied())?;
        let decoded = raw
            .map(|bytes| fabric_cli_output::input::strict_json_bounded(bytes, MAX_BYTES))
            .transpose()
            .map_err(|_| CreateError::InputRejected)?;
        if decoded.as_ref() != input.supplied() {
            return Err(CreateError::InputRejected);
        }
        let mut body = json!({
            "displayName": plan.target().name(),
            "type": OWNER,
            "folderId": null,
        });
        if let Some(description) = input.supplied().and_then(|input| input.get("description")) {
            body["description"] = description.clone();
        }
        let operation = ResourceOperation::new(
            ResourceMethod::Post,
            &[
                "workspaces",
                plan.target().parent_id().as_str(),
                ItemCollection::SparkJobDefinition.as_str(),
            ],
            Some(body),
            if wait {
                CompletionPolicy::WaitForResult
            } else {
                CompletionPolicy::ReturnAccepted
            },
        )
        .map_err(|_| CreateError::InputRejected)?;
        FabricOperation::from_resource(&operation).map_err(|_| CreateError::InputRejected)
    }
}
