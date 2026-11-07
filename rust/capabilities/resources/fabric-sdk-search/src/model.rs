// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{CatalogCursor, Guid, SearchMode, SearchScope};
use serde::Serialize;
use serde_json::{Value, json};
use std::{fmt, io};

/// One selected discovery source, not a credential or service endpoint.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Native public Catalog metadata search.
    Catalog,
    /// Explicit caller-admitted relevance retrieval.
    Relevance,
}

/// Canonicalize only documented type aliases; preserve all unknown types exactly.
pub fn canonical_type(value: &str) -> &str {
    match value {
        "AISkill" | "Data Agent" => "DataAgent",
        "DataWarehouse" => "Warehouse",
        other => other,
    }
}

/// Verified coordinates within distinct provider resource namespaces.
///
/// Construction validates syntax only. A provider must establish that returned
/// coordinates belong to its authenticated tenant before returning an observation.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CandidateIdentity {
    /// Fabric artifact identity; never a join on its display name.
    Item {
        /// Explicit source-established tenant.
        tenant_id: Guid,
        /// Artifact UUID, not a related SQL endpoint UUID.
        artifact_id: Guid,
    },
    /// A Catalog workspace entry, not an item lacking workspace hierarchy.
    Workspace {
        /// Explicit source-established tenant.
        tenant_id: Guid,
        /// Workspace UUID in its own identity namespace.
        workspace_id: Guid,
    },
}

impl CandidateIdentity {
    /// Construct item coordinates.
    ///
    /// # Errors
    /// Rejects malformed tenant or artifact UUID.
    pub fn item(tenant_id: &str, artifact_id: &str) -> Result<Self, SearchError> {
        Ok(Self::Item {
            tenant_id: Guid::new(tenant_id)?,
            artifact_id: Guid::new(artifact_id)?,
        })
    }

    /// Construct a workspace entry.
    ///
    /// # Errors
    /// Rejects malformed tenant or workspace UUID.
    pub fn workspace(tenant_id: &str, workspace_id: &str) -> Result<Self, SearchError> {
        Ok(Self::Workspace {
            tenant_id: Guid::new(tenant_id)?,
            workspace_id: Guid::new(workspace_id)?,
        })
    }

    /// Explicit source-established tenant.
    pub fn tenant_id(&self) -> &Guid {
        match self {
            Self::Item { tenant_id, .. } | Self::Workspace { tenant_id, .. } => tenant_id,
        }
    }
}

/// Source-local display metadata retained without manufacturing missing fields.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateMetadata {
    /// Optional service-returned display name; never fabricated from an identifier.
    pub display_name: Option<String>,
    /// Optional description from this source, not a generated answer.
    pub description: Option<String>,
}

/// Evidence for one candidate occurrence, retained on verified cross-source overlap.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    source: Source,
    rank: usize,
    native_type: String,
    metadata: CandidateMetadata,
}

impl Provenance {
    /// Originating provider.
    pub const fn source(&self) -> Source {
        self.source
    }

    /// One-based rank within this provider's supplied order; not a common score.
    pub const fn rank(&self) -> usize {
        self.rank
    }

    /// Original provider type before the explicit display-alias mapping.
    pub fn native_type(&self) -> &str {
        &self.native_type
    }

    /// Original display evidence; conflicting descriptions are not overwritten.
    pub fn metadata(&self) -> &CandidateMetadata {
        &self.metadata
    }
}

/// Typed result identity with one or more original provider occurrences.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    identity: CandidateIdentity,
    artifact_type: String,
    workspace_id: Option<Guid>,
    provenance: Vec<Provenance>,
}

impl Candidate {
    /// Construct one source occurrence without fabricating workspace evidence.
    ///
    /// # Errors
    /// Rejects zero rank, invalid/overlong metadata or contradictory workspace entries.
    pub fn new(
        identity: CandidateIdentity,
        native_type: &str,
        workspace_id: Option<Guid>,
        metadata: CandidateMetadata,
        source: Source,
        rank: usize,
    ) -> Result<Self, SearchError> {
        if rank == 0
            || native_type.trim().is_empty()
            || native_type.len() > 256
            || native_type.chars().any(char::is_control)
            || metadata
                .display_name
                .as_ref()
                .is_some_and(|value| value.len() > 4096)
            || metadata
                .description
                .as_ref()
                .is_some_and(|value| value.len() > 65_536)
        {
            return Err(SearchError::InvalidObservation);
        }
        let artifact_type = canonical_type(native_type).to_owned();
        if let CandidateIdentity::Workspace {
            workspace_id: actual,
            ..
        } = &identity
            && (artifact_type != "Workspace"
                || workspace_id.as_ref().is_some_and(|value| value != actual))
        {
            return Err(SearchError::IdentityConflict);
        }
        if matches!(&identity, CandidateIdentity::Item { .. }) && artifact_type == "Workspace" {
            return Err(SearchError::IdentityConflict);
        }
        Ok(Self {
            identity,
            artifact_type,
            workspace_id,
            provenance: vec![Provenance {
                source,
                rank,
                native_type: native_type.into(),
                metadata,
            }],
        })
    }

    /// Authoritative identity namespace and coordinates.
    pub fn identity(&self) -> &CandidateIdentity {
        &self.identity
    }

    /// Canonical known type, or the exact unknown native type.
    pub fn artifact_type(&self) -> &str {
        &self.artifact_type
    }

    /// Item ownership when actually supplied; never inferred from a workspace name.
    pub fn workspace_id(&self) -> Option<&Guid> {
        match &self.identity {
            CandidateIdentity::Workspace { workspace_id, .. } => Some(workspace_id),
            CandidateIdentity::Item { .. } => self.workspace_id.as_ref(),
        }
    }

    /// All original source-local metadata/rank evidence.
    pub fn provenance(&self) -> &[Provenance] {
        &self.provenance
    }

    pub(crate) fn merge(&mut self, other: Self) -> Result<(), SearchError> {
        if self.identity != other.identity
            || self.artifact_type != other.artifact_type
            || self
                .workspace_id()
                .zip(other.workspace_id())
                .is_some_and(|(left, right)| left != right)
        {
            return Err(SearchError::IdentityConflict);
        }
        if self.workspace_id.is_none() {
            self.workspace_id = other.workspace_id;
        }
        for provenance in other.provenance {
            if !self.provenance.contains(&provenance) {
                self.provenance.push(provenance);
            }
        }
        Ok(())
    }
}

/// Source-specific completeness, never a promise of complete tenant inventory.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Coverage {
    /// One native page of the accessible requested Catalog query.
    CatalogPage {
        /// Original next-page token when the service returned one.
        continuation: Option<String>,
    },
    /// Ranked retrieval has no established exhaustive inventory contract or cursor.
    RankedTopK,
    /// The provider explicitly could not establish completeness.
    Unknown,
    /// The provider stopped at an advertised finite boundary.
    Limited {
        /// Safe typed boundary explanation.
        reason: LimitReason,
    },
}

/// Safe source-boundary classifications rather than arbitrary provider text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LimitReason {
    /// Native source truncation or a source-specific top-k boundary.
    ProviderLimit,
    /// Local result count boundary.
    ResultLimit,
    /// Byte ceiling stopped observation.
    ByteLimit,
}

impl fmt::Debug for Coverage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CatalogPage { continuation } => formatter
                .debug_struct("CatalogPage")
                .field("has_continuation", &continuation.is_some())
                .finish(),
            Self::RankedTopK => formatter.write_str("RankedTopK"),
            Self::Unknown => formatter.write_str("Unknown"),
            Self::Limited { reason } => formatter.debug_tuple("Limited").field(reason).finish(),
        }
    }
}

impl Coverage {
    pub(crate) fn limited(&self) -> bool {
        matches!(
            self,
            Self::CatalogPage {
                continuation: Some(_)
            } | Self::Limited { .. }
        )
    }
}

/// Sanitized provider failure categories, with no raw response text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderErrorCode {
    /// The explicit caller lacks provider authentication.
    Unauthenticated,
    /// The selected provider denied access.
    AccessDenied,
    /// The provider did not finish in its caller-supplied budget.
    TimedOut,
    /// Provider unavailable; no alternative route is implied.
    Unavailable,
    /// Malformed, contradictory or unexpected response.
    InvalidResponse,
    /// The selected provider does not implement this request.
    UnsupportedCapability,
    /// Provider-local work was cancelled.
    Cancelled,
    /// Provider returned throttling evidence.
    Throttled,
    /// A finite acquisition boundary was exceeded.
    BudgetExceeded,
}

/// Structured safe failure evidence; never a reason to replay or switch identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, thiserror::Error)]
#[error("selected search provider failed")]
#[serde(rename_all = "camelCase")]
pub struct ProviderError {
    /// Machine-readable provider failure category.
    pub code: ProviderErrorCode,
    /// Actual HTTP status when observed; absent for pre-response failures.
    pub http_status: Option<u16>,
    /// Advertised Retry-After seconds; this library never automatically retries.
    pub retry_after_seconds: Option<u64>,
    /// Measured complete response bytes, absent before response/stream completion.
    pub response_bytes: Option<usize>,
}

impl ProviderError {
    /// Construct a failure without inventing HTTP or retry evidence.
    pub const fn new(code: ProviderErrorCode) -> Self {
        Self {
            code,
            http_status: None,
            retry_after_seconds: None,
            response_bytes: None,
        }
    }
}

/// Independent provider outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceStatus {
    /// A usable bounded observation, including a completed empty page.
    Completed,
    /// Usable observation with an explicit unobserved remainder/boundary.
    Limited,
    /// This selected source failed; not an empty successful source.
    Failed,
    /// This source was not selected by the request.
    NotSelected,
}

/// Per-source evidence retained outside projected result data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceEvidence {
    pub(crate) source: Source,
    pub(crate) status: SourceStatus,
    pub(crate) coverage: Coverage,
    pub(crate) error: Option<ProviderError>,
    pub(crate) http_status: Option<u16>,
    pub(crate) requests: usize,
    pub(crate) response_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) native_diagnostics: Option<Value>,
}

impl SourceEvidence {
    /// Originating source.
    pub const fn source(&self) -> Source {
        self.source
    }

    /// Independent selected/not-selected/usable/failed outcome.
    pub const fn status(&self) -> SourceStatus {
        self.status
    }

    /// Native coverage retained independently of projected hits.
    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    /// Classified failure, absent for usable sources.
    pub fn error(&self) -> Option<&ProviderError> {
        self.error.as_ref()
    }

    /// Actual HTTP response status, absent when no response was observed.
    pub const fn http_status(&self) -> Option<u16> {
        self.http_status
    }

    /// Provider invocation attempts, not an assertion about token/HTTP send counts.
    pub const fn requests(&self) -> usize {
        self.requests
    }

    /// Measured response bytes, absent when acquisition did not establish a count.
    pub const fn response_bytes(&self) -> Option<usize> {
        self.response_bytes
    }

    /// Bounded diagnostics actually returned by the source, outside projected data.
    pub fn native_diagnostics(&self) -> Option<&Value> {
        self.native_diagnostics.as_ref()
    }

    pub(crate) fn not_selected(source: Source) -> Self {
        Self {
            source,
            status: SourceStatus::NotSelected,
            coverage: Coverage::Unknown,
            error: None,
            http_status: None,
            requests: 0,
            response_bytes: Some(0),
            native_diagnostics: None,
        }
    }
}

/// One validated source response before pure federation.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceObservation {
    pub(crate) evidence: SourceEvidence,
    pub(crate) candidates: Vec<Candidate>,
}

impl SourceObservation {
    /// Construct a usable page/top-k observation.
    ///
    /// A selected provider performs at most one network request. The adapter must
    /// measure actual response bytes and enforce its streaming byte allowance.
    ///
    /// # Errors
    /// Rejects malformed status, wrong-source provenance, nonascending ranks,
    /// excessive rows and invalid source/cursor combinations.
    pub fn usable(
        source: Source,
        candidates: Vec<Candidate>,
        coverage: Coverage,
        response_bytes: usize,
        http_status: Option<u16>,
    ) -> Result<Self, SearchError> {
        if candidates.len() > 1000
            || http_status.is_some_and(|status| status != 200)
            || (source == Source::Relevance && matches!(coverage, Coverage::CatalogPage { .. }))
            || (source == Source::Catalog && matches!(coverage, Coverage::RankedTopK))
        {
            return Err(SearchError::InvalidObservation);
        }
        if let Coverage::CatalogPage {
            continuation: Some(token),
        } = &coverage
            && (token.trim().is_empty() || token.len() > 65_536)
        {
            return Err(SearchError::InvalidObservation);
        }
        let mut last_rank = 0;
        for candidate in &candidates {
            if candidate.provenance.len() != 1
                || candidate.provenance[0].source != source
                || candidate.provenance[0].rank <= last_rank
            {
                return Err(SearchError::InvalidObservation);
            }
            last_rank = candidate.provenance[0].rank;
        }
        Ok(Self {
            evidence: SourceEvidence {
                source,
                status: if coverage.limited() {
                    SourceStatus::Limited
                } else {
                    SourceStatus::Completed
                },
                coverage,
                error: None,
                http_status,
                requests: 1,
                response_bytes: Some(response_bytes),
                native_diagnostics: None,
            },
            candidates,
        })
    }

    /// Construct a classified failure with no usable candidates.
    ///
    /// # Errors
    /// Rejects impossible HTTP evidence rather than relabeling it.
    pub fn failed(source: Source, error: ProviderError) -> Result<Self, SearchError> {
        if error
            .http_status
            .is_some_and(|status| !(100..600).contains(&status))
        {
            return Err(SearchError::InvalidObservation);
        }
        Ok(Self {
            evidence: SourceEvidence {
                source,
                status: SourceStatus::Failed,
                coverage: Coverage::Unknown,
                http_status: error.http_status,
                response_bytes: error.response_bytes,
                native_diagnostics: None,
                error: Some(error),
                requests: 1,
            },
            candidates: Vec::new(),
        })
    }

    /// Source status and coverage, without projection.
    pub fn evidence(&self) -> &SourceEvidence {
        &self.evidence
    }

    /// Original provider order, before merging and the global cap.
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    /// Retains only structured, bounded native diagnostics supplied by this source.
    ///
    /// This does not request debugging, enable a service gate or synthesize fields.
    ///
    /// # Errors
    /// Rejects nonobject diagnostics and metadata above the local 1-MiB ceiling.
    pub fn with_native_diagnostics(
        mut self,
        diagnostics: Option<Value>,
    ) -> Result<Self, SearchError> {
        if let Some(value) = &diagnostics {
            if !value.is_object() {
                return Err(SearchError::InvalidObservation);
            }
            check_bytes(value, 1024 * 1024)?;
        }
        self.evidence.native_diagnostics = diagnostics;
        Ok(self)
    }

    pub(crate) fn reject(mut self) -> Self {
        self.candidates.clear();
        self.evidence.status = SourceStatus::Failed;
        self.evidence.coverage = Coverage::Unknown;
        self.evidence.error = Some(ProviderError {
            http_status: self.evidence.http_status,
            response_bytes: self.evidence.response_bytes,
            ..ProviderError::new(ProviderErrorCode::InvalidResponse)
        });
        self
    }
}

/// Projectable result payload; evidence lives in sibling receipt fields.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchData {
    /// Bounded identities with original per-source provenance.
    pub hits: Vec<Candidate>,
}

/// Search command completeness, not exhaustive inventory completeness.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchStatus {
    /// Selected source observations completed without failure or explicit limits.
    Succeeded,
    /// At least one selected source failed/was limited, or the global cap dropped hits.
    Partial,
}

/// Search-specific receipt, independent of unfinished shared tenant/effect receipts.
///
/// It does not claim that host approval occurred. Hosts must authorize the exact
/// scope, input, providers and effects before invoking [`crate::SearchOperation`].
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchReceipt<T = SearchData> {
    pub(crate) schema_version: &'static str,
    pub(crate) mode: SearchMode,
    pub(crate) scope: SearchScope,
    pub(crate) status: SearchStatus,
    pub(crate) data: T,
    pub(crate) sources: Vec<SourceEvidence>,
    pub(crate) continuation: Option<CatalogCursor>,
    pub(crate) dropped_candidates: usize,
}

impl<T> SearchReceipt<T> {
    /// Successful versus explicitly partial command outcome.
    pub const fn status(&self) -> SearchStatus {
        self.status
    }

    /// Typed data, separate from source evidence.
    pub fn data(&self) -> &T {
        &self.data
    }

    /// Independent source outcomes, including sources not selected.
    pub fn sources(&self) -> &[SourceEvidence] {
        &self.sources
    }

    /// Native Catalog resume only when no fetched distinct rows were discarded.
    pub fn continuation(&self) -> Option<&CatalogCursor> {
        self.continuation.as_ref()
    }

    /// Number of fetched distinct candidates omitted by the global cap.
    pub const fn dropped_candidates(&self) -> usize {
        self.dropped_candidates
    }

    /// Apply a caller-supplied local projection only to data, preserving evidence.
    ///
    /// The original typed receipt remains borrowed and intact on projection error.
    ///
    /// # Errors
    /// Returns the projection's original error without another provider call.
    pub fn project<U, E>(
        &self,
        project: impl FnOnce(&T) -> Result<U, E>,
    ) -> Result<SearchReceipt<U>, E> {
        Ok(SearchReceipt {
            schema_version: self.schema_version,
            mode: self.mode,
            scope: self.scope.clone(),
            status: self.status,
            data: project(&self.data)?,
            sources: self.sources.clone(),
            continuation: self.continuation.clone(),
            dropped_candidates: self.dropped_candidates,
        })
    }
}

/// Sanitized operation failure; no supplied query/cursor/service text is echoed.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// Local input validation failed before provider dispatch.
    #[error("invalid search input at {field}")]
    InvalidInput {
        /// A static registered field name, not supplied input.
        field: &'static str,
    },
    /// A selected provider was not injected; no fallback is permitted.
    #[error("selected search provider is not configured")]
    MissingProvider {
        /// Missing source.
        provider: Source,
    },
    /// Injected capabilities cannot honor a requested scope or filter.
    #[error("selected search provider does not support {feature}")]
    UnsupportedCapability {
        /// Source whose declared capability is insufficient.
        provider: Source,
        /// Static capability label.
        feature: &'static str,
    },
    /// A selected adapter rejected its exact request during pure preflight.
    #[error("selected search provider rejected input at {field}")]
    ProviderInput {
        /// Source whose input contract could not be satisfied.
        provider: Source,
        /// A static registered field/capability name.
        field: &'static str,
    },
    /// A source supplied malformed or contradictory observation evidence.
    #[error("invalid search provider observation")]
    InvalidObservation,
    /// Same identity has conflicting native type or workspace ownership.
    #[error("conflicting search identity ownership or type")]
    IdentityConflict,
    /// No selected source yielded a usable observation; completed-empty is usable.
    #[error("all selected search providers failed")]
    AllSourcesFailed {
        /// Independent safe failures, retained outside any projected result data.
        sources: Vec<SourceEvidence>,
    },
    /// Federation failed after source observations were acquired.
    #[error("search failed after observing selected providers")]
    ObservedFailure {
        /// Original classified operation failure.
        cause: Box<SearchError>,
        /// Original safe provider evidence, retained outside projected results.
        sources: Vec<SourceEvidence>,
    },
    /// Aggregate response or serialized output exceeded a finite local budget.
    #[error("search byte or request budget exceeded")]
    BudgetExceeded,
    /// Caller cancellation/deadline future completed; no remote cancel is implied.
    #[error("search was cancelled by the caller")]
    Cancelled,
}

pub(crate) fn check_bytes(value: &impl Serialize, limit: usize) -> Result<usize, SearchError> {
    let mut counter = ByteCounter { used: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| SearchError::BudgetExceeded)?;
    Ok(counter.used)
}

struct ByteCounter {
    used: usize,
    limit: usize,
}

impl io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.used) {
            return Err(io::Error::other("search serialization limit"));
        }
        self.used += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn output_schema(scope: Value, guid: Value) -> Value {
    let identity = json!({"oneOf":[
        {"type":"object","additionalProperties":false,
         "required":["kind","tenantId","artifactId"],
         "properties":{"kind":{"const":"item"},"tenantId":guid,"artifactId":guid}},
        {"type":"object","additionalProperties":false,
         "required":["kind","tenantId","workspaceId"],
         "properties":{"kind":{"const":"workspace"},"tenantId":guid,"workspaceId":guid}}
    ]});
    let metadata = json!({"type":"object","additionalProperties":false,
    "required":["displayName","description"],"properties":{
        "displayName":{"type":["string","null"],"maxLength":4096},
        "description":{"type":["string","null"],"maxLength":65536}
    }});
    let source = json!({"enum":[Source::Catalog,Source::Relevance]});
    let provenance = json!({"type":"object","additionalProperties":false,
    "required":["source","rank","nativeType","metadata"],
    "properties":{
        "source":source,"rank":{"type":"integer","minimum":1},
        "nativeType":{"type":"string","minLength":1,"maxLength":256},
        "metadata":metadata
    }});
    let candidate = json!({"type":"object","additionalProperties":false,
    "required":["identity","artifactType","workspaceId","provenance"],
    "properties":{
        "identity":identity,
        "artifactType":{"type":"string","minLength":1,"maxLength":256},
        "workspaceId":{"anyOf":[guid,{"type":"null"}]},
        "provenance":{"type":"array","minItems":1,"items":provenance}
    }});
    let coverage = json!({"oneOf":[
        {"type":"object","additionalProperties":false,"required":["kind","continuation"],
         "properties":{"kind":{"const":"catalogPage"},
                       "continuation":{"type":["string","null"]}}},
        {"type":"object","additionalProperties":false,"required":["kind"],
         "properties":{"kind":{"const":"rankedTopK"}}},
        {"type":"object","additionalProperties":false,"required":["kind"],
         "properties":{"kind":{"const":"unknown"}}},
        {"type":"object","additionalProperties":false,"required":["kind","reason"],
         "properties":{"kind":{"const":"limited"},
             "reason":{"enum":[LimitReason::ProviderLimit,LimitReason::ResultLimit,LimitReason::ByteLimit]}}}
    ]});
    let error = json!({"type":"object","additionalProperties":false,
    "required":["code","httpStatus","retryAfterSeconds","responseBytes"],
    "properties":{
        "code":{"enum":[ProviderErrorCode::Unauthenticated,ProviderErrorCode::AccessDenied,
            ProviderErrorCode::TimedOut,ProviderErrorCode::Unavailable,
            ProviderErrorCode::InvalidResponse,ProviderErrorCode::UnsupportedCapability,
            ProviderErrorCode::Cancelled,ProviderErrorCode::Throttled,ProviderErrorCode::BudgetExceeded]},
        "httpStatus":{"type":["integer","null"],"minimum":100,"maximum":599},
        "retryAfterSeconds":{"type":["integer","null"],"minimum":0},
        "responseBytes":{"type":["integer","null"],"minimum":0}
    }});
    let source_evidence = json!({"type":"object","additionalProperties":false,
    "required":["source","status","coverage","error","httpStatus","requests","responseBytes"],
    "properties":{
        "source":source,
        "status":{"enum":[SourceStatus::Completed,SourceStatus::Limited,SourceStatus::Failed,SourceStatus::NotSelected]},
        "coverage":coverage,
        "error":{"anyOf":[error,{"type":"null"}]},
        "httpStatus":{"type":["integer","null"],"minimum":100,"maximum":599},
        "requests":{"type":"integer","minimum":0,"maximum":1},
        "responseBytes":{"type":["integer","null"],"minimum":0},
        "nativeDiagnostics":{"type":"object"}
    }});
    let continuation = json!({"type":"object","additionalProperties":false,
    "required":["scope","token"],"properties":{
        "scope":scope,"token":{"type":"string","minLength":1}
    }});
    json!({
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "type":"object","additionalProperties":false,
        "required":["schemaVersion","mode","scope","status","data","sources",
                    "continuation","droppedCandidates"],
        "properties":{
            "schemaVersion":{"const":"0.1"},
            "mode":{"enum":[SearchMode::Catalog,SearchMode::Relevance,SearchMode::Hybrid]},
            "scope":scope,
            "status":{"enum":[SearchStatus::Succeeded,SearchStatus::Partial]},
            "data":{"type":"object","additionalProperties":false,"required":["hits"],
                "properties":{"hits":{"type":"array","maxItems":1000,
                    "items":candidate}}},
            "sources":{"type":"array","minItems":2,"maxItems":2,
                "items":source_evidence},
            "continuation":{"anyOf":[continuation,{"type":"null"}],
                "description":"Native Catalog only; null for hybrid or discarded fetched rows."},
            "droppedCandidates":{"type":"integer","minimum":0}
        }
    })
}
