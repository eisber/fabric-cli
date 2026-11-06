// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic public-boundary tests; no HTTP, credentials, runtime or local files.

#![deny(unnameable_test_items)]

use fabric_sdk_search::*;
use futures_util::{future::pending, task::noop_waker};
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

const TENANT: &str = "11111111-1111-4111-8111-111111111111";
const OTHER_TENANT: &str = "22222222-2222-4222-8222-222222222222";
const WORKSPACE: &str = "33333333-3333-4333-8333-333333333333";
const OTHER_WORKSPACE: &str = "44444444-4444-4444-8444-444444444444";
const A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const C: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const D: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

fn scope() -> SearchScope {
    SearchScope::tenant(TENANT).unwrap()
}

fn request(mode: SearchMode, limit: usize) -> SearchRequest {
    SearchRequest::Initial(
        InitialSearch::new(
            mode,
            scope(),
            Some(" sales && \"revenue\" ? * 🦀 ".into()),
            TypeFilters::default(),
            SearchLimits::new(limit, 50, 1024 * 1024, 2).unwrap(),
        )
        .unwrap(),
    )
}

fn candidate(source: Source, rank: usize, id: &str, kind: &str) -> Candidate {
    Candidate::new(
        CandidateIdentity::item(TENANT, id).unwrap(),
        kind,
        Some(Guid::new(WORKSPACE).unwrap()),
        CandidateMetadata {
            display_name: Some("same name".into()),
            description: Some(format!("source {source:?}")),
        },
        source,
        rank,
    )
    .unwrap()
}

fn observation(source: Source, rows: Vec<Candidate>) -> SourceObservation {
    SourceObservation::usable(
        source,
        rows,
        if source == Source::Catalog {
            Coverage::CatalogPage { continuation: None }
        } else {
            Coverage::RankedTopK
        },
        100,
        Some(200),
    )
    .unwrap()
}

fn failed(source: Source) -> SourceObservation {
    SourceObservation::failed(source, ProviderError::new(ProviderErrorCode::AccessDenied)).unwrap()
}

fn complete<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(&noop_waker()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("synthetic future must be immediately ready"),
    }
}

struct DropGuard(Arc<AtomicUsize>);

impl Drop for DropGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct Provider {
    capabilities: ProviderCapabilities,
    calls: AtomicUsize,
    inspections: AtomicUsize,
    outcome: Result<SourceObservation, ProviderError>,
    stay_pending: bool,
    dropped: Arc<AtomicUsize>,
}

impl Provider {
    fn new(source: Source, rows: Vec<Candidate>) -> Self {
        Self {
            capabilities: ProviderCapabilities {
                source,
                bound_tenant: Guid::new(TENANT).unwrap(),
                tenant_scope: true,
                workspace_scope: true,
                include_types: true,
                exclude_types: true,
                effective_types: None,
                catalog_resume: source == Source::Catalog,
            },
            calls: AtomicUsize::new(0),
            inspections: AtomicUsize::new(0),
            outcome: Ok(observation(source, rows)),
            stay_pending: false,
            dropped: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl SearchProvider for Provider {
    fn capabilities(&self) -> &ProviderCapabilities {
        self.inspections.fetch_add(1, Ordering::SeqCst);
        &self.capabilities
    }

    fn search<'a>(&'a self, request: ProviderRequest<'a>) -> SearchFuture<'a, SourceObservation> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.source(), self.capabilities.source);
        assert_eq!(request.max_requests(), 1);
        assert!(request.max_response_bytes() > 0);
        assert_eq!(
            request.max_response_bytes(),
            request.request().limits().max_bytes()
                / if request.request().mode() == SearchMode::Hybrid {
                    2
                } else {
                    1
                }
        );
        if let SearchRequest::Initial(initial) = request.request()
            && initial.query().is_some()
        {
            assert_eq!(initial.query(), Some(" sales && \"revenue\" ? * 🦀 "));
        }
        Box::pin(async move {
            let _guard = DropGuard(self.dropped.clone());
            if self.stay_pending {
                pending::<()>().await;
            }
            self.outcome.clone()
        })
    }
}

#[test]
fn identity_validation_is_structural_and_case_normalized() {
    for bad in ["", "tenant", "../id", "https://example.test", "0000-0000"] {
        assert!(Guid::new(bad).is_err());
        assert!(SearchScope::tenant(bad).is_err());
        assert!(CandidateIdentity::item(TENANT, bad).is_err());
    }
    assert_eq!(Guid::new(A).unwrap(), Guid::new(&A.to_uppercase()).unwrap());
    assert!(SearchScope::workspace(TENANT, WORKSPACE).is_ok());
}

#[test]
fn defaults_and_query_fidelity_are_owned_by_the_typed_contract() {
    let input = json!({"kind":"initial","scope":{"kind":"tenant","tenantId":TENANT},
                       "query":" sales && \"revenue\" ? * 🦀 "});
    let decoded = SearchRequest::from_value(input).unwrap();
    assert_eq!(decoded.mode(), SearchMode::Catalog);
    assert_eq!(decoded.limits().max_results(), 50);
    assert_eq!(decoded.limits().page_size(), 50);
    let encoded = serde_json::to_value(&decoded).unwrap();
    assert_eq!(encoded["query"], " sales && \"revenue\" ? * 🦀 ");
    assert!(!format!("{decoded:?}").contains("revenue"));
    let provider = Provider::new(Source::Catalog, Vec::new());
    let result = complete(SearchOperation::new(Some(&provider), None).execute(&decoded, pending()));
    assert!(result.is_ok());
    assert_eq!(provider.calls(), 1);
}

#[test]
fn invalid_json_combinations_never_become_normalized_requests() {
    let valid =
        json!({"kind":"initial","scope":{"kind":"tenant","tenantId":TENANT},"query":"text"});
    let mut cases = Vec::new();
    for (field, value) in [
        ("query", Value::Null),
        ("query", json!("  ")),
        ("query", json!(5)),
        ("mode", json!("semantic")),
        ("continuationToken", json!("private-token")),
        ("rawFilter", json!("Type eq 'Report'")),
        ("scope", json!({"kind":"tenant","tenantId":"private"})),
        ("filters", json!({"include":null})),
        ("limits", json!({"maxResults":0})),
        ("limits", json!({"maxRequests":3})),
        ("limits", json!({"pageSize":1001})),
    ] {
        let mut input = valid.clone();
        input[field] = value;
        cases.push(input);
    }
    cases.push(json!({"kind":"catalogResume","query":"fresh","cursor":{
        "scope":{"kind":"tenant","tenantId":TENANT},"token":"opaque"}}));
    for input in cases {
        let error = SearchRequest::from_value(input).unwrap_err();
        assert!(!error.to_string().contains("private"));
    }
}

#[test]
fn global_catalog_requires_text_or_filters_but_workspace_filter_only_is_explicit() {
    assert!(
        InitialSearch::new(
            SearchMode::Catalog,
            scope(),
            None,
            TypeFilters::default(),
            SearchLimits::default()
        )
        .is_err()
    );
    let scoped = InitialSearch::new(
        SearchMode::Catalog,
        SearchScope::workspace(TENANT, WORKSPACE).unwrap(),
        None,
        TypeFilters::default(),
        SearchLimits::default(),
    )
    .unwrap();
    let encoded = serde_json::to_value(SearchRequest::Initial(scoped)).unwrap();
    assert!(encoded.get("query").is_none());
    assert!(SearchRequest::from_value(encoded).is_ok());
}

#[test]
fn finite_numeric_and_ranked_policy_boundaries_are_validated() {
    assert!(SearchLimits::new(1, 1, 1, 1).is_ok());
    assert!(SearchLimits::new(1000, 1000, 1024, 2).is_ok());
    for limits in [(0, 1, 1, 1), (1, 0, 1, 1), (1, 1, 0, 1), (1, 1, 1, 0)] {
        assert!(SearchLimits::new(limits.0, limits.1, limits.2, limits.3).is_err());
    }
    assert!(SearchLimits::new(1001, 50, 1024, 1).is_err());
    for mode in [SearchMode::Relevance, SearchMode::Hybrid] {
        assert!(
            InitialSearch::new(
                mode,
                scope(),
                Some("text".into()),
                TypeFilters::default(),
                SearchLimits::new(51, 50, 1024, 2).unwrap()
            )
            .is_err()
        );
    }
    assert!(
        InitialSearch::new(
            SearchMode::Hybrid,
            scope(),
            Some("text".into()),
            TypeFilters::default(),
            SearchLimits::new(50, 50, 1024, 1).unwrap()
        )
        .is_err()
    );
}

#[test]
fn typed_filters_reject_duplicates_conflicts_and_cardinality_violations() {
    assert!(TypeFilters::new(vec!["AISkill".into()], vec!["DataAgent".into()]).is_err());
    assert!(TypeFilters::new(vec!["Warehouse".into(), "DataWarehouse".into()], vec![]).is_err());
    assert!(TypeFilters::new(vec!["".into()], vec![]).is_err());
    assert!(TypeFilters::new(vec!["x".repeat(51)], vec![]).is_err());
    assert!(TypeFilters::new(vec!["a\nb".into()], vec![]).is_err());
    assert!(TypeFilters::new(vec!["x' or Type eq 'Report".into()], vec![]).is_err());
    assert!(TypeFilters::new((0..500).map(|id| format!("Type{id}")).collect(), vec![]).is_ok());
    assert!(TypeFilters::new((0..501).map(|id| format!("Type{id}")).collect(), vec![]).is_err());
    assert_eq!(canonical_type("AISkill"), "DataAgent");
    assert_eq!(canonical_type("Data Agent"), "DataAgent");
    assert_eq!(canonical_type("DataWarehouse"), "Warehouse");
    assert_eq!(canonical_type("Future.native-Type"), "Future.native-Type");
}

#[test]
fn overlap_enriches_only_evidenced_workspace_and_keeps_source_order() {
    let without_workspace = Candidate::new(
        CandidateIdentity::item(TENANT, A).unwrap(),
        "Warehouse",
        None,
        CandidateMetadata {
            display_name: Some("name".into()),
            description: None,
        },
        Source::Catalog,
        1,
    )
    .unwrap();
    let result = federate(
        &request(SearchMode::Hybrid, 50),
        Some(observation(
            Source::Catalog,
            vec![
                without_workspace,
                candidate(Source::Catalog, 2, B, "Report"),
            ],
        )),
        Some(observation(
            Source::Relevance,
            vec![
                candidate(Source::Relevance, 1, A, "DataWarehouse"),
                candidate(Source::Relevance, 2, C, "Report"),
            ],
        )),
    )
    .unwrap();
    assert_eq!(
        result.data().hits[0].workspace_id(),
        Some(&Guid::new(WORKSPACE).unwrap())
    );
    assert_eq!(result.data().hits[0].provenance().len(), 2);
    assert_eq!(
        result.data().hits[1].identity(),
        &CandidateIdentity::item(TENANT, C).unwrap()
    );
    assert_eq!(
        result.data().hits[2].identity(),
        &CandidateIdentity::item(TENANT, B).unwrap()
    );
}

#[test]
fn same_source_duplicate_identity_retains_occurrences_without_duplicate_hits() {
    let result = federate(
        &request(SearchMode::Catalog, 50),
        Some(observation(
            Source::Catalog,
            vec![
                candidate(Source::Catalog, 1, A, "Report"),
                candidate(Source::Catalog, 2, A, "Report"),
            ],
        )),
        None,
    )
    .unwrap();
    assert_eq!(result.data().hits.len(), 1);
    assert_eq!(result.data().hits[0].provenance().len(), 2);
    assert_eq!(result.dropped_candidates(), 0);
}

#[test]
fn provider_response_allowances_partition_the_hybrid_budget() {
    let catalog = Provider::new(Source::Catalog, vec![]);
    let relevance = Provider::new(Source::Relevance, vec![]);
    let input = SearchRequest::Initial(
        InitialSearch::new(
            SearchMode::Hybrid,
            scope(),
            Some(" sales && \"revenue\" ? * 🦀 ".into()),
            TypeFilters::default(),
            SearchLimits::new(50, 50, 8193, 2).unwrap(),
        )
        .unwrap(),
    );
    let result =
        complete(SearchOperation::new(Some(&catalog), Some(&relevance)).execute(&input, pending()))
            .unwrap();
    assert_eq!(result.sources()[0].requests(), 1);
    assert_eq!(result.sources()[1].requests(), 1);
}

#[test]
fn one_schema_bundle_covers_both_request_variants_and_unprojected_evidence() {
    let schema = schemas();
    assert_eq!(schema.input["oneOf"].as_array().unwrap().len(), 2);
    assert_eq!(
        schema.input["oneOf"][0]["properties"]["mode"]["default"],
        "catalog"
    );
    assert_eq!(
        schema.input["oneOf"][1]["properties"]["kind"]["const"],
        "catalogResume"
    );
    assert_eq!(schema.input["oneOf"][1]["additionalProperties"], false);
    let properties = &schema.output["properties"];
    let details = &properties["meta"]["properties"]["sources"]["items"]["properties"]["details"]["properties"];
    assert!(details.get("status").is_some());
    assert!(details.get("continuation").is_some());
    assert!(details.get("responseBytes").is_some());
    assert!(properties["data"]["properties"].get("sources").is_none());
    assert_eq!(
        serde_json::to_value(&schema).unwrap()["input"],
        schema.input
    );
}

#[test]
fn alternation_is_stable_and_same_display_names_never_join() {
    let receipt = federate(
        &request(SearchMode::Hybrid, 50),
        Some(observation(
            Source::Catalog,
            vec![
                candidate(Source::Catalog, 1, A, "Report"),
                candidate(Source::Catalog, 2, B, "Report"),
            ],
        )),
        Some(observation(
            Source::Relevance,
            vec![
                candidate(Source::Relevance, 1, C, "Report"),
                candidate(Source::Relevance, 2, D, "Report"),
            ],
        )),
    )
    .unwrap();
    let ids: Vec<_> = receipt
        .data()
        .hits
        .iter()
        .map(|row| row.identity().clone())
        .collect();
    let expected: Vec<_> = [A, C, B, D]
        .into_iter()
        .map(|id| CandidateIdentity::item(TENANT, id).unwrap())
        .collect();
    assert_eq!(ids, expected);
}

#[test]
fn compatible_overlap_retains_both_original_metadata_and_ranks() {
    let receipt = federate(
        &request(SearchMode::Hybrid, 50),
        Some(observation(
            Source::Catalog,
            vec![candidate(Source::Catalog, 1, A, "DataAgent")],
        )),
        Some(observation(
            Source::Relevance,
            vec![candidate(Source::Relevance, 1, A, "AISkill")],
        )),
    )
    .unwrap();
    assert_eq!(receipt.data().hits.len(), 1);
    let row = &receipt.data().hits[0];
    assert_eq!(row.artifact_type(), "DataAgent");
    assert_eq!(row.provenance().len(), 2);
    assert_eq!(row.provenance()[1].native_type(), "AISkill");
    assert_ne!(
        row.provenance()[0].metadata().description,
        row.provenance()[1].metadata().description
    );
}

#[test]
fn conflicting_same_identity_type_or_workspace_is_an_explicit_error() {
    let wrong_owner = Candidate::new(
        CandidateIdentity::item(TENANT, A).unwrap(),
        "Report",
        Some(Guid::new(OTHER_WORKSPACE).unwrap()),
        CandidateMetadata {
            display_name: Some("name".into()),
            description: None,
        },
        Source::Relevance,
        1,
    )
    .unwrap();
    for conflict in [candidate(Source::Relevance, 1, A, "Notebook"), wrong_owner] {
        assert!(matches!(
            federate(
                &request(SearchMode::Hybrid, 50),
                Some(observation(
                    Source::Catalog,
                    vec![candidate(Source::Catalog, 1, A, "Report")]
                )),
                Some(observation(Source::Relevance, vec![conflict]))
            ),
            Err(SearchError::IdentityConflict)
        ));
    }
}

#[test]
fn unknown_types_and_missing_workspace_evidence_survive_without_invention() {
    let missing = Candidate::new(
        CandidateIdentity::item(TENANT, A).unwrap(),
        "FutureType",
        None,
        CandidateMetadata {
            display_name: Some("name".into()),
            description: None,
        },
        Source::Catalog,
        1,
    )
    .unwrap();
    let receipt = federate(
        &request(SearchMode::Catalog, 50),
        Some(observation(Source::Catalog, vec![missing])),
        None,
    )
    .unwrap();
    assert_eq!(receipt.data().hits[0].artifact_type(), "FutureType");
    assert!(receipt.data().hits[0].workspace_id().is_none());
}

#[test]
fn nullable_display_metadata_is_preserved_without_identity_or_name_substitution() {
    let row = Candidate::new(
        CandidateIdentity::item(TENANT, A).unwrap(),
        "Report",
        None,
        CandidateMetadata {
            display_name: None,
            description: None,
        },
        Source::Relevance,
        1,
    )
    .unwrap();
    let receipt = federate(
        &request(SearchMode::Relevance, 50),
        None,
        Some(observation(Source::Relevance, vec![row])),
    )
    .unwrap();
    let value = serde_json::to_value(receipt).unwrap();
    assert!(value["data"]["hits"][0]["provenance"][0]["metadata"]["displayName"].is_null());
    assert!(value["data"]["hits"][0]["workspaceId"].is_null());
}

#[test]
fn workspace_entries_are_a_distinct_identity_namespace() {
    let workspace = Candidate::new(
        CandidateIdentity::workspace(TENANT, A).unwrap(),
        "Workspace",
        None,
        CandidateMetadata {
            display_name: Some("same name".into()),
            description: None,
        },
        Source::Catalog,
        1,
    )
    .unwrap();
    let receipt = federate(
        &request(SearchMode::Hybrid, 50),
        Some(observation(Source::Catalog, vec![workspace])),
        Some(observation(
            Source::Relevance,
            vec![candidate(Source::Relevance, 1, A, "Report")],
        )),
    )
    .unwrap();
    assert_eq!(receipt.data().hits.len(), 2);
    assert!(matches!(
        receipt.data().hits[0].identity(),
        CandidateIdentity::Workspace { .. }
    ));
    assert!(
        Candidate::new(
            CandidateIdentity::item(TENANT, A).unwrap(),
            "Workspace",
            None,
            CandidateMetadata {
                display_name: Some("name".into()),
                description: None
            },
            Source::Catalog,
            1
        )
        .is_err()
    );
}

#[test]
fn completed_empty_is_usable_but_failed_sources_are_never_empty_success() {
    let empty = federate(
        &request(SearchMode::Catalog, 50),
        Some(observation(Source::Catalog, vec![])),
        None,
    )
    .unwrap();
    assert_eq!(empty.status(), SearchStatus::Succeeded);
    assert!(empty.data().hits.is_empty());
    let partial = federate(
        &request(SearchMode::Hybrid, 50),
        Some(observation(Source::Catalog, vec![])),
        Some(failed(Source::Relevance)),
    )
    .unwrap();
    assert_eq!(partial.status(), SearchStatus::Partial);
    assert_eq!(partial.sources()[1].status(), SourceStatus::Failed);
    assert!(matches!(
        federate(
            &request(SearchMode::Hybrid, 50),
            Some(failed(Source::Catalog)),
            Some(failed(Source::Relevance))
        ),
        Err(SearchError::AllSourcesFailed { .. })
    ));
}

#[test]
fn completed_top_k_succeeds_while_native_next_page_remains_partial() {
    let ranked = federate(
        &request(SearchMode::Relevance, 50),
        None,
        Some(observation(Source::Relevance, vec![])),
    )
    .unwrap();
    assert_eq!(ranked.status(), SearchStatus::Succeeded);
    assert_eq!(ranked.sources()[1].status(), SourceStatus::Completed);
    assert!(matches!(
        ranked.sources()[1].coverage(),
        Coverage::RankedTopK
    ));
    let explicitly_limited = SourceObservation::usable(
        Source::Relevance,
        vec![],
        Coverage::Limited {
            reason: LimitReason::ProviderLimit,
        },
        100,
        Some(200),
    )
    .unwrap();
    assert_eq!(
        federate(
            &request(SearchMode::Relevance, 50),
            None,
            Some(explicitly_limited)
        )
        .unwrap()
        .status(),
        SearchStatus::Partial
    );
    let catalog = SourceObservation::usable(
        Source::Catalog,
        vec![],
        Coverage::CatalogPage {
            continuation: Some("opaque".into()),
        },
        20,
        Some(200),
    )
    .unwrap();
    let receipt = federate(&request(SearchMode::Catalog, 50), Some(catalog), None).unwrap();
    assert_eq!(receipt.status(), SearchStatus::Partial);
    assert_eq!(receipt.continuation().unwrap().token(), "opaque");
}

#[test]
fn cap_drops_are_truthful_and_never_advertise_lossless_resume() {
    let catalog = SourceObservation::usable(
        Source::Catalog,
        vec![
            candidate(Source::Catalog, 1, A, "Report"),
            candidate(Source::Catalog, 2, B, "Report"),
        ],
        Coverage::CatalogPage {
            continuation: Some("native".into()),
        },
        100,
        Some(200),
    )
    .unwrap();
    let receipt = federate(
        &request(SearchMode::Catalog, 1),
        Some(catalog.clone()),
        None,
    )
    .unwrap();
    assert_eq!(receipt.dropped_candidates(), 1);
    assert!(receipt.continuation().is_none());
    assert_eq!(receipt.status(), SearchStatus::Partial);
    let hybrid = federate(
        &request(SearchMode::Hybrid, 1),
        Some(catalog),
        Some(observation(Source::Relevance, vec![])),
    )
    .unwrap();
    assert!(hybrid.continuation().is_none());
    assert_eq!(hybrid.dropped_candidates(), 1);
    assert!(matches!(
        hybrid.sources()[0].coverage(),
        Coverage::CatalogPage {
            continuation: Some(_)
        }
    ));
}

#[test]
fn projection_can_return_scalar_or_null_without_hiding_source_failures() {
    let receipt = federate(
        &request(SearchMode::Hybrid, 50),
        Some(observation(Source::Catalog, vec![])),
        Some(failed(Source::Relevance)),
    )
    .unwrap();
    let original = serde_json::to_value(&receipt).unwrap();
    for value in [json!(5), Value::Null] {
        let projected = receipt.project::<_, ()>(|_| Ok(value.clone())).unwrap();
        let mut encoded = serde_json::to_value(projected).unwrap();
        assert_eq!(encoded["data"], value);
        encoded["data"] = original["data"].clone();
        assert_eq!(encoded, original);
    }
    assert!(
        receipt
            .project::<Value, _>(|_| Err("projection failed"))
            .is_err()
    );
    assert_eq!(serde_json::to_value(receipt).unwrap(), original);
}

#[test]
fn malformed_observations_do_not_enter_the_merge() {
    assert!(
        SourceObservation::usable(
            Source::Catalog,
            vec![],
            Coverage::CatalogPage { continuation: None },
            0,
            Some(202)
        )
        .is_err()
    );
    assert!(
        SourceObservation::usable(Source::Catalog, vec![], Coverage::RankedTopK, 0, Some(200))
            .is_err()
    );
    assert!(
        SourceObservation::usable(
            Source::Relevance,
            vec![],
            Coverage::CatalogPage { continuation: None },
            0,
            Some(200)
        )
        .is_err()
    );
    assert!(
        SourceObservation::usable(
            Source::Catalog,
            vec![candidate(Source::Relevance, 1, A, "Report")],
            Coverage::Unknown,
            0,
            Some(200)
        )
        .is_err()
    );
    assert!(
        SourceObservation::usable(
            Source::Catalog,
            vec![
                candidate(Source::Catalog, 2, A, "Report"),
                candidate(Source::Catalog, 1, B, "Report")
            ],
            Coverage::Unknown,
            0,
            Some(200)
        )
        .is_err()
    );
}

#[test]
fn mismatched_tenant_is_a_failed_source_not_a_join_or_leak() {
    let wrong = Candidate::new(
        CandidateIdentity::item(OTHER_TENANT, A).unwrap(),
        "Report",
        None,
        CandidateMetadata {
            display_name: Some("private".into()),
            description: None,
        },
        Source::Relevance,
        1,
    )
    .unwrap();
    let receipt = federate(
        &request(SearchMode::Hybrid, 50),
        Some(observation(Source::Catalog, vec![])),
        Some(observation(Source::Relevance, vec![wrong])),
    )
    .unwrap();
    assert!(receipt.data().hits.is_empty());
    assert_eq!(receipt.sources()[1].status(), SourceStatus::Failed);
    assert!(!serde_json::to_string(&receipt).unwrap().contains("private"));
}

#[test]
fn missing_relevance_fails_before_catalog_dispatch_without_fallback() {
    let catalog = Provider::new(Source::Catalog, vec![]);
    let operation = SearchOperation::new(Some(&catalog), None);
    for mode in [SearchMode::Relevance, SearchMode::Hybrid] {
        assert!(matches!(
            complete(operation.execute(&request(mode, 50), pending())),
            Err(SearchError::MissingProvider {
                provider: Source::Relevance
            })
        ));
    }
    assert_eq!(catalog.calls(), 0);
    assert_eq!(catalog.inspections.load(Ordering::SeqCst), 0);
}

#[test]
fn unsupported_workspace_or_effective_type_precedes_both_sends() {
    let catalog = Provider::new(Source::Catalog, vec![]);
    let mut relevance = Provider::new(Source::Relevance, vec![]);
    relevance.capabilities.workspace_scope = false;
    relevance.capabilities.effective_types = Some(vec!["Report".into()]);
    let operation = SearchOperation::new(Some(&catalog), Some(&relevance));
    for (scope, filters) in [
        (
            SearchScope::workspace(TENANT, WORKSPACE).unwrap(),
            TypeFilters::default(),
        ),
        (
            scope(),
            TypeFilters::new(vec!["Notebook".into()], vec![]).unwrap(),
        ),
    ] {
        let input = SearchRequest::Initial(
            InitialSearch::new(
                SearchMode::Hybrid,
                scope,
                Some(" sales && \"revenue\" ? * 🦀 ".into()),
                filters,
                SearchLimits::default(),
            )
            .unwrap(),
        );
        assert!(matches!(
            complete(operation.execute(&input, pending())),
            Err(SearchError::UnsupportedCapability { .. })
        ));
    }
    assert_eq!((catalog.calls(), relevance.calls()), (0, 0));
}

#[test]
fn bound_provider_tenant_mismatch_is_rejected_before_both_sends() {
    let catalog = Provider::new(Source::Catalog, vec![]);
    let mut relevance = Provider::new(Source::Relevance, vec![]);
    relevance.capabilities.bound_tenant = Guid::new(OTHER_TENANT).unwrap();
    assert!(matches!(
        complete(
            SearchOperation::new(Some(&catalog), Some(&relevance))
                .execute(&request(SearchMode::Hybrid, 50), pending())
        ),
        Err(SearchError::UnsupportedCapability { .. })
    ));
    assert_eq!((catalog.calls(), relevance.calls()), (0, 0));
}

#[test]
fn catalog_only_never_consults_the_nonselected_relevance_provider() {
    let catalog = Provider::new(Source::Catalog, vec![]);
    let relevance = Provider::new(Source::Relevance, vec![]);
    let result = complete(
        SearchOperation::new(Some(&catalog), Some(&relevance))
            .execute(&request(SearchMode::Catalog, 50), pending()),
    )
    .unwrap();
    assert_eq!(catalog.calls(), 1);
    assert_eq!(relevance.calls(), 0);
    assert_eq!(relevance.inspections.load(Ordering::SeqCst), 0);
    assert_eq!(result.sources()[1].status(), SourceStatus::NotSelected);
}

#[test]
fn relevance_only_executes_without_catalog_but_remains_bounded_top_k() {
    let relevance = Provider::new(
        Source::Relevance,
        vec![candidate(Source::Relevance, 1, A, "Report")],
    );
    let result = complete(
        SearchOperation::new(None, Some(&relevance))
            .execute(&request(SearchMode::Relevance, 50), pending()),
    )
    .unwrap();
    assert_eq!(relevance.calls(), 1);
    assert_eq!(result.sources()[0].status(), SourceStatus::NotSelected);
    assert_eq!(result.status(), SearchStatus::Succeeded);
}

#[test]
fn returned_out_of_scope_or_filtered_rows_fail_the_entire_source() {
    let input = SearchRequest::Initial(
        InitialSearch::new(
            SearchMode::Hybrid,
            SearchScope::workspace(TENANT, WORKSPACE).unwrap(),
            Some("text".into()),
            TypeFilters::new(vec!["Notebook".into()], vec![]).unwrap(),
            SearchLimits::default(),
        )
        .unwrap(),
    );
    let result = federate(
        &input,
        Some(observation(
            Source::Catalog,
            vec![candidate(Source::Catalog, 1, A, "Report")],
        )),
        Some(observation(Source::Relevance, vec![])),
    )
    .unwrap();
    assert!(result.data().hits.is_empty());
    assert_eq!(result.sources()[0].status(), SourceStatus::Failed);
    assert_eq!(result.sources()[0].response_bytes(), Some(100));
    assert_eq!(result.sources()[0].http_status(), Some(200));
}

#[test]
fn source_failure_does_not_replay_or_hide_the_other_source() {
    let catalog = Provider::new(
        Source::Catalog,
        vec![candidate(Source::Catalog, 1, A, "Report")],
    );
    let mut relevance = Provider::new(Source::Relevance, vec![]);
    relevance.outcome = Err(ProviderError {
        code: ProviderErrorCode::Throttled,
        http_status: Some(429),
        retry_after_seconds: Some(10),
        response_bytes: Some(20),
    });
    let result = complete(
        SearchOperation::new(Some(&catalog), Some(&relevance))
            .execute(&request(SearchMode::Hybrid, 50), pending()),
    )
    .unwrap();
    assert_eq!(result.status(), SearchStatus::Partial);
    assert_eq!(result.data().hits.len(), 1);
    assert_eq!(
        result.sources()[1].error().unwrap().retry_after_seconds,
        Some(10)
    );
    assert_eq!((catalog.calls(), relevance.calls()), (1, 1));
}

#[test]
fn cancelled_or_unpolled_operation_has_zero_provider_activity() {
    let catalog = Provider::new(Source::Catalog, vec![]);
    let relevance = Provider::new(Source::Relevance, vec![]);
    let operation = SearchOperation::new(Some(&catalog), Some(&relevance));
    let input = request(SearchMode::Hybrid, 50);
    drop(operation.execute(&input, pending()));
    assert_eq!((catalog.calls(), relevance.calls()), (0, 0));
    assert!(matches!(
        complete(operation.execute(&input, async {})),
        Err(SearchError::Cancelled)
    ));
    assert_eq!((catalog.calls(), relevance.calls()), (0, 0));
    assert_eq!(catalog.inspections.load(Ordering::SeqCst), 0);
}

#[test]
fn midflight_cancellation_drops_both_provider_futures() {
    let mut catalog = Provider::new(Source::Catalog, vec![]);
    let mut relevance = Provider::new(Source::Relevance, vec![]);
    catalog.stay_pending = true;
    relevance.stay_pending = true;
    let operation = SearchOperation::new(Some(&catalog), Some(&relevance));
    let input = request(SearchMode::Hybrid, 50);
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = cancelled.clone();
    let cancel = futures_util::future::poll_fn(move |_| {
        if signal.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let mut work = pin!(operation.execute(&input, cancel));
    let waker = noop_waker();
    let mut context = Context::from_waker(&waker);
    assert!(work.as_mut().poll(&mut context).is_pending());
    assert_eq!((catalog.calls(), relevance.calls()), (1, 1));
    cancelled.store(true, Ordering::SeqCst);
    assert!(matches!(
        work.as_mut().poll(&mut context),
        Poll::Ready(Err(SearchError::Cancelled))
    ));
    assert_eq!(catalog.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(relevance.dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn aggregate_acquisition_and_unprojected_receipt_bytes_are_bounded() {
    let input = SearchRequest::Initial(
        InitialSearch::new(
            SearchMode::Catalog,
            scope(),
            Some("text".into()),
            TypeFilters::default(),
            SearchLimits::new(50, 50, 100, 1).unwrap(),
        )
        .unwrap(),
    );
    assert!(matches!(
        federate(&input, Some(observation(Source::Catalog, vec![])), None),
        Err(SearchError::BudgetExceeded)
    ));
    let mut failure = ProviderError::new(ProviderErrorCode::AccessDenied);
    failure.response_bytes = Some(2 * 1024 * 1024);
    assert!(matches!(
        federate(
            &request(SearchMode::Catalog, 50),
            Some(SourceObservation::failed(Source::Catalog, failure).unwrap()),
            None
        ),
        Err(SearchError::BudgetExceeded)
    ));
}

#[test]
fn catalog_resume_keeps_scope_and_opaque_bytes_without_fresh_criteria() {
    let cursor = CatalogCursor::from_native(scope(), " opaque+/=? token ".into()).unwrap();
    assert!(!format!("{cursor:?}").contains("opaque+/"));
    let input = SearchRequest::CatalogResume(CatalogResume::new(cursor, SearchLimits::default()));
    assert_eq!(input.mode(), SearchMode::Catalog);
    assert_eq!(input.scope(), &scope());
    let encoded = serde_json::to_value(&input).unwrap();
    assert_eq!(encoded["cursor"]["token"], " opaque+/=? token ");
    assert!(SearchRequest::from_value(encoded).is_ok());
    let catalog = Provider::new(Source::Catalog, vec![]);
    assert!(
        complete(SearchOperation::new(Some(&catalog), None).execute(&input, pending())).is_ok()
    );
    assert_eq!(catalog.calls(), 1);
}

#[test]
fn resumed_large_original_page_capped_locally_never_offers_lossless_next_cursor() {
    let input = SearchRequest::CatalogResume(CatalogResume::new(
        CatalogCursor::from_native(scope(), "original-page-size-1000".into()).unwrap(),
        SearchLimits::default(),
    ));
    let rows = (0..1000)
        .map(|index| {
            candidate(
                Source::Catalog,
                index + 1,
                &format!("{index:08x}-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
                "Report",
            )
        })
        .collect();
    let page = SourceObservation::usable(
        Source::Catalog,
        rows,
        Coverage::CatalogPage {
            continuation: Some("next-native-page".into()),
        },
        500_000,
        Some(200),
    )
    .unwrap();
    let receipt = federate(&input, Some(page), None).unwrap();
    assert_eq!(receipt.data().hits.len(), 50);
    assert_eq!(receipt.dropped_candidates(), 950);
    assert_eq!(receipt.status(), SearchStatus::Partial);
    assert!(receipt.continuation().is_none());
    assert!(matches!(
        receipt.sources()[0].coverage(),
        Coverage::CatalogPage {
            continuation: Some(_)
        }
    ));
}
