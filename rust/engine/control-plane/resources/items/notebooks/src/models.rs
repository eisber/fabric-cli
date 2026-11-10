// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Notebook definition, wire, execution and error contracts.

use azure_core::{credentials::AccessToken, http::Url};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk_resource_contracts::FabricServiceError;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap, fmt, future::Future, num::NonZeroUsize, pin::Pin, time::Duration,
};

/// One inline, base64-encoded definition file.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionPart {
    pub(crate) path: String,
    payload: String,
    payload_type: &'static str,
}

impl DefinitionPart {
    /// Encodes a relative definition path and its exact content bytes.
    ///
    /// # Errors
    /// Rejects empty, absolute, backslash-delimited, or traversing paths.
    pub fn inline(path: &str, bytes: &[u8]) -> Result<Self, NotebookErrorKind> {
        if path.is_empty()
            || path.contains(['\\', ':', '\0'])
            || path.split('/').any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(NotebookErrorKind::InvalidInput);
        }
        Ok(Self {
            path: path.to_owned(),
            payload: STANDARD.encode(bytes),
            payload_type: "InlineBase64",
        })
    }
}

/// An explicit Notebook definition, not a shell-only item.
#[derive(Clone, Serialize)]
pub struct NotebookDefinition {
    /// `ipynb` for exported notebooks, absent for Fabric Git source format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Ordered inline files; `.platform` precedes content and settings.
    pub parts: Vec<DefinitionPart>,
}

/// Typed item metadata needed for exact name/type selection.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NotebookItem {
    /// Fabric item ID.
    pub id: String,
    /// Service display name, never normalized.
    pub display_name: String,
    /// Service item type, never normalized.
    #[serde(rename = "type")]
    pub item_type: String,
}

/// One validated service request. Debug output never includes token or content.
pub struct NotebookRequest<'a> {
    pub(crate) method: &'static str,
    pub(crate) url: &'a Url,
    pub(crate) body: &'a [u8],
    pub(crate) token: &'a AccessToken,
    pub(crate) max_response_bytes: NonZeroUsize,
}

impl NotebookRequest<'_> {
    /// Returns GET or POST.
    pub const fn method(&self) -> &'static str {
        self.method
    }
    /// Returns the fixed-origin, validated Fabric URL.
    pub fn url(&self) -> &Url {
        self.url
    }
    /// Returns serialized JSON, empty for GET.
    pub fn body(&self) -> &[u8] {
        self.body
    }
    /// Returns the explicitly injected bearer token. Never log it.
    pub fn token(&self) -> &AccessToken {
        self.token
    }
    /// Returns the content type.
    pub const fn content_type(&self) -> &'static str {
        "application/json"
    }
    /// Returns the body limit the executor must enforce while reading.
    pub const fn max_response_bytes(&self) -> NonZeroUsize {
        self.max_response_bytes
    }
}

impl fmt::Debug for NotebookRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotebookRequest")
            .field("method", &self.method)
            .field("origin", &self.url.origin().ascii_serialization())
            .field("body", &"<definition>")
            .field("token", &"<injected>")
            .finish()
    }
}

/// Response data; status and payload must still be checked.
#[derive(Clone, Debug)]
pub struct NotebookResponse {
    /// HTTP status.
    pub status: u16,
    /// Body bounded by the request's byte limit.
    pub body: Vec<u8>,
    /// Headers, read case-insensitively.
    pub headers: BTreeMap<String, String>,
}

/// Executor failures without provider diagnostic or secret text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotebookExecutionError {
    /// Transport failed; a POST might already have reached the service.
    Failed,
    /// The caller's execution deadline expired.
    TimedOut,
    /// Request execution was cancelled locally.
    Cancelled,
    /// Streaming execution stopped before exceeding the response byte bound.
    ResponseLimit,
    /// A read exhausted its finite retry budget. No mutation was replayed.
    RetriesExhausted,
}

/// Operation-local seam following the workspace executor pattern.
///
/// Implementations provide bearer authentication from `request.token()`, enforce
/// request deadlines and streaming response limits, never follow redirects, and
/// release local resources when futures are dropped. They MUST NOT automatically
/// retry POSTs: a lost response does not prove the mutation failed. No production
/// HTTP stack, runtime, ambient credentials, or retry policy is installed here.
pub trait NotebookExecutor: Send + Sync {
    /// Executes exactly one request, with no automatic mutation replay.
    fn execute<'a>(
        &'a self,
        request: NotebookRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<NotebookResponse, NotebookExecutionError>> + Send + 'a>>;

    /// Asynchronously waits at least the requested service polling interval.
    ///
    /// Use the caller's runtime; never block its async thread or busy-spin.
    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), NotebookExecutionError>> + Send + '_>>;
}

/// Finite bounds across discovery, mutation and polling within one deployment.
#[derive(Clone, Copy, Debug)]
pub struct NotebookLimits {
    /// Maximum service requests, including pages and operation result retrieval.
    pub max_requests: NonZeroUsize,
    /// Maximum accumulated discovery items.
    pub max_items: NonZeroUsize,
    /// Maximum response bytes per request.
    pub max_response_bytes: NonZeroUsize,
    /// Largest accepted service Retry-After interval.
    pub max_poll_delay: Duration,
}

impl Default for NotebookLimits {
    fn default() -> Self {
        Self {
            max_requests: NonZeroUsize::new(100).unwrap(),
            max_items: NonZeroUsize::new(100_000).unwrap(),
            max_response_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
            max_poll_delay: Duration::from_secs(60),
        }
    }
}

/// Classification of a failed or incomplete native service operation.
#[derive(Clone, Debug, PartialEq)]
pub enum NotebookErrorKind {
    /// Invalid input, including unsafe service IDs or empty definitions.
    InvalidInput,
    /// Credential selection was not explicitly for Fabric.
    InvalidAudience,
    /// The injected credential could not acquire the required token.
    Credential,
    /// The executor reported an explicit failure.
    Execution(NotebookExecutionError),
    /// Fabric rejected the request; valid structured details are preserved as data.
    Http {
        /// HTTP status.
        status: u16,
        /// Service error code, retained even if other error fields are malformed.
        code: Option<String>,
        /// Structured error, when the response contains a valid one.
        error: Option<Box<FabricServiceError>>,
    },
    /// Service returned malformed content, headers, or an unknown operation state.
    InvalidResponse,
    /// A continuation or operation link was not the expected Fabric resource.
    InvalidLink,
    /// Requests, items, response bytes, or polling delay exceeded a caller bound.
    Limit,
    /// Discovery repeated a continuation token.
    RepeatedContinuation,
    /// The service reported operation failure or cancellation.
    OperationFailed {
        /// Reported terminal status (Failed or Cancelled).
        status: String,
        /// Structured service failure when supplied.
        error: Option<Box<FabricServiceError>>,
    },
    /// Local cancellation; never evidence of server rollback.
    Cancelled,
}

/// Failed/incomplete operation with reconciliation evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct NotebookError {
    /// Explicit failure classification.
    pub kind: NotebookErrorKind,
    /// Accepted operation ID, when observed. Poll it before attempting replay.
    pub operation_id: Option<String>,
    /// True once mutation execution may have started, even if no response arrived.
    pub mutation_may_have_occurred: bool,
}

impl fmt::Display for NotebookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("native Notebook operation failed or is incomplete; inspect its classification and reconciliation evidence")
    }
}

impl std::error::Error for NotebookError {}
