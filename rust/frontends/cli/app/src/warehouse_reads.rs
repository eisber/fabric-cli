// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Resolved Warehouse reads for common and artifact-facade hosts.
//!
//! Only metadata has a settled common GET path. Connection, restore and LIST
//! callers supply typed selections; this module does not extend global grammar.
//! All execution uses the existing operation session and caller-owned deadline.

use crate::{
    get_backend::{backend_error, json_payload},
    get_command::{GetCommandError, GetPayload},
};
use fabric_sdk::{
    Client,
    sql_databases::{
        ContinuationToken, FolderScope, WorkspaceId,
        warehouse::{
            ConnectionOptions, ListWarehouses, RestorePointId, WarehouseError, WarehouseRequest,
            WarehouseRequests, WarehouseTarget,
        },
    },
};
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationExecutionError, OperationExecutor, OperationLimits,
    OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_paging::{PageResponse, PagingError, PagingErrorKind, PagingLimits, TokenPager};
use fabric_sdk_workspaces::mutations::ResourceOutcome;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    convert::Infallible,
    fmt,
    future::{Future, pending, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

/// One explicit native item representation, never an implicit definition or SQL read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WarehouseRepresentation {
    /// Primary Warehouse metadata only.
    Metadata,
    /// Inert native connection fields, without connecting.
    Connection(ConnectionOptions),
    /// One exact decoded native opaque restore-point key.
    RestorePoint(RestorePointId),
}

/// Exact resolved owner and representation; construction performs no I/O.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WarehouseItemRead {
    owner: WarehouseTarget,
    representation: WarehouseRepresentation,
}

impl WarehouseItemRead {
    /// Bind concrete UUIDs and the exact Warehouse type before any dispatch.
    ///
    /// # Errors
    /// Rejects names, foreign owner types and malformed native identities.
    pub fn new(
        workspace: &str,
        item: &str,
        owner_type: &str,
        representation: WarehouseRepresentation,
    ) -> Result<Self, WarehouseReadError> {
        let workspace = workspace
            .parse()
            .map_err(|_| WarehouseError::InvalidInput)?;
        let item = item.parse().map_err(|_| WarehouseError::InvalidInput)?;
        Ok(Self {
            owner: WarehouseTarget::new(workspace, item, owner_type)?,
            representation,
        })
    }
}

/// One resolved collection with explicit native options, not a path proposal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WarehouseCollection {
    /// Warehouse metadata in the selected workspace/folder.
    Warehouses {
        /// Concrete native workspace.
        workspace: WorkspaceId,
        /// Explicit choice; never the service's implicit recursive=true.
        recursive: bool,
        /// Exact selected root, without a folder union.
        root_folder: FolderScope,
    },
    /// Restore points owned by exactly one Warehouse.
    RestorePoints(WarehouseTarget),
}

/// Caller-owned LIST bounds; no numeric product defaults are added.
#[derive(Clone, Copy, Debug)]
pub struct WarehousePageLimits {
    /// Entire native page must fit; rows are never silently dropped.
    pub max_records: NonZeroUsize,
    /// Compact unprojected data array bound.
    pub max_data_bytes: NonZeroUsize,
    /// Existing transport/session request, response and delay limits.
    pub operations: OperationLimits,
}

/// Typed original failures; service evidence is never rendered implicitly.
pub enum WarehouseReadError {
    /// Native descriptor/owner/shape/status failure.
    Native(WarehouseError),
    /// Original shared credential, endpoint, service, acceptance or cancellation failure.
    Operation(Box<OperationError>),
    /// Original shared continuation validation failure with observed whole-page evidence.
    Paging(Box<PagingError<Infallible>>),
    /// The entire page or singleton cannot fit the caller's data/record bound.
    BudgetExceeded,
    /// Native paging cannot be resumed safely, including URI-only continuations.
    InvalidContinuation,
    /// A continuation belongs to another identity, endpoint, collection or options.
    ResumeMismatch,
    /// Native bytes, headers or serialization are malformed.
    InvalidResponse,
}

impl fmt::Debug for WarehouseReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(error) => fmt::Debug::fmt(error, f),
            Self::Paging(error) => fmt::Debug::fmt(error, f),
            Self::Operation(_) => f.write_str("WarehouseReadError::Operation(<shared evidence>)"),
            Self::BudgetExceeded => f.write_str("WarehouseReadError::BudgetExceeded"),
            Self::InvalidContinuation => f.write_str("WarehouseReadError::InvalidContinuation"),
            Self::ResumeMismatch => f.write_str("WarehouseReadError::ResumeMismatch"),
            Self::InvalidResponse => f.write_str("WarehouseReadError::InvalidResponse"),
        }
    }
}

impl From<WarehouseError> for WarehouseReadError {
    fn from(error: WarehouseError) -> Self {
        Self::Native(error)
    }
}

impl fmt::Display for WarehouseReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Warehouse read failed or is incomplete; inspect typed evidence")
    }
}

impl std::error::Error for WarehouseReadError {}

/// Process-local native resume provenance, not a portable CLI cursor.
///
/// Private construction binds the selected credential object, account/tenant,
/// endpoint, collection, native options and output bounds. It grants no access.
pub struct WarehouseContinuation {
    client: Client,
    collection: WarehouseCollection,
    max_records: NonZeroUsize,
    max_data_bytes: NonZeroUsize,
    token: ContinuationToken,
    seen: BTreeSet<String>,
}

impl fmt::Debug for WarehouseContinuation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WarehouseContinuation(<bound native token>)")
    }
}

/// One whole native page and honest collection/resume evidence.
#[derive(Debug)]
pub struct WarehousePageRead {
    /// Common structured payload; `data` is always an array, even empty/singleton.
    ///
    /// `complete` is false when more collection rows remain. Common GET delivery
    /// consequently cannot mistake a page receipt for a complete file export.
    pub payload: GetPayload,
    /// Exact native next token with process-local identity/options provenance.
    pub continuation: Option<WarehouseContinuation>,
}

/// Existing service dependencies only; no runtime, credentials or HTTP stack is created.
pub struct WarehouseReadHost<'a> {
    /// Explicit SDK context with selected identity and endpoint.
    pub client: &'a Client,
    /// Caller-owned shared execution and streaming response limits.
    pub operations: &'a dyn OperationExecutor,
}

impl WarehouseReadHost<'_> {
    /// Acquire exactly one item representation using the merged native decoder.
    ///
    /// The caller supplies its running whole-invocation cancellation/deadline.
    /// No definition, SQL, metadata fallback or operation polling occurs.
    ///
    /// # Errors
    /// Preserves original failures and rejects malformed/foreign native data.
    pub async fn get(
        &self,
        selection: &WarehouseItemRead,
        limits: OperationLimits,
        max_data_bytes: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<GetPayload, WarehouseReadError> {
        let (data, representation) = match &selection.representation {
            WarehouseRepresentation::Metadata => {
                let request = WarehouseRequests::get(&selection.owner)?;
                (
                    self.execute(request, limits, cancelled).await?.1,
                    "metadata",
                )
            }
            WarehouseRepresentation::Connection(options) => {
                let request = WarehouseRequests::connection(&selection.owner, *options)?;
                (
                    self.execute(request, limits, cancelled).await?.1,
                    "connection",
                )
            }
            WarehouseRepresentation::RestorePoint(id) => {
                let request = WarehouseRequests::restore_point(&selection.owner, id)?;
                (
                    self.execute(request, limits, cancelled).await?.1,
                    "restorePoint",
                )
            }
        };
        check_data_bytes(&data, max_data_bytes)?;
        let mut payload = json_payload(data);
        payload.data["meta"]["requestOwner"] = owner_value(selection.owner);
        payload.data["meta"]["ownerEvidence"] = json!("selectedRequest");
        payload.data["meta"]["representation"] = json!(representation);
        Ok(payload)
    }

    /// Acquire exactly one bounded page, validating continuation with the shared pager.
    ///
    /// This does not auto-follow pages. Native row order, duplicates and unknown
    /// fields are preserved. A page that exceeds bounds fails whole: no next-page
    /// token can skip omitted rows. Resume is a new caller-owned invocation.
    ///
    /// # Errors
    /// Preserves native/shared failures; rejects foreign/repeated continuations
    /// and mismatched resume provenance before issuing another request.
    pub async fn list_page(
        &self,
        collection: &WarehouseCollection,
        resume: Option<&WarehouseContinuation>,
        limits: WarehousePageLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<WarehousePageRead, WarehouseReadError> {
        if let Some(resume) = resume
            && (resume.collection != *collection
                || resume.max_records != limits.max_records
                || resume.max_data_bytes != limits.max_data_bytes
                || resume.client.selection() != self.client.selection()
                || resume.client.context().endpoint() != self.client.context().endpoint()
                || !Arc::ptr_eq(resume.client.credential(), self.client.credential()))
        {
            return Err(WarehouseReadError::ResumeMismatch);
        }
        let token = resume.map(|resume| resume.token.clone());
        let (response, native, path) = match collection {
            WarehouseCollection::Warehouses {
                workspace,
                recursive,
                root_folder,
            } => {
                let request = WarehouseRequests::list_page(
                    workspace,
                    &ListWarehouses {
                        recursive: *recursive,
                        root_folder: root_folder.clone(),
                        continuation_token: token,
                    },
                )?;
                let path = request.operation().path().to_owned();
                let (response, native) =
                    self.execute(request, limits.operations, cancelled).await?;
                (response, native, path)
            }
            WarehouseCollection::RestorePoints(owner) => {
                let request = WarehouseRequests::restore_points_page(owner, token.as_ref())?;
                let path = request.operation().path().to_owned();
                let (response, native) =
                    self.execute(request, limits.operations, cancelled).await?;
                (response, native, path)
            }
        };
        let base = self
            .client
            .context()
            .endpoint()
            .url(&path)
            .map_err(|_| WarehouseReadError::InvalidResponse)?;
        let body_token = native.get("continuationToken").and_then(Value::as_str);
        let header_tokens: Vec<_> = response
            .headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("x-ms-continuation"))
            .map(|(_, token)| token.as_str())
            .collect();
        if header_tokens.len() > 1 {
            return Err(WarehouseReadError::InvalidContinuation);
        }
        let header_token = header_tokens.first().map(|token| (*token).to_owned());
        if header_token
            .as_deref()
            .is_some_and(|header| body_token.is_some_and(|body| body != header))
        {
            return Err(WarehouseReadError::InvalidContinuation);
        }
        let active = header_token
            .as_deref()
            .or(body_token)
            .filter(|token| !token.is_empty());
        if native["continuationUri"]
            .as_str()
            .is_some_and(|uri| !uri.is_empty())
            && active.is_none()
        {
            return Err(WarehouseReadError::InvalidContinuation);
        }
        if active.is_some_and(|token| ContinuationToken::new(token).is_err()) {
            return Err(WarehouseReadError::InvalidContinuation);
        }
        let pager = TokenPager::new(
            base,
            PagingLimits {
                max_pages: NonZeroUsize::new(1).expect("one native page"),
                max_items: limits.max_records,
                max_response_bytes: limits.operations.max_response_bytes,
            },
        )
        .map_err(|_| WarehouseReadError::InvalidContinuation)?;
        // Validate the already-acquired page; the pager cannot dispatch HTTP or
        // follow a URI. PageLimit is the normal bounded-page coverage boundary.
        let mut native_page = serde_json::Map::new();
        for field in ["continuationToken", "continuationUri"] {
            if let Some(value) = native.get(field) {
                native_page.insert(field.into(), value.clone());
            }
        }
        if let Some(header) = header_token {
            native_page.insert("headerContinuation".into(), json!(header));
        }
        let response = PageResponse {
            status: response.status,
            body: response.body,
            headers: response.headers,
        };
        let progress = match pager
            .collect(|_| ready(Ok::<_, Infallible>(response.clone())), pending())
            .await
        {
            Ok(progress) => progress,
            Err(error) if matches!(error.kind, PagingErrorKind::PageLimit) => error.partial,
            Err(error) => return Err(WarehouseReadError::Paging(Box::new(error))),
        };
        let data = Value::Array(progress.value);
        check_data_bytes(&data, limits.max_data_bytes)?;
        let mut seen = resume.map(|resume| resume.seen.clone()).unwrap_or_default();
        let continuation = if let Some(token) = progress.next_token {
            if !seen.insert(token.clone()) {
                return Err(WarehouseReadError::InvalidContinuation);
            }
            Some(WarehouseContinuation {
                client: self.client.clone(),
                collection: collection.clone(),
                max_records: limits.max_records,
                max_data_bytes: limits.max_data_bytes,
                token: ContinuationToken::new(token)
                    .map_err(|_| WarehouseReadError::InvalidContinuation)?,
                seen,
            })
        } else {
            None
        };
        let has_more = continuation.is_some();
        let mut payload = json_payload(data);
        payload.complete = !has_more;
        payload.data["meta"] = json!({
            "outcome": if has_more { "incomplete" } else { "completed" },
            "requestTarget": collection_value(collection),
            "ownerEvidence": "selectedRequest",
            "coverage": {
                "collectionComplete": !has_more,
                "pagesReceived": progress.pages_received,
                "recordsReturned": payload.data["data"].as_array().expect("page array").len(),
                "hasMore": has_more,
                "outputTruncated": false,
                "resumeKind": if has_more { "boundNativeToken" } else { "none" }
            },
            "nativePage": native_page
        });
        Ok(WarehousePageRead {
            payload,
            continuation,
        })
    }

    async fn execute<T>(
        &self,
        request: WarehouseRequest<T>,
        limits: OperationLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<(OperationResponse, Value), WarehouseReadError> {
        let operation = FabricOperation::from_resource(request.operation())
            .and_then(FabricOperation::with_metadata_read_evidence)
            .map_err(|_| WarehouseReadError::InvalidResponse)?;
        let mut session = OperationSession::with_context(
            self.client.context(),
            BorrowedOperations(self.operations),
            limits,
            cancelled,
        );
        let response = session
            .execute(operation)
            .await
            .map_err(|error| WarehouseReadError::Operation(Box::new(error)))?;
        let data = if response.body.is_empty() {
            None
        } else {
            Some(
                serde_json::from_slice::<Value>(&response.body)
                    .map_err(|_| WarehouseReadError::InvalidResponse)?,
            )
        };
        request.decode(ResourceOutcome::Completed {
            status: response.status,
            data: data.clone(),
        })?;
        Ok((response, data.ok_or(WarehouseReadError::InvalidResponse)?))
    }
}

fn check_data_bytes(data: &Value, cap: NonZeroUsize) -> Result<(), WarehouseReadError> {
    crate::get_command::bounded_json(data, cap.get())
        .map(|_| ())
        .map_err(|_| WarehouseReadError::BudgetExceeded)
}

fn owner_value(owner: WarehouseTarget) -> Value {
    json!({
        "workspaceId": owner.workspace_id().to_string(),
        "itemId": owner.warehouse_id().to_string(),
        "type": "Warehouse"
    })
}

fn collection_value(collection: &WarehouseCollection) -> Value {
    match collection {
        WarehouseCollection::Warehouses {
            workspace,
            recursive,
            root_folder,
        } => json!({
            "workspaceId": workspace.to_string(),
            "type": "Warehouse",
            "collection": "warehouses",
            "recursive": recursive,
            "rootFolderId": match root_folder {
                FolderScope::Workspace => None,
                FolderScope::Folder(id) => Some(id.to_string())
            }
        }),
        WarehouseCollection::RestorePoints(owner) => {
            let mut value = owner_value(*owner);
            value["collection"] = json!("restorePoints");
            value
        }
    }
}

pub(crate) async fn metadata(
    client: &Client,
    operations: &dyn OperationExecutor,
    workspace: &str,
    item: &str,
    limits: OperationLimits,
    cap: NonZeroUsize,
) -> Result<GetPayload, GetCommandError> {
    let selection = WarehouseItemRead::new(
        workspace,
        item,
        "Warehouse",
        WarehouseRepresentation::Metadata,
    )
    .map_err(|error| backend_error("InvalidWarehouseTarget", error))?;
    WarehouseReadHost { client, operations }
        .get(&selection, limits, cap, pending())
        .await
        .map_err(|error| backend_error("WarehouseMetadataFailed", error))
}

struct BorrowedOperations<'a>(&'a dyn OperationExecutor);

impl OperationExecutor for BorrowedOperations<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.0.execute(request)
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.0.wait(delay)
    }
}
