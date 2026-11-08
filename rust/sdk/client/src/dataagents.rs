// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public DataAgent metadata, exact definitions and explicit-stage configuration reads.
//!
//! These bindings reuse shared Fabric execution and definition decoding. They do
//! not answer questions, publish, query sources, navigate ontologies, discover
//! credentials, or substitute authored/served documentation for configuration.

mod registration;
pub use fabric_sdk_dataagent_ontology::scope::Guid;
pub use registration::register_reads;

use crate::{
    Client,
    items::ItemMetadata,
    representations::{
        BorrowedExecutor, DefinitionRead, RepresentationError, RepresentationLimits,
    },
};
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutor, OperationLimits,
    OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::{
    Page,
    mutations::{CompletionPolicy, ResourceMethod, ResourceOperation},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, future::Future, num::NonZeroUsize, time::Duration};

/// One already-resolved DataAgent owner, distinct from any referenced datasource.
#[derive(Clone, Eq, PartialEq)]
pub struct DataAgentTarget {
    workspace_id: Guid,
    agent_id: Guid,
}

impl DataAgentTarget {
    /// Validates the exact owner and nonzero identifiers before credential work.
    ///
    /// # Errors
    /// Rejects other owner types and invalid identifiers; aliases are resolved upstream.
    pub fn new(
        workspace_id: &str,
        agent_id: &str,
        owner_type: &str,
    ) -> Result<Self, DataAgentError> {
        if owner_type != "DataAgent" {
            return Err(DataAgentError::WrongOwner);
        }
        Ok(Self {
            workspace_id: Guid::new(workspace_id).map_err(|_| DataAgentError::InvalidInput)?,
            agent_id: Guid::new(agent_id).map_err(|_| DataAgentError::InvalidInput)?,
        })
    }

    /// The workspace owning the agent, not a referenced source workspace.
    pub fn workspace_id(&self) -> &Guid {
        &self.workspace_id
    }

    /// The addressed DataAgent item identifier.
    pub fn agent_id(&self) -> &Guid {
        &self.agent_id
    }
}

impl fmt::Debug for DataAgentTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DataAgentTarget(<resolved>)")
    }
}

/// Explicit configuration selection; neither stage is an immutable historical revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// Unpublished draft configuration at the native staging route.
    Staging,
    /// Last published configuration at the native base route.
    Published,
}

/// Only the documented DataAgent native definition format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeFormat {
    /// Explicit JSON provider format, not a CLI receipt serializer.
    Json,
}

/// A decoded datasource-page token; it is never interpreted as a URL.
#[derive(Clone, Eq, PartialEq)]
pub struct ContinuationToken(String);

impl ContinuationToken {
    /// Validates a nonempty opaque token without modifying or pre-encoding it.
    ///
    /// # Errors
    /// Rejects empty or control-bearing tokens before any execution.
    pub fn new(value: impl Into<String>) -> Result<Self, DataAgentError> {
        let value = value.into();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(DataAgentError::InvalidInput);
        }
        Ok(Self(value))
    }

    /// The exact decoded token, not suitable for diagnostics.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ContinuationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContinuationToken(<redacted>)")
    }
}

/// Documented permissions, not proof of the caller's actual rights.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadPermission {
    /// Item read with Item.Read.All or Item.ReadWrite.All.
    ItemRead,
    /// Definition retrieval requires item read/write and Item.ReadWrite.All.
    DefinitionReadWrite,
    /// Configuration read with Item or DataAgent read/read-write scopes.
    ConfigurationRead,
}

impl ReadPermission {
    /// Documented delegated alternatives, not token acquisition or permission proof.
    pub const fn delegated_scopes(self) -> &'static [&'static str] {
        match self {
            Self::ItemRead => &["Item.Read.All", "Item.ReadWrite.All"],
            Self::DefinitionReadWrite => &["Item.ReadWrite.All"],
            Self::ConfigurationRead => &[
                "Item.Read.All",
                "Item.ReadWrite.All",
                "DataAgent.Read.All",
                "DataAgent.ReadWrite.All",
            ],
        }
    }
}

/// Capability facts for host policy and local discovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadSemantics {
    /// Required resource/delegated permission class.
    pub permission: ReadPermission,
    /// Configuration management is a public Preview contract.
    pub preview: bool,
    /// Explicit configuration stage, absent for metadata and the full definition.
    pub stage: Option<Stage>,
}

/// Ordered, forward-compatible DataAgent metadata with validated owner identity.
#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct DataAgentMetadata(ItemMetadata);

impl DataAgentMetadata {
    /// Complete native metadata, including unknown fields and publishedDescription.
    pub fn as_value(&self) -> &Value {
        self.0.as_value()
    }
}

impl fmt::Debug for DataAgentMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DataAgentMetadata(<inert>)")
    }
}

/// Stored stage settings, not a hidden host prompt or distilled served context.
#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct DataAgentSettings(Value);

impl DataAgentSettings {
    /// Inert stored instructions. Reading them never invokes a model.
    pub fn ai_instructions(&self) -> &str {
        self.0["aiInstructions"]
            .as_str()
            .expect("validated settings")
    }

    /// Full configuration response, retaining unknown service fields.
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

impl fmt::Debug for DataAgentSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DataAgentSettings(<inert>)")
    }
}

/// Native datasource metadata; this identity is not the referenced Fabric item.
#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct DatasourceMetadata(Value);

impl DatasourceMetadata {
    /// The validated datasource-management ID, used only in the agent's scope.
    pub fn id(&self) -> &str {
        self.0["id"].as_str().expect("validated datasource")
    }

    /// Native record including source type and independently scoped references.
    ///
    /// Reference metadata does not authorize navigation, execution or attachment changes.
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

impl<'de> Deserialize<'de> for DatasourceMetadata {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if !value.is_object()
            || value
                .get("id")
                .and_then(Value::as_str)
                .is_none_or(|id| Guid::new(id).is_err())
            || !value.get("displayName").is_some_and(Value::is_string)
            || !value.get("type").is_some_and(Value::is_string)
        {
            return Err(serde::de::Error::custom("invalid datasource metadata"));
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for DatasourceMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DatasourceMetadata(<inert>)")
    }
}

/// One complete native page, not a promise that the collection was exhausted.
#[derive(Clone, PartialEq, Serialize)]
pub struct DatasourcePage {
    /// Full records and original continuation URI, retained as inert service data.
    pub page: Page<DatasourceMetadata>,
    /// Validated decoded token for an explicit next-page request.
    #[serde(skip)]
    pub next_token: Option<ContinuationToken>,
}

impl DatasourcePage {
    /// Whether the service reports no remaining records in this selected stage.
    pub fn is_complete(&self) -> bool {
        self.next_token.is_none() && self.page.continuation_uri.is_none()
    }
}

impl fmt::Debug for DatasourcePage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatasourcePage")
            .field("items", &self.page.value.len())
            .field("complete", &self.is_complete())
            .finish()
    }
}

/// Read failures never switch stages, authorities, representations or credentials.
#[derive(Debug)]
pub enum DataAgentError {
    /// An identifier or opaque selector is invalid.
    InvalidInput,
    /// The explicitly resolved item is not a DataAgent.
    WrongOwner,
    /// A response identifies a different item or workspace.
    IdentityMismatch,
    /// Known response fields or completion status are malformed.
    InvalidResponse,
    /// Original shared operation failure, including HTTP/permission/acceptance evidence.
    Operation(OperationError),
    /// Original exact-definition decoder failure, without a metadata fallback.
    Definition(RepresentationError),
    /// Cancellation or a byte limit after a response, preserving its bounded evidence.
    Response(Box<ReadResponseError>),
}

/// Observed native response retained when a later read phase cannot complete.
///
/// Fields are inert service data. Debug/Display never format headers or content.
pub struct ReadResponseError {
    /// Original session error, including accepted handle and dispatch uncertainty.
    pub error: OperationError,
    /// Exact already-bounded HTTP response; this is not a completed read result.
    pub response: OperationResponse,
}

impl fmt::Debug for ReadResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadResponseError")
            .field("status", &self.response.status)
            .field("bytes", &self.response.body.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for DataAgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid DataAgent read target or selector",
            Self::WrongOwner => "the read requires an exact DataAgent owner",
            Self::IdentityMismatch => "DataAgent response identity does not match the target",
            Self::InvalidResponse => "invalid DataAgent read response",
            Self::Operation(_) => "DataAgent read failed or is incomplete",
            Self::Definition(_) => "DataAgent definition is invalid or incomplete",
            Self::Response(_) => "DataAgent response was observed but the read did not complete",
        })
    }
}

impl std::error::Error for DataAgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Operation(error) => Some(error),
            Self::Definition(error) => Some(error),
            Self::Response(error) => Some(&error.error),
            _ => None,
        }
    }
}

/// A prepared fixed-route read and its decoder, shared across execution heads.
pub struct ReadRequest<T> {
    operation: ResourceOperation,
    target: DataAgentTarget,
    semantics: ReadSemantics,
    decoder: fn(&[u8], &DataAgentTarget) -> Result<T, DataAgentError>,
}

impl<T> ReadRequest<T> {
    /// Fixed public descriptor for offline method/path/permission inspection.
    pub fn operation(&self) -> &ResourceOperation {
        &self.operation
    }

    /// Documented capability facts, not permission to execute.
    pub const fn semantics(&self) -> ReadSemantics {
        self.semantics
    }

    /// Dispatches one read through the existing authenticated bounded operation session.
    ///
    /// # Errors
    /// Retains service errors and rejects undocumented acceptance, malformed
    /// responses and over-budget results. No continuation is followed implicitly.
    pub async fn execute(
        self,
        client: &Client,
        executor: &dyn OperationExecutor,
        limits: RepresentationLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<T, DataAgentError> {
        dispatch(
            client,
            executor,
            &self.operation,
            limits,
            cancelled,
            |response, _| {
                if response.status != 200 {
                    return Err(DataAgentError::InvalidResponse);
                }
                (self.decoder)(&response.body, &self.target)
            },
        )
        .await
    }
}

/// Only positively enumerated public reads; no private or mutating operation stubs.
pub struct DataAgentRequests;

impl DataAgentRequests {
    /// Reads exact native item metadata, with no definition or configuration fan-out.
    ///
    /// # Errors
    /// Returns local descriptor validation errors before credentials or execution.
    pub fn metadata(
        target: &DataAgentTarget,
    ) -> Result<ReadRequest<DataAgentMetadata>, DataAgentError> {
        make_read(target, None, &[], &[], decode_metadata)
    }

    /// Reads inert settings at the explicitly selected stage.
    ///
    /// # Errors
    /// Returns local descriptor validation errors; no absent-stage default exists.
    pub fn settings(
        target: &DataAgentTarget,
        stage: Stage,
    ) -> Result<ReadRequest<DataAgentSettings>, DataAgentError> {
        make_read(target, Some(stage), &["settings"], &[], decode_settings)
    }

    /// Reads one configured datasource page in the explicit stage.
    ///
    /// # Errors
    /// Returns local descriptor validation errors. A token is encoded once as
    /// data; a returned continuation URI is never used as an execution endpoint.
    pub fn datasources_page(
        target: &DataAgentTarget,
        stage: Stage,
        token: Option<&ContinuationToken>,
    ) -> Result<ReadRequest<DatasourcePage>, DataAgentError> {
        let query = token.map(|token| ("continuationToken", token.as_str()));
        make_read(
            target,
            Some(stage),
            &["datasources"],
            query.as_slice(),
            decode_datasources,
        )
    }

    /// Prepares the exact public definition POST, with provider default or explicit JSON.
    ///
    /// # Errors
    /// Returns local descriptor validation errors. Native formats outside the
    /// documented enum cannot be supplied through this binding.
    pub fn definition(
        target: &DataAgentTarget,
        format: Option<NativeFormat>,
    ) -> Result<DefinitionRequest, DataAgentError> {
        let query = format.map(|NativeFormat::Json| ("format", "JSON"));
        Ok(DefinitionRequest {
            operation: operation(
                target,
                None,
                &["getDefinition"],
                query.as_slice(),
                ResourceMethod::Post,
            )?,
        })
    }
}

/// Exact multipart retrieval, distinct from metadata and stored stage settings.
pub struct DefinitionRequest {
    operation: ResourceOperation,
}

impl DefinitionRequest {
    /// Native semantic-read POST descriptor; it is not a publish/update operation.
    pub fn operation(&self) -> &ResourceOperation {
        &self.operation
    }

    /// Definition retrieval needs read/write rights despite being a semantic read.
    pub const fn semantics(&self) -> ReadSemantics {
        ReadSemantics {
            permission: ReadPermission::DefinitionReadWrite,
            preview: false,
            stage: None,
        }
    }

    /// Retrieves exact parts or a real accepted handle through shared execution.
    ///
    /// # Errors
    /// Preserves original HTTP/operation evidence and shared strict definition
    /// errors. Acceptance is not completed content; observation is caller-owned.
    pub async fn execute(
        self,
        client: &Client,
        executor: &dyn OperationExecutor,
        limits: RepresentationLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<DefinitionRead, DataAgentError> {
        dispatch(
            client,
            executor,
            &self.operation,
            limits,
            cancelled,
            |response, operation_id| {
                if response.status == 202 {
                    return Ok(DefinitionRead::Accepted {
                        operation_id: operation_id.ok_or(DataAgentError::InvalidResponse)?,
                    });
                }
                if response.status != 200 {
                    return Err(DataAgentError::InvalidResponse);
                }
                DefinitionRead::from_completed_result(&response.body, limits)
                    .map_err(DataAgentError::Definition)
            },
        )
        .await
    }
}

fn operation(
    target: &DataAgentTarget,
    stage: Option<Stage>,
    tail: &[&str],
    query: &[(&str, &str)],
    method: ResourceMethod,
) -> Result<ResourceOperation, DataAgentError> {
    let mut segments = vec![
        "workspaces",
        target.workspace_id.as_str(),
        "dataAgents",
        target.agent_id.as_str(),
    ];
    if stage == Some(Stage::Staging) {
        segments.push("staging");
    }
    segments.extend_from_slice(tail);
    ResourceOperation::new(method, &segments, None, CompletionPolicy::ReturnAccepted)
        .and_then(|operation| {
            if query.is_empty() {
                Ok(operation)
            } else {
                operation.with_query(query)
            }
        })
        .map_err(|_| DataAgentError::InvalidInput)
}

fn make_read<T>(
    target: &DataAgentTarget,
    stage: Option<Stage>,
    tail: &[&str],
    query: &[(&str, &str)],
    decoder: fn(&[u8], &DataAgentTarget) -> Result<T, DataAgentError>,
) -> Result<ReadRequest<T>, DataAgentError> {
    Ok(ReadRequest {
        operation: operation(target, stage, tail, query, ResourceMethod::Get)?,
        target: target.clone(),
        semantics: ReadSemantics {
            permission: if stage.is_some() {
                ReadPermission::ConfigurationRead
            } else {
                ReadPermission::ItemRead
            },
            preview: stage.is_some(),
            stage,
        },
        decoder,
    })
}

async fn dispatch<T>(
    client: &Client,
    executor: &dyn OperationExecutor,
    operation: &ResourceOperation,
    limits: RepresentationLimits,
    cancelled: impl Future<Output = ()> + Send,
    decode: impl FnOnce(&OperationResponse, Option<String>) -> Result<T, DataAgentError>,
) -> Result<T, DataAgentError> {
    let operation =
        FabricOperation::from_resource(operation).map_err(|_| DataAgentError::InvalidInput)?;
    let mut session = OperationSession::with_context(
        client.context(),
        BorrowedExecutor(executor),
        OperationLimits {
            max_requests: NonZeroUsize::new(1).unwrap(),
            max_response_bytes: limits.max_response_bytes,
            max_poll_delay: Duration::ZERO,
        },
        cancelled,
    );
    let response = session
        .execute(operation)
        .await
        .map_err(DataAgentError::Operation)?;
    if response.body.len() > limits.max_result_bytes.get() {
        return Err(DataAgentError::Response(Box::new(ReadResponseError {
            error: session.evidence(OperationErrorKind::Limit),
            response,
        })));
    }
    session
        .check_cancelled()
        .await
        .map_err(DataAgentError::Operation)?;
    let result = decode(&response, session.operation_id().map(str::to_owned));
    if let Err(error) = session.check_cancelled().await {
        return Err(DataAgentError::Response(Box::new(ReadResponseError {
            error,
            response,
        })));
    }
    result
}

fn decode_metadata(
    body: &[u8],
    target: &DataAgentTarget,
) -> Result<DataAgentMetadata, DataAgentError> {
    let metadata: ItemMetadata =
        serde_json::from_slice(body).map_err(|_| DataAgentError::InvalidResponse)?;
    if metadata.item_type() != Some("DataAgent") {
        return Err(DataAgentError::WrongOwner);
    }
    let workspace = metadata
        .as_value()
        .get("workspaceId")
        .and_then(Value::as_str)
        .and_then(|id| Guid::new(id).ok())
        .ok_or(DataAgentError::InvalidResponse)?;
    let item = Guid::new(metadata.id()).map_err(|_| DataAgentError::InvalidResponse)?;
    if workspace != target.workspace_id || item != target.agent_id {
        return Err(DataAgentError::IdentityMismatch);
    }
    if let Some(properties) = metadata
        .as_value()
        .get("properties")
        .filter(|v| !v.is_null())
        && (!properties.is_object()
            || properties
                .get("publishedDescription")
                .is_some_and(|v| !v.is_null() && !v.is_string()))
    {
        return Err(DataAgentError::InvalidResponse);
    }
    Ok(DataAgentMetadata(metadata))
}

fn decode_settings(body: &[u8], _: &DataAgentTarget) -> Result<DataAgentSettings, DataAgentError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| DataAgentError::InvalidResponse)?;
    if !value.is_object() || !value.get("aiInstructions").is_some_and(Value::is_string) {
        return Err(DataAgentError::InvalidResponse);
    }
    Ok(DataAgentSettings(value))
}

fn decode_datasources(body: &[u8], _: &DataAgentTarget) -> Result<DatasourcePage, DataAgentError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| DataAgentError::InvalidResponse)?;
    if !value.is_object() {
        return Err(DataAgentError::InvalidResponse);
    }
    let page: Page<DatasourceMetadata> =
        serde_json::from_value(value).map_err(|_| DataAgentError::InvalidResponse)?;
    let next_token = page
        .continuation_token
        .clone()
        .map(ContinuationToken::new)
        .transpose()
        .map_err(|_| DataAgentError::InvalidResponse)?;
    if page
        .continuation_uri
        .as_ref()
        .is_some_and(|uri| uri.is_empty() || uri.chars().any(char::is_control))
    {
        return Err(DataAgentError::InvalidResponse);
    }
    Ok(DatasourcePage { page, next_token })
}
