// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public Ontology operations over existing caller-injected SDK execution.
//!
//! Public metadata, whole-model JSON and multipart definitions are distinct
//! representations. This crate does not select credentials, install HTTP, follow
//! private workload routes, convert OWL, or substitute metadata for denied source.

use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk_items::{
    ItemClient, ItemCollection, ItemError, ItemExecutor, ItemGetOptions, ItemMetadata,
};
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutor, OperationResponse,
    OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod, ResourceOperation};
use fabric_v2_contracts::ItemIdentity;
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeSet, fmt, future::Future, io, num::NonZeroUsize};

/// Exact public Ontology owner identity; construction performs no resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OntologyTarget(ItemIdentity);

impl OntologyTarget {
    /// Validates public workspace/item UUIDs before any credential or HTTP work.
    ///
    /// # Errors
    /// Rejects malformed or route-changing identifiers.
    pub fn new(workspace_id: &str, item_id: &str) -> Result<Self, InputError> {
        ItemIdentity::new(workspace_id, item_id)
            .map(Self)
            .map_err(|_| InputError::InvalidIdentity)
    }

    /// Returns the explicitly selected workspace ID.
    pub fn workspace_id(&self) -> &str {
        self.0.workspace_id()
    }

    /// Returns the explicitly selected Ontology item ID.
    pub fn item_id(&self) -> &str {
        self.0.item_id()
    }
}

/// Provider-established generation; unknown never means generation one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum ObservedGeneration {
    /// The provider supplied an integral generation, including future values.
    Known(u64),
    /// The metadata did not establish generation.
    Unknown,
}

/// Ordered, unmodified public metadata, separate from model/definition content.
#[derive(Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OntologyMetadata {
    metadata: ItemMetadata,
    generation: ObservedGeneration,
}

impl OntologyMetadata {
    /// Returns the original metadata, retaining missing/null and unknown fields.
    pub fn metadata(&self) -> &ItemMetadata {
        &self.metadata
    }

    /// Returns generation evidence without an assumed default.
    pub const fn generation(&self) -> ObservedGeneration {
        self.generation
    }
}

impl fmt::Debug for OntologyMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OntologyMetadata")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// Read public metadata through the existing typed item client, with no augmentation.
///
/// # Errors
/// Retains typed item failures and rejects wrong-owner/identity or malformed
/// generation metadata. A source-definition fallback is never attempted.
pub async fn get_metadata(
    client: &ItemClient,
    executor: &dyn ItemExecutor,
    target: &OntologyTarget,
    options: ItemGetOptions,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<OntologyMetadata, PublicError> {
    let metadata = client
        .get_item_metadata_from_collection(
            executor,
            target.workspace_id(),
            ItemCollection::Ontology,
            target.item_id(),
            options,
            cancelled,
        )
        .await
        .map_err(|cause| PublicError::metadata(target, cause))?;
    let value = metadata.as_value();
    if !metadata.id().eq_ignore_ascii_case(target.item_id())
        || metadata.item_type() != Some("Ontology")
        || value.get("workspaceId").is_some_and(|id| {
            !id.as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(target.workspace_id()))
        })
    {
        return Err(PublicError::invalid_metadata(target, metadata));
    }
    let generation = match value.get("properties") {
        None | Some(Value::Null) => ObservedGeneration::Unknown,
        Some(Value::Object(properties)) => match properties.get("generation") {
            None | Some(Value::Null) => ObservedGeneration::Unknown,
            Some(value) => match value.as_u64() {
                Some(generation) => ObservedGeneration::Known(generation),
                None => return Err(PublicError::invalid_metadata(target, metadata)),
            },
        },
        Some(_) => return Err(PublicError::invalid_metadata(target, metadata)),
    };
    Ok(OntologyMetadata {
        metadata,
        generation,
    })
}

/// Explicit public model form; native API default and authoring are not conflated.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModelForm {
    /// Provider default: Expanded, whose feature gate can reject the read.
    #[default]
    ProviderDefault,
    /// Authored/minimized model.
    Minimized,
    /// Authored model plus service projection.
    Expanded,
}

/// Explicit observation policy using the shared Fabric operation engine.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Observation {
    /// Return actual service acceptance without claiming completion.
    #[default]
    ReturnAccepted,
    /// Observe the original operation and fetch its advertised result.
    WaitForResult,
}

impl Observation {
    fn policy(self) -> CompletionPolicy {
        match self {
            Self::ReturnAccepted => CompletionPolicy::ReturnAccepted,
            Self::WaitForResult => CompletionPolicy::WaitForResult,
        }
    }
}

/// Public whole-model response, not internal TMSL++ or multipart ALM source.
#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct PublicModel(Value);

impl PublicModel {
    /// Returns exact parsed public response data, including future fields.
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    /// Returns the public model object, distinct from its response envelope.
    pub fn model(&self) -> &Value {
        &self.0["model"]
    }
}

impl fmt::Debug for PublicModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PublicModel(<source content>)")
    }
}

/// Explicit whole-model replacement in the public API's native JSON schema.
///
/// The request envelope and collection shapes are checked locally; the service
/// remains authoritative for individual native objects and feature-gated fields.
/// There is deliberately no conversion from an expanded [`PublicModel`].
#[derive(Clone)]
pub struct ModelReplacement(Vec<u8>);

impl ModelReplacement {
    /// Validates and serializes the public envelope once within its 16 MiB limit.
    ///
    /// # Errors
    /// Rejects nonmodel/internal envelopes, unknown top-level model collections,
    /// nonobject members and over-budget content before session execution.
    pub fn new(model: Value, description: Option<String>) -> Result<Self, InputError> {
        validate_model_collections(&model, true)?;
        let mut envelope = serde_json::Map::new();
        if let Some(description) = description {
            if description.len() > MAX_MODEL_REQUEST_BYTES {
                return Err(InputError::RequestLimit);
            }
            envelope.insert("description".into(), Value::String(description));
        }
        envelope.insert("model".into(), model);
        let mut writer = BoundedJson {
            bytes: Vec::new(),
            limit: MAX_MODEL_REQUEST_BYTES,
            exceeded: false,
        };
        if serde_json::to_writer(&mut writer, &Value::Object(envelope)).is_err() {
            return Err(if writer.exceeded {
                InputError::RequestLimit
            } else {
                InputError::InvalidModel
            });
        }
        Ok(Self(writer.bytes))
    }

    /// Returns exact bounded request bytes; do not log source content implicitly.
    pub fn body(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for ModelReplacement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelReplacement")
            .field("bytes", &self.0.len())
            .finish()
    }
}

/// Native public whole-model request ceiling, not a promise every host permits it.
pub const MAX_MODEL_REQUEST_BYTES: usize = 16 * 1024 * 1024;

const MODEL_COLLECTIONS: &[&str] = &[
    "expressions",
    "tables",
    "relationships",
    "namespaces",
    "entityRelationships",
    "entities",
    "rules",
    "metrics",
    "reusableProperties",
    "annotations",
];

fn validate_model_collections(model: &Value, writing: bool) -> Result<(), InputError> {
    let model = model.as_object().ok_or(InputError::InvalidModel)?;
    for (name, value) in model {
        let known = MODEL_COLLECTIONS.contains(&name.as_str());
        if (writing && !known)
            || (known
                && !value
                    .as_array()
                    .is_some_and(|members| members.iter().all(Value::is_object)))
        {
            return Err(InputError::InvalidModel);
        }
    }
    Ok(())
}

struct BoundedJson {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl io::Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other(
                "native model request exceeds its byte limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Native definition envelope preserved without local formatting or conversion.
#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct NativeDefinition(Value);

impl NativeDefinition {
    /// Returns the original complete public response envelope.
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    /// Returns original ordered multipart entries; no local file path is inferred.
    pub fn parts(&self) -> &[Value] {
        self.0["definition"]["parts"]
            .as_array()
            .expect("validated native definition parts")
    }
}

impl fmt::Debug for NativeDefinition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeDefinition")
            .field("parts", &self.parts().len())
            .finish_non_exhaustive()
    }
}

/// Caller-selected finite bounds for native definition inspection.
#[derive(Clone, Copy, Debug)]
pub struct DefinitionLimits {
    /// Maximum number of returned parts.
    pub max_parts: NonZeroUsize,
    /// Maximum aggregate decoded InlineBase64 bytes.
    pub max_decoded_bytes: NonZeroUsize,
}

/// Completed response or original accepted work, with owner scope retained.
pub enum PublicOutcome<T> {
    /// The endpoint returned a complete typed response.
    Completed {
        /// Explicit owner, independent of subsequent ambient selection.
        target: OntologyTarget,
        /// Validated representation.
        data: T,
        /// Original status/headers/body for evidence, not implicit logging.
        response: OperationResponse,
    },
    /// The service accepted work; no completion/result is invented.
    Accepted {
        /// Explicit original owner scope.
        target: OntologyTarget,
        /// Shared engine's provider-returned operation ID.
        operation_id: String,
        /// Original accepted response.
        response: OperationResponse,
    },
}

impl<T> fmt::Debug for PublicOutcome<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Completed { .. } => "PublicOutcome::Completed(<source content>)",
            Self::Accepted { .. } => "PublicOutcome::Accepted(<operation evidence>)",
        })
    }
}

/// Read the gated public whole-model API without falling back to a private route.
///
/// # Errors
/// Preserves feature/permission/service/operation failures and malformed responses.
pub async fn get_model<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    target: &OntologyTarget,
    form: ModelForm,
    observation: Observation,
) -> Result<PublicOutcome<PublicModel>, PublicError> {
    let mut operation = public_operation(target, "model", ResourceMethod::Get, None, observation)?;
    operation = match form {
        ModelForm::ProviderDefault => operation,
        ModelForm::Minimized => operation
            .with_query(&[("form", "Minimized")])
            .map_err(|_| PublicError::input(target))?,
        ModelForm::Expanded => operation
            .with_query(&[("form", "Expanded")])
            .map_err(|_| PublicError::input(target))?,
    };
    execute(session, target, operation, decode_public_model).await
}

/// Replace the public whole model using explicit source JSON, never implicit upsert.
///
/// This API replaces the model of an already-addressed Ontology item; it does not
/// create an item or promise compare-and-swap. No revision condition is invented.
///
/// # Errors
/// Preserves gate/validation/conflict errors, accepted work and uncertain effects.
pub async fn replace_model<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    target: &OntologyTarget,
    replacement: &ModelReplacement,
    observation: Observation,
) -> Result<PublicOutcome<PublicModel>, PublicError> {
    let body =
        serde_json::from_slice(replacement.body()).map_err(|_| PublicError::input(target))?;
    let operation = public_operation(
        target,
        "model",
        ResourceMethod::Post,
        Some(body),
        observation,
    )?;
    execute(session, target, operation, decode_public_model).await
}

fn decode_public_model(value: Value) -> Result<PublicModel, &'static str> {
    if !value.get("name").is_some_and(Value::is_string) {
        return Err("public model name is missing");
    }
    validate_model_collections(
        value.get("model").ok_or("public model object is missing")?,
        false,
    )
    .map_err(|_| "public model collections are invalid")?;
    if value
        .get("description")
        .is_some_and(|v| !v.is_null() && !v.is_string())
    {
        return Err("public model description is invalid");
    }
    Ok(PublicModel(value))
}

/// Retrieve the provider-default multipart public definition without transformation.
///
/// Unknown payload types remain opaque, preserving future native source. Known
/// InlineBase64 payloads are validated and bounded, never silently skipped.
///
/// # Errors
/// Preserves denial/LRO failures and rejects malformed/duplicate/over-budget parts.
pub async fn get_definition<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    target: &OntologyTarget,
    limits: DefinitionLimits,
    observation: Observation,
) -> Result<PublicOutcome<NativeDefinition>, PublicError> {
    let operation = public_operation(
        target,
        "getDefinition",
        ResourceMethod::Post,
        None,
        observation,
    )?;
    execute(session, target, operation, |value| {
        validate_definition(&value, limits)?;
        Ok(NativeDefinition(value))
    })
    .await
}

fn validate_definition(value: &Value, limits: DefinitionLimits) -> Result<(), &'static str> {
    let parts = value
        .get("definition")
        .and_then(|v| v.get("parts"))
        .and_then(Value::as_array)
        .ok_or("native definition parts are missing")?;
    if parts.len() > limits.max_parts.get() {
        return Err("native definition part limit exceeded");
    }
    let mut paths = BTreeSet::new();
    let mut remaining = limits.max_decoded_bytes.get();
    for part in parts {
        let path = part
            .get("path")
            .and_then(Value::as_str)
            .ok_or("native part path is missing")?;
        let payload = part
            .get("payload")
            .and_then(Value::as_str)
            .ok_or("native part payload is missing")?;
        let kind = part
            .get("payloadType")
            .and_then(Value::as_str)
            .ok_or("native part payload type is missing")?;
        if path.is_empty()
            || path.len() > 4096
            || path.chars().any(char::is_control)
            || !paths.insert(path)
        {
            return Err("native definition part paths are invalid or duplicate");
        }
        if kind == "InlineBase64" {
            let padding = payload
                .bytes()
                .rev()
                .take_while(|byte| *byte == b'=')
                .count()
                .min(2);
            let estimate = base64::decoded_len_estimate(payload.len()).saturating_sub(padding);
            if estimate > remaining {
                return Err("native definition decoded byte limit exceeded");
            }
            let decoded = STANDARD
                .decode(payload)
                .map_err(|_| "native part base64 is invalid")?;
            if decoded.len() > remaining {
                return Err("native definition decoded byte limit exceeded");
            }
            remaining -= decoded.len();
        } else if kind.is_empty() {
            return Err("native part payload type is empty");
        }
    }
    Ok(())
}

fn public_operation(
    target: &OntologyTarget,
    action: &str,
    method: ResourceMethod,
    body: Option<Value>,
    observation: Observation,
) -> Result<ResourceOperation, PublicError> {
    ResourceOperation::new(
        method,
        &[
            "workspaces",
            target.workspace_id(),
            "ontologies",
            target.item_id(),
            action,
        ],
        body,
        observation.policy(),
    )
    .map_err(|_| PublicError::input(target))
}

async fn execute<E: OperationExecutor, T>(
    session: &mut OperationSession<'_, E>,
    target: &OntologyTarget,
    operation: ResourceOperation,
    decode: impl FnOnce(Value) -> Result<T, &'static str>,
) -> Result<PublicOutcome<T>, PublicError> {
    let operation =
        FabricOperation::from_resource(&operation).map_err(|_| PublicError::input(target))?;
    let response = session
        .execute(operation)
        .await
        .map_err(|cause| PublicError::operation(target, cause))?;
    if response.status == 202 {
        let Some(operation_id) = session.operation_id().map(str::to_owned) else {
            return Err(PublicError::response(
                target,
                response,
                "acceptance has no operation ID",
                session.evidence(OperationErrorKind::InvalidResponse),
            ));
        };
        return Ok(PublicOutcome::Accepted {
            target: target.clone(),
            operation_id,
            response,
        });
    }
    if response.status != 200 {
        return Err(PublicError::response(
            target,
            response,
            "unexpected completed response status",
            session.evidence(OperationErrorKind::InvalidResponse),
        ));
    }
    let data = serde_json::from_slice(&response.body).map_err(|_| {
        PublicError::response(
            target,
            response.clone(),
            "response is not JSON",
            session.evidence(OperationErrorKind::InvalidResponse),
        )
    })?;
    let data = decode(data).map_err(|reason| {
        PublicError::response(
            target,
            response.clone(),
            reason,
            session.evidence(OperationErrorKind::InvalidResponse),
        )
    })?;
    Ok(PublicOutcome::Completed {
        target: target.clone(),
        data,
        response,
    })
}

/// Pre-dispatch input classification without sensitive supplied values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputError {
    /// Workspace/item identifiers must be public Fabric UUIDs.
    InvalidIdentity,
    /// The body is not the native public model envelope/collection shape.
    InvalidModel,
    /// The serialized model request exceeds the service ceiling.
    RequestLimit,
}

impl fmt::Display for InputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid or over-budget public Ontology input")
    }
}
impl std::error::Error for InputError {}

/// Original failure cause; untrusted service data is retained explicitly.
pub enum PublicErrorKind {
    /// A typed endpoint request could not be constructed.
    InvalidInput,
    /// Existing item SDK metadata failure.
    Metadata(Box<ItemError>),
    /// Existing shared Fabric operation failure and reconciliation evidence.
    Operation(Box<OperationError>),
    /// Malformed/wrong-owner metadata, retained without a healthy fallback.
    InvalidMetadata(ItemMetadata),
    /// Malformed/unexpected completed or accepted representation.
    InvalidResponse {
        /// Stable local explanation, not a raw parser/service error dump.
        reason: &'static str,
        /// Original bounded response; no metadata substitution.
        response: Box<OperationResponse>,
        /// Shared engine's exact accepted-operation and uncertain-effect evidence.
        evidence: Box<OperationError>,
    },
}

/// Scoped public-operation error. Debug/Display omit source content and tokens.
pub struct PublicError {
    /// Explicit original owner.
    pub target: OntologyTarget,
    /// Typed original cause and execution evidence.
    pub kind: PublicErrorKind,
}

impl PublicError {
    fn input(target: &OntologyTarget) -> Self {
        Self {
            target: target.clone(),
            kind: PublicErrorKind::InvalidInput,
        }
    }
    fn metadata(target: &OntologyTarget, cause: ItemError) -> Self {
        Self {
            target: target.clone(),
            kind: PublicErrorKind::Metadata(Box::new(cause)),
        }
    }
    fn invalid_metadata(target: &OntologyTarget, metadata: ItemMetadata) -> Self {
        Self {
            target: target.clone(),
            kind: PublicErrorKind::InvalidMetadata(metadata),
        }
    }
    fn operation(target: &OntologyTarget, cause: OperationError) -> Self {
        Self {
            target: target.clone(),
            kind: PublicErrorKind::Operation(Box::new(cause)),
        }
    }
    fn response(
        target: &OntologyTarget,
        response: OperationResponse,
        reason: &'static str,
        evidence: OperationError,
    ) -> Self {
        Self {
            target: target.clone(),
            kind: PublicErrorKind::InvalidResponse {
                reason,
                response: Box::new(response),
                evidence: Box::new(evidence),
            },
        }
    }
}

impl fmt::Debug for PublicError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PublicError(<scoped provider evidence>)")
    }
}
impl fmt::Display for PublicError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "public Ontology operation failed or is incomplete; inspect structured evidence",
        )
    }
}
impl std::error::Error for PublicError {}
