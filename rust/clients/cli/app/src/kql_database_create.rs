// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit-parent ReadWrite KQLDatabase creation over the shared create host.

use crate::{
    create_command::{CreateBinding, MAX_BYTES},
    get_target::is_id,
};
use fabric_sdk::{
    items::{
        ItemCollection,
        create::{
            CreateAction, CreateCapabilities, CreateError, CreateInputValidator, CreatePlan,
            CreateSupport, CreateTarget, DuplicateSupport, InputDisposition, SchemaRef,
            SkipSupport, TargetScope,
        },
    },
    kql_databases::DatabaseKind,
};
use fabric_sdk_operations::FabricOperation;
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::Value;

const OWNER: &str = "KQLDatabase";
const SCHEMA: &str = "fabric.kqlDatabase.readWrite.create/v1";

/// Creates only a ReadWrite database under an explicitly supplied existing Eventhouse.
///
/// GUID syntax is not proof of parent existence, workspace membership or permission.
/// The service remains authoritative; this binding never discovers or creates a parent.
pub struct KqlReadWriteCreate;

impl CreateInputValidator for KqlReadWriteCreate {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        input: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        let reject = || CreateError::InputRejected;
        if target.scope() != TargetScope::NamedLeaf
            || target.item_type() != OWNER
            || !is_id(target.parent_id().as_str())
            || schema.as_str() != SCHEMA
            || !target
                .name()
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            return Err(reject());
        }
        let body = input.and_then(Value::as_object).ok_or_else(reject)?;
        if body.keys().any(|key| {
            !matches!(
                key.as_str(),
                "displayName"
                    | "description"
                    | "folderId"
                    | "sensitivityLabelSettings"
                    | "creationPayload"
            )
        }) || body.get("displayName").and_then(Value::as_str) != Some(target.name())
        {
            return Err(reject());
        }
        if body.get("description").is_some_and(|value| {
            !value
                .as_str()
                .is_some_and(|text| text.chars().count() <= 256)
        }) || body.get("folderId").is_some_and(|value| !value.is_null())
        {
            return Err(reject());
        }
        if let Some(settings) = body.get("sensitivityLabelSettings") {
            let settings = settings.as_object().ok_or_else(reject)?;
            if settings
                .keys()
                .any(|key| !matches!(key.as_str(), "labelId" | "sensitivityLabelApplyStrategy"))
                || !settings
                    .get("labelId")
                    .and_then(Value::as_str)
                    .is_some_and(is_id)
                || settings
                    .get("sensitivityLabelApplyStrategy")
                    .is_some_and(|value| !matches!(value.as_str(), Some("ApplyOrFail" | "Ignore")))
            {
                return Err(reject());
            }
        }
        let payload = body
            .get("creationPayload")
            .and_then(Value::as_object)
            .ok_or_else(reject)?;
        if payload.len() != 2
            || !payload
                .get("databaseType")
                .cloned()
                .and_then(|value| serde_json::from_value::<DatabaseKind>(value).ok())
                .is_some_and(|kind| kind == DatabaseKind::ReadWrite)
            || !payload
                .get("parentEventhouseItemId")
                .and_then(Value::as_str)
                .is_some_and(|id| is_id(id) && id != "00000000-0000-0000-0000-000000000000")
        {
            return Err(reject());
        }
        Ok(InputDisposition::CreateOnly)
    }
}

impl CreateBinding for KqlReadWriteCreate {
    fn item_type(&self) -> &'static str {
        OWNER
    }

    fn capabilities(&self) -> CreateCapabilities {
        CreateCapabilities {
            schema: SchemaRef::new(SCHEMA).expect("constant bounded schema"),
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
            return Err(CreateError::UnexpectedOutcome);
        };
        let raw = raw.ok_or(CreateError::InputRejected)?;
        let body = fabric_cli_output::input::strict_json_bounded(raw, MAX_BYTES)
            .map_err(|_| CreateError::InputRejected)?;
        if input.supplied() != Some(&body) {
            return Err(CreateError::InputRejected);
        }
        self.validate(plan.target(), &self.capabilities().schema, Some(&body))?;
        FabricOperation::new(
            ResourceMethod::Post,
            &format!(
                "/v1/workspaces/{}/{}",
                plan.target().parent_id().as_str(),
                ItemCollection::KqlDatabase.as_str()
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
