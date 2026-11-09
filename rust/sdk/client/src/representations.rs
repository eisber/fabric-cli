// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit native definition and OneLake file reads for independent front ends.
//!
//! Definitions remain service data: paths, part order, duplicate paths, metadata
//! and Notebook outputs are not rewritten. Native format selects a service
//! representation, never an output serializer. No metadata fallback, packaging,
//! extraction, identity discovery or operation polling is performed.

use crate::Client;
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeError, OneLakeExecutor, OneLakeLimits, OneLakePath,
};
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutionError,
    OperationExecutor, OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::{Pin, pin},
    task::Poll,
    time::Duration,
};

/// One resolved generic item definition target, never a collection or arbitrary URL.
#[derive(Clone, Eq, PartialEq)]
pub struct DefinitionTarget {
    /// Explicit owning workspace identifier.
    pub workspace_id: String,
    /// Explicit item identifier.
    pub item_id: String,
    /// Optional explicit native-format token; no format is guessed when absent.
    ///
    /// Tokens start with an ASCII letter or digit and contain at most 128 ASCII
    /// letters, digits, dots, underscores or hyphens. The service determines which
    /// native formats its item supports; output formats are not selected here.
    pub native_format: Option<String>,
}

impl fmt::Debug for DefinitionTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DefinitionTarget(<resolved>)")
    }
}

/// Caller-owned finite bounds, without numeric product-policy defaults.
#[derive(Clone, Copy, Debug)]
pub struct RepresentationLimits {
    /// Maximum HTTP response bytes, also enforced defensively after execution.
    pub max_response_bytes: NonZeroUsize,
    /// Maximum completed raw definition envelope and compact JSON bytes, or file bytes.
    ///
    /// The complete definition is bounded before any part is selected. Neither
    /// limit truncates a result or counts a projection instead of its source.
    pub max_result_bytes: NonZeroUsize,
}

/// A completed raw definition envelope or an actual accepted operation handle.
#[derive(Clone, PartialEq)]
pub enum DefinitionRead {
    /// Full validated service envelope, including its `definition.parts` array.
    Completed {
        /// Exact parsed envelope, retaining unknown fields and part order.
        definition: Value,
    },
    /// HTTP 202, not completed data and not permission to replay the POST.
    Accepted {
        /// Validated service operation identifier, never synthesized.
        operation_id: String,
    },
}

impl DefinitionRead {
    /// Returns acceptance as the shared observer's existing-operation handle.
    ///
    /// Completed definitions have no accepted handle. The identifier carries no
    /// endpoint, tenant, credential or authorization: it is not a portable token.
    /// Observe it using the same [`Client`] and its immutable request context that
    /// produced the acceptance; this method performs no observation or I/O.
    pub fn accepted_handle(&self) -> Option<crate::observe::ReadHandle> {
        match self {
            Self::Accepted { operation_id } => Some(crate::observe::ReadHandle::Operation {
                operation_id: operation_id.clone(),
            }),
            Self::Completed { .. } => None,
        }
    }

    /// Validates a completed operation's `/result` body without performing I/O.
    ///
    /// Uses the same multipart-envelope and byte-budget validation as an immediate
    /// HTTP 200 definition read. The caller must establish that this body is the
    /// completed definition result, not an accepted response or operation status.
    /// Unknown fields, native part paths, ordering and duplicates remain data.
    ///
    /// # Errors
    /// Rejects invalid envelopes or complete raw/compact JSON beyond the caller's
    /// response or result bound. This never polls or manufactures an operation ID.
    pub fn from_completed_result(
        body: &[u8],
        limits: RepresentationLimits,
    ) -> Result<Self, RepresentationError> {
        if body.len() > limits.max_response_bytes.get()
            || body.len() > limits.max_result_bytes.get()
        {
            return Err(RepresentationError::BudgetExceeded);
        }
        let definition: Value =
            serde_json::from_slice(body).map_err(|_| RepresentationError::InvalidResponse)?;
        definition_parts(&definition)?;
        let mut size = BoundedSize {
            written: 0,
            limit: limits.max_result_bytes.get(),
            exceeded: false,
        };
        if serde_json::to_writer(&mut size, &definition).is_err() {
            return Err(if size.exceeded {
                RepresentationError::BudgetExceeded
            } else {
                RepresentationError::Encoding
            });
        }
        Ok(Self::Completed { definition })
    }

    /// Decodes one exact path's inline bytes without changing the definition.
    ///
    /// Matching is case-sensitive and does not normalize or extract paths.
    /// Standard base64 must have canonical padding and zero trailing pad bits;
    /// whitespace, URL-safe alphabets and guessed encodings are not accepted.
    /// Media types remain raw definition data, not inferred from a path suffix.
    ///
    /// # Errors
    /// Distinguishes incomplete results, malformed envelopes, missing or duplicate
    /// exact paths, unsupported payload types and invalid base64.
    pub fn part_bytes(&self, path: &str) -> Result<Vec<u8>, RepresentationError> {
        let Self::Completed { definition } = self else {
            return Err(RepresentationError::NotCompleted);
        };
        let mut matches = definition_parts(definition)?
            .iter()
            .filter(|part| part["path"].as_str() == Some(path));
        let part = matches.next().ok_or(RepresentationError::NotFound)?;
        if matches.next().is_some() {
            return Err(RepresentationError::Ambiguous);
        }
        if part["payloadType"].as_str() != Some("InlineBase64") {
            return Err(RepresentationError::Unsupported);
        }
        STANDARD
            .decode(
                part["payload"]
                    .as_str()
                    .ok_or(RepresentationError::InvalidResponse)?,
            )
            .map_err(|_| RepresentationError::InvalidBase64)
    }
}

impl fmt::Debug for DefinitionRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Completed { .. } => "DefinitionRead::Completed(<definition>)",
            Self::Accepted { .. } => "DefinitionRead::Accepted(<operation>)",
        })
    }
}

/// Exact bytes and original headers from one completed OneLake file GET.
///
/// Content-Type, content length and ETag, when advertised, remain in `headers`;
/// this type promises no range support, resume, extraction or transcoding.
#[derive(Clone, Eq, PartialEq)]
pub struct FileRead {
    /// Original binary bytes, including an empty file.
    pub bytes: Vec<u8>,
    /// Original response headers, without name/value normalization or guessing.
    pub headers: BTreeMap<String, String>,
}

impl fmt::Debug for FileRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileRead")
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// Explicit read failures; denied or unsupported retrieval never falls back.
#[derive(Debug)]
pub enum RepresentationError {
    /// Invalid identifier or native-format token, rejected before credential work.
    InvalidInput,
    /// The service returned an unexpected success status or invalid envelope.
    InvalidResponse,
    /// An actual Fabric operation failed, retaining its original evidence.
    Operation(OperationError),
    /// The existing OneLake client rejected or failed the file read.
    OneLake(OneLakeError),
    /// Complete source data exceeded the caller's response or result byte bound.
    BudgetExceeded,
    /// JSON data could not be encoded for result-size accounting.
    Encoding,
    /// The caller cancelled before a completed file result could be delivered.
    Cancelled,
    /// Part bytes were requested from a still-accepted operation.
    NotCompleted,
    /// No part has the exact selected path.
    NotFound,
    /// More than one part has the exact selected path.
    Ambiguous,
    /// The selected part is not an InlineBase64 payload.
    Unsupported,
    /// The selected part's payload is not strict canonical standard base64.
    InvalidBase64,
}

impl fmt::Display for RepresentationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid definition target or native format",
            Self::InvalidResponse => "invalid native representation response",
            Self::Operation(_) => "native definition retrieval failed or is incomplete",
            Self::OneLake(_) => "native file retrieval failed",
            Self::BudgetExceeded => "native representation exceeded its byte budget",
            Self::Encoding => "native representation JSON encoding failed",
            Self::Cancelled => "native representation retrieval was cancelled",
            Self::NotCompleted => "the definition operation is not completed",
            Self::NotFound => "the exact definition part was not found",
            Self::Ambiguous => "the exact definition part is ambiguous",
            Self::Unsupported => "the definition part payload type is unsupported",
            Self::InvalidBase64 => "the definition part payload is not canonical base64",
        })
    }
}

impl std::error::Error for RepresentationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Operation(error) => Some(error),
            Self::OneLake(error) => Some(error),
            _ => None,
        }
    }
}

/// Retrieves one generic item's exact native multipart definition.
///
/// Uses POST `/v1/workspaces/{workspace}/items/{item}/getDefinition`; this
/// semantic read may require read-and-write permissions at the service. It
/// acquires only the caller's explicitly selected Fabric credential. HTTP 202
/// returns a validated handle without polling, replaying or committing output.
///
/// # Errors
/// Rejects invalid inputs before credentials, preserves operation failures and
/// rejects malformed envelopes and complete results beyond either caller bound.
pub async fn read_definition(
    client: &Client,
    executor: &dyn OperationExecutor,
    target: &DefinitionTarget,
    limits: RepresentationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<DefinitionRead, RepresentationError> {
    let operation = definition_operation(client, target)?;
    execute_definition(client, executor, operation, limits, cancelled).await
}

pub(crate) async fn execute_definition(
    client: &Client,
    executor: &dyn OperationExecutor,
    operation: FabricOperation,
    limits: RepresentationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<DefinitionRead, RepresentationError> {
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
        .map_err(RepresentationError::Operation)?;
    if response.status == 202 {
        let operation_id = session
            .operation_id()
            .ok_or_else(|| {
                RepresentationError::Operation(
                    session.evidence(OperationErrorKind::InvalidResponse),
                )
            })?
            .to_owned();
        session
            .check_cancelled()
            .await
            .map_err(RepresentationError::Operation)?;
        return Ok(DefinitionRead::Accepted { operation_id });
    }
    if response.status != 200 {
        return Err(RepresentationError::InvalidResponse);
    }
    let read = DefinitionRead::from_completed_result(&response.body, limits)?;
    session
        .check_cancelled()
        .await
        .map_err(RepresentationError::Operation)?;
    Ok(read)
}

/// Reads one explicit OneLake file's exact bytes and same-response headers.
///
/// Delegates the fixed-origin, storage-audience request and streaming bound to
/// the existing client. The result bound applies to all bytes, not a decoded or
/// projected form. No metadata request, text decoding or alternate route runs.
///
/// # Errors
/// Preserves typed OneLake failures and rejects over-budget or cancelled results.
pub async fn read_file(
    client: &OneLakeClient,
    executor: &dyn OneLakeExecutor,
    path: &OneLakePath,
    limits: RepresentationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<FileRead, RepresentationError> {
    let mut cancelled = pin!(cancelled);
    let response = client
        .read_file(
            executor,
            path,
            OneLakeLimits {
                max_pages: NonZeroUsize::new(1).unwrap(),
                max_paths: NonZeroUsize::new(1).unwrap(),
                max_response_bytes: limits.max_response_bytes,
            },
            cancelled.as_mut(),
        )
        .await
        .map_err(RepresentationError::OneLake)?;
    if response.status != 200 {
        return Err(RepresentationError::InvalidResponse);
    }
    if response.body.len() > limits.max_result_bytes.get() {
        return Err(RepresentationError::BudgetExceeded);
    }
    poll_fn(|cx| {
        Poll::Ready(if cancelled.as_mut().poll(cx).is_ready() {
            Err(RepresentationError::Cancelled)
        } else {
            Ok(())
        })
    })
    .await?;
    Ok(FileRead {
        bytes: response.body,
        headers: response.headers,
    })
}

fn definition_operation(
    client: &Client,
    target: &DefinitionTarget,
) -> Result<FabricOperation, RepresentationError> {
    let identifier = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    };
    if !identifier(&target.workspace_id)
        || !identifier(&target.item_id)
        || target.native_format.as_deref().is_some_and(|format| {
            format.is_empty()
                || format.len() > 128
                || !format.as_bytes()[0].is_ascii_alphanumeric()
                || !format
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        })
    {
        return Err(RepresentationError::InvalidInput);
    }
    let mut path = format!(
        "/v1/workspaces/{}/items/{}/getDefinition",
        target.workspace_id, target.item_id
    );
    let mut url = client
        .context()
        .endpoint()
        .url(&path)
        .map_err(|_| RepresentationError::InvalidInput)?;
    if let Some(format) = &target.native_format {
        url.query_pairs_mut().append_pair("format", format);
    }
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    FabricOperation::new(
        ResourceMethod::Post,
        &path,
        Vec::new(),
        CompletionPolicy::ReturnAccepted,
    )
    .map_err(|_| RepresentationError::InvalidInput)
}

fn definition_parts(definition: &Value) -> Result<&[Value], RepresentationError> {
    let definition = definition
        .as_object()
        .and_then(|envelope| envelope.get("definition"))
        .and_then(Value::as_object)
        .ok_or(RepresentationError::InvalidResponse)?;
    if definition
        .get("format")
        .is_some_and(|format| !format.is_string())
    {
        return Err(RepresentationError::InvalidResponse);
    }
    let parts = definition
        .get("parts")
        .and_then(Value::as_array)
        .ok_or(RepresentationError::InvalidResponse)?;
    for part in parts {
        let part = part
            .as_object()
            .ok_or(RepresentationError::InvalidResponse)?;
        for field in ["path", "payloadType", "payload"] {
            if !part.get(field).is_some_and(Value::is_string) {
                return Err(RepresentationError::InvalidResponse);
            }
        }
    }
    Ok(parts)
}

struct BorrowedExecutor<'a>(&'a dyn OperationExecutor);

impl OperationExecutor for BorrowedExecutor<'_> {
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

struct BoundedSize {
    written: usize,
    limit: usize,
    exceeded: bool,
}

impl std::io::Write for BoundedSize {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit - self.written {
            self.exceeded = true;
            return Err(std::io::Error::other("representation byte budget exceeded"));
        }
        self.written += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
