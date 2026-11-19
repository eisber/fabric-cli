// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded typed Notebook creation, separate from the existing v1 Notebook API.
//!
//! Contract: [Create Notebook](https://learn.microsoft.com/en-us/rest/api/fabric/notebook/items/create-notebook),
//! documentation commit `edc068545c34ad50e4d543d40139cd360a3a2f7d`
//! (updated 2026-11-11). The native `POST /v1/workspaces/{workspaceId}/notebooks`
//! documents HTTP 201 with a Notebook, HTTP 202 with operation headers, and
//! `ItemDisplayNameAlreadyInUse` rejection. That collision response, **not**
//! preflight absence or the offline planner, supports [`CreateSupport::RejectExisting`].
//! There is no upsert, replacement, duplicate, or mutation replay here.
//!
//! The advertised schema accepts only `displayName`, optional `description`, and
//! optional `definition` with ordered inline-base64 parts. The service's optional
//! `folderId` and `sensitivityLabelSettings` are deliberately unsupported and
//! rejected, never dropped. Missing definition format uses the service's
//! `fabricGitSource` default without inserting a field. Payload content, metadata,
//! `.platform`, file order and explicit JSON bytes are never rewritten, decoded
//! into notebook cells, executed, published, or used to resolve dependencies.
//! Content-language and notebook-file semantic validation remain service-owned.
//!
//! Execute the returned operation through the caller's existing
//! [`fabric_sdk_operations::OperationSession`]. A planned skip is local to the
//! host and must not be passed to [`operation`]. Accepted is not completed;
//! result/identity verification remains the caller's responsibility.

use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk_items::{
    create::{
        ConflictPolicy, CreateAction, CreateCapabilities, CreateError, CreateInputValidator,
        CreatePlan, CreateSupport, CreateTarget, DuplicateSupport, InputDisposition, SchemaRef,
        SkipSupport, TargetScope,
    },
    links::{parse_link_json, validate_link_id},
};
use fabric_sdk_operations::FabricOperation;
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::HashSet;

/// Exact revision of the bounded typed Notebook create input schema.
pub const SCHEMA: &str = "fabric.notebook.create/v1";

/// Advertise native collision rejection and local exact-existing skip only.
///
/// The service's documented `ItemDisplayNameAlreadyInUse` response is the
/// create-time collision contract. No guarantee is inferred from discovery.
pub fn capabilities() -> CreateCapabilities {
    CreateCapabilities {
        schema: SchemaRef::new(SCHEMA).expect("fixed Notebook schema is a valid reference"),
        create: CreateSupport::RejectExisting,
        replacement: None,
        skip: SkipSupport::ExactExistingUnchanged,
        duplicate: DuplicateSupport::Unsupported,
    }
}

/// Offline validator for the exact Notebook named-leaf schema.
///
/// Omitted input creates a shell using only the destination display name.
/// Explicit input must be a complete accepted request, even when skipping an
/// existing item. Raw callers must also use a duplicate-rejecting JSON parser;
/// [`operation`] independently rechecks the original bytes with the SDK parser.
#[derive(Clone, Copy, Debug, Default)]
pub struct NotebookCreateValidator;

impl CreateInputValidator for NotebookCreateValidator {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        if target.scope() != TargetScope::NamedLeaf
            || target.item_type() != "Notebook"
            || schema.as_str() != SCHEMA
        {
            return Err(CreateError::InputRejected);
        }
        validate_link_id(target.parent_id().as_str()).map_err(|_| CreateError::InputRejected)?;
        if let Some(value) = supplied {
            require_object_shapes(value)?;
            let request: CreateNotebookRequest =
                serde_json::from_value(value.clone()).map_err(|_| CreateError::InputRejected)?;
            request.validate(target)?;
        }
        Ok(InputDisposition::CreateOnly)
    }
}

/// Bind a create plan to the native Notebook endpoint without performing I/O.
///
/// Explicit raw JSON must match the planner's validated value and is retained
/// byte-for-byte, including whitespace, escaping and part order. Neither a
/// supplied value without its raw bytes nor raw bytes without supplied input
/// are accepted. Omitted input serializes only the exact target display name.
/// Default completion returns acceptance; `wait` requests the shared engine's
/// operation result, not a second mutation or a Notebook-specific polling loop.
///
/// # Errors
/// Returns [`CreateError::UnsupportedPolicy`] for overwrite/duplicate, and
/// [`CreateError::InputRejected`] for non-create actions, wrong target/schema,
/// malformed/duplicate JSON, input substitution, or an invalid request.
pub fn operation(
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
    NotebookCreateValidator.validate(input.target(), input.schema(), input.supplied())?;
    let body = match (input.supplied(), raw) {
        (None, None) => serde_json::to_vec(&serde_json::json!({
            "displayName": plan.target().name(),
        }))
        .map_err(|_| CreateError::InputRejected)?,
        (Some(expected), Some(raw)) => {
            let text = std::str::from_utf8(raw).map_err(|_| CreateError::InputRejected)?;
            let actual = parse_link_json(text).map_err(|_| CreateError::InputRejected)?;
            if &actual != expected {
                return Err(CreateError::InputRejected);
            }
            raw.to_vec()
        }
        _ => return Err(CreateError::InputRejected),
    };
    FabricOperation::new(
        ResourceMethod::Post,
        &format!(
            "/v1/workspaces/{}/notebooks",
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

fn require_object_shapes(value: &Value) -> Result<(), CreateError> {
    // Serde structs otherwise also accept positional JSON arrays.
    if !value.is_object() {
        return Err(CreateError::InputRejected);
    }
    if let Some(definition) = value.get("definition")
        && (!definition.is_object()
            || definition
                .get("parts")
                .and_then(Value::as_array)
                .is_some_and(|parts| parts.iter().any(|part| !part.is_object())))
    {
        return Err(CreateError::InputRejected);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateNotebookRequest {
    display_name: String,
    #[serde(default, deserialize_with = "present")]
    description: Option<String>,
    #[serde(default, deserialize_with = "present")]
    definition: Option<NotebookDefinition>,
}

impl CreateNotebookRequest {
    fn validate(&self, target: &CreateTarget) -> Result<(), CreateError> {
        if self.display_name != target.name()
            || self
                .description
                .as_ref()
                .is_some_and(|description| description.chars().count() > 256)
        {
            return Err(CreateError::InputRejected);
        }
        if let Some(definition) = &self.definition {
            definition.validate()?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NotebookDefinition {
    #[serde(default, rename = "format")]
    _format: NotebookFormat,
    parts: Vec<NotebookDefinitionPart>,
}

impl NotebookDefinition {
    fn validate(&self) -> Result<(), CreateError> {
        if self.parts.is_empty() {
            return Err(CreateError::InputRejected);
        }
        let mut paths = HashSet::new();
        for part in &self.parts {
            if part.path.is_empty()
                || part.path.contains(['\\', ':'])
                || part.path.chars().any(char::is_control)
                || part
                    .path
                    .split('/')
                    .any(|segment| matches!(segment, "" | "." | ".."))
                || !paths.insert(part.path.as_str())
                || STANDARD.decode(&part.payload).is_err()
            {
                return Err(CreateError::InputRejected);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(try_from = "String")]
enum NotebookFormat {
    Ipynb,
    #[default]
    FabricGitSource,
}

impl TryFrom<String> for NotebookFormat {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "ipynb" => Ok(Self::Ipynb),
            "fabricGitSource" => Ok(Self::FabricGitSource),
            _ => Err("unsupported Notebook definition format"),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotebookDefinitionPart {
    path: String,
    payload: String,
    #[serde(rename = "payloadType")]
    _payload_type: InlinePayloadType,
}

#[derive(Deserialize)]
#[serde(try_from = "String")]
enum InlinePayloadType {
    InlineBase64,
}

impl TryFrom<String> for InlinePayloadType {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "InlineBase64" => Ok(Self::InlineBase64),
            _ => Err("unsupported Notebook payload type"),
        }
    }
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
