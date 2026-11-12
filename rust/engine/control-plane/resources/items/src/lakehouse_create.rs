// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native Lakehouse creation over the shared create planner and operation engine.
//!
//! This bounded schema supports metadata and a metadata-only public definition.
//! It does not create tables, upload files, execute Spark, or separately provision
//! a SQL endpoint. Fabric owns the SQL endpoint side effect of Lakehouse creation.
//! The caller owns resolution, raw input, immediate approval, context and output.

use crate::{
    ItemCollection,
    create::{
        ConflictPolicy, CreateAction, CreateCapabilities, CreateError, CreateInputValidator,
        CreatePlan, CreateSupport, CreateTarget, DuplicateSupport, InputDisposition, SchemaRef,
        SkipSupport, TargetScope,
    },
    links::{parse_link_json, validate_link_id},
    ml_models::SensitivityLabelSettings,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use std::fmt;

/// Revision of the advertised metadata-only Lakehouse creation schema.
pub const SCHEMA: &str = "fabric.lakehouse.create.metadata/v1";

/// Maximum raw JSON input, independently enforced before parsing or decoding.
pub const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

/// Explicit input retaining the original bytes before JSON decoding.
///
/// The host must keep omission separate from an explicitly empty input source.
/// Debug never includes native metadata, definition content or source bytes.
#[derive(Clone, PartialEq)]
pub struct LakehouseCreateInput {
    raw: Vec<u8>,
    value: Value,
}

impl LakehouseCreateInput {
    /// Decode one bounded UTF-8 JSON document with duplicate rejection at every depth.
    ///
    /// # Errors
    /// Rejects empty/oversized/non-UTF-8 input, duplicate members, malformed JSON
    /// and trailing documents without echoing any source content.
    pub fn from_bytes(raw: &[u8]) -> Result<Self, ResourceError> {
        if raw.is_empty() || raw.len() > MAX_INPUT_BYTES {
            return Err(ResourceError::input());
        }
        let text = std::str::from_utf8(raw).map_err(|_| ResourceError::input())?;
        let value = parse_link_json(text)?;
        Ok(Self {
            raw: raw.to_vec(),
            value,
        })
    }

    /// Exact source bytes, without newline, encoding or base64 normalization.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// Decoded input for the shared planner; this alone is not schema evidence.
    pub fn value(&self) -> &Value {
        &self.value
    }
}

impl fmt::Debug for LakehouseCreateInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LakehouseCreateInput")
            .field("bytes", &self.raw.len())
            .finish_non_exhaustive()
    }
}

/// Pure target-specific validation; it never resolves, authenticates or dispatches.
pub struct LakehouseCreateSchema;

impl LakehouseCreateSchema {
    /// Advertise only native reject-existing creation and exact-leaf skip.
    ///
    /// The API documents `ItemDisplayNameAlreadyInUse`, not repeated-name
    /// duplication or complete identity-preserving replacement. Preflight is a
    /// separate caller observation, never the source of a concurrency guarantee.
    ///
    /// # Errors
    /// Preserves an invalid schema-reference error.
    pub fn capabilities() -> Result<CreateCapabilities, CreateError> {
        Ok(CreateCapabilities {
            schema: SchemaRef::new(SCHEMA)?,
            create: CreateSupport::RejectExisting,
            replacement: None,
            skip: SkipSupport::ExactExistingUnchanged,
            duplicate: DuplicateSupport::Unsupported,
        })
    }
}

impl CreateInputValidator for LakehouseCreateSchema {
    fn validate(
        &self,
        target: &CreateTarget,
        schema: &SchemaRef,
        supplied: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        if schema.as_str() != SCHEMA {
            return Err(CreateError::InputRejected);
        }
        validate_target(target).map_err(|_| CreateError::InputRejected)?;
        if let Some(value) = supplied {
            validate_body(target, value).map_err(|_| CreateError::InputRejected)?;
        }
        Ok(InputDisposition::CreateOnly)
    }
}

/// An immutable Lakehouse POST derived only from a shared planner create action.
///
/// Construction is not approval. The host must approve this exact target, body
/// and selected credential/endpoint context immediately before executing it.
#[derive(Clone)]
pub struct LakehouseCreateRequest {
    target: CreateTarget,
    body: Value,
}

impl LakehouseCreateRequest {
    /// Bind the native request to the shared planner's validated input and target.
    ///
    /// Omitted input creates only the exact target display name. Explicit input
    /// must itself contain that name; the adapter never silently inserts one.
    /// Skip has no native request, and overwrite/duplicate are not admitted.
    ///
    /// # Errors
    /// Rejects non-create actions, wrong schemas/types/scopes/UUIDs, and any
    /// target/body mismatch before dispatch.
    pub fn from_plan(plan: &CreatePlan) -> Result<Self, ResourceError> {
        if !matches!(plan.policy(), ConflictPolicy::Error | ConflictPolicy::Skip) {
            return Err(ResourceError::input());
        }
        let CreateAction::Create { input } = plan.action() else {
            return Err(ResourceError::input());
        };
        if input.schema().as_str() != SCHEMA || input.target() != plan.target() {
            return Err(ResourceError::input());
        }
        validate_target(plan.target())?;
        let body = match input.supplied() {
            Some(body) => {
                validate_body(plan.target(), body)?;
                body.clone()
            }
            None => json!({"displayName":plan.target().name()}),
        };
        Ok(Self {
            target: plan.target().clone(),
            body,
        })
    }

    /// Exact selected target, independent of any later environment/context change.
    pub fn target(&self) -> &CreateTarget {
        &self.target
    }

    /// Exact validated JSON body, including native null and base64 spellings.
    pub fn body(&self) -> &Value {
        &self.body
    }

    /// Documented initial success statuses; acceptance is not completion.
    pub const fn expected_statuses(&self) -> &'static [u16] {
        &[201, 202]
    }

    /// Describe one native POST using existing operation and completion policies.
    ///
    /// No preflight read, result lookup, retry, polling or SQL provisioning is
    /// added. A concurrent name conflict remains a native failure, not a skip.
    ///
    /// # Errors
    /// Preserves shared operation validation errors before dispatch.
    pub fn operation(
        &self,
        completion: CompletionPolicy,
    ) -> Result<ResourceOperation, ResourceError> {
        ResourceOperation::new(
            ResourceMethod::Post,
            &[
                "workspaces",
                self.target.parent_id().as_str(),
                ItemCollection::Lakehouse.as_str(),
            ],
            Some(self.body.clone()),
            completion,
        )
    }

    /// Execute once through the caller's existing engine, without outcome rewriting.
    ///
    /// The caller must enforce immediate exact-request approval and context binding.
    /// Native resource metadata, unknown fields, operation IDs and uncertainty
    /// remain in the shared outcome/error; no handle is promoted to a resource ID.
    ///
    /// # Errors
    /// Retains the executor's service, cancellation, timeout and partial evidence.
    pub async fn execute(
        &self,
        executor: &mut dyn ResourceOperations,
        completion: CompletionPolicy,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor.execute(self.operation(completion)?).await
    }
}

impl fmt::Debug for LakehouseCreateRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LakehouseCreateRequest")
            .field("schema", &SCHEMA)
            .finish_non_exhaustive()
    }
}

fn validate_target(target: &CreateTarget) -> Result<(), ResourceError> {
    if target.scope() != TargetScope::NamedLeaf
        || target.item_type() != "Lakehouse"
        || target.name().trim().is_empty()
        || target.name().chars().any(char::is_control)
    {
        return Err(ResourceError::input());
    }
    validate_link_id(target.parent_id().as_str())
}

fn validate_body(target: &CreateTarget, value: &Value) -> Result<(), ResourceError> {
    if !value.is_object()
        || ["creationPayload", "sensitivityLabelSettings", "definition"]
            .iter()
            .any(|field| value.get(field).is_some_and(|value| !value.is_object()))
        || value
            .pointer("/definition/parts")
            .and_then(Value::as_array)
            .is_some_and(|parts| parts.iter().any(|part| !part.is_object()))
    {
        return Err(ResourceError::input());
    }
    let body: CreateBody =
        serde_json::from_value(value.clone()).map_err(|_| ResourceError::input())?;
    if body.display_name != target.name()
        || body
            .description
            .as_ref()
            .is_some_and(|description| description.chars().count() > 256)
    {
        return Err(ResourceError::input());
    }
    if let Some(Some(folder)) = body.folder_id {
        validate_link_id(&folder)?;
    }
    if body
        .creation_payload
        .is_some_and(|payload| !payload.enable_schemas)
    {
        return Err(ResourceError::input());
    }
    if let Some(settings) = body.sensitivity_label_settings {
        settings.validate()?;
    }
    if let Some(definition) = body.definition {
        validate_definition(definition)?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateBody {
    display_name: String,
    #[serde(default, deserialize_with = "non_null")]
    description: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    folder_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "non_null")]
    creation_payload: Option<CreationPayload>,
    #[serde(default, deserialize_with = "non_null")]
    sensitivity_label_settings: Option<SensitivityLabelSettings>,
    #[serde(default, deserialize_with = "non_null")]
    definition: Option<Definition>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreationPayload {
    enable_schemas: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    #[serde(default, deserialize_with = "non_null")]
    format: Option<String>,
    parts: Vec<DefinitionPart>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DefinitionPart {
    path: String,
    payload: String,
    payload_type: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LakehouseMetadata {
    #[serde(rename = "defaultSchema", default, deserialize_with = "non_null")]
    _default_schema: Option<String>,
}

fn validate_definition(definition: Definition) -> Result<(), ResourceError> {
    if definition
        .format
        .as_ref()
        .is_some_and(|format| format != "LakehouseDefinitionV1")
        || definition.parts.len() != 1
    {
        return Err(ResourceError::input());
    }
    let part = &definition.parts[0];
    if part.path != "lakehouse.metadata.json"
        || part.payload_type != "InlineBase64"
        || part.payload.len() > MAX_INPUT_BYTES
    {
        return Err(ResourceError::input());
    }
    let bytes = STANDARD
        .decode(&part.payload)
        .map_err(|_| ResourceError::input())?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ResourceError::input())?;
    let value = parse_link_json(text)?;
    if !value.is_object() {
        return Err(ResourceError::input());
    }
    let _: LakehouseMetadata = serde_json::from_value(value).map_err(|_| ResourceError::input())?;
    Ok(())
}

fn non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}
