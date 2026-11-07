// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded primary workspace metadata and one native page of workspace items.
//!
//! These reads never discover names, enrich metadata, enumerate folders, or
//! expose a resumable service token. Data budgets measure compact UTF-8 JSON
//! before any caller-owned projection, separately from transport safety limits.

use crate::{ApiClient, ApiError, ExecutionError, Limits, Paging, Request, Response, cancellable};
use azure_core::http::Method;
use serde_json::Value;
use std::{
    fmt,
    future::{Future, ready},
    num::NonZeroUsize,
    pin::pin,
};

/// The explicitly selected workspace-owned read surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceReadKind {
    /// Primary workspace metadata, without enrichment.
    Metadata,
    /// One native service page of workspace items, without a folder union.
    Items,
}

/// A canonical workspace identifier and explicit finite output bounds.
///
/// Construction performs no credential acquisition, discovery, or network I/O.
/// Debug output excludes the identifier.
#[derive(Clone)]
pub struct WorkspaceReadRequest {
    workspace_id: String,
    kind: WorkspaceReadKind,
    max_data_bytes: NonZeroUsize,
    max_records: Option<NonZeroUsize>,
    recursive: bool,
}

impl WorkspaceReadRequest {
    /// Selects primary metadata for an exact, hyphenated ASCII workspace UUID.
    ///
    /// Hexadecimal letters are normalized to lowercase. Names, path fragments,
    /// braces, surrounding whitespace, and noncanonical UUID layouts are rejected.
    pub fn metadata(
        workspace: &str,
        max_data_bytes: NonZeroUsize,
    ) -> Result<Self, WorkspaceReadError> {
        Ok(Self {
            workspace_id: canonical_workspace(workspace)?,
            kind: WorkspaceReadKind::Metadata,
            max_data_bytes,
            max_records: None,
            recursive: false,
        })
    }

    /// Selects one native items page with explicit record, byte and recursion limits.
    ///
    /// The record and data bounds do not alter native paging or request limits.
    /// No implicit type filter, root folder, name resolution, or folder union is used.
    pub fn items(
        workspace: &str,
        max_records: NonZeroUsize,
        max_data_bytes: NonZeroUsize,
        recursive: bool,
    ) -> Result<Self, WorkspaceReadError> {
        Ok(Self {
            workspace_id: canonical_workspace(workspace)?,
            kind: WorkspaceReadKind::Items,
            max_data_bytes,
            max_records: Some(max_records),
            recursive,
        })
    }

    /// Borrows the canonical workspace UUID; callers must not log it.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Returns the explicitly selected read surface.
    pub const fn kind(&self) -> WorkspaceReadKind {
        self.kind
    }

    /// Returns the compact serialized data budget, before projection.
    pub const fn max_data_bytes(&self) -> NonZeroUsize {
        self.max_data_bytes
    }
}

impl fmt::Debug for WorkspaceReadRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceReadRequest")
            .field("kind", &self.kind)
            .field("max_data_bytes", &self.max_data_bytes)
            .field("max_records", &self.max_records)
            .field("recursive", &self.recursive)
            .finish_non_exhaustive()
    }
}

/// Sanitized workspace read failures, without service text, identifiers or tokens.
///
/// Host-only configuration and output variants support a unified envelope;
/// the SDK performs no configuration or output I/O.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceReadError {
    /// The workspace identifier or request is invalid.
    InvalidInput,
    /// The caller has not configured a workspace read host.
    NotConfigured,
    /// The injected client does not support this bound Fabric read.
    UnsupportedCapability,
    /// The caller's output destination already exists.
    OutputExists,
    /// The caller could not reserve or write its output destination.
    OutputFailed,
    /// The status, response identity, JSON shape or continuation is invalid.
    InvalidResponse,
    /// Credential acquisition failed or the service returned HTTP 401.
    Unauthenticated,
    /// The service returned HTTP 403; no privileged fallback was attempted.
    AccessDenied,
    /// The service returned HTTP 404.
    NotFound,
    /// No whole result fits, or a shared finite safety limit was exceeded.
    BudgetExceeded,
    /// Caller cancellation won and pending local work was dropped.
    Cancelled,
    /// The injected executor's deadline elapsed.
    Timeout,
    /// Another service or shared execution failure occurred.
    OperationFailed,
}

impl WorkspaceReadError {
    /// Returns a stable code containing no untrusted provider data.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput => "InvalidInput",
            Self::NotConfigured => "NotConfigured",
            Self::UnsupportedCapability => "UnsupportedCapability",
            Self::OutputExists => "OutputExists",
            Self::OutputFailed => "OutputFailed",
            Self::InvalidResponse => "InvalidResponse",
            Self::Unauthenticated => "Unauthenticated",
            Self::AccessDenied => "AccessDenied",
            Self::NotFound => "NotFound",
            Self::BudgetExceeded => "BudgetExceeded",
            Self::Cancelled => "Cancelled",
            Self::Timeout => "Timeout",
            Self::OperationFailed => "OperationFailed",
        }
    }
}

impl fmt::Display for WorkspaceReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "Invalid workspace read input.",
            Self::NotConfigured => "The workspace read host is not configured.",
            Self::UnsupportedCapability => "The workspace read capability is unavailable.",
            Self::OutputExists => "The workspace read output destination already exists.",
            Self::OutputFailed => "The workspace read output could not be written.",
            Self::InvalidResponse => "The workspace read response is invalid.",
            Self::Unauthenticated => "Authentication is required for the workspace read.",
            Self::AccessDenied => "Access to the workspace read was denied.",
            Self::NotFound => "The requested workspace resource was not found.",
            Self::BudgetExceeded => "The workspace read exceeded its finite budget.",
            Self::Cancelled => "The workspace read was cancelled.",
            Self::Timeout => "The workspace read timed out.",
            Self::OperationFailed => "The workspace read failed.",
        })
    }
}

impl std::error::Error for WorkspaceReadError {}

/// Whole primary metadata or an ordered whole-row items prefix.
///
/// Item truncation is an explicit incomplete success, not evidence that the
/// workspace has no other items. No raw service token or resumable cursor is
/// returned. Debug output excludes the service data.
#[derive(Clone, PartialEq)]
pub struct WorkspaceReadResult {
    /// Unprojected primary metadata object or items array, including unknown fields.
    pub data: Value,
    /// True only when the entire selected native collection or metadata fits.
    pub complete: bool,
    /// True when native continuation, record or data bounds leave items unread.
    pub truncated: bool,
}

impl fmt::Debug for WorkspaceReadResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceReadResult")
            .field("complete", &self.complete)
            .field("truncated", &self.truncated)
            .finish_non_exhaustive()
    }
}

/// Reads only the explicitly selected primary metadata or native items surface.
///
/// Reuses the injected [`ApiClient`] for credentials, execution and paging.
/// Primary metadata rejects active continuations. Items reject malformed or
/// conflicting continuation fields before the shared one-page paging policy.
/// The response safety ceiling remains independent of the explicit data budget.
/// Cancellation is checked before authentication and again before committing
/// decoded, budgeted data. The caller owns deadlines and the async runtime.
pub async fn read_workspace(
    client: &ApiClient,
    request: &WorkspaceReadRequest,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<WorkspaceReadResult, WorkspaceReadError> {
    let mut cancelled = pin!(cancelled);
    let result = match request.kind {
        WorkspaceReadKind::Metadata => metadata(client, request, cancelled.as_mut()).await,
        WorkspaceReadKind::Items => items(client, request, cancelled.as_mut()).await,
    };
    // The cancellation future may already be complete and must not be repolled.
    if matches!(result, Err(WorkspaceReadError::Cancelled)) {
        return result;
    }
    cancellable(ready(result), cancelled.as_mut())
        .await
        .map_err(read_error)?
}

fn canonical_workspace(workspace: &str) -> Result<String, WorkspaceReadError> {
    if workspace.len() != 36
        || !workspace.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Err(WorkspaceReadError::InvalidInput);
    }
    Ok(workspace.to_ascii_lowercase())
}

async fn metadata(
    client: &ApiClient,
    request: &WorkspaceReadRequest,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<WorkspaceReadResult, WorkspaceReadError> {
    let route = Request::fabric(Method::Get, &["workspaces", request.workspace_id()])
        .map_err(read_error)?;
    let response = client
        .send(&route, Limits::default().max_response_bytes, cancelled)
        .await
        .map_err(read_error)?;
    crate::require_status(&response, &[200]).map_err(read_error)?;
    let data: Value =
        serde_json::from_slice(&response.body).map_err(|_| WorkspaceReadError::InvalidResponse)?;
    if !data.is_object()
        || !matching_workspace(data.get("id"), request.workspace_id())
        || data.get("displayName").and_then(Value::as_str).is_none()
    {
        return Err(WorkspaceReadError::InvalidResponse);
    }
    let token = continuation_string(data.get("continuationToken")).map_err(read_error)?;
    let uri = continuation_string(data.get("continuationUri")).map_err(read_error)?;
    let header = continuation_header(&response).map_err(read_error)?;
    if [token, uri, header]
        .into_iter()
        .flatten()
        .any(|value| !value.is_empty())
    {
        return Err(WorkspaceReadError::InvalidResponse);
    }
    if serialized_bytes(&data)? > request.max_data_bytes.get() {
        return Err(WorkspaceReadError::BudgetExceeded);
    }
    Ok(WorkspaceReadResult {
        data,
        complete: true,
        truncated: false,
    })
}

async fn items(
    client: &ApiClient,
    request: &WorkspaceReadRequest,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<WorkspaceReadResult, WorkspaceReadError> {
    let route = Request::fabric(
        Method::Get,
        &["workspaces", request.workspace_id(), "items"],
    )
    .and_then(|route| route.with_query(&[("recursive".into(), request.recursive.to_string())]))
    .map_err(read_error)?;
    let limits = Limits {
        max_pages: NonZeroUsize::new(1).expect("one page is nonzero"),
        ..Limits::default()
    };
    let collection = match client
        .collect_with(
            route,
            "value",
            Paging::Query,
            limits,
            cancelled,
            decode_items_page,
        )
        .await
    {
        Ok(collection) => collection,
        Err(error)
            if error.error == ApiError::Limit
                && error.partial.pages == 1
                && error.partial.next_token.is_some() =>
        {
            // Only the page limit can retain one committed page and a validated
            // next token here. Response/row limits cannot commit the first page;
            // unsafe/repeated continuations have a distinct shared error.
            *error.partial
        }
        Err(error) => return Err(read_error(error.error)),
    };
    if collection.rows.iter().any(|row| {
        !row.is_object()
            || ["id", "displayName", "type"]
                .iter()
                .any(|field| row.get(*field).and_then(Value::as_str).is_none())
            || row
                .get("workspaceId")
                .is_some_and(|id| !matching_workspace(Some(id), request.workspace_id()))
    }) {
        return Err(WorkspaceReadError::InvalidResponse);
    }
    let max_records = request
        .max_records
        .ok_or(WorkspaceReadError::InvalidInput)?
        .get();
    let mut bytes = 2usize;
    if bytes > request.max_data_bytes.get() {
        return Err(WorkspaceReadError::BudgetExceeded);
    }
    let native_count = collection.rows.len();
    let mut rows = Vec::new();
    for row in collection.rows {
        if rows.len() == max_records {
            break;
        }
        let next_bytes = bytes
            .checked_add(serialized_bytes(&row)?)
            .and_then(|size| size.checked_add(usize::from(!rows.is_empty())));
        let Some(next_bytes) = next_bytes.filter(|size| *size <= request.max_data_bytes.get())
        else {
            if rows.is_empty() {
                return Err(WorkspaceReadError::BudgetExceeded);
            }
            break;
        };
        bytes = next_bytes;
        rows.push(row);
    }
    let truncated = rows.len() < native_count || collection.next_token.is_some();
    Ok(WorkspaceReadResult {
        data: Value::Array(rows),
        complete: !truncated,
        truncated,
    })
}

fn decode_items_page(response: &Response) -> Result<Value, ApiError> {
    crate::require_status(response, &[200])?;
    let data: Value = serde_json::from_slice(&response.body).map_err(|_| ApiError::InvalidJson)?;
    if !data.is_object() {
        return Err(ApiError::InvalidResponse);
    }
    let body = continuation_string(data.get("continuationToken"))?;
    let header = continuation_header(response)?;
    let uri = continuation_string(data.get("continuationUri"))?;
    if let (Some(header), Some(body)) = (header, body)
        && header != body
    {
        return Err(ApiError::Continuation);
    }
    if uri.is_some_and(|uri| !uri.is_empty())
        && !header.or(body).is_some_and(|token| !token.is_empty())
    {
        return Err(ApiError::Continuation);
    }
    Ok(data)
}

fn continuation_string(value: Option<&Value>) -> Result<Option<&str>, ApiError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.as_str())),
        _ => Err(ApiError::Continuation),
    }
}

fn continuation_header(response: &Response) -> Result<Option<&str>, ApiError> {
    let mut token: Option<&str> = None;
    for (_, value) in response
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("x-ms-continuation"))
    {
        if token.is_some_and(|prior| prior != value.as_str()) {
            return Err(ApiError::Continuation);
        }
        token = Some(value.as_str());
    }
    Ok(token)
}

fn matching_workspace(id: Option<&Value>, workspace: &str) -> bool {
    id.and_then(Value::as_str)
        .is_some_and(|id| id.eq_ignore_ascii_case(workspace))
}

fn serialized_bytes(value: &Value) -> Result<usize, WorkspaceReadError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|_| WorkspaceReadError::InvalidResponse)
}

fn read_error(error: ApiError) -> WorkspaceReadError {
    match error {
        ApiError::UnsupportedRequest | ApiError::InvalidAudience => {
            WorkspaceReadError::UnsupportedCapability
        }
        ApiError::InvalidInput => WorkspaceReadError::InvalidInput,
        ApiError::Credential => WorkspaceReadError::Unauthenticated,
        ApiError::Cancelled | ApiError::Execution(ExecutionError::Cancelled) => {
            WorkspaceReadError::Cancelled
        }
        ApiError::Execution(ExecutionError::TimedOut) => WorkspaceReadError::Timeout,
        ApiError::Limit | ApiError::Execution(ExecutionError::ResponseLimit) => {
            WorkspaceReadError::BudgetExceeded
        }
        ApiError::Http { status: 401, .. } | ApiError::Service { status: 401, .. } => {
            WorkspaceReadError::Unauthenticated
        }
        ApiError::Http { status: 403, .. } | ApiError::Service { status: 403, .. } => {
            WorkspaceReadError::AccessDenied
        }
        ApiError::Http { status: 404, .. } | ApiError::Service { status: 404, .. } => {
            WorkspaceReadError::NotFound
        }
        ApiError::UnsupportedStatus(_)
        | ApiError::InvalidResponse
        | ApiError::InvalidJson
        | ApiError::Continuation => WorkspaceReadError::InvalidResponse,
        ApiError::Operation(_)
        | ApiError::MutationUncertain(_)
        | ApiError::Service { .. }
        | ApiError::Http { .. }
        | ApiError::Execution(ExecutionError::Failed | ExecutionError::RetriesExhausted) => {
            WorkspaceReadError::OperationFailed
        }
    }
}
