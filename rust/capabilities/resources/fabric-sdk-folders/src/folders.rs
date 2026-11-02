// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{FabricServiceError, Folder, FolderClient, FolderExecutionError, FolderResponse};
use azure_core::{credentials::AccessToken, http::Url};
use fabric_core::{
    Audience, AuthenticatedExecution, AuthenticatedFuture, EndpointBinding, RequestError,
    ServiceOrigin,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::{Pin, pin},
    task::Poll,
};

/// A validated read-only request. Debug excludes identifiers, query and credentials.
pub struct FolderRequest<'a> {
    url: Url,
    token: &'a AccessToken,
    max_response_bytes: NonZeroUsize,
}
impl FolderRequest<'_> {
    /// Returns GET, the only supported folder method.
    pub const fn method(&self) -> &'static str {
        "GET"
    }
    /// Returns the validated URL; its query/identifiers are untrusted data.
    pub fn url(&self) -> &Url {
        &self.url
    }
    /// Returns the injected token, never for logging or persistence.
    pub fn token(&self) -> &AccessToken {
        self.token
    }
    /// Returns the streaming body limit that the executor must enforce.
    pub const fn max_response_bytes(&self) -> NonZeroUsize {
        self.max_response_bytes
    }
    /// Returns the Python helper's request content type.
    pub const fn content_type(&self) -> &'static str {
        "application/json"
    }
}
impl fmt::Debug for FolderRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FolderRequest")
            .field("method", &"GET")
            .field("host", &self.url.host_str())
            .field("token", &"<injected>")
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

/// Caller-owned execution with finite deadlines/retries, bounded streaming and
/// redirects disabled. Future drop must release local request resources.
pub trait FolderExecutor: Send + Sync {
    /// Executes one validated GET without discovering credentials.
    fn execute<'a>(
        &'a self,
        request: FolderRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<FolderResponse, FolderExecutionError>> + Send + 'a>>;
}

/// Explicitly supported Python scalar query values, using Python `str` spelling.
///
/// `None` is present and serializes as `None`, not an absent attribute. Strings
/// are literal (already-percent-encoded input is encoded again). Floats, arrays,
/// objects and custom Python objects are intentionally not representable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum FolderQueryValue {
    /// Python `None`.
    None,
    /// Python `True` or `False`, with case preserved.
    Bool(bool),
    /// A signed 64-bit integer; arbitrary-precision integers remain unsupported.
    Integer(i64),
    /// A literal string, including an empty string.
    String(String),
}
impl FolderQueryValue {
    fn python_string(&self) -> String {
        match self {
            Self::None => "None".to_owned(),
            Self::Bool(true) => "True".to_owned(),
            Self::Bool(false) => "False".to_owned(),
            Self::Integer(value) => value.to_string(),
            Self::String(value) => value.clone(),
        }
    }
}
impl From<&str> for FolderQueryValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

/// Nonzero traversal/allocation safety bounds, not Python pagination metadata.
#[derive(Clone, Copy, Debug)]
pub struct FolderListLimits {
    /// Maximum fully committed pages.
    pub max_pages: NonZeroUsize,
    /// Maximum rows, including duplicates.
    pub max_items: NonZeroUsize,
    /// Maximum bytes per response.
    pub max_response_bytes: NonZeroUsize,
}
fn body_limit() -> NonZeroUsize {
    NonZeroUsize::new(4 * 1024 * 1024).unwrap()
}
impl Default for FolderListLimits {
    fn default() -> Self {
        Self {
            max_pages: NonZeroUsize::new(100).unwrap(),
            max_items: NonZeroUsize::new(100_000).unwrap(),
            max_response_bytes: body_limit(),
        }
    }
}

/// LIST query and bounds, preserving Python attribute presence and merge order.
#[derive(Clone, Debug, Default)]
pub struct FolderListOptions {
    /// Ordered request parameters. Last value wins in the first key's position.
    /// An existing `recursive` key overrides `recursive`, even if null or false.
    pub query: Vec<(String, FolderQueryValue)>,
    /// Python `args.recursive`; absent defaults to the string `True`.
    ///
    /// In the absence of a query override, strings containing `?`, `&`, `=`,
    /// `#` or control characters fail before authentication instead of allowing
    /// Python's naive URI splitting to inject parameters or raise a raw error.
    pub recursive: Option<FolderQueryValue>,
    /// Python `args.folder_id`: present replaces `rootFolderId` in place; null
    /// becomes `None`. This is a query value, not a path identifier.
    pub root_folder_id: Option<FolderQueryValue>,
    /// Explicit safety limits.
    pub limits: FolderListLimits,
}

/// GET query and response bound. No recursive/root defaults are added.
#[derive(Clone, Debug)]
pub struct FolderGetOptions {
    /// Ordered request parameters with the same replacement/encoding as LIST.
    pub query: Vec<(String, FolderQueryValue)>,
    /// Maximum bytes, enforced while streaming and again before decoding.
    pub max_response_bytes: NonZeroUsize,
}
impl Default for FolderGetOptions {
    fn default() -> Self {
        Self {
            query: Vec::new(),
            max_response_bytes: body_limit(),
        }
    }
}

/// Fully committed rows, successful only when traversal is complete.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FolderList {
    /// Service order and duplicates, without tree reconstruction or filtering.
    pub value: Vec<Folder>,
    /// Actual committed pages, not Python's recursive `total_pages` metadata.
    pub pages_received: usize,
    /// Next unconsumed opaque token, when known; treat it as sensitive data.
    pub next_token: Option<String>,
}

/// Folder read failure classification.
#[derive(Clone, Debug, PartialEq)]
pub enum FolderErrorKind {
    /// Explicit selection is not Fabric; no credential is acquired.
    InvalidAudience,
    /// A supplied identifier is empty or could change the path.
    InvalidIdentifier,
    /// A recursive attribute would invoke unsupported Python URI-split quirks.
    InvalidQuery,
    /// The injected credential failed; no fallback is attempted.
    Credential,
    /// Caller-owned execution failed.
    Execution(FolderExecutionError),
    /// A failing HTTP status, with untrusted diagnostics retained only as data.
    Http {
        /// Actual HTTP status.
        status: u16,
        /// Python-compatible error-code precedence.
        code: String,
        /// Structured service error, when valid.
        service_error: Option<Box<FabricServiceError>>,
        /// Original response headers, including request IDs.
        headers: std::collections::BTreeMap<String, String>,
    },
    /// LRO/empty status or a discarded follow-up 201 is not a typed completion.
    UnsupportedStatus(u16),
    /// Malformed JSON, envelope, continuation token or required row fields.
    InvalidResponse,
    /// Active continuation URI changes origin/route, or GET has a next page.
    InvalidContinuation,
    /// A continuation token has already been requested.
    RepeatedContinuation,
    /// Another page remains beyond the finite page bound.
    PageLimit,
    /// A full page would exceed the accumulated row bound.
    ItemLimit,
    /// The response exceeded its byte bound.
    ResponseLimit,
    /// Explicit cancellation won; in-flight futures were dropped.
    Cancelled,
}
impl fmt::Display for FolderErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidAudience => "folder operations require the Fabric audience",
            Self::InvalidIdentifier => "invalid folder request identifier",
            Self::InvalidQuery => "invalid folder recursive query value",
            Self::Credential => "the injected credential could not acquire a Fabric token",
            Self::Execution(_) => "folder request execution failed",
            Self::Http { .. } => "Fabric rejected the folder request",
            Self::UnsupportedStatus(_) => "the response is not a completed folder response",
            Self::InvalidResponse => "Fabric returned an invalid folder response",
            Self::InvalidContinuation => "Fabric returned an invalid folder continuation URI",
            Self::RepeatedContinuation => "Fabric repeated a folder continuation token",
            Self::PageLimit => "folder pagination reached its page bound",
            Self::ItemLimit => "folder pagination reached its item bound",
            Self::ResponseLimit => "the folder response exceeded its byte bound",
            Self::Cancelled => "folder operation was cancelled",
        })
    }
}

/// A failed GET; Display excludes service diagnostics and supplied values.
#[derive(Clone, Debug, PartialEq)]
pub struct FolderError {
    /// Failure classification.
    pub kind: FolderErrorKind,
}
impl fmt::Display for FolderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(f)
    }
}
impl std::error::Error for FolderError {}

/// A failed/incomplete LIST with explicit previously committed progress.
#[derive(Clone, Debug, PartialEq)]
pub struct FolderListError {
    /// Failure classification.
    pub kind: FolderErrorKind,
    /// Full prior pages, never a complete successful result.
    pub partial: FolderList,
}
impl fmt::Display for FolderListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(f)
    }
}
impl std::error::Error for FolderListError {}

impl FolderClient {
    /// Lists folders recursively by default, without sorting or building a tree.
    ///
    /// Each page reacquires the Fabric scope from the injected provider.
    /// Cancellation wins simultaneous readiness and drops in-flight futures.
    ///
    /// # Errors
    /// Returns typed failures with fully committed progress. Active continuation
    /// URIs are validated, never followed; only the token updates the original URL.
    pub async fn list_folders(
        &self,
        executor: &dyn FolderExecutor,
        workspace_id: &str,
        options: FolderListOptions,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<FolderList, FolderListError> {
        let mut partial = FolderList::default();
        let mut cancelled = pin!(cancelled);
        match self
            .collect(
                executor,
                workspace_id,
                options,
                &mut partial,
                cancelled.as_mut(),
            )
            .await
        {
            Ok(()) => Ok(partial),
            Err(kind) => Err(FolderListError { kind, partial }),
        }
    }

    async fn collect(
        &self,
        executor: &dyn FolderExecutor,
        workspace_id: &str,
        options: FolderListOptions,
        progress: &mut FolderList,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<(), FolderErrorKind> {
        or_cancel(std::future::ready(()), cancelled.as_mut()).await?;
        self.validate_audience()?;
        let base = folder_url(self.context().endpoint(), workspace_id, None)?;
        let mut query = list_query(&options)?;
        let mut seen = HashSet::new();
        if let Some((_, token)) = query.iter().find(|(key, _)| key == "continuationToken")
            && !token.is_empty()
        {
            seen.insert(token.clone());
        }
        loop {
            let mut url = base.clone();
            apply_query(&mut url, &query);
            let response = self
                .read(
                    executor,
                    url,
                    options.limits.max_response_bytes,
                    cancelled.as_mut(),
                )
                .await?;
            if progress.pages_received > 0 && response.status == 201 {
                return Err(FolderErrorKind::UnsupportedStatus(201));
            }
            let body: Value = serde_json::from_slice(&response.body)
                .map_err(|_| FolderErrorKind::InvalidResponse)?;
            let rows: Vec<Folder> = serde_json::from_value(
                body.get("value")
                    .cloned()
                    .ok_or(FolderErrorKind::InvalidResponse)?,
            )
            .map_err(|_| FolderErrorKind::InvalidResponse)?;
            let next = next_token(&response, &body)?;
            if rows.len() > options.limits.max_items.get() - progress.value.len() {
                return Err(FolderErrorKind::ItemLimit);
            }
            progress.value.extend(rows);
            progress.pages_received += 1;
            progress.next_token = next;
            let Some(next) = &progress.next_token else {
                return Ok(());
            };
            match body.get("continuationUri") {
                Some(Value::String(uri)) => {
                    validate_continuation(uri, base.path(), self.context().endpoint())?
                }
                None | Some(Value::Null) => {}
                _ => return Err(FolderErrorKind::InvalidResponse),
            }
            if !seen.insert(next.clone()) {
                return Err(FolderErrorKind::RepeatedContinuation);
            }
            if progress.pages_received >= options.limits.max_pages.get() {
                return Err(FolderErrorKind::PageLimit);
            }
            put_query(&mut query, "continuationToken".to_owned(), next.clone());
            or_cancel(yield_once(), cancelled.as_mut()).await?;
        }
    }

    /// Gets one folder, preserving raw parent/unknown fields and explicit nulls.
    ///
    /// # Errors
    /// Rejects malformed/empty payloads and truthy continuations instead of
    /// aggregating arbitrary object GETs. No parent/child lookup is performed.
    pub async fn get_folder(
        &self,
        executor: &dyn FolderExecutor,
        workspace_id: &str,
        folder_id: &str,
        options: FolderGetOptions,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Folder, FolderError> {
        let mut cancelled = pin!(cancelled);
        self.get_one(
            executor,
            workspace_id,
            folder_id,
            options,
            cancelled.as_mut(),
        )
        .await
        .map_err(|kind| FolderError { kind })
    }

    async fn get_one(
        &self,
        executor: &dyn FolderExecutor,
        workspace_id: &str,
        folder_id: &str,
        options: FolderGetOptions,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<Folder, FolderErrorKind> {
        or_cancel(std::future::ready(()), cancelled.as_mut()).await?;
        self.validate_audience()?;
        let mut url = folder_url(self.context().endpoint(), workspace_id, Some(folder_id))?;
        apply_query(&mut url, &query_strings(&options.query));
        let response = self
            .read(
                executor,
                url,
                options.max_response_bytes,
                cancelled.as_mut(),
            )
            .await?;
        let body: Value =
            serde_json::from_slice(&response.body).map_err(|_| FolderErrorKind::InvalidResponse)?;
        if next_token(&response, &body)?.is_some() {
            return Err(FolderErrorKind::InvalidContinuation);
        }
        serde_json::from_value(body).map_err(|_| FolderErrorKind::InvalidResponse)
    }

    fn validate_audience(&self) -> Result<(), FolderErrorKind> {
        if self.selection().audience() != Audience::Fabric {
            return Err(FolderErrorKind::InvalidAudience);
        }
        Ok(())
    }

    async fn read(
        &self,
        executor: &dyn FolderExecutor,
        url: Url,
        limit: NonZeroUsize,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<FolderResponse, FolderErrorKind> {
        let response = self
            .context()
            .execute(
                ServiceOrigin::Fabric,
                &url,
                &mut FolderRead { executor, limit },
                cancelled.as_mut(),
            )
            .await
            .map_err(|error| match error {
                RequestError::InvalidAudience => FolderErrorKind::InvalidAudience,
                RequestError::InvalidEndpoint => FolderErrorKind::InvalidContinuation,
                RequestError::Credential => FolderErrorKind::Credential,
                RequestError::Cancelled => FolderErrorKind::Cancelled,
                RequestError::Execution(FolderExecutionError::ResponseLimit) => {
                    FolderErrorKind::ResponseLimit
                }
                RequestError::Execution(error) => FolderErrorKind::Execution(error),
            })?;
        if response.body.len() > limit.get() {
            return Err(FolderErrorKind::ResponseLimit);
        }
        match response.status {
            200 | 201 => Ok(response),
            202 | 204 => Err(FolderErrorKind::UnsupportedStatus(response.status)),
            _ => Err(service_failure(&response)),
        }
    }
}

struct FolderRead<'a> {
    executor: &'a dyn FolderExecutor,
    limit: NonZeroUsize,
}

impl AuthenticatedExecution for FolderRead<'_> {
    type Response = FolderResponse;
    type Error = FolderExecutionError;

    fn execute<'a>(
        &'a mut self,
        url: &'a Url,
        token: &'a AccessToken,
    ) -> AuthenticatedFuture<'a, Self::Response, Self::Error> {
        self.executor.execute(FolderRequest {
            url: url.clone(),
            token,
            max_response_bytes: self.limit,
        })
    }
}

async fn or_cancel<T>(
    operation: impl Future<Output = T>,
    mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
) -> Result<T, FolderErrorKind> {
    let mut operation = pin!(operation);
    poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(FolderErrorKind::Cancelled));
        }
        operation.as_mut().poll(cx).map(Ok)
    })
    .await
}
async fn yield_once() {
    let mut yielded = false;
    poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}
fn folder_url(
    endpoint: &EndpointBinding,
    workspace: &str,
    folder: Option<&str>,
) -> Result<Url, FolderErrorKind> {
    for id in std::iter::once(workspace).chain(folder) {
        if id.is_empty()
            || matches!(id, "." | "..")
            || id
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | '?' | '#'))
        {
            return Err(FolderErrorKind::InvalidIdentifier);
        }
    }
    let mut url = endpoint.base_url();
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| FolderErrorKind::InvalidIdentifier)?;
        path.extend(["v1", "workspaces", workspace, "folders"]);
        if let Some(folder) = folder {
            path.push(folder);
        }
    }
    Ok(url)
}
fn list_query(options: &FolderListOptions) -> Result<Vec<(String, String)>, FolderErrorKind> {
    let mut query = query_strings(&options.query);
    if let Some(root) = &options.root_folder_id {
        put_query(&mut query, "rootFolderId".to_owned(), root.python_string());
    }
    if !query.iter().any(|(key, _)| key == "recursive") {
        let recursive = options
            .recursive
            .as_ref()
            .map_or_else(|| "True".to_owned(), FolderQueryValue::python_string);
        if recursive
            .chars()
            .any(|c| c.is_control() || matches!(c, '?' | '&' | '=' | '#'))
        {
            return Err(FolderErrorKind::InvalidQuery);
        }
        put_query(&mut query, "recursive".to_owned(), recursive);
    }
    Ok(query)
}
fn query_strings(query: &[(String, FolderQueryValue)]) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for (key, value) in query {
        put_query(&mut result, key.clone(), value.python_string());
    }
    result
}
fn put_query(query: &mut Vec<(String, String)>, key: String, value: String) {
    if let Some(pair) = query.iter_mut().find(|(name, _)| name == &key) {
        pair.1 = value;
    } else {
        query.push((key, value));
    }
}
fn apply_query(url: &mut Url, query: &[(String, String)]) {
    if !query.is_empty() {
        url.set_query(Some(
            &query
                .iter()
                .map(|(key, value)| format!("{}={}", quote_plus(key), quote_plus(value)))
                .collect::<Vec<_>>()
                .join("&"),
        ));
    }
}
// Python urlencode preserves '~' but escapes '*', unlike WHATWG form encoding.
fn quote_plus(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte))
            }
            b' ' => encoded.push('+'),
            byte => {
                encoded.push('%');
                encoded.push(char::from(HEX[usize::from(byte >> 4)]));
                encoded.push(char::from(HEX[usize::from(byte & 15)]));
            }
        }
    }
    encoded
}
fn header<'a>(response: &'a FolderResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}
fn next_token(response: &FolderResponse, body: &Value) -> Result<Option<String>, FolderErrorKind> {
    let token = match header(response, "x-ms-continuation") {
        Some(token) => Some(token),
        None => match body.get("continuationToken") {
            Some(Value::String(token)) => Some(token.as_str()),
            None | Some(Value::Null) => None,
            _ => return Err(FolderErrorKind::InvalidResponse),
        },
    };
    Ok(token.filter(|token| !token.is_empty()).map(str::to_owned))
}
fn service_failure(response: &FolderResponse) -> FolderErrorKind {
    let body = serde_json::from_slice::<Value>(&response.body).ok();
    let code = match response.status {
        401 | 403 => header(response, "x-ms-public-api-error-code")
            .filter(|code| !code.is_empty())
            .or_else(|| header(response, "x-ms-error-code").filter(|code| !code.is_empty()))
            .unwrap_or(if response.status == 401 {
                "Unauthorized"
            } else {
                "Forbidden"
            })
            .to_owned(),
        404 => "NotFound".to_owned(),
        _ => body
            .as_ref()
            .and_then(|body| body.get("errorCode"))
            .and_then(Value::as_str)
            .unwrap_or("UnknownError")
            .to_owned(),
    };
    FolderErrorKind::Http {
        status: response.status,
        code,
        service_error: body
            .and_then(|body| serde_json::from_value(body).ok())
            .map(Box::new),
        headers: response.headers.clone(),
    }
}
fn validate_continuation(
    uri: &str,
    path: &str,
    endpoint: &EndpointBinding,
) -> Result<(), FolderErrorKind> {
    let url = Url::parse(uri).map_err(|_| FolderErrorKind::InvalidContinuation)?;
    let raw_path = uri
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|start| &rest[start..]))
        .and_then(|rest| rest.split(['?', '#']).next());
    if endpoint.validate_url(&url).is_err() || url.path() != path || raw_path != Some(path) {
        return Err(FolderErrorKind::InvalidContinuation);
    }
    Ok(())
}
