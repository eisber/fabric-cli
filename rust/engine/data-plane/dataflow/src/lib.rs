// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native Dataflow Execute Query using a caller-owned shared operation session.
//!
//! No credentials, transport, runtime, scheduler, Arrow decoder or identity
//! discovery is created here. Accepted execution is not a completed query.
//! Successful bytes still require Arrow decoding and inspection of the service's
//! `PQ Arrow Metadata` failure column before reporting tabular success.

use fabric_sdk_operations::{
    FabricOperation, OperationAccept, OperationError, OperationErrorKind, OperationExecutor,
    OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Local UTF-8 byte limit for a named query, not a service query-length claim.
pub const MAX_QUERY_NAME_BYTES: usize = 4096;
/// Local serialized JSON request byte limit, including the optional mashup.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// The service's maximum query execution duration, not a client deadline.
pub const SERVICE_QUERY_MAX_SECONDS: u64 = 90;

#[cfg(test)]
mod ci_measurement {
    #[test]
    fn query_name_budget_fits_within_request_budget() {
        assert!(std::hint::black_box(super::MAX_QUERY_NAME_BYTES) < super::MAX_REQUEST_BYTES);
    }
}

/// Explicit response encoding; never automatically retried with another version.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ArrowVersion {
    /// Original encoding, also supported through on-premises data gateways.
    #[serde(rename = "1")]
    V1,
    /// Newer encoding, not supported through on-premises data gateways.
    #[serde(rename = "2")]
    V2,
}

impl ArrowVersion {
    /// Exact validated `Accept` value sent by the shared execution engine.
    pub const fn accept(self) -> OperationAccept {
        match self {
            Self::V1 => OperationAccept::ArrowV1,
            Self::V2 => OperationAccept::ArrowV2,
        }
    }

    const fn parameter(self) -> &'static str {
        match self {
            Self::V1 => "1",
            Self::V2 => "2",
        }
    }
}

/// Explicit UUID target; no path lookup or ambient workspace selection.
#[derive(Clone, Eq, PartialEq)]
pub struct ExecuteQueryTarget {
    workspace_id: String,
    dataflow_id: String,
}

impl ExecuteQueryTarget {
    /// Validate exact dashed UUIDs before any credential can be acquired.
    ///
    /// # Errors
    /// Rejects names, empty IDs, whitespace, path fragments and malformed UUIDs.
    pub fn new(workspace_id: &str, dataflow_id: &str) -> Result<Self, ExecuteQueryError> {
        if !valid_uuid(workspace_id) || !valid_uuid(dataflow_id) {
            return Err(ExecuteQueryError::input(
                ExecuteQueryErrorKind::InvalidTarget,
            ));
        }
        Ok(Self {
            workspace_id: workspace_id.to_owned(),
            dataflow_id: dataflow_id.to_owned(),
        })
    }

    /// Caller-supplied workspace UUID.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Caller-supplied Dataflow item UUID.
    pub fn dataflow_id(&self) -> &str {
        &self.dataflow_id
    }

    fn path(&self) -> String {
        format!(
            "/v1/workspaces/{}/dataflows/{}/executeQuery",
            self.workspace_id, self.dataflow_id
        )
    }
}

impl fmt::Debug for ExecuteQueryTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecuteQueryTarget")
            .field("workspace_id", &"<explicit>")
            .field("dataflow_id", &"<explicit>")
            .finish()
    }
}

/// Typed wire request plus out-of-body media negotiation.
///
/// Serialization uses exactly the official JSON field names. The Arrow version
/// is a header, never an undocumented request-body field. Construction is bounded;
/// execution revalidates the fully serialized payload before credential work.
#[derive(Clone, Serialize)]
pub struct ExecuteQueryRequest {
    #[serde(rename = "queryName")]
    query_name: String,
    #[serde(
        rename = "customMashupDocument",
        skip_serializing_if = "Option::is_none"
    )]
    custom_mashup_document: Option<String>,
    #[serde(skip)]
    arrow_version: ArrowVersion,
}

impl ExecuteQueryRequest {
    /// Construct a nonblank named query with explicit Arrow negotiation.
    ///
    /// # Errors
    /// Rejects control characters, empty/blank names and local byte-budget excess.
    pub fn new(
        query_name: impl Into<String>,
        arrow_version: ArrowVersion,
    ) -> Result<Self, ExecuteQueryError> {
        let request = Self {
            query_name: query_name.into(),
            custom_mashup_document: None,
            arrow_version,
        };
        request.body()?;
        Ok(request)
    }

    /// Override the service's default mashup with an explicit document.
    ///
    /// This validates local shape and bounds, not Power Query M semantics.
    ///
    /// # Errors
    /// Rejects blank/NUL-containing documents and serialized request-budget excess.
    pub fn with_custom_mashup_document(
        mut self,
        document: impl Into<String>,
    ) -> Result<Self, ExecuteQueryError> {
        self.custom_mashup_document = Some(document.into());
        self.body()?;
        Ok(self)
    }

    /// Exact named query, which may contain source-sensitive information.
    pub fn query_name(&self) -> &str {
        &self.query_name
    }

    /// Optional caller-supplied source-sensitive mashup.
    pub fn custom_mashup_document(&self) -> Option<&str> {
        self.custom_mashup_document.as_deref()
    }

    /// Negotiated version; no gateway detection or version fallback is attempted.
    pub const fn arrow_version(&self) -> ArrowVersion {
        self.arrow_version
    }

    fn body(&self) -> Result<Vec<u8>, ExecuteQueryError> {
        if self.query_name.trim().is_empty()
            || self.query_name.chars().any(char::is_control)
            || self
                .custom_mashup_document
                .as_deref()
                .is_some_and(|document| document.trim().is_empty() || document.contains('\0'))
        {
            return Err(ExecuteQueryError::input(
                ExecuteQueryErrorKind::InvalidRequest,
            ));
        }
        if self.query_name.len() > MAX_QUERY_NAME_BYTES
            || self
                .custom_mashup_document
                .as_ref()
                .is_some_and(|document| document.len() > MAX_REQUEST_BYTES)
        {
            return Err(ExecuteQueryError::input(
                ExecuteQueryErrorKind::RequestLimit,
            ));
        }
        let body = serde_json::to_vec(self)
            .map_err(|_| ExecuteQueryError::input(ExecuteQueryErrorKind::InvalidRequest))?;
        if body.len() > MAX_REQUEST_BYTES {
            return Err(ExecuteQueryError::input(
                ExecuteQueryErrorKind::RequestLimit,
            ));
        }
        Ok(body)
    }
}

impl fmt::Debug for ExecuteQueryRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecuteQueryRequest")
            .field("query_name", &"<redacted>")
            .field(
                "custom_mashup_document",
                &self.custom_mashup_document.as_ref().map(|_| "<redacted>"),
            )
            .field("arrow_version", &self.arrow_version)
            .finish()
    }
}

/// Explicit native completion policy, without Python compatibility heuristics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecuteQueryCompletion {
    /// Return an actual 200 or actual 202; acceptance is not decoded as Arrow.
    ReturnAccepted,
    /// Follow the shared bounded state/result protocol to retrieve actual bytes.
    WaitForResult,
}

impl ExecuteQueryCompletion {
    const fn shared(self) -> CompletionPolicy {
        match self {
            Self::ReturnAccepted => CompletionPolicy::ReturnAccepted,
            Self::WaitForResult => CompletionPolicy::WaitForResult,
        }
    }
}

/// Sanitized endpoint-level failure classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecuteQueryErrorKind {
    /// Explicit workspace/item IDs are not dashed UUIDs.
    InvalidTarget,
    /// Invalid local request shape, not an M-language semantic validation.
    InvalidRequest,
    /// Local named-query or serialized request byte budget exceeded.
    RequestLimit,
    /// Shared credential, service, deadline, cancellation, LRO or byte-limit error.
    Operation,
    /// Accepted response supplied no usable native operation handle.
    MissingOperationHandle,
    /// Successful transport status is not the endpoint's 200 or 202 contract.
    UnexpectedStatus,
    /// Completed result omitted Content-Type; unverified bytes are not Arrow.
    MissingContentType,
    /// Duplicate, malformed or non-Arrow Content-Type.
    InvalidContentType,
    /// A returned Arrow version parameter differs from explicit negotiation.
    ArrowVersionMismatch,
}

/// Preserved service/reconciliation evidence with source-sensitive debug redaction.
///
/// Display and Debug never print request values, response bytes, raw headers,
/// service messages or operation IDs. Explicit accessors retain that evidence
/// for a caller's own safe diagnostics; do not log it indiscriminately.
pub struct ExecuteQueryError {
    kind: ExecuteQueryErrorKind,
    operation_error: Option<Box<OperationError>>,
    response: Option<OperationResponse>,
}

impl ExecuteQueryError {
    /// Stable sanitized classification.
    pub const fn kind(&self) -> ExecuteQueryErrorKind {
        self.kind
    }

    /// Exact shared operation failure or endpoint validation evidence.
    pub fn operation_error(&self) -> Option<&OperationError> {
        self.operation_error.as_deref()
    }

    /// Raw unexpected response, when endpoint validation follows successful I/O.
    ///
    /// The bytes and headers are untrusted and may contain source-sensitive data.
    pub fn response(&self) -> Option<&OperationResponse> {
        self.response.as_ref()
    }

    fn input(kind: ExecuteQueryErrorKind) -> Self {
        Self {
            kind,
            operation_error: None,
            response: None,
        }
    }

    fn operation(error: OperationError) -> Self {
        Self {
            kind: ExecuteQueryErrorKind::Operation,
            operation_error: Some(Box::new(error)),
            response: None,
        }
    }
}

impl fmt::Debug for ExecuteQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecuteQueryError")
            .field("kind", &self.kind)
            .field(
                "operation_error",
                &self.operation_error.as_ref().map(|_| "<evidence>"),
            )
            .field("response", &self.response.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl fmt::Display for ExecuteQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Dataflow Execute Query failed or is incomplete ({:?})",
            self.kind
        )
    }
}

impl std::error::Error for ExecuteQueryError {}

/// Execute one native named query using the caller's existing bounded session.
///
/// An actual 202 remains accepted, not a fabricated job/scheduler handle. A 200
/// preserves every byte; its Content-Type must identify Arrow streaming. If a
/// version parameter is present it must match; an omitted parameter relies on
/// the documented request negotiation, not automatic version guessing. Missing
/// Content-Type is rejected. Arrow/PQ metadata decoding belongs to the consumer.
///
/// The service's 90-second query duration is separate from the caller's shared
/// total deadline and cancellation. Neither local failure nor dropping the
/// future cancels remote work, rolls back execution or authorizes replay.
///
/// # Errors
/// Validates inputs before credential acquisition. Preserves shared service,
/// operation ID and uncertain-dispatch evidence; never retries a mutation.
pub async fn execute_query<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    target: &ExecuteQueryTarget,
    request: &ExecuteQueryRequest,
    completion: ExecuteQueryCompletion,
) -> Result<OperationResponse, ExecuteQueryError> {
    let body = request.body()?;
    let operation = FabricOperation::new(
        ResourceMethod::Post,
        &target.path(),
        body,
        completion.shared(),
    )
    .map_err(|kind| ExecuteQueryError::operation(session.evidence(kind)))?
    .with_accept(request.arrow_version.accept());
    let response = session
        .execute(operation)
        .await
        .map_err(ExecuteQueryError::operation)?;
    validate_response(session, response, request.arrow_version)
}

/// Explicitly wait for an earlier accepted query without replaying its POST.
///
/// Supply a fresh operation session and the originally negotiated version. The
/// shared engine validates operation links, counts the original request against
/// its budget and retains conservative mutation evidence. Only result retrieval
/// negotiates Arrow; state polling retains JSON defaults.
///
/// # Errors
/// Rejects invalid/exhausted sessions, non-202 input, unsafe handles, cancellation,
/// service failures, bounds and invalid result media while retaining evidence.
pub async fn wait_for_query<E: OperationExecutor>(
    session: &mut OperationSession<'_, E>,
    accepted: OperationResponse,
    arrow_version: ArrowVersion,
) -> Result<OperationResponse, ExecuteQueryError> {
    let response = session
        .reconcile_accepted_with_accept(
            accepted,
            CompletionPolicy::WaitForResult,
            arrow_version.accept(),
        )
        .await
        .map_err(ExecuteQueryError::operation)?;
    validate_response(session, response, arrow_version)
}

fn validate_response<E: OperationExecutor>(
    session: &OperationSession<'_, E>,
    response: OperationResponse,
    version: ArrowVersion,
) -> Result<OperationResponse, ExecuteQueryError> {
    let failure = match response.status {
        202 if session.operation_id().is_none() => {
            Some(ExecuteQueryErrorKind::MissingOperationHandle)
        }
        202 => None,
        200 => validate_media(&response, version).err(),
        _ => Some(ExecuteQueryErrorKind::UnexpectedStatus),
    };
    if let Some(kind) = failure {
        return Err(ExecuteQueryError {
            kind,
            operation_error: Some(Box::new(
                session.evidence(OperationErrorKind::InvalidResponse),
            )),
            response: Some(response),
        });
    }
    Ok(response)
}

fn validate_media(
    response: &OperationResponse,
    version: ArrowVersion,
) -> Result<(), ExecuteQueryErrorKind> {
    let mut values = response
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"));
    let value = values
        .next()
        .map(|(_, value)| value)
        .ok_or(ExecuteQueryErrorKind::MissingContentType)?;
    if values.next().is_some()
        || value.len() > 1024
        || !value.is_ascii()
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(ExecuteQueryErrorKind::InvalidContentType);
    }
    let mut parts = value.split(';');
    if !parts.next().is_some_and(|media| {
        media
            .trim()
            .eq_ignore_ascii_case("application/vnd.apache.arrow.stream")
    }) {
        return Err(ExecuteQueryErrorKind::InvalidContentType);
    }
    let mut seen_version = false;
    for (index, parameter) in parts.enumerate() {
        let (name, value) = parameter
            .trim()
            .split_once('=')
            .ok_or(ExecuteQueryErrorKind::InvalidContentType)?;
        let name = name.trim();
        let value = value.trim();
        if index >= 8 || name.is_empty() || value.is_empty() {
            return Err(ExecuteQueryErrorKind::InvalidContentType);
        }
        if name.eq_ignore_ascii_case("pq-arrow-version") {
            if seen_version {
                return Err(ExecuteQueryErrorKind::InvalidContentType);
            }
            seen_version = true;
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            if !matches!(value, "1" | "2") {
                return Err(ExecuteQueryErrorKind::InvalidContentType);
            }
            if value != version.parameter() {
                return Err(ExecuteQueryErrorKind::ArrowVersionMismatch);
            }
        } else if !name.bytes().all(media_token) || !value.bytes().all(media_token) {
            return Err(ExecuteQueryErrorKind::InvalidContentType);
        }
    }
    Ok(())
}

fn media_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn valid_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
