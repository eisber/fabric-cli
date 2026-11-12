// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Metadata-only CopyJob creation, not connector configuration or job execution.

use crate::{
    items::create::{
        CreateAction, CreateCapabilities, CreateError, CreateInputValidator, CreatePlan,
        CreateSupport, CreateTarget, DuplicateSupport, InputDisposition, SchemaRef, SkipSupport,
        TargetScope,
    },
    v2::WorkspaceIdentity,
};
use fabric_sdk_items::links::parse_link_json;
use fabric_sdk_operations::FabricOperation;
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::{Value, json};

/// Exact bounded metadata contract; not the full provider definition schema.
pub const SCHEMA: &str = "fabric.copyjob.metadata-create/v1";
/// Raw JSON bound shared with the opt-in create host.
pub const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

/// Pure native CopyJob schema and request binding for the shared create planner.
#[derive(Clone, Copy, Debug, Default)]
pub struct CopyJobCreate;

impl CopyJobCreate {
    /// Only error and exact-existing skip are supported, never overwrite/upsert.
    ///
    /// The official API documents `ItemDisplayNameAlreadyInUse`; a service
    /// collision remains a failure, including when a leaf appeared after
    /// discovery. Discovery does not establish an atomic namespace reservation.
    pub fn capabilities(self) -> CreateCapabilities {
        CreateCapabilities {
            schema: SchemaRef::new(SCHEMA).expect("static CopyJob schema"),
            create: CreateSupport::RejectExisting,
            replacement: None,
            skip: SkipSupport::ExactExistingUnchanged,
            duplicate: DuplicateSupport::Unsupported,
        }
    }

    /// Machine-readable supported input, deliberately narrower than the API.
    pub fn input_schema(self) -> Value {
        json!({
            "type":"object",
            "required":["displayName"],
            "properties":{
                "displayName":{"type":"string","minLength":1,"maxLength":256},
                "description":{"type":"string","maxLength":256}
            },
            "additionalProperties":false
        })
    }

    /// Bind one immutable SDK create plan to its original unique-key JSON bytes.
    ///
    /// Omitted input derives only displayName from the exact destination. Explicit
    /// input is sent byte-for-byte, with no added type, folder or definition.
    /// The caller owns identity, immediate approval, execution and observation.
    ///
    /// # Errors
    /// Rejects substituted bytes, unsupported actions, schemas, names and fields
    /// before credentials or dispatch. No retry or delete/recreate is installed.
    pub fn operation(
        self,
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
        let body = match (raw, input.supplied()) {
            (None, None) => serde_json::to_vec(&json!({"displayName":plan.target().name()}))
                .map_err(|_| CreateError::InputRejected)?,
            (Some(bytes), Some(value)) => {
                if bytes.len() > MAX_INPUT_BYTES
                    || parse_link_json(
                        std::str::from_utf8(bytes).map_err(|_| CreateError::InputRejected)?,
                    )
                    .map_err(|_| CreateError::InputRejected)?
                        != *value
                {
                    return Err(CreateError::InputRejected);
                }
                bytes.to_vec()
            }
            _ => return Err(CreateError::InputRejected),
        };
        FabricOperation::new(
            ResourceMethod::Post,
            &format!(
                "/v1/workspaces/{}/copyJobs",
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

impl CreateInputValidator for CopyJobCreate {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        let name = target.name();
        if schema.as_str() != SCHEMA
            || target.scope() != TargetScope::NamedLeaf
            || target.item_type() != "CopyJob"
            || WorkspaceIdentity::new(target.parent_id().as_str()).is_err()
            || name.trim().is_empty()
            || name.trim() != name
            || name.chars().count() > 256
            || matches!(name, "." | "..")
            || name.contains(['/', '\\', '%', '?', '#'])
            || name.chars().any(char::is_control)
        {
            return Err(CreateError::InputRejected);
        }
        if let Some(value) = supplied {
            let object = value.as_object().ok_or(CreateError::InputRejected)?;
            if object
                .keys()
                .any(|key| !matches!(key.as_str(), "displayName" | "description"))
                || object.get("displayName").and_then(Value::as_str) != Some(name)
                || object.get("description").is_some_and(|description| {
                    !description
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
