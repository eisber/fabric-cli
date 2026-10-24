// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded GET `/v1/capacities` through caller-owned execution.

use crate::{Capacity, CapacityClient, FabricServiceError, Page};
use azure_core::{credentials::AccessToken, http::Url};
use fabric_core::Audience;
pub use fabric_sdk_workspaces::{
    WorkspaceExecutionError as CapacityExecutionError, WorkspaceResponse as CapacityResponse,
};
use serde_json::Value;
use std::{
    collections::HashSet,
    fmt,
    future::{Future, poll_fn},
    num::NonZeroUsize,
    pin::{Pin, pin},
    task::Poll,
};

const CAPACITIES_URL: &str = "https://api.fabric.microsoft.com/v1/capacities";

/// A validated GET, with no arbitrary endpoint, headers, or auth scope.
pub struct CapacityRequest<'a> {
    url: Url,
    token: &'a AccessToken,
    max_response_bytes: NonZeroUsize,
}

impl CapacityRequest<'_> {
    /// Returns GET, the only method in this slice.
    pub const fn method(&self) -> &'static str {
        "GET"
    }
    /// Returns the fixed endpoint and encoded token; do not log the query.
    pub fn url(&self) -> &Url {
        &self.url
    }
    /// Returns the injected bearer token; never log or persist it.
    pub fn token(&self) -> &AccessToken {
        self.token
    }
    /// Returns the byte limit the executor must enforce during streaming.
    pub const fn max_response_bytes(&self) -> NonZeroUsize {
        self.max_response_bytes
    }
    /// Returns the content type used by the Python helper.
    pub const fn content_type(&self) -> &'static str {
        "application/json"
    }
}

impl fmt::Debug for CapacityRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapacityRequest")
            .field("method", &"GET")
            .field("endpoint", &CAPACITIES_URL)
            .field("token", &"<injected>")
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

/// Injected execution; implementations enforce streaming limits and finite policy.
///
/// Executors must disable redirects and credential discovery, and release local
/// resources when their future is dropped. A dropped request is not success.
/// The optional shared HTTP adapter supplies existing read retry/deadline policy.
pub trait CapacityExecutor: Send + Sync {
    /// Executes one validated request using the supplied Fabric token.
    fn execute<'a>(
        &'a self,
        request: CapacityRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<CapacityResponse, CapacityExecutionError>> + Send + 'a>>;
}

/// Explicit nonzero traversal and allocation safety bounds.
#[derive(Clone, Copy, Debug)]
pub struct CapacityListLimits {
    /// Maximum decoded pages, including empty pages.
    pub max_pages: NonZeroUsize,
    /// Maximum accumulated rows, including duplicates.
    pub max_items: NonZeroUsize,
    /// Maximum bytes per response, also enforced by the executor while reading.
    pub max_response_bytes: NonZeroUsize,
}

impl Default for CapacityListLimits {
    fn default() -> Self {
        Self {
            max_pages: NonZeroUsize::new(100).unwrap(),
            max_items: NonZeroUsize::new(100_000).unwrap(),
            max_response_bytes: NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
        }
    }
}

/// Committed decoded progress; success is returned only at terminal continuation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CapacityList {
    /// Raw rows in service page order, including duplicates.
    pub value: Vec<Capacity>,
    /// Actual accumulated pages, not Python's recursive `total_pages` metadata.
    pub pages_received: usize,
    /// The next unconsumed token, when known; never log it.
    pub next_token: Option<String>,
}

/// Distinct failure classifications; no incomplete success fallback.
#[derive(Clone, Debug, PartialEq)]
pub enum CapacityListErrorKind {
    /// Selection is not for Fabric; no credential is acquired.
    InvalidAudience,
    /// The injected credential failed, without trying another provider.
    Credential,
    /// The executor failed with a sanitized classification.
    Execution(CapacityExecutionError),
    /// Service rejection, preserving source error-code precedence.
    Http {
        /// HTTP status.
        status: u16,
        /// Source-compatible error code or its defined fallback.
        code: String,
        /// Structured untrusted service error, when valid.
        service_error: Option<Box<FabricServiceError>>,
    },
    /// 202/LRO and 204 are not completed list pages.
    UnsupportedStatus(u16),
    /// A page, row, or selected token was malformed.
    InvalidResponse,
    /// An active continuation URI did not match the fixed endpoint.
    InvalidContinuation,
    /// A token repeated, indicating a cycle.
    RepeatedContinuation,
    /// Another page remained at the page bound.
    PageLimit,
    /// A page would exceed the total row bound.
    ItemLimit,
    /// A response exceeded the byte bound.
    ResponseLimit,
    /// Cancellation won before, during, or between requests.
    Cancelled,
}

/// A failed or incomplete traversal with explicitly retained prior pages.
#[derive(Clone, Debug, PartialEq)]
pub struct CapacityListError {
    /// Sanitized failure classification.
    pub kind: CapacityListErrorKind,
    /// Fully committed pages only; never treat this as a complete listing.
    pub partial: CapacityList,
}

impl fmt::Display for CapacityListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.kind {
            CapacityListErrorKind::InvalidAudience => {
                "capacity listing requires the Fabric audience"
            }
            CapacityListErrorKind::Credential => {
                "the injected credential could not acquire a Fabric token"
            }
            CapacityListErrorKind::Execution(_) => "capacity request execution failed",
            CapacityListErrorKind::Http { .. } => "Fabric rejected the capacity request",
            CapacityListErrorKind::UnsupportedStatus(_) => {
                "the response is not a completed capacity-list page"
            }
            CapacityListErrorKind::InvalidResponse => {
                "Fabric returned an invalid capacity-list page"
            }
            CapacityListErrorKind::InvalidContinuation => {
                "Fabric returned an invalid capacity continuation URI"
            }
            CapacityListErrorKind::RepeatedContinuation => {
                "Fabric repeated a capacity continuation token"
            }
            CapacityListErrorKind::PageLimit => "capacity pagination reached its page bound",
            CapacityListErrorKind::ItemLimit => "capacity pagination reached its item bound",
            CapacityListErrorKind::ResponseLimit => "the capacity response exceeded its byte bound",
            CapacityListErrorKind::Cancelled => "capacity listing was cancelled",
        })
    }
}

impl std::error::Error for CapacityListError {}

impl CapacityClient {
    /// Lists raw capacities through injected execution and cancellation.
    ///
    /// Cancellation takes priority over a simultaneously ready response, dropping
    /// in-flight credential/executor futures. Use `std::future::pending()` when
    /// cancellation is driven solely by dropping this call.
    ///
    /// # Errors
    /// Returns typed identity, execution, service, payload, safety-bound, or
    /// cancellation failures, preserving fully committed progress explicitly.
    pub async fn list_capacities(
        &self,
        executor: &dyn CapacityExecutor,
        limits: CapacityListLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<CapacityList, CapacityListError> {
        let mut progress = CapacityList::default();
        let mut cancelled = pin!(cancelled);
        match self
            .collect(executor, limits, &mut progress, cancelled.as_mut())
            .await
        {
            Ok(()) => Ok(progress),
            Err(kind) => Err(CapacityListError {
                kind,
                partial: progress,
            }),
        }
    }

    async fn collect(
        &self,
        executor: &dyn CapacityExecutor,
        limits: CapacityListLimits,
        progress: &mut CapacityList,
        mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
    ) -> Result<(), CapacityListErrorKind> {
        or_cancel(std::future::ready(()), cancelled.as_mut()).await?;
        if self.selection().audience() != Audience::Fabric {
            return Err(CapacityListErrorKind::InvalidAudience);
        }
        let mut seen = HashSet::new();
        loop {
            let mut url = Url::parse(CAPACITIES_URL)
                .map_err(|_| CapacityListErrorKind::InvalidContinuation)?;
            if let Some(token) = &progress.next_token {
                url.query_pairs_mut()
                    .append_pair("continuationToken", token);
                // Python urlencode differs from WHATWG form encoding for these two bytes.
                let query = url.query().unwrap().replace("%7E", "~").replace('*', "%2A");
                url.set_query(Some(&query));
            }
            let token = or_cancel(
                self.credential()
                    .get_token(&[Audience::Fabric.scope()], None),
                cancelled.as_mut(),
            )
            .await?
            .map_err(|_| CapacityListErrorKind::Credential)?;
            let response = or_cancel(
                executor.execute(CapacityRequest {
                    url,
                    token: &token,
                    max_response_bytes: limits.max_response_bytes,
                }),
                cancelled.as_mut(),
            )
            .await?
            .map_err(|error| match error {
                CapacityExecutionError::ResponseLimit => CapacityListErrorKind::ResponseLimit,
                error => CapacityListErrorKind::Execution(error),
            })?;
            or_cancel(std::future::ready(()), cancelled.as_mut()).await?;
            if response.body.len() > limits.max_response_bytes.get() {
                return Err(CapacityListErrorKind::ResponseLimit);
            }
            match response.status {
                200 | 201 => {}
                202 | 204 => return Err(CapacityListErrorKind::UnsupportedStatus(response.status)),
                _ => return Err(service_failure(&response)),
            }
            let mut payload: Value = serde_json::from_slice(&response.body)
                .map_err(|_| CapacityListErrorKind::InvalidResponse)?;
            let object = payload
                .as_object_mut()
                .ok_or(CapacityListErrorKind::InvalidResponse)?;
            let body_token = object.remove("continuationToken");
            let uri = object.remove("continuationUri");
            let next = match header(&response, "x-ms-continuation") {
                Some(token) => Some(token.to_owned()),
                None => match body_token {
                    Some(Value::String(token)) => Some(token),
                    None | Some(Value::Null) => None,
                    _ => return Err(CapacityListErrorKind::InvalidResponse),
                },
            }
            .filter(|token| !token.is_empty());
            let page: Page<Capacity> = serde_json::from_value(payload)
                .map_err(|_| CapacityListErrorKind::InvalidResponse)?;
            if page.value.len() > limits.max_items.get() - progress.value.len() {
                return Err(CapacityListErrorKind::ItemLimit);
            }
            progress.value.extend(page.value);
            progress.pages_received += 1;
            progress.next_token = next;
            let Some(next) = &progress.next_token else {
                return Ok(());
            };
            match uri {
                Some(Value::String(uri)) => validate_continuation(&uri)?,
                None | Some(Value::Null) => {}
                _ => return Err(CapacityListErrorKind::InvalidContinuation),
            }
            if !seen.insert(next.clone()) {
                return Err(CapacityListErrorKind::RepeatedContinuation);
            }
            if progress.pages_received >= limits.max_pages.get() {
                return Err(CapacityListErrorKind::PageLimit);
            }
        }
    }
}

async fn or_cancel<T>(
    operation: impl Future<Output = T>,
    mut cancelled: Pin<&mut (impl Future<Output = ()> + Send)>,
) -> Result<T, CapacityListErrorKind> {
    let mut operation = pin!(operation);
    poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(CapacityListErrorKind::Cancelled));
        }
        operation.as_mut().poll(cx).map(Ok)
    })
    .await
}

fn header<'a>(response: &'a CapacityResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn service_failure(response: &CapacityResponse) -> CapacityListErrorKind {
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
            .filter(|code| !code.is_empty())
            .unwrap_or("UnknownError")
            .to_owned(),
    };
    CapacityListErrorKind::Http {
        status: response.status,
        code,
        service_error: body
            .and_then(|body| serde_json::from_value(body).ok())
            .map(Box::new),
    }
}

fn validate_continuation(uri: &str) -> Result<(), CapacityListErrorKind> {
    let url = Url::parse(uri).map_err(|_| CapacityListErrorKind::InvalidContinuation)?;
    if url.scheme() != "https"
        || url.host_str() != Some("api.fabric.microsoft.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.path() != "/v1/capacities"
    {
        return Err(CapacityListErrorKind::InvalidContinuation);
    }
    Ok(())
}
