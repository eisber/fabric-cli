// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native CopyJob metadata, single-page listing and exact definition reads.
//!
//! Uses existing item and operation executors with caller-injected credentials,
//! bounds and cancellation. Definition acceptance is not completed source;
//! execution, reset, transfer and source editing are deliberately absent.

mod registration;
pub use registration::register;
pub mod create;

use crate::{
    Client,
    items::{ItemCollection, ItemError, ItemExecutor, ItemGetOptions, ItemMetadata},
    representations::{
        DefinitionRead, RepresentationError, RepresentationLimits, execute_definition,
    },
    v2::{ItemIdentity, WorkspaceIdentity},
};
use fabric_sdk_operations::{FabricOperation, OperationExecutor};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde::Serialize;
use serde_json::Value;
use std::{fmt, future::Future, num::NonZeroUsize};

/// Exact CopyJob workspace/item UUIDs, not a name, logical ID or run handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyJobTarget(ItemIdentity);

impl CopyJobTarget {
    /// Validates concrete identity without credentials, resolution or service I/O.
    ///
    /// # Errors
    /// Rejects malformed and route-changing identifiers.
    pub fn new(workspace_id: &str, item_id: &str) -> Result<Self, CopyJobError> {
        ItemIdentity::new(workspace_id, item_id)
            .map(Self)
            .map_err(|_| CopyJobError::InvalidInput)
    }

    /// Explicit owning workspace UUID.
    pub fn workspace_id(&self) -> &str {
        self.0.workspace_id()
    }

    /// Explicit concrete CopyJob UUID.
    pub fn item_id(&self) -> &str {
        self.0.item_id()
    }
}

/// Validated CopyJob metadata, preserving parsed order, nulls and future fields.
#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CopyJobMetadata(ItemMetadata);

impl CopyJobMetadata {
    /// Unmodified parsed metadata; not a writable or authenticated connection.
    pub fn as_value(&self) -> &Value {
        self.0.as_value()
    }
}

impl fmt::Debug for CopyJobMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CopyJobMetadata(<service data>)")
    }
}

/// No denied, malformed, incomplete or uncertain read becomes empty success.
#[derive(Debug)]
pub enum CopyJobError {
    /// Invalid identity, format or explicit list options before service I/O.
    InvalidInput,
    /// Malformed native result or inconsistent concrete owner identity.
    InvalidResponse,
    /// An unadvertised metadata body or status, preserving the actual outcome.
    UnexpectedOutcome(ResourceOutcome),
    /// Existing item reader failure, with structured service evidence.
    Metadata(ItemError),
    /// Existing definition/operation failure, with dispatch and handle evidence.
    Definition(RepresentationError),
    /// Existing shared list operation failure, not a discarded page.
    Operation(ResourceError),
}

impl fmt::Display for CopyJobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid CopyJob read target or options",
            Self::InvalidResponse => "invalid CopyJob native response or owner identity",
            Self::UnexpectedOutcome(_) => "CopyJob metadata response is not a completed page",
            Self::Metadata(_) => "CopyJob metadata retrieval failed",
            Self::Definition(_) => "CopyJob definition retrieval failed or is incomplete",
            Self::Operation(_) => "CopyJob page retrieval failed or is incomplete",
        })
    }
}

impl std::error::Error for CopyJobError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Metadata(error) => Some(error),
            Self::Definition(error) => Some(error),
            Self::Operation(error) => Some(error),
            _ => None,
        }
    }
}

/// Native metadata GET; never augments with a source definition or runtime state.
///
/// # Errors
/// Preserves item failures and rejects missing, malformed or mismatched identity.
pub async fn get_metadata(
    client: &Client,
    executor: &dyn ItemExecutor,
    target: &CopyJobTarget,
    max_response_bytes: NonZeroUsize,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<CopyJobMetadata, CopyJobError> {
    let metadata = client
        .get_item_metadata_from_collection(
            executor,
            target.workspace_id(),
            ItemCollection::CopyJob,
            target.item_id(),
            ItemGetOptions {
                query: Vec::new(),
                max_response_bytes,
            },
            cancelled,
        )
        .await
        .map_err(CopyJobError::Metadata)?;
    validate_metadata(&metadata, target.workspace_id(), Some(target.item_id()))?;
    Ok(CopyJobMetadata(metadata))
}

fn validate_metadata(
    metadata: &ItemMetadata,
    workspace: &str,
    item: Option<&str>,
) -> Result<(), CopyJobError> {
    let value = metadata.as_value();
    let observed_workspace = value["workspaceId"]
        .as_str()
        .ok_or(CopyJobError::InvalidResponse)?;
    let observed = CopyJobTarget::new(observed_workspace, metadata.id())
        .map_err(|_| CopyJobError::InvalidResponse)?;
    if metadata.item_type() != Some("CopyJob")
        || !observed.workspace_id().eq_ignore_ascii_case(workspace)
        || item.is_some_and(|item| !observed.item_id().eq_ignore_ascii_case(item))
        || value
            .get("description")
            .is_some_and(|v| !v.is_null() && !v.is_string())
        || ["folderId", "logicalId"].iter().any(|field| {
            value.get(field).is_some_and(|v| {
                !v.is_null()
                    && !v
                        .as_str()
                        .is_some_and(|id| WorkspaceIdentity::new(id).is_ok())
            })
        })
    {
        return Err(CopyJobError::InvalidResponse);
    }
    Ok(())
}

/// Optional native format token; absence leaves the service default unchanged.
///
/// # Errors
/// Rejects unsafe tokens, not unknown provider formats. Supported format names
/// remain service-authoritative; this does not select an output serializer.
pub fn validate_native_format(format: Option<&str>) -> Result<(), CopyJobError> {
    if format.is_some_and(|value| {
        value.is_empty()
            || value.len() > 128
            || !value.as_bytes()[0].is_ascii_alphanumeric()
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    }) {
        return Err(CopyJobError::InvalidInput);
    }
    Ok(())
}

/// One POST to the native CopyJob getDefinition route, without polling or retry.
///
/// # Errors
/// Preserves denial, response loss and accepted-operation evidence. Completed
/// results require the canonical content part, with no metadata fallback.
pub async fn get_definition(
    client: &Client,
    executor: &dyn OperationExecutor,
    target: &CopyJobTarget,
    native_format: Option<&str>,
    limits: RepresentationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<DefinitionRead, CopyJobError> {
    validate_native_format(native_format)?;
    let operation = ResourceOperation::new(
        ResourceMethod::Post,
        &[
            "workspaces",
            target.workspace_id(),
            "copyJobs",
            target.item_id(),
            "getDefinition",
        ],
        None,
        CompletionPolicy::ReturnAccepted,
    )
    .and_then(|operation| match native_format {
        Some(format) => operation.with_query(&[("format", format)]),
        None => Ok(operation),
    })
    .map_err(|_| CopyJobError::InvalidInput)?;
    let operation = FabricOperation::new(
        operation.method(),
        operation.path(),
        Vec::new(),
        operation.completion(),
    )
    .map_err(|_| CopyJobError::InvalidInput)?;
    let read = execute_definition(client, executor, operation, limits, cancelled)
        .await
        .map_err(CopyJobError::Definition)?;
    validate_definition(&read)?;
    Ok(read)
}

/// Checks completed CopyJob source, including results from the shared observer.
///
/// # Errors
/// Requires one exact canonical JSON content part. It preserves the envelope,
/// unknown fields, optional .platform, part order and future provider job modes.
/// No source/destination connection, checkpoint or execution capability is inferred.
pub fn validate_definition(read: &DefinitionRead) -> Result<(), CopyJobError> {
    let DefinitionRead::Completed { .. } = read else {
        return Ok(());
    };
    let bytes = read
        .part_bytes("copyjob-content.json")
        .map_err(CopyJobError::Definition)?;
    let content: Value =
        serde_json::from_slice(&bytes).map_err(|_| CopyJobError::InvalidResponse)?;
    let properties = content["properties"]
        .as_object()
        .ok_or(CopyJobError::InvalidResponse)?;
    if !properties
        .get("jobMode")
        .is_some_and(|v| v.as_str().is_some_and(|mode| !mode.is_empty()))
        || !content["activities"]
            .as_array()
            .is_some_and(|activities| activities.iter().all(Value::is_object))
    {
        return Err(CopyJobError::InvalidResponse);
    }
    Ok(())
}

/// Explicit native item-list options; no recursion or folder default is installed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyJobListOptions {
    /// Caller-selected native recursive flag, always sent explicitly.
    pub recursive: bool,
    /// Concrete root folder UUID, or None for the workspace.
    pub root_folder_id: Option<String>,
    /// Actual decoded service token; encoded exactly once and never followed as a URL.
    pub continuation_token: Option<String>,
}

/// One bounded native item-list page, not CopyJob runs or an exhaustive archive.
#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CopyJobPage(Value);

impl CopyJobPage {
    /// Original page data, including native continuations, ordering and duplicates.
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    /// Actual complete records in this response, before caller projection.
    pub fn records(&self) -> &[Value] {
        self.0["value"].as_array().expect("validated CopyJob page")
    }

    /// Whether the native page leaves more records unobserved.
    ///
    /// Even false does not establish a transactional snapshot or complete history.
    pub fn has_continuation(&self) -> bool {
        ["continuationToken", "continuationUri"]
            .iter()
            .any(|field| {
                self.0[*field]
                    .as_str()
                    .is_some_and(|value| !value.is_empty())
            })
    }
}

impl fmt::Debug for CopyJobPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CopyJobPage")
            .field("records", &self.records().len())
            .field("has_continuation", &self.has_continuation())
            .finish()
    }
}

/// Prepares one native workspace CopyJob metadata page for the common list owner.
///
/// # Errors
/// Rejects invalid IDs and control-bearing/empty continuation before execution.
/// The caller's shared ResourceOperations owns HTTP bounds/time/cancellation.
pub fn list_operation(
    workspace_id: &str,
    options: &CopyJobListOptions,
) -> Result<ResourceOperation, CopyJobError> {
    WorkspaceIdentity::new(workspace_id).map_err(|_| CopyJobError::InvalidInput)?;
    if options
        .root_folder_id
        .as_deref()
        .is_some_and(|id| WorkspaceIdentity::new(id).is_err())
        || options
            .continuation_token
            .as_deref()
            .is_some_and(|v| v.is_empty() || v.chars().any(char::is_control))
    {
        return Err(CopyJobError::InvalidInput);
    }
    let mut query = vec![(
        "recursive",
        if options.recursive { "true" } else { "false" },
    )];
    if let Some(folder) = &options.root_folder_id {
        query.push(("rootFolderId", folder));
    }
    if let Some(token) = &options.continuation_token {
        query.push(("continuationToken", token));
    }
    ResourceOperation::new(
        ResourceMethod::Get,
        &["workspaces", workspace_id, "copyJobs"],
        None,
        CompletionPolicy::ReturnAccepted,
    )
    .and_then(|op| op.with_query(&query))
    .map_err(|_| CopyJobError::InvalidInput)
}

/// Validates one native page from a bounded shared list operation.
///
/// # Errors
/// Rejects acceptance, wrong owners/UUIDs and malformed page/continuation shapes.
/// Native continuation URIs remain inert data; the common pager validates any follow-up.
pub fn decode_page(
    workspace_id: &str,
    outcome: ResourceOutcome,
) -> Result<CopyJobPage, CopyJobError> {
    WorkspaceIdentity::new(workspace_id).map_err(|_| CopyJobError::InvalidInput)?;
    let ResourceOutcome::Completed {
        status: 200,
        data: Some(ref value),
    } = outcome
    else {
        return Err(CopyJobError::UnexpectedOutcome(outcome));
    };
    let records = value["value"]
        .as_array()
        .ok_or(CopyJobError::InvalidResponse)?;
    for record in records {
        let metadata: ItemMetadata =
            serde_json::from_value(record.clone()).map_err(|_| CopyJobError::InvalidResponse)?;
        validate_metadata(&metadata, workspace_id, None)?;
    }
    for field in ["continuationToken", "continuationUri"] {
        if value
            .get(field)
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return Err(CopyJobError::InvalidResponse);
        }
    }
    Ok(CopyJobPage(value.clone()))
}

/// Delegates one metadata page request, leaving paging/projection to its owner.
///
/// # Errors
/// Preserves original shared failures and rejects unverified page data.
pub async fn list_page(
    executor: &mut dyn ResourceOperations,
    workspace_id: &str,
    options: &CopyJobListOptions,
) -> Result<CopyJobPage, CopyJobError> {
    let operation = list_operation(workspace_id, options)?;
    let outcome = executor
        .execute(operation)
        .await
        .map_err(CopyJobError::Operation)?;
    decode_page(workspace_id, outcome)
}
