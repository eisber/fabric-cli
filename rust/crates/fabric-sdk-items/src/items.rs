// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{FabricServiceError, Item, ItemClient, ItemMetadata, Page};
use azure_core::{credentials::AccessToken, http::Url};
use fabric_core::Audience;
use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::{Pin, pin},
    task::Poll,
};

const ORIGIN: &str = "https://api.fabric.microsoft.com";

/// A validated read-only item request. Debug excludes identifiers, query and token.
pub struct ItemRequest<'a> {
    url: Url,
    token: &'a AccessToken,
    max_response_bytes: NonZeroUsize,
}

impl ItemRequest<'_> {
    /// Returns GET, the only method in this service slice.
    pub const fn method(&self) -> &'static str {
        "GET"
    }
    /// Returns the validated URL. Query tokens and identifiers are untrusted data.
    pub fn url(&self) -> &Url {
        &self.url
    }
    /// Returns the caller's Fabric token, never for logging or persistence.
    pub fn token(&self) -> &AccessToken {
        self.token
    }
    /// Returns the maximum body size the executor must enforce while streaming.
    pub const fn max_response_bytes(&self) -> NonZeroUsize {
        self.max_response_bytes
    }
    /// Returns the content type used by the Python request helper.
    pub const fn content_type(&self) -> &'static str {
        "application/json"
    }
}

impl fmt::Debug for ItemRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ItemRequest")
            .field("method", &"GET")
            .field("origin", &ORIGIN)
            .field("token", &"<injected>")
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

/// Executor response data, not an assertion of service success.
#[derive(Clone, Debug)]
pub struct ItemResponse {
    /// HTTP response status.
    pub status: u16,
    /// Untrusted body, bounded by the executor and checked again by the operation.
    pub body: Vec<u8>,
    /// Response headers; operations look them up case-insensitively.
    pub headers: BTreeMap<String, String>,
}

/// Sanitized execution failure without provider diagnostic text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemExecutionError {
    /// Transport or execution failed.
    Failed,
    /// The request deadline elapsed.
    TimedOut,
    /// The executor cancelled the request.
    Cancelled,
    /// The finite read retry budget was exhausted.
    RetriesExhausted,
    /// Streaming stopped at the response byte bound.
    ResponseLimit,
}

/// Caller-owned execution, with no credential discovery or runtime installation.
///
/// Implementations must enforce streaming bounds and finite deadlines/retries,
/// disable redirects, and release local resources on future drop. Dropping a
/// future is cancellation, never evidence that a complete response was received.
pub trait ItemExecutor: Send + Sync {
    /// Executes a single validated GET using the supplied token.
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>>;
}

/// Nonzero allocation and traversal safety limits, not Python pagination quirks.
#[derive(Clone, Copy, Debug)]
pub struct ItemListLimits {
    /// Maximum fully accumulated pages.
    pub max_pages: NonZeroUsize,
    /// Maximum accumulated rows, including duplicates.
    pub max_items: NonZeroUsize,
    /// Maximum bytes per response.
    pub max_response_bytes: NonZeroUsize,
}

impl Default for ItemListLimits {
    fn default() -> Self {
        Self {
            max_pages: NonZeroUsize::new(100).unwrap(),
            max_items: NonZeroUsize::new(100_000).unwrap(),
            max_response_bytes: default_response_limit(),
        }
    }
}

fn default_response_limit() -> NonZeroUsize {
    NonZeroUsize::new(4 * 1024 * 1024).unwrap()
}

/// LIST options. Query parameters are sent to the service, not local predicates.
#[derive(Clone, Debug, Default)]
pub struct ItemListOptions {
    /// Ordered string parameters, e.g. `type=Notebook`.
    ///
    /// Repeated keys replace their value in the first key's position, matching
    /// Python's insertion-ordered request dictionary. Continuation replaces an
    /// initial `continuationToken` in place. Values are form-URL-encoded once.
    pub query: Vec<(String, String)>,
    /// Explicit traversal and allocation bounds.
    pub limits: ItemListLimits,
}

/// GET options for a generic item, without specialized collection/definition APIs.
#[derive(Clone, Debug)]
pub struct ItemGetOptions {
    /// Ordered string parameters with the same encoding/replacement rules as LIST.
    pub query: Vec<(String, String)>,
    /// Maximum bytes per response, enforced during execution and before decoding.
    pub max_response_bytes: NonZeroUsize,
}

impl Default for ItemGetOptions {
    fn default() -> Self {
        Self {
            query: Vec::new(),
            max_response_bytes: default_response_limit(),
        }
    }
}

/// Fully committed traversal progress, returned as success only when complete.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemList {
    /// Rows in source order, preserving duplicates, case and service type data.
    pub value: Vec<Item>,
    /// Actual fully decoded page count, not Python's `total_pages` metadata.
    pub pages_received: usize,
    /// Next unconsumed token when known. Treat it as sensitive opaque data.
    pub next_token: Option<String>,
}

/// Item operation failure classification.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemErrorKind {
    /// Explicit selection was not for Fabric; no token was requested.
    InvalidAudience,
    /// An identifier could change the route or has no valid segment.
    InvalidIdentifier,
    /// The injected credential failed, without fallback.
    Credential,
    /// Caller-injected execution failed.
    Execution(ItemExecutionError),
    /// A failing HTTP status, retaining structured body and headers as data.
    Http {
        /// Actual HTTP status.
        status: u16,
        /// Python-compatible error-code precedence and fallback.
        code: String,
        /// Valid structured service error, if present.
        service_error: Option<Box<FabricServiceError>>,
        /// Unmodified response headers, including diagnostic request IDs.
        headers: BTreeMap<String, String>,
    },
    /// A 202/LRO, 204 or discarded follow-up 201 is not a completed typed response.
    UnsupportedStatus(u16),
    /// Malformed JSON, missing required fields or an invalid typed envelope.
    InvalidResponse,
    /// A supplied active continuation URI changed origin or collection path.
    InvalidContinuation,
    /// The service repeated a token, including an initially supplied token.
    RepeatedContinuation,
    /// More pages remain beyond the finite page bound.
    PageLimit,
    /// A full page would exceed the total row bound.
    ItemLimit,
    /// A response exceeded the byte bound.
    ResponseLimit,
    /// Explicit cancellation completed. In-flight futures were dropped.
    Cancelled,
}

/// A failed item GET. Its Display never echoes untrusted or secret data.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemError {
    /// Failure classification and structured diagnostics.
    pub kind: ItemErrorKind,
}

impl fmt::Display for ItemErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ItemErrorKind::InvalidAudience => "item operations require the Fabric audience",
            ItemErrorKind::InvalidIdentifier => "invalid item request identifier",
            ItemErrorKind::Credential => "the injected credential could not acquire a Fabric token",
            ItemErrorKind::Execution(_) => "item request execution failed",
            ItemErrorKind::Http { .. } => "Fabric rejected the item request",
            ItemErrorKind::UnsupportedStatus(_) => "the response is not a completed item response",
            ItemErrorKind::InvalidResponse => "Fabric returned an invalid item response",
            ItemErrorKind::InvalidContinuation => {
                "Fabric returned an invalid item continuation URI"
            }
            ItemErrorKind::RepeatedContinuation => "Fabric repeated an item continuation token",
            ItemErrorKind::PageLimit => "item pagination reached its page bound",
            ItemErrorKind::ItemLimit => "item pagination reached its item bound",
            ItemErrorKind::ResponseLimit => "the item response exceeded its byte bound",
            ItemErrorKind::Cancelled => "item operation was cancelled",
        })
    }
}
impl fmt::Display for ItemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(f)
    }
}
impl std::error::Error for ItemError {}

/// A failed or incomplete LIST with explicit committed progress.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemListError {
    /// Failure classification.
    pub kind: ItemErrorKind,
    /// Previously committed full pages, never a complete successful result.
    pub partial: ItemList,
}
impl fmt::Display for ItemListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(f)
    }
}
impl std::error::Error for ItemListError {}

impl ItemClient {
    /// Lists generic items across a workspace, preserving service rows unchanged.
    ///
    /// Credentials are acquired for every page. Cancellation wins a simultaneous
    /// ready result and drops in-flight credential/executor futures. A pending
    /// cancellation future leaves cancellation to dropping this call.
    ///
    /// # Errors
    /// Returns typed identity, path, execution, service, malformed-payload,
    /// continuation, cancellation or safety-bound errors with explicit progress.
    pub async fn list_items(
        &self,
        executor: &dyn ItemExecutor,
        workspace_id: &str,
        options: ItemListOptions,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ItemList, ItemListError> {
        let mut progress = ItemList::default();
        let mut cancelled = pin!(cancelled);
        let result = self
            .collect_items(
                executor,
                workspace_id,
                options,
                &mut progress,
                cancelled.as_mut(),
            )
            .await;
        match result {
            Ok(()) => Ok(progress),
            Err(kind) => Err(ItemListError {
                kind,
                partial: progress,
            }),
        }
    }

    async fn collect_items(
        &self,
        executor: &dyn ItemExecutor,
        workspace_id: &str,
        options: ItemListOptions,
        progress: &mut ItemList,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<(), ItemErrorKind> {
        or_cancel(std::future::ready(()), cancelled.as_mut()).await?;
        self.validate_audience()?;
        let base = item_url(workspace_id, None)?;
        let mut query = unique_query(options.query);
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
                return Err(ItemErrorKind::UnsupportedStatus(201));
            }
            let page: Page<Item> = serde_json::from_slice(&response.body)
                .map_err(|_| ItemErrorKind::InvalidResponse)?;
            if page.value.len() > options.limits.max_items.get() - progress.value.len() {
                return Err(ItemErrorKind::ItemLimit);
            }
            progress.value.extend(page.value);
            progress.pages_received += 1;
            progress.next_token = header(&response, "x-ms-continuation")
                .map(str::to_owned)
                .or(page.continuation_token)
                .filter(|token| !token.is_empty());
            let Some(next) = &progress.next_token else {
                return Ok(());
            };
            if let Some(uri) = page.continuation_uri {
                validate_continuation(&uri, base.path())?;
            }
            if !seen.insert(next.clone()) {
                return Err(ItemErrorKind::RepeatedContinuation);
            }
            if progress.pages_received >= options.limits.max_pages.get() {
                return Err(ItemErrorKind::PageLimit);
            }
            put_query(&mut query, "continuationToken".to_owned(), next.clone());
            or_cancel(Self::yield_once(), cancelled.as_mut()).await?;
        }
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

    /// Gets one generic `/items/{id}` resource without fetching definitions.
    ///
    /// # Errors
    /// Returns typed identity, path, execution, service, malformed-payload,
    /// cancellation or byte-bound failures. A truthy continuation is rejected:
    /// Python's aggregation of arbitrary GET object lists is not a single item.
    pub async fn get_item(
        &self,
        executor: &dyn ItemExecutor,
        workspace_id: &str,
        item_id: &str,
        options: ItemGetOptions,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<Item, ItemError> {
        let mut cancelled = pin!(cancelled);
        self.get_one(executor, workspace_id, item_id, options, cancelled.as_mut())
            .await
            .map_err(|kind| ItemError { kind })
    }

    /// Gets generic item metadata without changing the parsed HTTP field order.
    ///
    /// Uses the same request, credential, continuation, cancellation and response
    /// policies as [`Self::get_item`]. Only the returned representation differs.
    ///
    /// # Errors
    /// Returns the same typed failures as [`Self::get_item`], including invalid
    /// JSON or missing/non-string required `id` and `displayName` fields.
    pub async fn get_item_metadata(
        &self,
        executor: &dyn ItemExecutor,
        workspace_id: &str,
        item_id: &str,
        options: ItemGetOptions,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<ItemMetadata, ItemError> {
        let mut cancelled = pin!(cancelled);
        self.get_one(executor, workspace_id, item_id, options, cancelled.as_mut())
            .await
            .map_err(|kind| ItemError { kind })
    }

    async fn get_one<T: serde::de::DeserializeOwned>(
        &self,
        executor: &dyn ItemExecutor,
        workspace_id: &str,
        item_id: &str,
        options: ItemGetOptions,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<T, ItemErrorKind> {
        or_cancel(std::future::ready(()), cancelled.as_mut()).await?;
        self.validate_audience()?;
        let mut url = item_url(workspace_id, Some(item_id))?;
        apply_query(&mut url, &unique_query(options.query));
        let response = self
            .read(
                executor,
                url,
                options.max_response_bytes,
                cancelled.as_mut(),
            )
            .await?;
        let value: serde_json::Value =
            serde_json::from_slice(&response.body).map_err(|_| ItemErrorKind::InvalidResponse)?;
        let token = match header(&response, "x-ms-continuation") {
            Some(token) => Some(token),
            None => match value.get("continuationToken") {
                Some(serde_json::Value::String(token)) => Some(token.as_str()),
                None | Some(serde_json::Value::Null) => None,
                _ => return Err(ItemErrorKind::InvalidResponse),
            },
        };
        if token.is_some_and(|token| !token.is_empty()) {
            return Err(ItemErrorKind::InvalidContinuation);
        }
        serde_json::from_value(value).map_err(|_| ItemErrorKind::InvalidResponse)
    }

    fn validate_audience(&self) -> Result<(), ItemErrorKind> {
        if self.selection().audience() != Audience::Fabric {
            return Err(ItemErrorKind::InvalidAudience);
        }
        Ok(())
    }

    async fn read(
        &self,
        executor: &dyn ItemExecutor,
        url: Url,
        limit: NonZeroUsize,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<ItemResponse, ItemErrorKind> {
        let token = or_cancel(
            self.credential()
                .get_token(&[Audience::Fabric.scope()], None),
            cancelled.as_mut(),
        )
        .await?
        .map_err(|_| ItemErrorKind::Credential)?;
        let response = or_cancel(
            executor.execute(ItemRequest {
                url,
                token: &token,
                max_response_bytes: limit,
            }),
            cancelled.as_mut(),
        )
        .await?
        .map_err(|error| match error {
            ItemExecutionError::ResponseLimit => ItemErrorKind::ResponseLimit,
            error => ItemErrorKind::Execution(error),
        })?;
        or_cancel(std::future::ready(()), cancelled.as_mut()).await?;
        if response.body.len() > limit.get() {
            return Err(ItemErrorKind::ResponseLimit);
        }
        match response.status {
            200 | 201 => Ok(response),
            202 | 204 => Err(ItemErrorKind::UnsupportedStatus(response.status)),
            _ => Err(service_failure(&response)),
        }
    }
}

async fn or_cancel<T>(
    operation: impl Future<Output = T>,
    mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
) -> Result<T, ItemErrorKind> {
    let mut operation = pin!(operation);
    poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(ItemErrorKind::Cancelled));
        }
        operation.as_mut().poll(cx).map(Ok)
    })
    .await
}

fn item_url(workspace: &str, item: Option<&str>) -> Result<Url, ItemErrorKind> {
    for id in std::iter::once(workspace).chain(item) {
        if id.is_empty()
            || matches!(id, "." | "..")
            || id
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | '?' | '#'))
        {
            return Err(ItemErrorKind::InvalidIdentifier);
        }
    }
    let mut url = Url::parse(ORIGIN).map_err(|_| ItemErrorKind::InvalidIdentifier)?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| ItemErrorKind::InvalidIdentifier)?;
        path.extend(["v1", "workspaces", workspace, "items"]);
        if let Some(item) = item {
            path.push(item);
        }
    }
    Ok(url)
}

fn unique_query(pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for (key, value) in pairs {
        put_query(&mut result, key, value);
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
        let encoded = query
            .iter()
            .map(|(key, value)| format!("{}={}", quote_plus(key), quote_plus(value)))
            .collect::<Vec<_>>()
            .join("&");
        url.set_query(Some(&encoded));
    }
}

// Python urlencode leaves '~' unescaped and encodes '*', unlike WHATWG form encoding.
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
fn header<'a>(response: &'a ItemResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}
fn service_failure(response: &ItemResponse) -> ItemErrorKind {
    let body = serde_json::from_slice::<serde_json::Value>(&response.body).ok();
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
            .and_then(serde_json::Value::as_str)
            .unwrap_or("UnknownError")
            .to_owned(),
    };
    ItemErrorKind::Http {
        status: response.status,
        code,
        service_error: body
            .and_then(|body| serde_json::from_value(body).ok())
            .map(Box::new),
        headers: response.headers.clone(),
    }
}
fn validate_continuation(uri: &str, path: &str) -> Result<(), ItemErrorKind> {
    let url = Url::parse(uri).map_err(|_| ItemErrorKind::InvalidContinuation)?;
    let raw_path = uri
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|start| &rest[start..]))
        .and_then(|rest| rest.split(['?', '#']).next());
    if url.scheme() != "https"
        || url.host_str() != Some("api.fabric.microsoft.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.path() != path
        || raw_path != Some(path)
    {
        return Err(ItemErrorKind::InvalidContinuation);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
