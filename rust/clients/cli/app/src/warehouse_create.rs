// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Warehouse-only native create schema over the shared create host and SDK.

use crate::{
    create_command::{AnticipatedCreateEffect, CreateBinding, CreateEffectKind, MAX_BYTES},
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

/// Default case-sensitive collation, matching the legacy Warehouse create choice.
pub const DEFAULT_COLLATION: &str = "Latin1_General_100_BIN2_UTF8";
/// The only supported case-insensitive alternative in the native API contract.
pub const CASE_INSENSITIVE_COLLATION: &str = "Latin1_General_100_CI_AS_KS_WS_SC_UTF8";

const SCHEMA: &str = "fabric.warehouse.create/v1";
const CONSUMPTION_CONTRACT: &str = "https://learn.microsoft.com/en-us/fabric/data-warehouse/usage-reporting (source c9d49756c2e184b61acde34d32f9aff63595db5b)";

/// Native Warehouse creation only; no definition, SQL, query or child provisioning.
pub struct WarehouseCreateBinding;

impl CreateInputValidator for WarehouseCreateBinding {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        if schema.as_str() != SCHEMA
            || target.scope() != TargetScope::NamedLeaf
            || target.item_type() != "Warehouse"
            || !is_id(target.parent_id().as_str())
        {
            return Err(CreateError::InputRejected);
        }
        payload(target, supplied)?;
        Ok(InputDisposition::CreateOnly)
    }
}

impl CreateBinding for WarehouseCreateBinding {
    fn item_type(&self) -> &'static str {
        "Warehouse"
    }

    fn capabilities(&self) -> CreateCapabilities {
        CreateCapabilities {
            schema: SchemaRef::new(SCHEMA).expect("static Warehouse schema"),
            // The native endpoint documents ItemDisplayNameAlreadyInUse, not upsert.
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
        let decoded = raw
            .map(|bytes| fabric_cli_output::input::strict_json_bounded(bytes, MAX_BYTES))
            .transpose()
            .map_err(|_| CreateError::InputRejected)?;
        if decoded.as_ref() != input.supplied()
            || input.target() != plan.target()
            || self.validate(plan.target(), input.schema(), input.supplied())?
                != InputDisposition::CreateOnly
        {
            return Err(CreateError::InputRejected);
        }
        let operation = ResourceOperation::new(
            ResourceMethod::Post,
            &[
                "workspaces",
                plan.target().parent_id().as_str(),
                ItemCollection::Warehouse.as_str(),
            ],
            Some(payload(plan.target(), input.supplied())?),
            if wait {
                CompletionPolicy::WaitForResult
            } else {
                CompletionPolicy::ReturnAccepted
            },
        )
        .map_err(|_| CreateError::InputRejected)?;
        FabricOperation::from_resource(&operation).map_err(|_| CreateError::InputRejected)
    }

    fn anticipated_effects(
        &self,
        plan: &CreatePlan,
    ) -> Result<Vec<AnticipatedCreateEffect>, CreateError> {
        let CreateAction::Create { input } = plan.action() else {
            return Err(CreateError::UnsupportedPolicy);
        };
        self.validate(plan.target(), input.schema(), input.supplied())?;
        Ok(vec![AnticipatedCreateEffect::new(
            CreateEffectKind::MeteredConsumption,
            Some("Warehouse"),
            "Warehouse activity can consume Fabric capacity, including provider-managed background work. This request creates an empty Warehouse; it submits no SQL and claims no metered amount or observed charge.",
            CONSUMPTION_CONTRACT,
        )?])
    }
}

fn payload(target: &CreateTarget, supplied: Option<&Value>) -> Result<Value, CreateError> {
    let name = target.name();
    if name.trim().is_empty()
        || name.chars().any(char::is_control)
        || name.contains(['/', '\\', '?', '#', '%', '\0'])
        || matches!(name, "." | "..")
        || name.chars().count() > 256
    {
        return Err(CreateError::InputRejected);
    }
    let mut body = json!({
        "displayName":name,
        "creationPayload":{"collationType":DEFAULT_COLLATION}
    });
    if let Some(supplied) = supplied {
        let object = supplied.as_object().ok_or(CreateError::InputRejected)?;
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "displayName" | "description" | "creationPayload"
            )
        }) || object
            .get("displayName")
            .is_some_and(|value| value.as_str() != Some(name))
        {
            return Err(CreateError::InputRejected);
        }
        if let Some(description) = object.get("description") {
            let description = description.as_str().ok_or(CreateError::InputRejected)?;
            if description.chars().count() > 256 {
                return Err(CreateError::InputRejected);
            }
            body["description"] = json!(description);
        }
        if let Some(creation) = object.get("creationPayload") {
            let creation = creation.as_object().ok_or(CreateError::InputRejected)?;
            if creation.keys().any(|key| key != "collationType") {
                return Err(CreateError::InputRejected);
            }
            if let Some(collation) = creation.get("collationType") {
                if !matches!(
                    collation.as_str(),
                    Some(DEFAULT_COLLATION | CASE_INSENSITIVE_COLLATION)
                ) {
                    return Err(CreateError::InputRejected);
                }
                body["creationPayload"]["collationType"] = collation.clone();
            }
        }
    }
    Ok(body)
}
