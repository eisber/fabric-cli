// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Adapter contracts over synthetic shared executors, never ambient credentials.

#![deny(unnameable_test_items)]

use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_sdk_api::{
    ApiClient, ApiError, Audience, CredentialSelection, ExecutionError, Executor, Request,
    Response, TokenCredential,
};
use fabric_sdk_catalog::CatalogClient;
use fabric_sdk_relevance::{
    ArtifactType, DiscoveryExecutor, DiscoveryFuture, DiscoveryRequest, RelevanceClient,
};
use fabric_sdk_search::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, poll_fn},
    num::NonZeroUsize,
    pin::{Pin, pin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

const TENANT: &str = "11111111-1111-4111-8111-111111111111";
const WORKSPACE: &str = "22222222-2222-4222-8222-222222222222";
const ITEM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OTHER_ITEM: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const QUERY_ID: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const TEXT: &str = " sales && \"revenue\" ? * 🦀 ";

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
}

impl TokenCredential for Credential {
    fn get_token<'life0, 'life1, 'life2, 'life3, 'async_trait>(
        &'life0 self,
        scopes: &'life1 [&'life2 str],
        _: Option<TokenRequestOptions<'life3>>,
    ) -> Pin<Box<dyn Future<Output = azure_core::Result<AccessToken>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        'life3: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            assert_eq!(scopes, &["https://api.fabric.microsoft.com/.default"]);
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(AccessToken::new(
                "synthetic-offline-search-token",
                OffsetDateTime::UNIX_EPOCH,
            ))
        })
    }
}

#[derive(Clone, Debug)]
struct Recorded {
    body: Value,
    limit: usize,
}

struct Guard<'a>(&'a AtomicUsize);

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct CatalogExecutor {
    replies: Mutex<VecDeque<Result<Response, ExecutionError>>>,
    recorded: Mutex<Vec<Recorded>>,
    stay_pending: bool,
    dropped: AtomicUsize,
}

impl Executor for CatalogExecutor {
    fn execute<'a>(
        &'a self,
        request: &'a Request,
        _: &'a AccessToken,
        limit: NonZeroUsize,
    ) -> Pin<Box<dyn Future<Output = Result<Response, ExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(
                request.url().as_str(),
                "https://api.fabric.microsoft.com/v1/catalog/search"
            );
            self.recorded.lock().unwrap().push(Recorded {
                body: serde_json::from_slice(request.body()).unwrap(),
                limit: limit.get(),
            });
            let _guard = Guard(&self.dropped);
            if self.stay_pending {
                pending::<()>().await;
            }
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("extra Catalog call")
        })
    }
}

#[derive(Default)]
struct RelevanceExecutor {
    replies: Mutex<VecDeque<Result<Response, ApiError>>>,
    recorded: Mutex<Vec<(DiscoveryRequest, usize)>>,
    stay_pending: bool,
    dropped: AtomicUsize,
}

impl DiscoveryExecutor for RelevanceExecutor {
    fn discover<'a>(
        &'a self,
        request: &'a DiscoveryRequest,
        limit: NonZeroUsize,
    ) -> DiscoveryFuture<'a> {
        Box::pin(async move {
            self.recorded
                .lock()
                .unwrap()
                .push((request.clone(), limit.get()));
            let _guard = Guard(&self.dropped);
            if self.stay_pending {
                pending::<()>().await;
            }
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("extra RE call")
        })
    }
}

fn complete<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("synthetic future must complete without a runtime"),
    }
}

fn response(status: u16, body: Value) -> Response {
    Response {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

fn tenant() -> Guid {
    Guid::new(TENANT).unwrap()
}

fn scope() -> SearchScope {
    SearchScope::tenant(TENANT).unwrap()
}

fn request(mode: SearchMode) -> SearchRequest {
    initial(mode, scope(), Some(TEXT), TypeFilters::default())
}

fn initial(
    mode: SearchMode,
    scope: SearchScope,
    text: Option<&str>,
    filters: TypeFilters,
) -> SearchRequest {
    SearchRequest::Initial(
        InitialSearch::new(
            mode,
            scope,
            text.map(str::to_owned),
            filters,
            SearchLimits::default(),
        )
        .unwrap(),
    )
}

fn catalog(
    reply: Result<Response, ExecutionError>,
) -> (CatalogProvider, Arc<CatalogExecutor>, Arc<Credential>) {
    let executor = Arc::new(CatalogExecutor {
        replies: Mutex::new(VecDeque::from([reply])),
        ..Default::default()
    });
    let credential = Arc::new(Credential::default());
    let api = ApiClient::new(
        credential.clone(),
        CredentialSelection::new("synthetic-account", TENANT, Audience::Fabric).unwrap(),
        executor.clone(),
    );
    (
        CatalogProvider::new(CatalogClient::new(api), tenant()),
        executor,
        credential,
    )
}

fn relevance(reply: Result<Response, ApiError>) -> (RelevanceProvider, Arc<RelevanceExecutor>) {
    let executor = Arc::new(RelevanceExecutor {
        replies: Mutex::new(VecDeque::from([reply])),
        ..Default::default()
    });
    (
        RelevanceProvider::new(RelevanceClient::new(executor.clone()), tenant()),
        executor,
    )
}

fn item(id: &str, kind: &str) -> Value {
    json!({
        "id":id,"type":kind,"catalogEntryType":"FabricItem",
        "displayName":null,"description":"actual source description",
        "hierarchy":{"workspace":{"id":WORKSPACE}}
    })
}

fn artifact(id: Option<&str>, kind: &str) -> Value {
    json!({
        "documentType":kind,"objectId":id,"workspaceId":WORKSPACE,
        "displayName":null,"workspaceName":null,"url":null,
        "score":0.5,"searchScore":null,"matchedProperties":null,
        "matchedSignals":null,"relevance":3
    })
}

fn result(artifacts: Vec<Value>) -> Value {
    json!({"queryId":QUERY_ID,"artifacts":artifacts})
}

fn failed_sources(error: SearchError) -> Vec<SourceEvidence> {
    match error {
        SearchError::AllSourcesFailed { sources } => sources,
        other => panic!("unexpected failure: {other:?}"),
    }
}

#[test]
fn catalog_maps_exact_body_union_and_actual_measurements_without_hydration() {
    let payload = json!({"value":[
        item(ITEM,"FutureType"),
        {"id":WORKSPACE,"type":"Workspace","catalogEntryType":"Workspace","displayName":"Workspace"}
    ]});
    let reply = response(200, payload);
    let bytes = reply.body.len();
    let (provider, executor, credential) = catalog(Ok(reply));
    let receipt = complete(
        SearchOperation::new(Some(&provider), None)
            .execute(&request(SearchMode::Catalog), pending()),
    )
    .unwrap();
    assert_eq!(receipt.data().hits[0].artifact_type(), "FutureType");
    assert_eq!(
        receipt.data().hits[0].provenance()[0]
            .metadata()
            .display_name,
        None
    );
    assert!(matches!(
        receipt.data().hits[1].identity(),
        CandidateIdentity::Workspace { .. }
    ));
    assert_eq!(receipt.sources()[0].response_bytes(), Some(bytes));
    assert_eq!(receipt.sources()[0].http_status(), Some(200));
    assert_eq!(receipt.sources()[1].response_bytes(), Some(0));
    let recorded = executor.recorded.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].body, json!({"search":TEXT,"pageSize":50}));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn catalog_workspace_and_type_filters_are_native_and_filter_only_is_allowed() {
    let (provider, executor, _) = catalog(Ok(response(200, json!({"value":[]}))));
    let input = initial(
        SearchMode::Catalog,
        SearchScope::workspace(TENANT, WORKSPACE).unwrap(),
        None,
        TypeFilters::new(vec!["Report".into()], vec!["Notebook".into()]).unwrap(),
    );
    complete(SearchOperation::new(Some(&provider), None).execute(&input, pending())).unwrap();
    let recorded = executor.recorded.lock().unwrap();
    assert!(recorded[0].body.get("search").is_none());
    assert_eq!(
        recorded[0].body["filter"],
        format!("(Type eq 'Report') and (Type ne 'Notebook') and (WorkspaceId eq '{WORKSPACE}')")
    );
}

#[test]
fn catalog_resume_sends_token_only_and_capped_page_is_not_resumable() {
    let (provider, executor, _) = catalog(Ok(response(
        200,
        json!({"value":[item(ITEM,"Report"),item(OTHER_ITEM,"Report")],
               "continuationToken":"next"}),
    )));
    let input = SearchRequest::CatalogResume(CatalogResume::new(
        CatalogCursor::from_native(scope(), " opaque-original ".into()).unwrap(),
        SearchLimits::new(1, 1, 1024 * 1024, 1).unwrap(),
    ));
    let receipt =
        complete(SearchOperation::new(Some(&provider), None).execute(&input, pending())).unwrap();
    assert_eq!(
        executor.recorded.lock().unwrap()[0].body,
        json!({"continuationToken":" opaque-original "})
    );
    assert_eq!(receipt.data().hits.len(), 1);
    assert_eq!(receipt.dropped_candidates(), 1);
    assert!(receipt.continuation().is_none());
}

#[test]
fn catalog_malformed_identity_discriminator_hierarchy_or_metadata_fails_the_source() {
    let mut cases = vec![item("not-a-guid", "Report")];
    for (field, value) in [
        ("id", Value::Null),
        ("type", Value::Null),
        ("catalogEntryType", json!("UnknownEntry")),
        ("hierarchy", json!({})),
        ("displayName", json!(5)),
    ] {
        let mut row = item(ITEM, "Report");
        row[field] = value;
        cases.push(row);
    }
    let mut row = item(ITEM, "Report");
    row["hierarchy"]["workspace"]["id"] = json!("invalid");
    cases.push(row);
    for row in cases {
        let reply = response(200, json!({"value":[row]}));
        let bytes = reply.body.len();
        let (provider, _, _) = catalog(Ok(reply));
        let sources = failed_sources(
            complete(
                SearchOperation::new(Some(&provider), None)
                    .execute(&request(SearchMode::Catalog), pending()),
            )
            .unwrap_err(),
        );
        assert_eq!(
            sources[0].error().unwrap().code,
            ProviderErrorCode::InvalidResponse
        );
        assert_eq!(sources[0].http_status(), Some(200));
        assert_eq!(sources[0].response_bytes(), Some(bytes));
    }
}

#[test]
fn catalog_denial_and_throttling_preserve_actual_safe_evidence() {
    for (status, code) in [
        (401, ProviderErrorCode::Unauthenticated),
        (403, ProviderErrorCode::AccessDenied),
        (429, ProviderErrorCode::Throttled),
    ] {
        let mut reply = response(
            status,
            json!({"errorCode":"Denied","message":"private-body"}),
        );
        reply.headers.insert("Retry-After".into(), "17".into());
        let bytes = reply.body.len();
        let (provider, executor, _) = catalog(Ok(reply));
        let sources = failed_sources(
            complete(
                SearchOperation::new(Some(&provider), None)
                    .execute(&request(SearchMode::Catalog), pending()),
            )
            .unwrap_err(),
        );
        let error = sources[0].error().unwrap();
        assert_eq!(error.code, code);
        assert_eq!(error.http_status, Some(status));
        assert_eq!(error.response_bytes, Some(bytes));
        assert_eq!(error.retry_after_seconds, Some(17));
        assert!(
            !serde_json::to_string(&sources)
                .unwrap()
                .contains("private-body")
        );
        assert_eq!(executor.recorded.lock().unwrap().len(), 1);
    }
}

#[test]
fn catalog_transport_failure_does_not_invent_zero_bytes_or_http_status() {
    let (provider, _, _) = catalog(Err(ExecutionError::TimedOut));
    let sources = failed_sources(
        complete(
            SearchOperation::new(Some(&provider), None)
                .execute(&request(SearchMode::Catalog), pending()),
        )
        .unwrap_err(),
    );
    let error = sources[0].error().unwrap();
    assert_eq!(error.code, ProviderErrorCode::TimedOut);
    assert_eq!(sources[0].response_bytes(), None);
    assert_eq!(sources[0].http_status(), None);
    assert!(serde_json::to_value(&sources).unwrap()[0]["responseBytes"].is_null());
}

#[test]
fn relevance_default_types_text_rank_and_actual_measurements_are_preserved() {
    let mut first = artifact(Some(ITEM), "Report");
    first["score"] = json!(0.1);
    first["workspaceId"] = Value::Null;
    let mut second = artifact(Some(OTHER_ITEM), "SemanticModel");
    second["score"] = json!(1000.0);
    let reply = response(200, result(vec![first, second]));
    let bytes = reply.body.len();
    let (provider, executor) = relevance(Ok(reply));
    let receipt = complete(
        SearchOperation::new(None, Some(&provider))
            .execute(&request(SearchMode::Relevance), pending()),
    )
    .unwrap();
    assert_eq!(receipt.status(), SearchStatus::Succeeded);
    assert_eq!(
        receipt.data().hits[0].identity(),
        &CandidateIdentity::item(TENANT, ITEM).unwrap()
    );
    assert!(receipt.data().hits[0].workspace_id().is_none());
    assert!(
        receipt.data().hits[0].provenance()[0]
            .metadata()
            .display_name
            .is_none()
    );
    assert_eq!(receipt.sources()[1].response_bytes(), Some(bytes));
    assert_eq!(receipt.sources()[1].http_status(), Some(200));
    let recorded = executor.recorded.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0.text(), TEXT);
    assert_eq!(
        recorded[0].0.query(),
        vec![("searchText".into(), TEXT.into())]
    );
    assert_eq!(
        recorded[0].0.body(),
        json!({"discoveryScope":{"queryScope":"Global","allowedArtifactTypes":[3,2]},"top":50,"userIntent":"search-intent"})
    );
}

#[test]
fn relevance_exclusion_is_computed_over_the_declared_finite_set() {
    let (provider, executor) = relevance(Ok(response(
        200,
        result(vec![artifact(Some(ITEM), "SemanticModel")]),
    )));
    let input = initial(
        SearchMode::Relevance,
        scope(),
        Some(TEXT),
        TypeFilters::new(vec![], vec!["Report".into()]).unwrap(),
    );
    complete(SearchOperation::new(None, Some(&provider)).execute(&input, pending())).unwrap();
    assert_eq!(
        executor.recorded.lock().unwrap()[0].0.artifact_types(),
        &[ArtifactType::SemanticModel]
    );
}

#[test]
fn empty_effective_type_set_and_native_text_limit_fail_before_both_dispatches() {
    for (text, filters) in [
        (
            TEXT.to_owned(),
            TypeFilters::new(vec![], vec!["Report".into(), "SemanticModel".into()]).unwrap(),
        ),
        ("a".repeat(501), TypeFilters::default()),
        (format!("{}a", "🦀".repeat(250)), TypeFilters::default()),
        ("🦀".repeat(251), TypeFilters::default()),
    ] {
        let (catalog, catalog_executor, credential) =
            catalog(Ok(response(200, json!({"value":[]}))));
        let (relevance, re_executor) = relevance(Ok(response(200, result(vec![]))));
        let input = initial(SearchMode::Hybrid, scope(), Some(&text), filters);
        assert!(matches!(
            complete(
                SearchOperation::new(Some(&catalog), Some(&relevance)).execute(&input, pending())
            ),
            Err(SearchError::ProviderInput { .. })
        ));
        assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
        assert!(catalog_executor.recorded.lock().unwrap().is_empty());
        assert!(re_executor.recorded.lock().unwrap().is_empty());
    }
}

#[test]
fn unsupported_effective_type_has_zero_catalog_and_relevance_sends_in_either_scope() {
    for (scope, filters) in [
        (
            SearchScope::workspace(TENANT, WORKSPACE).unwrap(),
            TypeFilters::new(vec!["Lakehouse".into()], vec![]).unwrap(),
        ),
        (
            scope(),
            TypeFilters::new(vec!["Lakehouse".into()], vec![]).unwrap(),
        ),
    ] {
        let (catalog, catalog_executor, credential) =
            catalog(Ok(response(200, json!({"value":[]}))));
        let (relevance, re_executor) = relevance(Ok(response(200, result(vec![]))));
        let input = initial(SearchMode::Hybrid, scope, Some(TEXT), filters);
        assert!(matches!(
            complete(
                SearchOperation::new(Some(&catalog), Some(&relevance)).execute(&input, pending())
            ),
            Err(SearchError::UnsupportedCapability { .. })
        ));
        assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
        assert!(catalog_executor.recorded.lock().unwrap().is_empty());
        assert!(re_executor.recorded.lock().unwrap().is_empty());
    }
}

#[test]
fn relevance_exact_workspace_is_a_native_uuid_restriction_not_name_or_global_search() {
    let (provider, executor) = relevance(Ok(response(
        200,
        result(vec![artifact(Some(ITEM), "Report")]),
    )));
    let input = initial(
        SearchMode::Relevance,
        SearchScope::workspace(TENANT, WORKSPACE).unwrap(),
        Some(TEXT),
        TypeFilters::default(),
    );
    let receipt =
        complete(SearchOperation::new(None, Some(&provider)).execute(&input, pending())).unwrap();
    assert_eq!(
        receipt.data().hits[0].workspace_id(),
        Some(&Guid::new(WORKSPACE).unwrap())
    );
    let recorded = executor.recorded.lock().unwrap();
    assert_eq!(
        recorded[0].0.body(),
        json!({
            "discoveryScope":{
                "queryScope":"Workspace",
                "artifactObjectIds":[WORKSPACE],
                "allowedArtifactTypes":[3,2]
            },
            "top":50,
            "userIntent":"search-intent"
        })
    );
    assert_eq!(recorded[0].0.text(), TEXT);
    assert!(
        recorded[0].0.body()["discoveryScope"]
            .get("workspaceNames")
            .is_none()
    );
}

#[test]
fn empty_guid_workspace_is_invalid_input_before_either_hybrid_provider_dispatches() {
    let (catalog, catalog_executor, credential) = catalog(Ok(response(200, json!({"value":[]}))));
    let (relevance, re_executor) = relevance(Ok(response(200, result(vec![]))));
    let input = initial(
        SearchMode::Hybrid,
        SearchScope::workspace(TENANT, "00000000-0000-0000-0000-000000000000").unwrap(),
        Some(TEXT),
        TypeFilters::default(),
    );
    assert!(matches!(
        complete(SearchOperation::new(Some(&catalog), Some(&relevance)).execute(&input, pending())),
        Err(SearchError::ProviderInput {
            provider: Source::Relevance,
            field: "workspaceId"
        })
    ));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(catalog_executor.recorded.lock().unwrap().is_empty());
    assert!(re_executor.recorded.lock().unwrap().is_empty());
}

#[test]
fn hybrid_exact_workspace_failure_does_not_leak_or_relabel_out_of_scope_relevance_rows() {
    for workspace in [Value::Null, json!(OTHER_ITEM)] {
        let (catalog, _, _) = catalog(Ok(response(200, json!({"value":[item(ITEM,"Report")]}))));
        let mut row = artifact(Some(OTHER_ITEM), "Report");
        row["workspaceId"] = workspace;
        let (relevance, executor) = relevance(Ok(response(200, result(vec![row]))));
        let input = initial(
            SearchMode::Hybrid,
            SearchScope::workspace(TENANT, WORKSPACE).unwrap(),
            Some(TEXT),
            TypeFilters::default(),
        );
        let receipt = complete(
            SearchOperation::new(Some(&catalog), Some(&relevance)).execute(&input, pending()),
        )
        .unwrap();
        assert_eq!(receipt.status(), SearchStatus::Partial);
        assert_eq!(receipt.data().hits.len(), 1);
        assert_eq!(
            receipt.data().hits[0].identity(),
            &CandidateIdentity::item(TENANT, ITEM).unwrap()
        );
        assert_eq!(receipt.sources()[1].status(), SourceStatus::Failed);
        assert_eq!(
            executor.recorded.lock().unwrap()[0].0.body()["discoveryScope"]["queryScope"],
            "Workspace"
        );
    }
}

#[test]
fn explicitly_admitted_dataagent_maps_only_the_known_alias_and_retains_both_sources() {
    let (catalog, _, _) = catalog(Ok(response(200, json!({"value":[item(ITEM,"DataAgent")]}))));
    let re_executor = Arc::new(RelevanceExecutor {
        replies: Mutex::new(VecDeque::from([Ok(response(
            200,
            result(vec![artifact(Some(ITEM), "AISkill")]),
        ))])),
        ..Default::default()
    });
    let relevance = RelevanceProvider::with_effective_types(
        RelevanceClient::new(re_executor.clone()),
        tenant(),
        vec![ArtifactType::AISkill, ArtifactType::Ontology],
    )
    .unwrap();
    let input = initial(
        SearchMode::Hybrid,
        scope(),
        Some(TEXT),
        TypeFilters::new(vec!["DataAgent".into()], vec![]).unwrap(),
    );
    let receipt =
        complete(SearchOperation::new(Some(&catalog), Some(&relevance)).execute(&input, pending()))
            .unwrap();
    assert_eq!(receipt.data().hits.len(), 1);
    assert_eq!(receipt.data().hits[0].artifact_type(), "DataAgent");
    assert_eq!(receipt.data().hits[0].provenance().len(), 2);
    assert_eq!(
        receipt.data().hits[0].provenance()[1].native_type(),
        "AISkill"
    );
    assert_eq!(
        re_executor.recorded.lock().unwrap()[0].0.body()["discoveryScope"]["allowedArtifactTypes"],
        json!([5003])
    );
}

#[test]
fn relevance_disabled_is_unavailable_not_a_healthy_empty_response() {
    let (provider, _) = relevance(Ok(response(
        200,
        json!({"queryId":"00000000-0000-0000-0000-000000000000","artifacts":[]}),
    )));
    let sources = failed_sources(
        complete(
            SearchOperation::new(None, Some(&provider))
                .execute(&request(SearchMode::Relevance), pending()),
        )
        .unwrap_err(),
    );
    assert_eq!(
        sources[1].error().unwrap().code,
        ProviderErrorCode::Unavailable
    );
    assert_eq!(sources[1].http_status(), Some(200));
    assert!(sources[1].response_bytes().is_some());
}

#[test]
fn relevance_null_or_malformed_authoritative_identity_fails_instead_of_skipping() {
    let mut wrong_workspace = artifact(Some(ITEM), "Report");
    wrong_workspace["workspaceId"] = json!("not-guid");
    for row in [
        artifact(None, "Report"),
        artifact(Some("not-guid"), "Report"),
        wrong_workspace,
    ] {
        let reply = response(200, result(vec![row]));
        let bytes = reply.body.len();
        let (provider, _) = relevance(Ok(reply));
        let sources = failed_sources(
            complete(
                SearchOperation::new(None, Some(&provider))
                    .execute(&request(SearchMode::Relevance), pending()),
            )
            .unwrap_err(),
        );
        assert_eq!(
            sources[1].error().unwrap().code,
            ProviderErrorCode::InvalidResponse
        );
        assert_eq!(sources[1].response_bytes(), Some(bytes));
        assert_eq!(sources[1].http_status(), Some(200));
    }
}

#[test]
fn malformed_native_relevance_dto_retains_measured_response_failure_evidence() {
    let mut missing_required = artifact(Some(ITEM), "Report");
    missing_required
        .as_object_mut()
        .unwrap()
        .remove("displayName");
    let mut wrong_scalar = artifact(Some(ITEM), "Report");
    wrong_scalar["relevance"] = json!(5);
    for row in [missing_required, wrong_scalar] {
        let reply = response(200, result(vec![row]));
        let bytes = reply.body.len();
        let (provider, _) = relevance(Ok(reply));
        let sources = failed_sources(
            complete(
                SearchOperation::new(None, Some(&provider))
                    .execute(&request(SearchMode::Relevance), pending()),
            )
            .unwrap_err(),
        );
        assert_eq!(
            sources[1].error().unwrap().code,
            ProviderErrorCode::InvalidResponse
        );
        assert_eq!(sources[1].response_bytes(), Some(bytes));
        assert_eq!(sources[1].http_status(), Some(200));
    }
}

#[test]
fn related_metadata_types_cannot_be_admitted_as_request_types() {
    for types in [
        vec![],
        vec![ArtifactType::Report, ArtifactType::Report],
        vec![ArtifactType::RdlReport],
        vec![ArtifactType::Workspace],
    ] {
        let executor = Arc::new(RelevanceExecutor::default());
        assert!(
            RelevanceProvider::with_effective_types(
                RelevanceClient::new(executor.clone()),
                tenant(),
                types,
            )
            .is_err()
        );
        assert!(executor.recorded.lock().unwrap().is_empty());
    }
}

#[test]
fn relevance_throttle_keeps_safe_status_retry_hint_and_measured_failure_bytes() {
    let mut reply = response(
        429,
        json!({"error":{"code":"Throttled","message":"private-response"}}),
    );
    reply.headers.insert("rEtRy-AfTeR".into(), "12".into());
    let bytes = reply.body.len();
    let (provider, executor) = relevance(Ok(reply));
    let sources = failed_sources(
        complete(
            SearchOperation::new(None, Some(&provider))
                .execute(&request(SearchMode::Relevance), pending()),
        )
        .unwrap_err(),
    );
    let error = sources[1].error().unwrap();
    assert_eq!(error.code, ProviderErrorCode::Throttled);
    assert_eq!(error.http_status, Some(429));
    assert_eq!(error.response_bytes, Some(bytes));
    assert_eq!(error.retry_after_seconds, Some(12));
    assert!(
        !serde_json::to_string(&sources)
            .unwrap()
            .contains("private-response")
    );
    assert_eq!(executor.recorded.lock().unwrap().len(), 1);
}

#[test]
fn relevance_nested_and_legacy_code_denials_remain_safe_observed_failures() {
    for (status, code) in [
        (401, ProviderErrorCode::Unauthenticated),
        (403, ProviderErrorCode::AccessDenied),
    ] {
        for body in [
            json!({"error":{"code":"Denied","message":"private-response"}}),
            json!({"code":"Denied","message":"private-response"}),
        ] {
            let reply = response(status, body);
            let bytes = reply.body.len();
            let (provider, _) = relevance(Ok(reply));
            let sources = failed_sources(
                complete(
                    SearchOperation::new(None, Some(&provider))
                        .execute(&request(SearchMode::Relevance), pending()),
                )
                .unwrap_err(),
            );
            assert_eq!(sources[1].error().unwrap().code, code);
            assert_eq!(sources[1].http_status(), Some(status));
            assert_eq!(sources[1].response_bytes(), Some(bytes));
            assert!(
                !serde_json::to_string(&sources)
                    .unwrap()
                    .contains("private-response")
            );
        }
    }
}

#[test]
fn hybrid_preserves_usable_catalog_when_relevance_transport_measurement_is_unavailable() {
    let (catalog, catalog_executor, _) =
        catalog(Ok(response(200, json!({"value":[item(ITEM,"Report")]}))));
    let (relevance, re_executor) = relevance(Err(ApiError::Execution(ExecutionError::TimedOut)));
    let receipt = complete(
        SearchOperation::new(Some(&catalog), Some(&relevance))
            .execute(&request(SearchMode::Hybrid), pending()),
    )
    .unwrap();
    assert_eq!(receipt.status(), SearchStatus::Partial);
    assert_eq!(receipt.data().hits.len(), 1);
    assert_eq!(receipt.sources()[1].response_bytes(), None);
    assert_eq!(receipt.sources()[1].http_status(), None);
    assert_eq!(
        receipt.sources()[1].error().unwrap().code,
        ProviderErrorCode::TimedOut
    );
    assert_eq!(
        catalog_executor.recorded.lock().unwrap()[0].limit,
        1024 * 1024 / 2
    );
    assert_eq!(re_executor.recorded.lock().unwrap()[0].1, 1024 * 1024 / 2);
}

#[test]
fn cancellation_precedes_native_token_and_provider_access() {
    let (catalog, catalog_executor, credential) = catalog(Ok(response(200, json!({"value":[]}))));
    let (relevance, re_executor) = relevance(Ok(response(200, result(vec![]))));
    assert!(matches!(
        complete(
            SearchOperation::new(Some(&catalog), Some(&relevance))
                .execute(&request(SearchMode::Hybrid), async {})
        ),
        Err(SearchError::Cancelled)
    ));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(catalog_executor.recorded.lock().unwrap().is_empty());
    assert!(re_executor.recorded.lock().unwrap().is_empty());
}

#[test]
fn cancellation_drops_both_concrete_inflight_clients_without_replay() {
    let catalog_executor = Arc::new(CatalogExecutor {
        stay_pending: true,
        ..Default::default()
    });
    let credential = Arc::new(Credential::default());
    let catalog = CatalogProvider::new(
        CatalogClient::new(ApiClient::new(
            credential,
            CredentialSelection::new("synthetic-account", TENANT, Audience::Fabric).unwrap(),
            catalog_executor.clone(),
        )),
        tenant(),
    );
    let re_executor = Arc::new(RelevanceExecutor {
        stay_pending: true,
        ..Default::default()
    });
    let relevance = RelevanceProvider::new(RelevanceClient::new(re_executor.clone()), tenant());
    let operation = SearchOperation::new(Some(&catalog), Some(&relevance));
    let input = request(SearchMode::Hybrid);
    let signal = AtomicBool::new(false);
    let cancel = poll_fn(|_| {
        if signal.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let mut future = pin!(operation.execute(&input, cancel));
    let mut context = Context::from_waker(Waker::noop());
    assert!(future.as_mut().poll(&mut context).is_pending());
    assert_eq!(catalog_executor.recorded.lock().unwrap().len(), 1);
    assert_eq!(re_executor.recorded.lock().unwrap().len(), 1);
    signal.store(true, Ordering::SeqCst);
    assert!(matches!(
        future.as_mut().poll(&mut context),
        Poll::Ready(Err(SearchError::Cancelled))
    ));
    assert_eq!(catalog_executor.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(re_executor.dropped.load(Ordering::SeqCst), 1);
}
