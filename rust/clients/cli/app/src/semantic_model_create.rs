// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Blank SemanticModel binding over the shared v2 create planner and host.

use crate::{create_command::CreateBinding, get_target::is_id};
use fabric_cli_mutations::BlankSemanticModelCreate;
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
use serde::Deserialize;
use serde_json::Value;

const SCHEMA: &str = "fabric.semanticModel.blank-create/v1";

/// Only blank creation with the existing embedded native definition is bound.
pub struct BlankSemanticModelBinding;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Metadata {
    display_name: String,
    #[serde(default, deserialize_with = "description")]
    description: Option<String>,
}

fn description<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

fn body(target: &CreateTarget, supplied: Option<&Value>) -> Result<Value, CreateError> {
    if target.scope() != TargetScope::NamedLeaf
        || target.item_type() != "SemanticModel"
        || !is_id(target.parent_id().as_str())
    {
        return Err(CreateError::InputRejected);
    }
    let metadata = supplied
        .map(|value| {
            if !value.is_object() {
                return Err(CreateError::InputRejected);
            }
            serde_json::from_value::<Metadata>(value.clone())
                .map_err(|_| CreateError::InputRejected)
        })
        .transpose()?;
    if metadata
        .as_ref()
        .is_some_and(|metadata| metadata.display_name != target.name())
    {
        return Err(CreateError::InputRejected);
    }
    BlankSemanticModelCreate::new(
        target.name(),
        metadata
            .as_ref()
            .and_then(|metadata| metadata.description.as_deref()),
    )
    .map(BlankSemanticModelCreate::into_payload)
    .map_err(|_| CreateError::InputRejected)
}

impl CreateInputValidator for BlankSemanticModelBinding {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        if schema.as_str() != SCHEMA {
            return Err(CreateError::InputRejected);
        }
        body(target, supplied)?;
        Ok(InputDisposition::CreateOnly)
    }
}

impl CreateBinding for BlankSemanticModelBinding {
    fn item_type(&self) -> &'static str {
        "SemanticModel"
    }

    fn capabilities(&self) -> CreateCapabilities {
        CreateCapabilities {
            schema: SchemaRef::new(SCHEMA).expect("fixed schema"),
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
        if input.target() != plan.target() || input.schema().as_str() != SCHEMA {
            return Err(CreateError::InputRejected);
        }
        let parsed = raw
            .map(|raw| {
                fabric_cli_output::input::strict_json_bounded(raw, crate::create_command::MAX_BYTES)
            })
            .transpose()
            .map_err(|_| CreateError::InputRejected)?;
        if parsed.as_ref() != input.supplied() {
            return Err(CreateError::InputRejected);
        }
        self.validate(plan.target(), input.schema(), input.supplied())?;
        let operation = ResourceOperation::new(
            ResourceMethod::Post,
            &[
                "workspaces",
                plan.target().parent_id().as_str(),
                ItemCollection::SemanticModel.as_str(),
            ],
            Some(body(plan.target(), input.supplied())?),
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
