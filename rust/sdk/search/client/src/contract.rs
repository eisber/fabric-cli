// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{SearchError, canonical_type};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fmt, num::NonZeroUsize};

const MAX_QUERY_CHARS: usize = 32_768;
const MAX_CURSOR_BYTES: usize = 65_536;
const MAX_TYPE_CHARS: usize = 50;
const MAX_TYPES: usize = 500;
const MAX_RESULTS: usize = 1000;
const RANKED_LIMIT: usize = 50;
const DEFAULT_BYTES: usize = 1024 * 1024;
const UUID_PATTERN: &str =
    "^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$";

/// A structurally validated UUID, canonicalized to lowercase for identity equality.
///
/// Validation does not establish existence, authorization or credential binding.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Guid(String);

impl Guid {
    /// Validate exact hexadecimal 8-4-4-4-12 shape without probing a provider.
    ///
    /// # Errors
    /// Returns sanitized invalid-input evidence for malformed identifiers.
    pub fn new(value: &str) -> Result<Self, SearchError> {
        if value.len() != 36
            || !value.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
        {
            return Err(SearchError::InvalidInput { field: "identity" });
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    /// Return the canonical UUID string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Explicit discovery semantics; only Catalog has native continuation.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    /// Public Catalog metadata discovery, not content or model search.
    #[default]
    Catalog,
    /// Explicitly admitted ranked retrieval through an injected provider.
    Relevance,
    /// Both selected sources with independent outcomes and bounded federation.
    Hybrid,
}

/// One explicit caller-selected tenant or workspace scope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SearchScope {
    /// Search accessible entries in this caller-bound tenant.
    Tenant {
        /// Explicit tenant identity, not a credential discovered by this library.
        tenant_id: Guid,
    },
    /// Search an exact workspace, never global search relabeled as scoped.
    Workspace {
        /// Explicit tenant identity.
        tenant_id: Guid,
        /// Authoritative workspace UUID.
        workspace_id: Guid,
    },
}

impl SearchScope {
    /// Construct a tenant scope.
    ///
    /// # Errors
    /// Rejects malformed tenant UUIDs.
    pub fn tenant(tenant_id: &str) -> Result<Self, SearchError> {
        Ok(Self::Tenant {
            tenant_id: Guid::new(tenant_id)?,
        })
    }

    /// Construct a workspace scope without resolving names or enumerating items.
    ///
    /// # Errors
    /// Rejects either malformed UUID.
    pub fn workspace(tenant_id: &str, workspace_id: &str) -> Result<Self, SearchError> {
        Ok(Self::Workspace {
            tenant_id: Guid::new(tenant_id)?,
            workspace_id: Guid::new(workspace_id)?,
        })
    }

    /// Return the selected tenant.
    pub fn tenant_id(&self) -> &Guid {
        match self {
            Self::Tenant { tenant_id } | Self::Workspace { tenant_id, .. } => tenant_id,
        }
    }

    /// Return workspace identity only for an explicitly workspace-scoped request.
    pub fn workspace_id(&self) -> Option<&Guid> {
        match self {
            Self::Tenant { .. } => None,
            Self::Workspace { workspace_id, .. } => Some(workspace_id),
        }
    }
}

/// Validated typed filters; arbitrary provider filter expressions are not admitted.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeFilters {
    include: Vec<String>,
    exclude: Vec<String>,
}

impl TypeFilters {
    /// Validate finite type lists and reject duplicate or contradictory values.
    ///
    /// Only the documented display aliases in [`canonical_type`] are normalized.
    ///
    /// # Errors
    /// Rejects empty/overlong/control-bearing types and more than 500 total values.
    pub fn new(include: Vec<String>, exclude: Vec<String>) -> Result<Self, SearchError> {
        if include.len().saturating_add(exclude.len()) > MAX_TYPES {
            return Err(SearchError::InvalidInput { field: "filters" });
        }
        let normalize = |values: Vec<String>| -> Result<Vec<String>, SearchError> {
            let mut result = Vec::new();
            for value in values {
                if value.trim().is_empty()
                    || value.chars().count() > MAX_TYPE_CHARS
                    || value.chars().any(char::is_control)
                    || value.contains(['\'', '"'])
                {
                    return Err(SearchError::InvalidInput { field: "filters" });
                }
                let value = canonical_type(&value).to_owned();
                if result.contains(&value) {
                    return Err(SearchError::InvalidInput { field: "filters" });
                }
                result.push(value);
            }
            Ok(result)
        };
        let include = normalize(include)?;
        let exclude = normalize(exclude)?;
        if include.iter().any(|value| exclude.contains(value)) {
            return Err(SearchError::InvalidInput { field: "filters" });
        }
        Ok(Self { include, exclude })
    }

    /// Return canonical include values, retaining unknown type strings.
    pub fn include(&self) -> &[String] {
        &self.include
    }

    /// Return canonical exclude values.
    pub fn exclude(&self) -> &[String] {
        &self.exclude
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }
}

/// Finite invocation ceilings, not promises about server scan cost.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchLimits {
    max_results: NonZeroUsize,
    page_size: NonZeroUsize,
    max_bytes: NonZeroUsize,
    max_requests: NonZeroUsize,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self::new(50, 50, DEFAULT_BYTES, 2).expect("finite constant search limits")
    }
}

impl SearchLimits {
    /// Construct finite limits for one page/top-k from each selected source.
    ///
    /// `max_bytes` bounds aggregate reported response bytes, serialized source
    /// observations and the entire unprojected receipt. Each provider receives
    /// this ceiling and a one-request allowance. Providers must enforce streaming
    /// byte bounds themselves; this layer cannot undo an oversized network read.
    ///
    /// # Errors
    /// Rejects zero, page/result counts above 1000, and request ceilings above two.
    pub fn new(
        max_results: usize,
        page_size: usize,
        max_bytes: usize,
        max_requests: usize,
    ) -> Result<Self, SearchError> {
        if max_results > MAX_RESULTS || page_size > MAX_RESULTS || max_requests > 2 {
            return Err(SearchError::InvalidInput { field: "limits" });
        }
        let positive =
            |value| NonZeroUsize::new(value).ok_or(SearchError::InvalidInput { field: "limits" });
        Ok(Self {
            max_results: positive(max_results)?,
            page_size: positive(page_size)?,
            max_bytes: positive(max_bytes)?,
            max_requests: positive(max_requests)?,
        })
    }

    /// Maximum returned identities after federation.
    pub const fn max_results(self) -> usize {
        self.max_results.get()
    }

    /// Native Catalog page-size request; not an all-pages result count.
    pub const fn page_size(self) -> usize {
        self.page_size.get()
    }

    /// Maximum aggregate response/serialized receipt bytes before projection.
    pub const fn max_bytes(self) -> usize {
        self.max_bytes.get()
    }

    /// Total selected provider request allowance.
    pub const fn max_requests(self) -> usize {
        self.max_requests.get()
    }
}

/// Initial text and typed filters, unchanged by ranking or query expansion.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitialSearch {
    mode: SearchMode,
    scope: SearchScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    filters: TypeFilters,
    limits: SearchLimits,
}

impl InitialSearch {
    /// Validate an initial request before capability checks or provider access.
    ///
    /// Ranked modes require nonblank text and at most 50 results (local policy,
    /// not an assertion about a native RE server limit). Catalog also permits
    /// filter-only/workspace-only discovery. Text is never trimmed or rewritten.
    ///
    /// # Errors
    /// Rejects blank/overlong supplied text, unbounded modes and empty global input.
    pub fn new(
        mode: SearchMode,
        scope: SearchScope,
        query: Option<String>,
        filters: TypeFilters,
        limits: SearchLimits,
    ) -> Result<Self, SearchError> {
        if query
            .as_ref()
            .is_some_and(|text| text.trim().is_empty() || text.chars().count() > MAX_QUERY_CHARS)
            || (mode != SearchMode::Catalog && query.is_none())
            || (mode == SearchMode::Catalog
                && query.is_none()
                && filters.is_empty()
                && scope.workspace_id().is_none())
        {
            return Err(SearchError::InvalidInput { field: "query" });
        }
        if mode != SearchMode::Catalog && limits.max_results() > RANKED_LIMIT {
            return Err(SearchError::InvalidInput {
                field: "maxResults",
            });
        }
        if mode == SearchMode::Hybrid && limits.max_requests() < 2 {
            return Err(SearchError::InvalidInput {
                field: "maxRequests",
            });
        }
        if mode == SearchMode::Hybrid && limits.max_bytes() < 2 {
            return Err(SearchError::InvalidInput { field: "maxBytes" });
        }
        Ok(Self {
            mode,
            scope,
            query,
            filters,
            limits,
        })
    }

    /// Selected source semantics.
    pub const fn mode(&self) -> SearchMode {
        self.mode
    }

    /// Explicit caller-bound scope.
    pub fn scope(&self) -> &SearchScope {
        &self.scope
    }

    /// Original text, without term rewriting, expansion or model synthesis.
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// Validated typed filters.
    pub fn filters(&self) -> &TypeFilters {
        &self.filters
    }

    /// Invocation ceilings.
    pub const fn limits(&self) -> SearchLimits {
        self.limits
    }
}

impl fmt::Debug for InitialSearch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InitialSearch")
            .field("mode", &self.mode)
            .field("scope", &self.scope)
            .field("query", &"<supplied text>")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// Native opaque Catalog cursor with explicit caller-supplied scope binding.
///
/// This association is not a signature or proof of the token's embedded scope.
/// A host importing a token must retain/verify the original caller binding.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogCursor {
    scope: SearchScope,
    token: String,
}

impl CatalogCursor {
    /// Associate a native token with its original explicit scope.
    ///
    /// # Errors
    /// Rejects empty tokens and a local 64-KiB allocation ceiling, not a service limit.
    pub fn from_native(scope: SearchScope, token: String) -> Result<Self, SearchError> {
        if token.trim().is_empty() || token.len() > MAX_CURSOR_BYTES {
            return Err(SearchError::InvalidInput { field: "cursor" });
        }
        Ok(Self { scope, token })
    }

    /// Original scope, never re-resolved from a display name on resume.
    pub fn scope(&self) -> &SearchScope {
        &self.scope
    }

    /// Original opaque native token, without normalization.
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl fmt::Debug for CatalogCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CatalogCursor")
            .field("scope", &self.scope)
            .field("token", &"<opaque cursor>")
            .finish()
    }
}

/// Resume of one native Catalog page, with no fresh query/filter/mode fields.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogResume {
    cursor: CatalogCursor,
    limits: SearchLimits,
}

impl CatalogResume {
    /// Construct a Catalog-only resume; the adapter sends the token, not page size.
    pub const fn new(cursor: CatalogCursor, limits: SearchLimits) -> Self {
        Self { cursor, limits }
    }

    /// Original native cursor and scope.
    pub fn cursor(&self) -> &CatalogCursor {
        &self.cursor
    }

    /// Local output/acquisition ceilings; not fresh service filter parameters.
    pub const fn limits(&self) -> SearchLimits {
        self.limits
    }
}

/// Mutually exclusive initial and native Catalog continuation contracts.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SearchRequest {
    /// Fresh query/filter request.
    Initial(InitialSearch),
    /// Native Catalog continuation only; hybrid has no common resume protocol.
    CatalogResume(CatalogResume),
}

impl SearchRequest {
    /// Decode the public JSON shape through the same validated constructors.
    ///
    /// # Errors
    /// Rejects unknown fields, wrong/null types and invalid semantic combinations.
    pub fn from_value(value: Value) -> Result<Self, SearchError> {
        let raw: RawRequest = serde_json::from_value(value)
            .map_err(|_| SearchError::InvalidInput { field: "input" })?;
        match raw {
            RawRequest::Initial {
                mode,
                scope,
                query,
                filters,
                limits,
            } => Ok(Self::Initial(InitialSearch::new(
                mode,
                scope.validate()?,
                query,
                TypeFilters::new(filters.include, filters.exclude)?,
                limits.validate()?,
            )?)),
            RawRequest::CatalogResume { cursor, limits } => {
                Ok(Self::CatalogResume(CatalogResume::new(
                    CatalogCursor::from_native(cursor.scope.validate()?, cursor.token)?,
                    limits.validate()?,
                )))
            }
        }
    }

    /// Selected sources; resume always selects Catalog only.
    pub const fn mode(&self) -> SearchMode {
        match self {
            Self::Initial(initial) => initial.mode(),
            Self::CatalogResume(_) => SearchMode::Catalog,
        }
    }

    /// Original explicit scope.
    pub fn scope(&self) -> &SearchScope {
        match self {
            Self::Initial(initial) => initial.scope(),
            Self::CatalogResume(resume) => resume.cursor().scope(),
        }
    }

    /// Finite invocation limits.
    pub const fn limits(&self) -> SearchLimits {
        match self {
            Self::Initial(initial) => initial.limits(),
            Self::CatalogResume(resume) => resume.limits(),
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum RawRequest {
    Initial {
        #[serde(default)]
        mode: SearchMode,
        scope: RawScope,
        #[serde(default, deserialize_with = "present_query")]
        query: Option<String>,
        #[serde(default)]
        filters: RawFilters,
        #[serde(default)]
        limits: RawLimits,
    },
    CatalogResume {
        cursor: RawCursor,
        #[serde(default)]
        limits: RawLimits,
    },
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum RawScope {
    Tenant {
        tenant_id: String,
    },
    Workspace {
        tenant_id: String,
        workspace_id: String,
    },
}

impl RawScope {
    fn validate(self) -> Result<SearchScope, SearchError> {
        match self {
            Self::Tenant { tenant_id } => SearchScope::tenant(&tenant_id),
            Self::Workspace {
                tenant_id,
                workspace_id,
            } => SearchScope::workspace(&tenant_id, &workspace_id),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFilters {
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCursor {
    scope: RawScope,
    token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawLimits {
    #[serde(default = "default_count")]
    max_results: usize,
    #[serde(default = "default_count")]
    page_size: usize,
    #[serde(default = "default_bytes")]
    max_bytes: usize,
    #[serde(default = "default_requests")]
    max_requests: usize,
}

impl Default for RawLimits {
    fn default() -> Self {
        Self {
            max_results: default_count(),
            page_size: default_count(),
            max_bytes: default_bytes(),
            max_requests: default_requests(),
        }
    }
}

impl RawLimits {
    fn validate(self) -> Result<SearchLimits, SearchError> {
        SearchLimits::new(
            self.max_results,
            self.page_size,
            self.max_bytes,
            self.max_requests,
        )
    }
}

const fn default_count() -> usize {
    50
}
const fn default_bytes() -> usize {
    DEFAULT_BYTES
}
const fn default_requests() -> usize {
    2
}

fn present_query<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

/// The single public schema bundle consumed by SDK, CLI and MCP projections.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSchemas {
    /// Draft 2020-12 request schema, with adapter-enforced cross-field rules described.
    pub input: Value,
    /// Canonical shared receipt schema; typed source facts live in `meta.sources`.
    pub output: Value,
}

/// Build schemas from this contract's enums, field limits and input variants.
///
/// No host owns a parallel search schema. Descriptions identify semantic checks
/// (provider capabilities, cumulative filter count, nonblank text and cursor
/// binding) that JSON Schema alone does not establish.
pub fn schemas() -> SearchSchemas {
    let guid = json!({"type":"string","pattern":UUID_PATTERN});
    let scope = json!({"oneOf":[
        {"type":"object","additionalProperties":false,"required":["kind","tenantId"],
         "properties":{"kind":{"const":"tenant"},"tenantId":guid}},
        {"type":"object","additionalProperties":false,
         "required":["kind","tenantId","workspaceId"],
         "properties":{"kind":{"const":"workspace"},"tenantId":guid,"workspaceId":guid}}
    ]});
    let limits = json!({"type":"object","additionalProperties":false,"properties":{
        "maxResults":{"type":"integer","minimum":1,"maximum":MAX_RESULTS,"default":default_count()},
        "pageSize":{"type":"integer","minimum":1,"maximum":MAX_RESULTS,"default":default_count()},
        "maxBytes":{"type":"integer","minimum":1,"maximum":usize::MAX,"default":default_bytes()},
        "maxRequests":{"type":"integer","minimum":1,"maximum":2,"default":default_requests()}
    }});
    let type_list = json!({"type":"array","maxItems":MAX_TYPES,"uniqueItems":true,
        "items":{"type":"string","minLength":1,"maxLength":MAX_TYPE_CHARS,
            "pattern":"^[^'\"\\u0000-\\u001F\\u007F-\\u009F]+$"}});
    let modes: Vec<_> = [
        SearchMode::Catalog,
        SearchMode::Relevance,
        SearchMode::Hybrid,
    ]
    .into_iter()
    .map(|mode| serde_json::to_value(mode).expect("search mode serializes"))
    .collect();
    let input = json!({
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "description":"Validated SDK search input. Text is not rewritten. At most 500 total include/exclude values; no duplicate/conflicting canonical types. Ranked modes require nonblank query, maxResults <= 50; hybrid maxRequests >= 2. Catalog requires text, filters or a workspace. Provider capabilities and original cursor caller binding are checked separately.",
        "oneOf":[
            {"type":"object","additionalProperties":false,"required":["kind","scope"],
             "properties":{
                "kind":{"const":"initial"},
                "mode":{"enum":modes,"default":SearchMode::default()},
                "scope":scope,
                "query":{"type":"string","minLength":1,"maxLength":MAX_QUERY_CHARS},
                "filters":{"type":"object","additionalProperties":false,"properties":{
                    "include":type_list,"exclude":type_list}},
                "limits":limits
             },
             "allOf":[
                {"if":{"required":["mode"],"properties":{"mode":{"enum":[SearchMode::Relevance,SearchMode::Hybrid]}}},
                 "then":{"required":["query"],"properties":{"limits":{"properties":{"maxResults":{"maximum":RANKED_LIMIT}}}}}},
                {"if":{"required":["mode"],"properties":{"mode":{"const":SearchMode::Hybrid}}},
                 "then":{"properties":{"limits":{"properties":{"maxRequests":{"minimum":2},"maxBytes":{"minimum":2}}}}}}
             ]},
            {"type":"object","additionalProperties":false,"required":["kind","cursor"],
             "properties":{
                "kind":{"const":"catalogResume"},
                "cursor":{"type":"object","additionalProperties":false,"required":["scope","token"],
                    "properties":{"scope":scope,"token":{"type":"string","minLength":1,
                      "description":"Opaque token; SDK also enforces a local 65536-byte ceiling."}}},
                "limits":limits
             }}
        ]
    });
    SearchSchemas {
        input,
        output: crate::bridge::output_schema(&crate::model::output_schema(scope, guid)),
    }
}
