// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded OneLake DFS list/read/properties with explicitly injected credentials.
//!
//! The executor adapts the existing shared HTTP policy. This crate installs no
//! transport, retries, identity backend, runtime or table interpretation.

mod mutations;
pub mod shortcuts;
use azure_core::{credentials::AccessToken, http::Url};
use fabric_core::{Audience, CredentialSelection, TokenCredential};
pub use mutations::{
    OneLakeMutationError, OneLakeUpload, OneLakeWriteLimits, OneLakeWriteOutcome, OneLakeWriteStep,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::{Pin, pin},
    sync::Arc,
    task::Poll,
};

const ORIGIN: &str = "https://onelake.dfs.fabric.microsoft.com";

/// Validated workspace/item/relative DFS resource, not an arbitrary URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OneLakePath {
    workspace: String,
    item: String,
    relative: String,
}

impl OneLakePath {
    /// Construct a fixed-origin resource, preserving spaces and Unicode names.
    ///
    /// # Errors
    /// Rejects empty IDs, absolute paths, traversal, backslashes and URL injection.
    /// An empty relative path selects the item root.
    pub fn new(workspace: &str, item: &str, relative: &str) -> Result<Self, OneLakeError> {
        let id = |value: &str| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        };
        if !id(workspace)
            || !id(item)
            || relative.starts_with('/')
            || relative.contains(['\\', '\0', '?', '#', '%'])
            || (!relative.is_empty()
                && relative
                    .trim_end_matches('/')
                    .split('/')
                    .any(|segment| segment.is_empty() || matches!(segment, "." | "..")))
            || relative.chars().any(char::is_control)
        {
            return Err(OneLakeError::InvalidInput);
        }
        Ok(Self {
            workspace: workspace.into(),
            item: item.into(),
            relative: relative.trim_end_matches('/').into(),
        })
    }

    /// Explicit workspace identifier.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// Explicit item identifier.
    pub fn item(&self) -> &str {
        &self.item
    }

    /// Relative resource beneath the item, retaining case.
    pub fn relative(&self) -> &str {
        &self.relative
    }

    fn url(&self) -> Result<Url, OneLakeError> {
        let mut url = Url::parse(ORIGIN).map_err(|_| OneLakeError::InvalidInput)?;
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| OneLakeError::InvalidInput)?;
        segments.push(&self.workspace).push(&self.item);
        if !self.relative.is_empty() {
            segments.extend(self.relative.split('/'));
        }
        drop(segments);
        Ok(url)
    }
}

/// Finite OneLake methods, never caller-selected arbitrary method strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OneLakeMethod {
    /// List a directory or read file bytes.
    Get,
    /// Read path properties without a response body.
    Head,
    /// Create a directory/file or rename a path.
    Put,
    /// Append/flush file content.
    Patch,
    /// Delete a directory/file path recursively.
    Delete,
}

/// A validated DFS request. Debug output excludes resource names and token.
pub struct OneLakeRequest<'a> {
    method: OneLakeMethod,
    url: Url,
    token: &'a AccessToken,
    max_response_bytes: NonZeroUsize,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

impl OneLakeRequest<'_> {
    /// Finite HTTP method.
    pub const fn method(&self) -> OneLakeMethod {
        self.method
    }

    /// Validated public DFS URL; continuation values are opaque and must not be logged.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Injected storage token. Never log or persist it.
    pub fn token(&self) -> &AccessToken {
        self.token
    }

    /// Streaming byte limit the executor must enforce.
    pub const fn max_response_bytes(&self) -> NonZeroUsize {
        self.max_response_bytes
    }

    /// Exact bytes, empty for reads and bodyless operations.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Source-specific safe headers; credentials/framing are executor-owned.
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }
}

impl fmt::Debug for OneLakeRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OneLakeRequest")
            .field("method", &self.method)
            .field("origin", &ORIGIN)
            .field("token", &"<injected>")
            .finish()
    }
}

/// Raw bounded DFS response, whose status must still be checked.
#[derive(Clone, Debug)]
pub struct OneLakeResponse {
    /// Actual HTTP status.
    pub status: u16,
    /// Exact bytes; never interpreted as JSON for file reads.
    pub body: Vec<u8>,
    /// Response headers, read case-insensitively.
    pub headers: BTreeMap<String, String>,
}

/// Sanitized transport failure categories, shared HTTP remains the implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OneLakeExecutionError {
    /// Transport failed.
    Failed,
    /// Overall request deadline elapsed.
    TimedOut,
    /// Read retry budget exhausted.
    RetriesExhausted,
    /// Body streaming reached its byte bound.
    ResponseLimit,
    /// Caller cancelled execution.
    Cancelled,
}

/// Typed adapter boundary to the existing shared HTTP execution policy.
pub trait OneLakeExecutor: Send + Sync {
    /// Execute one fixed-origin DFS request with no redirect or identity fallback.
    ///
    /// Implementations must enforce byte/deadline limits and release local
    /// resources when dropped. No new transport or retry stack belongs here.
    fn execute<'a>(
        &'a self,
        request: OneLakeRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OneLakeResponse, OneLakeExecutionError>> + Send + 'a>>;
}

/// Finite per-list/per-request safety limits.
#[derive(Clone, Copy, Debug)]
pub struct OneLakeLimits {
    /// Maximum fully consumed list pages.
    pub max_pages: NonZeroUsize,
    /// Maximum accumulated path rows.
    pub max_paths: NonZeroUsize,
    /// Maximum bytes in any response (including a file read).
    pub max_response_bytes: NonZeroUsize,
}

impl Default for OneLakeLimits {
    fn default() -> Self {
        Self {
            max_pages: NonZeroUsize::new(100).unwrap(),
            max_paths: NonZeroUsize::new(100_000).unwrap(),
            max_response_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
        }
    }
}

/// Explicit failures; a 403 or malformed response never becomes `exists=false`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OneLakeError {
    /// Unsafe path or invalid configuration.
    InvalidInput,
    /// Selection did not explicitly request the OneLake audience.
    InvalidAudience,
    /// Injected credential could not obtain the storage token.
    Credential,
    /// Shared executor failed.
    Execution(OneLakeExecutionError),
    /// Service rejected the request, with its header error code retained as data.
    Http {
        /// Actual service status.
        status: u16,
        /// Storage `x-ms-error-code`, if present.
        code: Option<String>,
    },
    /// Malformed path listing or unexpected successful status.
    InvalidResponse,
    /// A page/path/byte bound was reached before complete success.
    Limit,
    /// A continuation token repeated.
    RepeatedContinuation,
    /// Caller cancelled; no response is claimed.
    Cancelled,
}

impl fmt::Display for OneLakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid OneLake resource path",
            Self::InvalidAudience => "OneLake requires an explicitly selected storage audience",
            Self::Credential => "the injected credential could not acquire a storage token",
            Self::Execution(_) => "OneLake request execution failed",
            Self::Http { .. } => "OneLake rejected the resource request",
            Self::InvalidResponse => "OneLake returned an invalid response",
            Self::Limit => "OneLake traversal or response reached its safety bound",
            Self::RepeatedContinuation => "OneLake repeated a continuation token",
            Self::Cancelled => "OneLake operation was cancelled",
        })
    }
}

impl std::error::Error for OneLakeError {}

/// Complete listing with unmodified source rows, in service/page order.
#[derive(Clone, Debug, PartialEq)]
pub struct OneLakeList {
    /// Exact path metadata, including extension/shortcut fields.
    pub paths: Vec<Value>,
    /// Actual number of complete pages received.
    pub pages_received: usize,
}

/// Source-compatible directory metadata, preserving the first page's extra fields.
#[derive(Clone, Debug, PartialEq)]
pub struct OneLakeDirectoryMetadata {
    /// Original first-page object with source list-append/token-removal semantics.
    pub data: Value,
    /// Actual complete pages retrieved, distinct from Python's `total_pages` quirk.
    pub pages_received: usize,
}

struct DirectoryCollection {
    paths: Vec<Value>,
    pages_received: usize,
    metadata: Option<Value>,
}

/// A focused client with no dependency on CLI, deployment or identity backends.
#[derive(Clone)]
pub struct OneLakeClient {
    credential: Arc<dyn TokenCredential>,
    selection: CredentialSelection,
}

impl fmt::Debug for OneLakeClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OneLakeClient")
            .field("credential", &"<injected>")
            .field("selection", &self.selection)
            .finish()
    }
}

impl OneLakeClient {
    /// Store the caller's provider and explicit selection, without any I/O.
    pub fn new(credential: Arc<dyn TokenCredential>, selection: CredentialSelection) -> Self {
        Self {
            credential,
            selection,
        }
    }

    /// List paths under an item-relative directory, preserving raw metadata.
    ///
    /// Continuation is copied into `continuationToken`, exactly as pinned Python
    /// does for storage. No response link or arbitrary origin is followed.
    ///
    /// # Errors
    /// Returns explicit service/credential/cancellation/bound failures. Partial
    /// listings are never returned as complete or used to infer absence.
    pub async fn list(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        recursive: bool,
        limits: OneLakeLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeList, OneLakeError> {
        let result = self
            .collect_directory(executor, path, recursive, limits, false, cancelled)
            .await?;
        Ok(OneLakeList {
            paths: result.paths,
            pages_received: result.pages_received,
        })
    }

    /// Read source-compatible directory metadata for OneLake GET composition.
    ///
    /// Reuses the same bounded collection loop as `list`, retains first-page extra
    /// fields and source generic list-append/continuation/201 quirks. Captured page
    /// bytes and the merged object are additionally bounded by max_response_bytes
    /// in aggregate; an oversized metadata result is never a partial success.
    ///
    /// # Errors
    /// Returns the same explicit SDK failures as list, and rejects malformed
    /// non-array merge fields instead of coercing them into successful metadata.
    pub async fn directory_metadata(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        limits: OneLakeLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeDirectoryMetadata, OneLakeError> {
        let result = self
            .collect_directory(executor, path, false, limits, true, cancelled)
            .await?;
        Ok(OneLakeDirectoryMetadata {
            data: result.metadata.ok_or(OneLakeError::InvalidResponse)?,
            pages_received: result.pages_received,
        })
    }

    async fn collect_directory(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        recursive: bool,
        limits: OneLakeLimits,
        capture: bool,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<DirectoryCollection, OneLakeError> {
        let mut cancelled = pin!(cancelled);
        let mut paths = Vec::new();
        let mut seen = HashSet::new();
        let mut continuation: Option<String> = None;
        let mut metadata = Vec::new();
        let mut captured_bytes = 0_usize;
        for page in 1..=limits.max_pages.get() {
            let mut url = Url::parse(ORIGIN).map_err(|_| OneLakeError::InvalidInput)?;
            url.set_path(&format!("/{}/", path.workspace));
            let directory = if path.relative.is_empty() {
                format!("{}/", path.item)
            } else {
                format!("{}/{}", path.item, path.relative)
            };
            url.query_pairs_mut()
                .append_pair("recursive", if recursive { "true" } else { "false" })
                .append_pair("resource", "filesystem")
                .append_pair("directory", &directory)
                .append_pair("getShortcutMetadata", "true");
            if let Some(token) = &continuation {
                url.query_pairs_mut()
                    .append_pair("continuationToken", token);
            }
            let response = self
                .request(
                    executor,
                    OneLakeMethod::Get,
                    url,
                    limits.max_response_bytes,
                    cancelled.as_mut(),
                )
                .await?;
            let value: Value = serde_json::from_slice(&response.body)
                .map_err(|_| OneLakeError::InvalidResponse)?;
            let rows = value
                .get("paths")
                .and_then(Value::as_array)
                .ok_or(OneLakeError::InvalidResponse)?;
            if rows.len() > limits.max_paths.get() - paths.len() {
                return Err(OneLakeError::Limit);
            }
            if rows
                .iter()
                .any(|row| !row.is_object() || row.get("name").and_then(Value::as_str).is_none())
            {
                return Err(OneLakeError::InvalidResponse);
            }
            paths.extend(rows.iter().cloned());
            continuation = match header(&response.headers, "x-ms-continuation") {
                Some(token) => (!token.is_empty()).then(|| token.to_owned()),
                None => match value.get("continuationToken") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(token)) => (!token.is_empty()).then(|| token.clone()),
                    _ => return Err(OneLakeError::InvalidResponse),
                },
            };
            if capture {
                captured_bytes = captured_bytes
                    .checked_add(response.body.len())
                    .ok_or(OneLakeError::Limit)?;
                if captured_bytes > limits.max_response_bytes.get() {
                    return Err(OneLakeError::Limit);
                }
                metadata.push((response.status, value));
            }
            let Some(next) = &continuation else {
                return Ok(DirectoryCollection {
                    paths,
                    pages_received: page,
                    metadata: if capture {
                        Some(merge_metadata(metadata, limits.max_response_bytes)?)
                    } else {
                        None
                    },
                });
            };
            if !seen.insert(next.clone()) {
                return Err(OneLakeError::RepeatedContinuation);
            }
        }
        Err(OneLakeError::Limit)
    }

    /// Read exact file bytes, including non-JSON and empty files.
    ///
    /// # Errors
    /// Returns explicit service, credential, cancellation and byte-bound failures.
    pub async fn read(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        limits: OneLakeLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Vec<u8>, OneLakeError> {
        Ok(self
            .read_file(executor, path, limits, cancelled)
            .await?
            .body)
    }

    /// Read file bytes and their original response headers in one GET.
    ///
    /// Copy consumers need the source media type/length from this same response,
    /// not an additional HEAD or inferred metadata.
    ///
    /// # Errors
    /// Retains the same identity, response, cancellation and byte-bound failures
    /// as `read`; no empty/error fallback or alternate provider is installed.
    pub async fn read_file(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        limits: OneLakeLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<OneLakeResponse, OneLakeError> {
        let mut cancelled = pin!(cancelled);
        self.request(
            executor,
            OneLakeMethod::Get,
            path.url()?,
            limits.max_response_bytes,
            cancelled.as_mut(),
        )
        .await
    }

    /// Read exact HEAD properties (content length/type/etag and other headers).
    ///
    /// # Errors
    /// A missing resource remains HTTP 404; it is not silently mapped to empty data.
    pub async fn properties(
        &self,
        executor: &dyn OneLakeExecutor,
        path: &OneLakePath,
        limits: OneLakeLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<BTreeMap<String, String>, OneLakeError> {
        let mut cancelled = pin!(cancelled);
        Ok(self
            .request(
                executor,
                OneLakeMethod::Head,
                path.url()?,
                limits.max_response_bytes,
                cancelled.as_mut(),
            )
            .await?
            .headers)
    }

    async fn request(
        &self,
        executor: &dyn OneLakeExecutor,
        method: OneLakeMethod,
        url: Url,
        limit: NonZeroUsize,
        cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<OneLakeResponse, OneLakeError> {
        let response = self
            .exchange(
                executor,
                Exchange {
                    method,
                    url,
                    body: Vec::new(),
                    headers: Vec::new(),
                    limit,
                },
                cancelled,
                &mut false,
            )
            .await?;
        if matches!(response.status, 200 | 201) {
            Ok(response)
        } else {
            Err(OneLakeError::InvalidResponse)
        }
    }

    async fn exchange(
        &self,
        executor: &dyn OneLakeExecutor,
        exchange: Exchange,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
        dispatched: &mut bool,
    ) -> Result<OneLakeResponse, OneLakeError> {
        or_cancel(std::future::ready(()), cancelled.as_mut()).await?;
        if self.selection.audience() != Audience::OneLake {
            return Err(OneLakeError::InvalidAudience);
        }
        let token = or_cancel(
            self.credential
                .get_token(&[Audience::OneLake.scope()], None),
            cancelled.as_mut(),
        )
        .await?
        .map_err(|_| OneLakeError::Credential)?;
        or_cancel(std::future::ready(()), cancelled.as_mut()).await?;
        if matches!(
            exchange.method,
            OneLakeMethod::Put | OneLakeMethod::Patch | OneLakeMethod::Delete
        ) {
            *dispatched = true;
        }
        let response = or_cancel(
            executor.execute(OneLakeRequest {
                method: exchange.method,
                url: exchange.url,
                token: &token,
                max_response_bytes: exchange.limit,
                body: exchange.body,
                headers: exchange.headers,
            }),
            cancelled.as_mut(),
        )
        .await?
        .map_err(OneLakeError::Execution)?;
        or_cancel(std::future::ready(()), cancelled.as_mut()).await?;
        if response.body.len() > exchange.limit.get() {
            return Err(OneLakeError::Limit);
        }
        match response.status {
            200 | 201 | 202 | 204 => Ok(response),
            202..=299 => Err(OneLakeError::InvalidResponse),
            status => Err(OneLakeError::Http {
                status,
                code: if matches!(status, 401 | 403) {
                    header(&response.headers, "x-ms-public-api-error-code")
                        .filter(|code| !code.is_empty())
                        .or_else(|| {
                            header(&response.headers, "x-ms-error-code")
                                .filter(|code| !code.is_empty())
                        })
                        .map(str::to_owned)
                } else {
                    header(&response.headers, "x-ms-error-code").map(str::to_owned)
                },
            }),
        }
    }
}

struct Exchange {
    method: OneLakeMethod,
    url: Url,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
    limit: NonZeroUsize,
}

fn merge_metadata(
    mut pages: Vec<(u16, Value)>,
    limit: NonZeroUsize,
) -> Result<Value, OneLakeError> {
    let mut next = pages.pop().ok_or(OneLakeError::InvalidResponse)?;
    while let Some(mut page) = pages.pop() {
        if next.0 == 200 {
            let following = next.1.as_object().ok_or(OneLakeError::InvalidResponse)?;
            let current = page
                .1
                .as_object_mut()
                .ok_or(OneLakeError::InvalidResponse)?;
            for (key, value) in current.iter_mut() {
                if let Value::Array(values) = value
                    && let Some(additional) = following.get(key)
                {
                    values.extend(
                        additional
                            .as_array()
                            .ok_or(OneLakeError::InvalidResponse)?
                            .iter()
                            .cloned(),
                    );
                }
            }
            current.remove("continuationToken");
            current.remove("continuationUri");
            current.insert("total_pages".into(), 1.into());
            page.0 = 200;
        }
        next = page;
    }
    if serde_json::to_vec(&next.1)
        .map_err(|_| OneLakeError::InvalidResponse)?
        .len()
        > limit.get()
    {
        return Err(OneLakeError::Limit);
    }
    Ok(next.1)
}

async fn or_cancel<T>(
    future: impl Future<Output = T>,
    mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
) -> Result<T, OneLakeError> {
    let mut future = pin!(future);
    poll_fn(|context| {
        if cancelled.as_mut().poll(context).is_ready() {
            return Poll::Ready(Err(OneLakeError::Cancelled));
        }
        future.as_mut().poll(context).map(Ok)
    })
    .await
}

fn header<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}
