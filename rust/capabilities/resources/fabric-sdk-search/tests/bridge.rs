// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Approval and shared-receipt contracts with caller-owned synthetic providers.

#![deny(unnameable_test_items)]

use fabric_sdk_search::*;
use fabric_v2_contracts::{
    ApprovalPolicy, Coverage as CoreCoverage, Effect, Outcome, Precondition, PreparedOperation,
    TargetKind,
};
use serde_json::{Value, json};
use std::{
    future::{Future, pending},
    pin::pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

const TENANT: &str = "11111111-1111-4111-8111-111111111111";
const OTHER_TENANT: &str = "22222222-2222-4222-8222-222222222222";
const WORKSPACE: &str = "33333333-3333-4333-8333-333333333333";
const A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

struct TestPolicy;

impl ApprovalPolicy for TestPolicy {
    type Error = &'static str;

    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
        if operation.operation_id() == SEARCH_OPERATION_ID
            && operation.effects().contains(&Effect::RemoteRead)
        {
            Ok(())
        } else {
            Err("caller refused")
        }
    }
}

struct Stub {
    capabilities: ProviderCapabilities,
    calls: AtomicUsize,
    result: Result<SourceObservation, ProviderError>,
}

impl SearchProvider for Stub {
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn search<'a>(&'a self, _: ProviderRequest<'a>) -> SearchFuture<'a, SourceObservation> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(self.result.clone()))
    }
}

fn stub(source: Source) -> Stub {
    Stub {
        capabilities: ProviderCapabilities {
            source,
            bound_tenant: Guid::new(TENANT).unwrap(),
            tenant_scope: true,
            workspace_scope: source == Source::Catalog,
            include_types: true,
            exclude_types: true,
            effective_types: None,
            catalog_resume: source == Source::Catalog,
        },
        calls: AtomicUsize::new(0),
        result: Ok(SourceObservation::usable(
            source,
            vec![],
            if source == Source::Catalog {
                Coverage::CatalogPage { continuation: None }
            } else {
                Coverage::RankedTopK
            },
            100,
            Some(200),
        )
        .unwrap()),
    }
}

fn request(mode: SearchMode) -> SearchRequest {
    SearchRequest::Initial(
        InitialSearch::new(
            mode,
            SearchScope::tenant(TENANT).unwrap(),
            Some("exact original text".into()),
            TypeFilters::default(),
            SearchLimits::default(),
        )
        .unwrap(),
    )
}

fn complete<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("synthetic future must be immediately ready"),
    }
}

#[test]
fn preparation_binds_real_tenant_and_exact_serialized_input_without_default_approval() {
    let input = request(SearchMode::Catalog);
    let prepared = prepare_search(&input, "/").unwrap();
    assert_eq!(prepared.operation_id(), "catalog.search");
    assert_eq!(prepared.input(), &serde_json::to_value(&input).unwrap());
    assert_eq!(prepared.effects(), &[Effect::RemoteRead]);
    assert_eq!(
        prepared.target().identity().kind(),
        TargetKind::TenantCollection
    );
    assert_eq!(prepared.target().identity().tenant_id(), Some(TENANT));
    assert_eq!(prepared.target().identity().workspace_id(), None);
    assert_eq!(prepared.target().identity().item_id(), None);
    let descriptor = search_descriptor().unwrap();
    assert_eq!(descriptor.id(), SEARCH_OPERATION_ID);
    assert_eq!(descriptor.input_schema(), &schemas().input);
    assert_eq!(descriptor.output_schema(), &schemas().output);
    assert!(prepare_search(&input, "/workspaces/anything").is_err());
}

#[test]
fn workspace_preparation_uses_real_collection_and_requires_exact_canonical_path() {
    let input = SearchRequest::Initial(
        InitialSearch::new(
            SearchMode::Catalog,
            SearchScope::workspace(TENANT, WORKSPACE).unwrap(),
            None,
            TypeFilters::default(),
            SearchLimits::default(),
        )
        .unwrap(),
    );
    let prepared = prepare_search(&input, &format!("/workspaces/{WORKSPACE}")).unwrap();
    assert_eq!(prepared.target().identity().workspace_id(), Some(WORKSPACE));
    assert_eq!(
        prepared.target().identity().kind(),
        TargetKind::WorkspaceCollection
    );
    assert!(prepare_search(&input, "/").is_err());
}

#[test]
fn mismatched_input_or_tenant_approval_never_reaches_a_provider() {
    let provider = stub(Source::Catalog);
    let operation = SearchOperation::new(Some(&provider), None);
    let input = request(SearchMode::Catalog);
    let prepared = prepare_search(&input, "/").unwrap();
    let approved = prepared.authorize(&mut TestPolicy).unwrap();
    let changed = SearchRequest::Initial(
        InitialSearch::new(
            SearchMode::Catalog,
            SearchScope::tenant(OTHER_TENANT).unwrap(),
            Some("changed".into()),
            TypeFilters::default(),
            SearchLimits::default(),
        )
        .unwrap(),
    );
    assert!(matches!(
        complete(execute_approved_search(
            &approved,
            &changed,
            &operation,
            pending()
        )),
        Err(SearchBridgeError::ApprovalMismatch)
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn model_effect_cannot_be_removed_or_added_implicitly() {
    let provider = stub(Source::Relevance);
    let operation = SearchOperation::new(None, Some(&provider));
    let input = request(SearchMode::Relevance);
    let prepared = prepare_search(&input, "/").unwrap();
    assert_eq!(
        prepared.effects(),
        &[Effect::RemoteRead, Effect::ModelExecution]
    );
    let downgraded = prepared.with_optional_effects(vec![]).unwrap();
    let approved = downgraded.authorize(&mut TestPolicy).unwrap();
    assert!(matches!(
        complete(execute_approved_search(
            &approved,
            &input,
            &operation,
            pending()
        )),
        Err(SearchBridgeError::ApprovalMismatch)
    ));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn canonical_receipt_uses_shared_metadata_and_unprojected_typed_source_details() {
    let provider = stub(Source::Catalog);
    let operation = SearchOperation::new(Some(&provider), None);
    let input = request(SearchMode::Catalog);
    let prepared = prepare_search(&input, "/").unwrap();
    let approved = prepared.authorize(&mut TestPolicy).unwrap();
    let receipt = complete(execute_approved_search(
        &approved,
        &input,
        &operation,
        pending(),
    ))
    .unwrap();
    assert!(matches!(receipt.outcome(), Outcome::Succeeded { .. }));
    let value = serde_json::to_value(&receipt).unwrap();
    assert_eq!(value["meta"]["operationId"], SEARCH_OPERATION_ID);
    assert_eq!(
        value["meta"]["sources"][0]["details"]["status"],
        "completed"
    );
    assert_eq!(value["meta"]["sources"][0]["details"]["responseBytes"], 100);
    assert_eq!(
        value["meta"]["sources"][0]["details"]["scope"]["tenantId"],
        TENANT
    );
    assert!(value["data"].get("sources").is_none());
    assert!(value.get("sources").is_none());
    assert_eq!(
        schemas().output["properties"]["meta"]["properties"]["operationId"]["const"],
        SEARCH_OPERATION_ID
    );
}

#[test]
fn completed_ranked_source_succeeds_with_unknown_inventory_and_unprojected_evidence() {
    let catalog = stub(Source::Catalog);
    let relevance = stub(Source::Relevance);
    let operation = SearchOperation::new(Some(&catalog), Some(&relevance));
    let input = request(SearchMode::Hybrid);
    let prepared = prepare_search(&input, "/").unwrap();
    let approved = prepared.authorize(&mut TestPolicy).unwrap();
    let receipt = complete(execute_approved_search(
        &approved,
        &input,
        &operation,
        pending(),
    ))
    .unwrap();
    assert!(matches!(receipt.outcome(), Outcome::Succeeded { .. }));
    assert_eq!(receipt.evidence().coverage, CoreCoverage::Unknown);
    assert_eq!(receipt.sources()[1].coverage, CoreCoverage::Unknown);
    let original = serde_json::to_value(&receipt).unwrap();
    let mut projected = original.clone();
    projected["data"] = Value::Null;
    assert_eq!(projected["meta"], original["meta"]);
    assert_eq!(projected["status"], "succeeded");
    assert!(projected.get("error").is_none());
    assert_eq!(
        projected["meta"]["sources"][1]["details"]["coverage"]["kind"],
        "rankedTopK"
    );
}

#[test]
fn all_failed_sources_return_failed_shared_receipt_without_fabricated_empty_data() {
    let mut catalog = stub(Source::Catalog);
    let mut relevance = stub(Source::Relevance);
    catalog.result = Err(ProviderError::new(ProviderErrorCode::AccessDenied));
    relevance.result = Err(ProviderError::new(ProviderErrorCode::TimedOut));
    let operation = SearchOperation::new(Some(&catalog), Some(&relevance));
    let input = request(SearchMode::Hybrid);
    let prepared = prepare_search(&input, "/").unwrap();
    let approved = prepared.authorize(&mut TestPolicy).unwrap();
    let receipt = complete(execute_approved_search(
        &approved,
        &input,
        &operation,
        pending(),
    ))
    .unwrap();
    assert!(matches!(receipt.outcome(), Outcome::Failed { .. }));
    let value = serde_json::to_value(receipt).unwrap();
    assert!(value.get("data").is_none());
    assert_eq!(value["meta"]["sources"][0]["error"]["code"], "AccessDenied");
    assert_eq!(value["meta"]["sources"][1]["error"]["code"], "TimedOut");
    assert!(value["meta"]["sources"][0]["details"]["responseBytes"].is_null());
}

#[test]
fn local_output_effect_requires_exact_explicit_guard_and_other_guards_are_rejected() {
    let provider = stub(Source::Catalog);
    let operation = SearchOperation::new(Some(&provider), None);
    let input = request(SearchMode::Catalog);
    let prepared = prepare_search(&input, "/")
        .unwrap()
        .with_optional_effects(vec![Effect::LocalWrite])
        .unwrap();
    let approved = prepared.authorize(&mut TestPolicy).unwrap();
    assert!(matches!(
        complete(execute_approved_search(
            &approved,
            &input,
            &operation,
            pending()
        )),
        Err(SearchBridgeError::ApprovalMismatch)
    ));
    let prepared = prepared
        .with_preconditions(vec![Precondition::LocalOutput {
            path: "C:\\output\\search.json".into(),
        }])
        .unwrap();
    let approved = prepared.authorize(&mut TestPolicy).unwrap();
    let receipt = complete(execute_approved_search(
        &approved,
        &input,
        &operation,
        pending(),
    ))
    .unwrap();
    assert_eq!(receipt.preconditions(), prepared.preconditions());
    let unrelated = prepare_search(&input, "/")
        .unwrap()
        .with_preconditions(vec![Precondition::ExistingOnly])
        .unwrap();
    let approved = unrelated.authorize(&mut TestPolicy).unwrap();
    assert!(matches!(
        complete(execute_approved_search(
            &approved,
            &input,
            &operation,
            pending()
        )),
        Err(SearchBridgeError::ApprovalMismatch)
    ));
}

#[test]
fn capped_catalog_preserves_raw_cursor_evidence_but_canonical_resume_is_null() {
    let mut catalog = stub(Source::Catalog);
    let rows = [A, B]
        .into_iter()
        .enumerate()
        .map(|(index, id)| {
            Candidate::new(
                CandidateIdentity::item(TENANT, id).unwrap(),
                "Report",
                None,
                CandidateMetadata {
                    display_name: None,
                    description: None,
                },
                Source::Catalog,
                index + 1,
            )
            .unwrap()
        })
        .collect();
    catalog.result = Ok(SourceObservation::usable(
        Source::Catalog,
        rows,
        Coverage::CatalogPage {
            continuation: Some("raw-next".into()),
        },
        500,
        Some(200),
    )
    .unwrap());
    let input = SearchRequest::Initial(
        InitialSearch::new(
            SearchMode::Catalog,
            SearchScope::tenant(TENANT).unwrap(),
            Some("text".into()),
            TypeFilters::default(),
            SearchLimits::new(1, 50, 1024 * 1024, 1).unwrap(),
        )
        .unwrap(),
    );
    let prepared = prepare_search(&input, "/").unwrap();
    let approved = prepared.authorize(&mut TestPolicy).unwrap();
    let operation = SearchOperation::new(Some(&catalog), None);
    let receipt = complete(execute_approved_search(
        &approved,
        &input,
        &operation,
        pending(),
    ))
    .unwrap();
    let value = serde_json::to_value(receipt).unwrap();
    assert!(value["meta"]["evidence"]["coverage"]["continuation"].is_null());
    assert!(value["meta"]["sources"][0]["details"]["continuation"].is_null());
    assert_eq!(
        value["meta"]["sources"][0]["details"]["droppedCandidates"],
        1
    );
    assert_eq!(
        value["meta"]["sources"][0]["details"]["coverage"]["continuation"],
        "raw-next"
    );
}

#[test]
fn file_output_preserves_required_model_effect_and_exact_guard() {
    let catalog = stub(Source::Catalog);
    let relevance = stub(Source::Relevance);
    let operation = SearchOperation::new(Some(&catalog), Some(&relevance));
    let input = request(SearchMode::Hybrid);
    let guard = Precondition::LocalOutput {
        path: "C:\\output\\exact-search.json".into(),
    };
    let missing_model = prepare_search(&input, "/")
        .unwrap()
        .with_optional_effects(vec![Effect::LocalWrite])
        .unwrap()
        .with_preconditions(vec![guard.clone()])
        .unwrap();
    let approved = missing_model.authorize(&mut TestPolicy).unwrap();
    assert!(matches!(
        complete(execute_approved_search(
            &approved,
            &input,
            &operation,
            pending()
        )),
        Err(SearchBridgeError::ApprovalMismatch)
    ));
    assert_eq!(catalog.calls.load(Ordering::SeqCst), 0);
    assert_eq!(relevance.calls.load(Ordering::SeqCst), 0);

    let prepared = prepare_search(&input, "/")
        .unwrap()
        .with_optional_effects(vec![Effect::ModelExecution, Effect::LocalWrite])
        .unwrap()
        .with_preconditions(vec![guard.clone()])
        .unwrap();
    assert_eq!(prepared.input(), &serde_json::to_value(&input).unwrap());
    assert!(
        prepared
            .clone()
            .with_optional_effects(vec![Effect::ModelExecution])
            .is_err()
    );
    let approved = prepared.authorize(&mut TestPolicy).unwrap();
    let receipt = complete(execute_approved_search(
        &approved,
        &input,
        &operation,
        pending(),
    ))
    .unwrap();
    assert_eq!(receipt.preconditions(), &[guard]);
    let value = serde_json::to_value(receipt).unwrap();
    assert_eq!(
        value["meta"]["declaredEffects"],
        json!(["remoteRead", "modelExecution", "localWrite"])
    );
    assert_eq!(
        value["meta"]["preconditions"][0]["path"],
        "C:\\output\\exact-search.json"
    );
}

#[test]
fn federation_conflict_retains_original_source_evidence_in_failed_shared_receipt() {
    let mut catalog = stub(Source::Catalog);
    let mut relevance = stub(Source::Relevance);
    for (provider, source, kind) in [
        (&mut catalog, Source::Catalog, "Report"),
        (&mut relevance, Source::Relevance, "SemanticModel"),
    ] {
        provider.result = Ok(SourceObservation::usable(
            source,
            vec![
                Candidate::new(
                    CandidateIdentity::item(TENANT, A).unwrap(),
                    kind,
                    None,
                    CandidateMetadata {
                        display_name: None,
                        description: None,
                    },
                    source,
                    1,
                )
                .unwrap(),
            ],
            if source == Source::Catalog {
                Coverage::CatalogPage { continuation: None }
            } else {
                Coverage::RankedTopK
            },
            200,
            Some(200),
        )
        .unwrap());
    }
    let input = request(SearchMode::Hybrid);
    let prepared = prepare_search(&input, "/").unwrap();
    let approved = prepared.authorize(&mut TestPolicy).unwrap();
    let operation = SearchOperation::new(Some(&catalog), Some(&relevance));
    let receipt = complete(execute_approved_search(
        &approved,
        &input,
        &operation,
        pending(),
    ))
    .unwrap();
    let value = serde_json::to_value(receipt).unwrap();
    assert_eq!(value["status"], "failed");
    assert_eq!(value["error"]["code"], "IdentityConflict");
    assert_eq!(value["meta"]["sources"].as_array().unwrap().len(), 2);
}
