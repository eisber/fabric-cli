// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standalone, metadata-only Eventhouse creation over the shared create host.

use crate::{
    create_command::{AnticipatedCreateEffect, CreateBinding, CreateEffectKind, MAX_BYTES},
    get_target::is_id,
};
use fabric_sdk::items::create::{
    ConflictPolicy, CreateAction, CreateCapabilities, CreateError, CreateInputValidator,
    CreatePlan, CreateSupport, CreateTarget, DuplicateSupport, InputDisposition, SchemaRef,
    SkipSupport, TargetScope,
};
use fabric_sdk_operations::FabricOperation;
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::{Value, json};

const SCHEMA: &str = "fabric.eventhouse.create-metadata/v1";

/// Native Eventhouse binding; never invokes the v1 KQL dependency flow.
pub struct EventhouseCreate;

impl CreateInputValidator for EventhouseCreate {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        if schema.as_str() != SCHEMA
            || target.scope() != TargetScope::NamedLeaf
            || target.item_type() != "Eventhouse"
            || !is_id(target.parent_id().as_str())
            || target.name().is_empty()
            || !target
                .name()
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            return Err(CreateError::InputRejected);
        }
        if let Some(supplied) = supplied {
            let object = supplied.as_object().ok_or(CreateError::InputRejected)?;
            if object
                .keys()
                .any(|key| !matches!(key.as_str(), "displayName" | "description"))
                || object.get("displayName").and_then(Value::as_str) != Some(target.name())
                || object.get("description").is_some_and(|value| {
                    !value
                        .as_str()
                        .is_some_and(|text| text.chars().count() <= 256)
                })
            {
                return Err(CreateError::InputRejected);
            }
        }
        Ok(InputDisposition::CreateOnly)
    }
}

impl CreateBinding for EventhouseCreate {
    fn item_type(&self) -> &'static str {
        "Eventhouse"
    }

    fn capabilities(&self) -> CreateCapabilities {
        CreateCapabilities {
            schema: SchemaRef::new(SCHEMA).expect("static Eventhouse schema"),
            create: CreateSupport::RejectExisting,
            replacement: None,
            skip: SkipSupport::ExactExistingUnchanged,
            duplicate: DuplicateSupport::Unsupported,
        }
    }

    fn anticipated_effects(
        &self,
        plan: &CreatePlan,
    ) -> Result<Vec<AnticipatedCreateEffect>, CreateError> {
        let CreateAction::Create { input } = plan.action() else {
            return Err(CreateError::UnsupportedPolicy);
        };
        if input.target() != plan.target() {
            return Err(CreateError::InputRejected);
        }
        self.validate(plan.target(), input.schema(), input.supplied())?;
        Ok(vec![
            AnticipatedCreateEffect::new(
                CreateEffectKind::ChildResourceCreation,
                Some("KQLDatabase"),
                "Provider creation may create a same-name default KQL database, as documented for Eventhouse creation. The client does not issue a separate KQL database POST or discover children.",
                "https://learn.microsoft.com/en-us/fabric/real-time-intelligence/create-eventhouse#create-an-eventhouse (source 165f66efc3c8da81a156627a63eb0b8e0cb0c7ef)",
            )?,
            AnticipatedCreateEffect::new(
                CreateEffectKind::MeteredConsumption,
                Some("Eventhouse"),
                "The provider-managed Eventhouse and its databases may consume capacity and incur metered UpTime or storage charges when active. This request submits no query, run, start or minimum-consumption setting.",
                "https://learn.microsoft.com/en-us/fabric/real-time-intelligence/real-time-intelligence-consumption (source b659751abeffc3a236c6a5be0c9a1fc1233a80aa)",
            )?,
        ])
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
        if input.target() != plan.target() {
            return Err(CreateError::InputRejected);
        }
        self.validate(plan.target(), input.schema(), input.supplied())?;
        let body = match (raw, input.supplied()) {
            (Some(raw), Some(supplied)) => {
                let decoded = fabric_cli_output::input::strict_json_bounded(raw, MAX_BYTES)
                    .map_err(|_| CreateError::InputRejected)?;
                if &decoded != supplied {
                    return Err(CreateError::InputRejected);
                }
                raw.to_vec()
            }
            (None, None) => serde_json::to_vec(&json!({"displayName":plan.target().name()}))
                .map_err(|_| CreateError::InputRejected)?,
            _ => return Err(CreateError::InputRejected),
        };
        FabricOperation::new(
            ResourceMethod::Post,
            &format!(
                "/v1/workspaces/{}/eventhouses",
                plan.target().parent_id().as_str()
            ),
            body,
            if wait {
                CompletionPolicy::WaitForResult
            } else {
                CompletionPolicy::ReturnAccepted
            },
        )
        .map_err(|_| CreateError::InputRejected)
    }
}
