// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{CatalogClient, SearchPage, search_payload};
use azure_core::http::Method;
use fabric_sdk_api::{ApiError, Request};
use serde_json::{Value, json};
use std::{fmt, future::Future, num::NonZeroUsize};

/// Default initial Catalog page size; resume uses the service's opaque token.
pub const DEFAULT_CATALOG_PAGE_SIZE: usize = 50;

/// One decoded page with actual response measurements retained for federation.
#[derive(Clone, Debug)]
pub struct CatalogPageObservation {
    /// Validated rows and the original next-page token.
    pub page: SearchPage,
    /// Actual completed response status.
    pub http_status: u16,
    /// Actual received JSON body bytes, before projection or reserialization.
    pub response_bytes: usize,
}

/// Catalog failure retaining available response facts without inventing measurements.
#[derive(Clone, Debug)]
pub struct CatalogObservationError {
    /// Original classified API failure.
    pub error: ApiError,
    /// Actual response status, absent before a response was obtained.
    pub http_status: Option<u16>,
    /// Measured complete response bytes, absent for transport/stream failures.
    pub response_bytes: Option<usize>,
    /// Advertised numeric Retry-After seconds; no retry is performed.
    pub retry_after_seconds: Option<u64>,
}

impl fmt::Display for CatalogObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.error, formatter)
    }
}

impl std::error::Error for CatalogObservationError {}

/// Validated Catalog filters, with no arbitrary OData expression input.
///
/// Inclusion types are OR-ed, exclusion types are AND-ed, and workspace IDs are
/// OR-ed. Those groups are combined with AND. Unknown type names are retained:
/// validation protects the filter syntax, not a local service-type allowlist.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct CatalogFilter {
    include_types: Vec<String>,
    exclude_types: Vec<String>,
    workspace_ids: Vec<String>,
}

impl CatalogFilter {
    /// Validates and stores filters without normalization or I/O.
    ///
    /// At most 500 total include/exclude type values are accepted. Each must
    /// contain non-whitespace text, be at most 50 Unicode scalar values long,
    /// and contain neither quotes nor control characters. At most 12 workspace
    /// IDs are accepted, each in exact 8-4-4-4-12 hexadecimal GUID syntax.
    /// Empty vectors produce an empty filter, suitable for a text-only request.
    pub fn new(
        include_types: Vec<String>,
        exclude_types: Vec<String>,
        workspace_ids: Vec<String>,
    ) -> Result<Self, ApiError> {
        if include_types
            .len()
            .checked_add(exclude_types.len())
            .is_none_or(|count| count > 500)
            || workspace_ids.len() > 12
            || include_types.iter().chain(&exclude_types).any(|value| {
                value.trim().is_empty()
                    || value.chars().count() > 50
                    || value
                        .chars()
                        .any(|c| c.is_control() || matches!(c, '\'' | '"'))
            })
            || workspace_ids.iter().any(|id| !is_guid(id))
        {
            return Err(ApiError::InvalidInput);
        }
        Ok(Self {
            include_types,
            exclude_types,
            workspace_ids,
        })
    }

    /// Borrows included type names in caller order, without a local allowlist.
    pub fn include_types(&self) -> &[String] {
        &self.include_types
    }

    /// Borrows excluded type names in caller order.
    pub fn exclude_types(&self) -> &[String] {
        &self.exclude_types
    }

    /// Borrows validated workspace GUIDs in their original spelling.
    pub fn workspace_ids(&self) -> &[String] {
        &self.workspace_ids
    }

    /// Reports whether no filtering constraint was supplied.
    pub fn is_empty(&self) -> bool {
        self.include_types.is_empty()
            && self.exclude_types.is_empty()
            && self.workspace_ids.is_empty()
    }

    fn expression(&self) -> Option<String> {
        let mut groups = Vec::new();
        for (values, property, operator, join) in [
            (&self.include_types, "Type", "eq", " or "),
            (&self.exclude_types, "Type", "ne", " and "),
            (&self.workspace_ids, "WorkspaceId", "eq", " or "),
        ] {
            if !values.is_empty() {
                let clauses: Vec<_> = values
                    .iter()
                    .map(|value| format!("{property} {operator} '{value}'"))
                    .collect();
                groups.push(format!("({})", clauses.join(join)));
            }
        }
        (!groups.is_empty()).then(|| groups.join(" and "))
    }
}

impl fmt::Debug for CatalogFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CatalogFilter")
            .field("include_count", &self.include_types.len())
            .field("exclude_count", &self.exclude_types.len())
            .field("workspace_count", &self.workspace_ids.len())
            .finish()
    }
}

fn is_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Immutable validated fields of an initial Catalog request.
///
/// Construct through [`CatalogRequest::initial`]; private fields prevent
/// bypassing validation or mutating a valid request into an unbounded one.
#[derive(Clone, PartialEq, Eq)]
pub struct CatalogInitialRequest {
    search: Option<String>,
    filter: CatalogFilter,
    page_size: NonZeroUsize,
}

impl CatalogInitialRequest {
    /// Borrows the exact search text, if supplied.
    pub fn search(&self) -> Option<&str> {
        self.search.as_deref()
    }

    /// Borrows the validated filter.
    pub fn filter(&self) -> &CatalogFilter {
        &self.filter
    }

    /// Returns the initial page size, always in 1..=1000.
    pub fn page_size(&self) -> NonZeroUsize {
        self.page_size
    }
}

impl fmt::Debug for CatalogInitialRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CatalogInitialRequest")
            .field("has_search", &self.search.is_some())
            .field("filter", &self.filter)
            .field("page_size", &self.page_size)
            .finish()
    }
}

/// Immutable opaque continuation, with no fresh search, filter or page size.
///
/// Construct through [`CatalogRequest::resume`]. Tokens are not trimmed,
/// interpreted as URLs, or used to select credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct CatalogResumeRequest {
    token: String,
}

impl CatalogResumeRequest {
    /// Borrows the exact opaque continuation; do not log it.
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl fmt::Debug for CatalogResumeRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CatalogResumeRequest")
            .finish_non_exhaustive()
    }
}

/// Separate initial and token-only resume requests for bounded Catalog reads.
///
/// Variant payloads have private fields so all instances remain validated.
/// This API is additive: legacy [`crate::SearchOptions`] semantics are unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogRequest {
    /// Fresh search-only, filter-only, or combined query with finite page size.
    Initial(CatalogInitialRequest),
    /// Continuation of the service-bound query, without fresh parameters.
    Resume(CatalogResumeRequest),
}

impl CatalogRequest {
    /// Creates an initial request with page size 1..=1000, defaulting to 50.
    ///
    /// A supplied search must not be blank and is otherwise preserved exactly.
    /// With no search, at least one typed filter constraint is required.
    pub fn initial(
        search: Option<String>,
        filter: CatalogFilter,
        page_size: Option<usize>,
    ) -> Result<Self, ApiError> {
        let page_size = page_size.unwrap_or(DEFAULT_CATALOG_PAGE_SIZE);
        if !(1..=1000).contains(&page_size)
            || search.as_ref().is_some_and(|text| text.trim().is_empty())
            || (search.is_none() && filter.is_empty())
        {
            return Err(ApiError::InvalidInput);
        }
        Ok(Self::Initial(CatalogInitialRequest {
            search,
            filter,
            page_size: NonZeroUsize::new(page_size).ok_or(ApiError::InvalidInput)?,
        }))
    }

    /// Creates a token-only request, rejecting blank tokens without changing bytes.
    pub fn resume(token: String) -> Result<Self, ApiError> {
        if token.trim().is_empty() {
            return Err(ApiError::InvalidInput);
        }
        Ok(Self::Resume(CatalogResumeRequest { token }))
    }

    /// Builds the exact Fabric v1 POST body using shared bounded JSON encoding.
    ///
    /// Resume emits only `continuationToken`, never fresh search/filter/pageSize.
    pub fn request(&self) -> Result<Request, ApiError> {
        let payload = match self {
            Self::Initial(initial) => {
                let mut payload = json!({"pageSize": initial.page_size.get()});
                if let Some(search) = &initial.search {
                    payload["search"] = Value::String(search.clone());
                }
                if let Some(filter) = initial.filter.expression() {
                    payload["filter"] = Value::String(filter);
                }
                payload
            }
            Self::Resume(resume) => json!({"continuationToken": resume.token}),
        };
        Request::fabric(Method::Post, &["catalog", "search"])?.with_json(&payload)
    }
}

impl CatalogClient {
    /// Reads one Catalog page with caller-injected transport and cancellation.
    ///
    /// Rows, unknown service types and opaque next tokens are retained without
    /// hydration, local filtering, or additional requests. Callers own cross-page
    /// request/row/cycle bounds. A terminal page does not certify tenant coverage.
    pub async fn search_v2_page(
        &self,
        request: &CatalogRequest,
        byte_limit: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<SearchPage, ApiError> {
        self.search_v2_observed(request, byte_limit, cancelled)
            .await
            .map(|observation| observation.page)
            .map_err(|failure| failure.error)
    }

    /// Reads one page while preserving actual response evidence for a search host.
    ///
    /// # Errors
    /// Retains measured status, body size and throttling hints when a response
    /// exists; pre-response failures keep those facts unavailable.
    pub async fn search_v2_observed(
        &self,
        request: &CatalogRequest,
        byte_limit: NonZeroUsize,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<CatalogPageObservation, CatalogObservationError> {
        let unobserved = |error| CatalogObservationError {
            error,
            http_status: None,
            response_bytes: None,
            retry_after_seconds: None,
        };
        let response = self
            .api
            .send(
                &request.request().map_err(unobserved)?,
                byte_limit,
                cancelled,
            )
            .await
            .map_err(unobserved)?;
        let failure = |error| CatalogObservationError {
            error,
            http_status: Some(response.status),
            response_bytes: Some(response.body.len()),
            retry_after_seconds: response
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
                .and_then(|(_, value)| value.parse().ok()),
        };
        let value = search_payload(&response).map_err(failure)?;
        let rows = value
            .get("value")
            .and_then(Value::as_array)
            .ok_or_else(|| failure(ApiError::InvalidResponse))?;
        if rows.iter().any(|row| !row.is_object()) {
            return Err(failure(ApiError::InvalidResponse));
        }
        let next_token = match value.get("continuationToken") {
            None | Some(Value::Null) => None,
            Some(Value::String(token)) => (!token.is_empty()).then(|| token.clone()),
            _ => return Err(failure(ApiError::Continuation)),
        };
        Ok(CatalogPageObservation {
            page: SearchPage {
                rows: rows.clone(),
                next_token,
            },
            http_status: response.status,
            response_bytes: response.body.len(),
        })
    }
}
