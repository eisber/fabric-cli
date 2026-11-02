// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared continuation-token collection without HTTP, credentials or a runtime.
//!
//! Generalizes the existing item/workspace traversal policy without modifying
//! those public APIs or their independent oracles. Source token precedence and
//! opaque query encoding follow Python `cf67fd52`; bounds, accurate progress and
//! active continuation-origin/route checks are explicit SDK safety behavior.

use azure_core::http::Url;
pub use fabric_sdk_workspaces::{models::Page, workspaces::WorkspaceResponse as PageResponse};
use serde_json::Value;
use std::{
    collections::HashSet,
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::pin,
    task::Poll,
};

/// Finite bounds shared by every consumer of this collector.
#[derive(Clone, Copy, Debug)]
pub struct PagingLimits {
    /// Maximum committed pages.
    pub max_pages: NonZeroUsize,
    /// Maximum accumulated rows, including duplicates.
    pub max_items: NonZeroUsize,
    /// Maximum decoded response bytes per page, also passed to the fetcher.
    pub max_response_bytes: NonZeroUsize,
}

impl Default for PagingLimits {
    fn default() -> Self {
        Self {
            max_pages: NonZeroUsize::new(100).unwrap(),
            max_items: NonZeroUsize::new(100_000).unwrap(),
            max_response_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
        }
    }
}

/// A collector-validated page fetch, not another HTTP implementation.
#[derive(Clone)]
pub struct PageRequest {
    url: Url,
    max_response_bytes: NonZeroUsize,
}

impl PageRequest {
    /// Same-origin, same-collection URL with a once-encoded opaque token.
    pub fn url(&self) -> &Url {
        &self.url
    }
    /// Decoded byte bound the fetcher must enforce while streaming.
    pub fn max_response_bytes(&self) -> NonZeroUsize {
        self.max_response_bytes
    }
}

impl fmt::Debug for PageRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageRequest")
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

/// Actual committed traversal progress, never Python's recursive `total_pages` quirk.
#[derive(Clone, Default, PartialEq)]
pub struct PagedValues {
    /// Ordered rows, preserving duplicates and each object's parsed key order.
    pub value: Vec<Value>,
    /// Number of fully committed pages.
    pub pages_received: usize,
    /// Next unconsumed token when known; treat it as opaque sensitive data.
    pub next_token: Option<String>,
}

impl fmt::Debug for PagedValues {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PagedValues")
            .field("rows", &self.value.len())
            .field("pages_received", &self.pages_received)
            .field("has_next_token", &self.next_token.is_some())
            .finish()
    }
}

/// Collector configuration failure, before fetching any page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidPagingUrl;

impl fmt::Display for InvalidPagingUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "paging requires an explicit HTTPS collection without credentials or a fragment",
        )
    }
}
impl std::error::Error for InvalidPagingUrl {}

/// Typed collector failure retaining caller-owned fetch errors separately.
#[derive(Debug)]
pub enum PagingErrorKind<E> {
    /// The injected fetch/credential/service boundary failed.
    Fetch(E),
    /// Only completed HTTP 200 pages may be accumulated.
    UnexpectedStatus(u16),
    /// Malformed JSON, non-array rows, or an active non-string body token.
    InvalidResponse,
    /// A decoded response exceeded the configured byte bound.
    ResponseLimit,
    /// An active URI changed origin or exact collection route.
    InvalidContinuation,
    /// A service token repeats, including the initial query token.
    RepeatedContinuation,
    /// More pages remain after the committed page bound.
    PageLimit,
    /// A whole page would exceed the accumulated row bound.
    ItemLimit,
    /// Caller cancellation won; pending fetch work was dropped.
    Cancelled,
}

impl<E> fmt::Display for PagingErrorKind<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Fetch(_) => "the injected page fetch failed",
            Self::UnexpectedStatus(_) => "the response is not a completed page",
            Self::InvalidResponse => "the service returned an invalid page",
            Self::ResponseLimit => "the page exceeded its byte bound",
            Self::InvalidContinuation => "the service returned an invalid continuation URI",
            Self::RepeatedContinuation => "the service repeated a continuation token",
            Self::PageLimit => "paging reached its page bound",
            Self::ItemLimit => "paging reached its row bound",
            Self::Cancelled => "page collection was cancelled",
        })
    }
}

/// Explicit incomplete result: full earlier pages are retained, never a silent success.
#[derive(Debug)]
pub struct PagingError<E> {
    /// Failure classification, including the original fetch error.
    pub kind: PagingErrorKind<E>,
    /// Committed progress before the failed/incomplete boundary.
    pub partial: PagedValues,
}

impl<E> fmt::Display for PagingError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(f)
    }
}
impl<E: fmt::Debug> std::error::Error for PagingError<E> {}

/// Shared transport-neutral collector bound to an explicit collection.
#[derive(Clone)]
pub struct TokenPager {
    base: Url,
    limits: PagingLimits,
    collection_field: String,
}

impl fmt::Debug for TokenPager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenPager")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl TokenPager {
    /// Bind an HTTPS origin and exact collection route without I/O.
    ///
    /// Endpoint authorization belongs to the service that supplies this URL.
    /// The collector never follows a returned URI or changes the supplied origin.
    pub fn new(base: Url, limits: PagingLimits) -> Result<Self, InvalidPagingUrl> {
        if base.scheme() != "https"
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.fragment().is_some()
        {
            return Err(InvalidPagingUrl);
        }
        Ok(Self {
            base,
            limits,
            collection_field: "value".into(),
        })
    }

    /// Selects the exact root JSON array field, without rewriting response bytes.
    ///
    /// The default is `value`. Service adapters can select `accessDetails`,
    /// `domains` or another source-defined root key. This is a literal key, not
    /// a query/path expression. A missing or non-array field fails explicitly.
    /// Original response byte bounds and continuation semantics are unchanged.
    pub fn with_collection_field(mut self, field: impl Into<String>) -> Self {
        self.collection_field = field.into();
        self
    }

    /// Collect through a caller-injected async fetcher, retaining precise partial errors.
    ///
    /// The fetcher owns credential acquisition, HTTP, retries and deadlines, and
    /// must enforce the passed streaming bound. Cancellation wins ready results.
    /// Unknown/invalid terminal unused URIs are ignored just as in the source;
    /// active continuation URIs are validated but never followed.
    pub async fn collect<E, F, Fetch>(
        &self,
        fetch: F,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<PagedValues, PagingError<E>>
    where
        E: Send,
        F: FnMut(PageRequest) -> Fetch + Send,
        Fetch: Future<Output = Result<PageResponse, E>> + Send,
    {
        let mut partial = PagedValues::default();
        let result = {
            let mut cancelled = pin!(cancelled);
            let mut work = pin!(self.collect_inner(fetch, &mut partial));
            poll_fn(|cx| {
                if cancelled.as_mut().poll(cx).is_ready() {
                    Poll::Ready(Err(PagingErrorKind::Cancelled))
                } else {
                    work.as_mut().poll(cx)
                }
            })
            .await
        };
        match result {
            Ok(()) => Ok(partial),
            Err(kind) => Err(PagingError { kind, partial }),
        }
    }

    async fn collect_inner<E, F, Fetch>(
        &self,
        mut fetch: F,
        progress: &mut PagedValues,
    ) -> Result<(), PagingErrorKind<E>>
    where
        F: FnMut(PageRequest) -> Fetch,
        Fetch: Future<Output = Result<PageResponse, E>>,
    {
        let mut query = Vec::new();
        for (key, value) in self.base.query_pairs() {
            put_query(&mut query, key.into_owned(), value.into_owned());
        }
        let mut seen = HashSet::new();
        if let Some((_, token)) = query.iter().find(|(key, _)| key == "continuationToken")
            && !token.is_empty()
        {
            seen.insert(token.clone());
        }
        loop {
            let mut url = self.base.clone();
            let encoded = query
                .iter()
                .map(|(key, value)| format!("{}={}", quote_plus(key), quote_plus(value)))
                .collect::<Vec<_>>()
                .join("&");
            url.set_query((!encoded.is_empty()).then_some(encoded.as_str()));
            let response = fetch(PageRequest {
                url,
                max_response_bytes: self.limits.max_response_bytes,
            })
            .await
            .map_err(PagingErrorKind::Fetch)?;
            if response.body.len() > self.limits.max_response_bytes.get() {
                return Err(PagingErrorKind::ResponseLimit);
            }
            if response.status != 200 {
                return Err(PagingErrorKind::UnexpectedStatus(response.status));
            }
            let value: Value = serde_json::from_slice(&response.body)
                .map_err(|_| PagingErrorKind::InvalidResponse)?;
            let mut object = match value {
                Value::Object(object) => object,
                _ => return Err(PagingErrorKind::InvalidResponse),
            };
            let continuation_token = object.get("continuationToken").cloned();
            let continuation_uri = object.get("continuationUri").cloned();
            let rows = match object.remove(&self.collection_field) {
                Some(Value::Array(rows)) => rows,
                _ => return Err(PagingErrorKind::InvalidResponse),
            };
            let header_token = response
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("x-ms-continuation"));
            let token = if let Some((_, token)) = header_token {
                Some(token.clone()).filter(|token| !token.is_empty())
            } else {
                match continuation_token.as_ref() {
                    Some(Value::String(token)) if !token.is_empty() => Some(token.clone()),
                    None | Some(Value::Null | Value::Bool(false)) => None,
                    Some(Value::String(value)) if value.is_empty() => None,
                    Some(Value::Array(value)) if value.is_empty() => None,
                    Some(Value::Object(value)) if value.is_empty() => None,
                    Some(Value::Number(value)) if value.as_f64() == Some(0.0) => None,
                    _ => return Err(PagingErrorKind::InvalidResponse),
                }
            };
            let page = Page {
                value: rows,
                continuation_token: token,
                continuation_uri: None,
            };
            if page.value.len() > self.limits.max_items.get() - progress.value.len() {
                return Err(PagingErrorKind::ItemLimit);
            }
            progress.value.extend(page.value);
            progress.pages_received += 1;
            progress.next_token = page.continuation_token;
            let Some(next) = &progress.next_token else {
                return Ok(());
            };
            if let Some(uri) = continuation_uri.as_ref().filter(|uri| !uri.is_null()) {
                let uri = uri.as_str().ok_or(PagingErrorKind::InvalidContinuation)?;
                if !valid_continuation(uri, &self.base) {
                    return Err(PagingErrorKind::InvalidContinuation);
                }
            }
            if !seen.insert(next.clone()) {
                return Err(PagingErrorKind::RepeatedContinuation);
            }
            if progress.pages_received >= self.limits.max_pages.get() {
                return Err(PagingErrorKind::PageLimit);
            }
            put_query(&mut query, "continuationToken".into(), next.clone());
            yield_once().await;
        }
    }
}

fn put_query(query: &mut Vec<(String, String)>, key: String, value: String) {
    if let Some((_, old)) = query.iter_mut().find(|(existing, _)| existing == &key) {
        *old = value;
    } else {
        query.push((key, value));
    }
}

fn quote_plus(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut result = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(char::from(byte))
            }
            b' ' => result.push('+'),
            byte => {
                result.push('%');
                result.push(char::from(HEX[usize::from(byte >> 4)]));
                result.push(char::from(HEX[usize::from(byte & 15)]));
            }
        }
    }
    result
}

fn valid_continuation(uri: &str, base: &Url) -> bool {
    let Ok(url) = Url::parse(uri) else {
        return false;
    };
    let raw_path = uri
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|start| &rest[start..]))
        .and_then(|rest| rest.split(['?', '#']).next());
    url.scheme() == base.scheme()
        && url.host_str() == base.host_str()
        && url.port_or_known_default() == base.port_or_known_default()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.path() == base.path()
        && raw_path == Some(base.path())
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
