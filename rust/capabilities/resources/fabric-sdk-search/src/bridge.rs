// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    CatalogCursor, Coverage, SearchData, SearchError, SearchMode, SearchOperation, SearchRequest,
    SearchScope, SearchStatus, SourceEvidence, SourceStatus, schemas,
};
use fabric_v2_contracts::{
    ApprovedOperation, Availability, Binding, ContractError, Coverage as CoreCoverage, Effect,
    ExecutionEvidence, ExecutionReceipt, FailureInfo, Generation, OperationDescriptor, Outcome,
    Precondition, PreparedOperation, ReceiptBuildError, ReceiptError, Registry, ResolvedTarget,
    SourceEvidence as CoreSourceEvidence, SourceEvidenceError, Target, TargetKind, TenantIdentity,
    Verb, WorkspaceIdentity,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::future::Future;

/// One operation identity for Catalog, relevance and hybrid search heads.
pub const SEARCH_OPERATION_ID: &str = "catalog.search";

/// Return the one pure operation descriptor for SDK/CLI/schema projections.
///
/// # Errors
/// Returns shared registration validation errors; no identity/provider is accessed.
pub fn search_descriptor() -> Result<OperationDescriptor, ContractError> {
    let contract = schemas();
    OperationDescriptor::new(
        SEARCH_OPERATION_ID,
        Verb::Search,
        contract.input,
        contract.output,
        vec![Effect::RemoteRead],
    )?
    .with_optional_effects(vec![Effect::ModelExecution, Effect::LocalWrite])
}

/// Typed operation-specific source facts embedded in shared receipt metadata.
///
/// The flattened observation retains canonical source status, byte measurement,
/// retry hint and native coverage. Sibling fields disambiguate raw native cursor
/// evidence from an actually executable, lossless Catalog-only continuation.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSourceDetails {
    /// Original source-specific observation.
    #[serde(flatten)]
    pub observation: SourceEvidence,
    /// Requested provider semantics.
    pub mode: SearchMode,
    /// Explicit caller-bound scope, including tenant for workspace selections.
    pub scope: SearchScope,
    /// Executable native Catalog continuation, never a common hybrid cursor.
    pub continuation: Option<CatalogCursor>,
    /// Known discarded distinct rows; unavailable when federation failed.
    pub dropped_candidates: Option<usize>,
}

/// Preparation/execution bridge failures retaining any constructed receipt.
#[derive(Debug, thiserror::Error)]
pub enum SearchBridgeError {
    /// Scope and canonical path disagree.
    #[error("search requires the exact canonical path for its explicit scope")]
    InvalidCanonicalPath,
    /// Approval does not bind exactly this request, target and effect set.
    #[error("search approval does not match the immutable operation request")]
    ApprovalMismatch,
    /// Invalid shared operation registration.
    #[error("search operation registration is invalid")]
    Registration(#[from] ContractError),
    /// Invalid shared target/preparation evidence.
    #[error("search operation preparation is invalid")]
    Preparation(#[from] ReceiptError),
    /// Typed data could not be represented as JSON.
    #[error("search receipt serialization failed")]
    Serialization,
    /// Shared receipt construction rejected evidence without discarding it.
    #[error("search result contradicts shared receipt evidence")]
    ReceiptBuild {
        /// Original result and aggregate evidence.
        failure: Box<ReceiptBuildError<SearchData>>,
        /// Independent source facts from the same execution.
        sources: Vec<CoreSourceEvidence>,
    },
    /// Shared source attachment retained the original receipt and observations.
    #[error("search source evidence contradicts the shared receipt")]
    Sources(Box<SourceEvidenceError<SearchData>>),
    /// The complete unprojected canonical receipt exceeds the selected bound.
    #[error("canonical search receipt exceeds the output byte budget")]
    OutputBudget {
        /// Original receipt, not permission to replay or silently truncate it.
        receipt: Box<ExecutionReceipt<SearchData>>,
    },
}

/// Prepare the single search operation over a real tenant/workspace collection.
///
/// This performs no authorization, credentials, I/O or provider execution. The
/// caller must obtain immediate approval through its own `ApprovalPolicy`.
/// ModelExecution is selected only for explicit relevance/hybrid modes.
/// LocalWrite is merely selectable for an exact host-owned LocalOutput guard;
/// this SDK never performs file output or adds metering claims.
///
/// # Errors
/// Rejects a path other than `/` or the exact canonical workspace UUID path.
pub fn prepare_search(
    request: &SearchRequest,
    canonical_path: &str,
) -> Result<PreparedOperation, SearchBridgeError> {
    let expected_path = match request.scope().workspace_id() {
        None => "/".to_owned(),
        Some(workspace) => format!("/workspaces/{}", workspace.as_str()),
    };
    if canonical_path != expected_path {
        return Err(SearchBridgeError::InvalidCanonicalPath);
    }
    let (kind, owner) = match request.scope() {
        SearchScope::Tenant { .. } => (TargetKind::TenantCollection, "Tenant"),
        SearchScope::Workspace { .. } => (TargetKind::WorkspaceCollection, "Workspace"),
    };
    let selection = Target {
        kind,
        owner_type: owner.into(),
        facet: "catalog".into(),
        representation: "metadata".into(),
        generation: None,
    };
    let resolved = match request.scope() {
        SearchScope::Tenant { tenant_id } => ResolvedTarget::tenant_collection(
            canonical_path,
            TenantIdentity::new(tenant_id.as_str())?,
            selection.clone(),
        )?,
        SearchScope::Workspace { workspace_id, .. } => ResolvedTarget::workspace_collection(
            canonical_path,
            WorkspaceIdentity::new(workspace_id.as_str())?,
            selection.clone(),
        )?,
    };
    let mut registry = Registry::default();
    registry.register(search_descriptor()?)?;
    registry.bind(
        Binding::new(
            owner,
            "catalog",
            "metadata",
            Generation::Any,
            SEARCH_OPERATION_ID,
            Availability::Available,
        )?
        .with_target_kind(kind),
    )?;
    Ok(PreparedOperation::new(
        registry.resolve(Verb::Search, &selection, None)?,
        resolved,
        serde_json::to_value(request).map_err(|_| SearchBridgeError::Serialization)?,
    )?
    .with_optional_effects(if request.mode() == SearchMode::Catalog {
        vec![]
    } else {
        vec![Effect::ModelExecution]
    })?)
}

/// Execute only the exact immutable approved request and return a shared receipt.
///
/// No SDK policy approves on the caller's behalf. Wrong input/tenant/path/type,
/// omitted model effects, unrelated effects and unsupported preconditions fail
/// before provider access. A host may select LocalWrite only together with one
/// exact LocalOutput guard that its own policy preflights and approves.
///
/// Provider/partial/all-source failures become truthful shared outcomes with
/// source evidence in `meta.sources`, never empty success-shaped JSON.
///
/// # Errors
/// Returns approval mismatch, contradictory evidence or an output-budget error;
/// construction/output errors retain the original result and source facts.
pub async fn execute_approved_search(
    approved: &ApprovedOperation<'_>,
    request: &SearchRequest,
    operation: &SearchOperation<'_>,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<ExecutionReceipt<SearchData>, SearchBridgeError> {
    let prepared = approved.prepared();
    let expected = prepare_search(request, prepared.target().canonical_path())?;
    let mut expected_effects = expected.effects().to_vec();
    match prepared.preconditions() {
        [] => {}
        [Precondition::LocalOutput { .. }] => expected_effects.push(Effect::LocalWrite),
        _ => return Err(SearchBridgeError::ApprovalMismatch),
    }
    expected_effects.sort();
    let mut effects = prepared.effects().to_vec();
    effects.sort();
    if prepared.operation_id() != expected.operation_id()
        || prepared.input() != expected.input()
        || prepared.target() != expected.target()
        || prepared.availability() != expected.availability()
        || effects != expected_effects
    {
        return Err(SearchBridgeError::ApprovalMismatch);
    }
    let (outcome, coverage, native_sources, continuation, dropped) =
        match operation.execute(request, cancelled).await {
            Ok(receipt) => {
                let coverage = aggregate_coverage(&receipt);
                let outcome = if receipt.status == SearchStatus::Succeeded {
                    Outcome::Succeeded { data: receipt.data }
                } else {
                    Outcome::Partial {
                        data: receipt.data,
                        error: failure("PartialSearch", "Selected search coverage is incomplete."),
                    }
                };
                (
                    outcome,
                    coverage,
                    receipt.sources,
                    receipt.continuation,
                    Some(receipt.dropped_candidates),
                )
            }
            Err(error) => {
                let code = search_error_code(&error);
                let sources = match error {
                    SearchError::AllSourcesFailed { sources }
                    | SearchError::ObservedFailure { sources, .. } => sources,
                    _ => vec![],
                };
                (
                    Outcome::Failed {
                        error: failure(code, "Search did not produce a usable completed result."),
                    },
                    CoreCoverage::Unknown,
                    sources,
                    None,
                    None,
                )
            }
        };
    let selected_statuses: Vec<_> = native_sources
        .iter()
        .filter(|source| source.status() != SourceStatus::NotSelected)
        .collect();
    let http_status = if selected_statuses.len() == 1 {
        selected_statuses[0].http_status()
    } else {
        None
    };
    let sources = native_sources
        .into_iter()
        .map(|source| {
            let coverage = source_coverage(&source);
            let error = source.error().map(|error| {
                failure(
                    provider_error_code(error.code),
                    "Selected search provider failed.",
                )
            });
            let source_name = match source.source() {
                crate::Source::Catalog => "catalog",
                crate::Source::Relevance => "relevance",
            };
            let details = serde_json::to_value(SearchSourceDetails {
                observation: source.clone(),
                mode: request.mode(),
                scope: request.scope().clone(),
                continuation: if source.source() == crate::Source::Catalog {
                    continuation.clone()
                } else {
                    None
                },
                dropped_candidates: dropped,
            })
            .map_err(|_| SearchBridgeError::Serialization)?;
            Ok(CoreSourceEvidence {
                source: source_name.into(),
                http_status: source.http_status(),
                coverage,
                error,
                details: Some(details),
            })
        })
        .collect::<Result<Vec<_>, SearchBridgeError>>()?;
    let receipt = ExecutionReceipt::new(
        approved,
        outcome,
        ExecutionEvidence {
            http_status,
            handles: vec![],
            coverage,
            mutation_may_have_occurred: false,
        },
    )
    .map_err(|failure| SearchBridgeError::ReceiptBuild {
        failure,
        sources: sources.clone(),
    })?
    .with_source_evidence(sources)
    .map_err(SearchBridgeError::Sources)?;
    if crate::model::check_bytes(&receipt, request.limits().max_bytes()).is_err() {
        return Err(SearchBridgeError::OutputBudget {
            receipt: Box::new(receipt),
        });
    }
    Ok(receipt)
}

fn aggregate_coverage(receipt: &crate::SearchReceipt) -> CoreCoverage {
    if receipt.status() == SearchStatus::Partial {
        let mut reasons: Vec<_> = receipt
            .sources()
            .iter()
            .filter(|source| {
                matches!(
                    source.status(),
                    SourceStatus::Failed | SourceStatus::Limited
                )
            })
            .map(|source| {
                format!("{:?}_{:?}", source.source(), source.status()).to_ascii_lowercase()
            })
            .collect();
        if receipt.dropped_candidates() != 0 {
            reasons.push("global_result_cap".into());
        }
        if reasons.is_empty() {
            reasons.push("source_coverage_incomplete".into());
        }
        CoreCoverage::Incomplete {
            reasons,
            continuation: receipt
                .continuation()
                .map(|cursor| cursor.token().to_owned()),
        }
    } else if receipt.sources().iter().any(|source| {
        source.status() != SourceStatus::NotSelected
            && matches!(source.coverage(), Coverage::Unknown | Coverage::RankedTopK)
    }) {
        CoreCoverage::Unknown
    } else {
        CoreCoverage::Complete
    }
}

fn source_coverage(source: &SourceEvidence) -> CoreCoverage {
    match source.status() {
        SourceStatus::NotSelected => CoreCoverage::Unknown,
        SourceStatus::Failed | SourceStatus::Limited => CoreCoverage::Incomplete {
            reasons: vec![
                match source.status() {
                    SourceStatus::Failed => "source_failed",
                    _ => "source_limited",
                }
                .into(),
            ],
            continuation: None,
        },
        SourceStatus::Completed => match source.coverage() {
            Coverage::CatalogPage { continuation: None } => CoreCoverage::Complete,
            _ => CoreCoverage::Unknown,
        },
    }
}

fn failure(code: &str, message: &str) -> FailureInfo {
    FailureInfo {
        code: code.into(),
        message: message.into(),
    }
}

fn search_error_code(error: &SearchError) -> &'static str {
    match error {
        SearchError::InvalidInput { .. } | SearchError::ProviderInput { .. } => "InvalidInput",
        SearchError::MissingProvider { .. } => "NotConfigured",
        SearchError::UnsupportedCapability { .. } => "UnsupportedCapability",
        SearchError::IdentityConflict => "IdentityConflict",
        SearchError::InvalidObservation => "InvalidResponse",
        SearchError::AllSourcesFailed { .. } => "SearchFailed",
        SearchError::ObservedFailure { cause, .. } => search_error_code(cause),
        SearchError::BudgetExceeded => "BudgetExceeded",
        SearchError::Cancelled => "Cancelled",
    }
}

fn provider_error_code(code: crate::ProviderErrorCode) -> &'static str {
    use crate::ProviderErrorCode;
    match code {
        ProviderErrorCode::Unauthenticated => "Unauthenticated",
        ProviderErrorCode::AccessDenied => "AccessDenied",
        ProviderErrorCode::TimedOut => "TimedOut",
        ProviderErrorCode::Unavailable => "Unavailable",
        ProviderErrorCode::InvalidResponse => "InvalidResponse",
        ProviderErrorCode::UnsupportedCapability => "UnsupportedCapability",
        ProviderErrorCode::Cancelled => "Cancelled",
        ProviderErrorCode::Throttled => "Throttled",
        ProviderErrorCode::BudgetExceeded => "BudgetExceeded",
    }
}

pub(crate) fn output_schema(raw: &Value) -> Value {
    let data = raw["properties"]["data"].clone();
    let source = raw["properties"]["sources"]["items"].clone();
    let scope = raw["properties"]["scope"].clone();
    let cursor = raw["properties"]["continuation"].clone();
    let coverage = json!({"oneOf":[
        {"type":"object","required":["kind"],"properties":{"kind":{"const":"complete"}}},
        {"type":"object","required":["kind"],"properties":{"kind":{"const":"unknown"}}},
        {"type":"object","required":["kind","reasons","continuation"],"properties":{
            "kind":{"const":"incomplete"},"reasons":{"type":"array","minItems":1,"items":{"type":"string"}},
            "continuation":{"type":["string","null"]}}}
    ]});
    let failure = json!({"type":"object","additionalProperties":false,
        "required":["code","message"],"properties":{"code":{"type":"string"},"message":{"type":"string"}}});
    let mut details = source;
    details["properties"]["mode"] = raw["properties"]["mode"].clone();
    details["properties"]["scope"] = scope;
    details["properties"]["continuation"] = cursor;
    details["properties"]["droppedCandidates"] = json!({"type":["integer","null"],"minimum":0});
    if let Some(required) = details["required"].as_array_mut() {
        required.extend(
            ["mode", "scope", "continuation", "droppedCandidates"].map(|field| json!(field)),
        );
    }
    json!({
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "description":"Unprojected shared search receipt. Completed requested top-k retrieval can succeed with unknown inventory coverage; actual failures, known limits/continuation or discarded fetched rows remain partial/failed. Explicit local projection may replace only data; meta.sources and all identity/effect evidence remain unchanged.",
        "type":"object","additionalProperties":false,"required":["schemaVersion","status","meta"],
        "properties":{
            "schemaVersion":{"const":"0.1"},"status":{"enum":["succeeded","partial","failed"]},
            "data":data,"error":failure,
            "meta":{"type":"object","additionalProperties":false,
                "required":["operationId","target","declaredEffects","preconditions","evidence"],
                "properties":{
                    "operationId":{"const":SEARCH_OPERATION_ID},
                    "target":{"type":"object","required":["canonicalPath","identity","selection","child"],
                        "properties":{"canonicalPath":{"type":"string"},
                            "identity":{"type":"object","properties":{
                                "kind":{"enum":["tenantCollection","workspaceCollection"]},
                                "tenantId":{"type":"string"},"workspaceId":{"type":"string"}}},
                            "selection":{"type":"object","required":["kind","ownerType","facet","representation","generation"],
                                "properties":{"kind":{"enum":["tenantCollection","workspaceCollection"]},
                                    "ownerType":{"enum":["Tenant","Workspace"]},"facet":{"const":"catalog"},
                                    "representation":{"const":"metadata"},"generation":{"type":"null"}}},
                            "child":{"type":"null"}}},
                    "declaredEffects":{"type":"array","minItems":1,"uniqueItems":true,
                        "items":{"enum":[Effect::RemoteRead,Effect::ModelExecution,Effect::LocalWrite]}},
                    "preconditions":{"type":"array","maxItems":1,"items":{
                        "type":"object","required":["kind","path"],
                        "properties":{"kind":{"const":"localOutput"},"path":{"type":"string"}}}},
                    "evidence":{"type":"object",
                        "required":["httpStatus","handles","coverage","mutationMayHaveOccurred"],
                        "properties":{"httpStatus":{"type":["integer","null"]},"handles":{"type":"array","maxItems":0},
                            "coverage":coverage,"mutationMayHaveOccurred":{"const":false}}},
                    "sources":{"type":"array","maxItems":2,"items":{
                        "type":"object","additionalProperties":false,"required":["source","httpStatus","coverage","error","details"],
                        "properties":{"source":{"enum":["catalog","relevance"]},
                            "httpStatus":{"type":["integer","null"]},"coverage":coverage,
                            "error":{"anyOf":[failure,{"type":"null"}]},"details":details}}}
                }}
        },
        "allOf":[
            {"if":{"properties":{"status":{"const":"succeeded"}}},"then":{"required":["data"],"not":{"required":["error"]}}},
            {"if":{"properties":{"status":{"const":"partial"}}},"then":{"required":["data","error"]}},
            {"if":{"properties":{"status":{"const":"failed"}}},"then":{"required":["error"],"not":{"required":["data"]}}}
        ]
    })
}
