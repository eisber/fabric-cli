// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Search clients exercise the native shared HTTP adapter without service access.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::search_command::{SearchDelivery, parse_search, sdk_request};
use fabric_sdk::search::{
    Guid, RelevanceProvider, SearchOperation, execute_approved_search, prepare_search,
};
use fabric_sdk::v2::{ApprovalPolicy, Effect, PreparedOperation};
use fabric_sdk::{EndpointBinding, ServiceOrigin};
use fabric_sdk_api::{ApiClient, Audience, CredentialSelection};
use fabric_sdk_catalog::{CatalogClient, CatalogFilter, CatalogRequest};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_relevance::{ArtifactType, DiscoveryRequest, DiscoveryScope, RelevanceClient};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::pending,
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Debug, Default)]
struct Credential(AtomicUsize);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        Ok(AccessToken::new(
            "offline-search-wire-only",
            OffsetDateTime::from_unix_timestamp(4_000_000_000).unwrap(),
        ))
    }
}

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(pending())
    }
}

#[derive(Debug)]
struct Wire {
    requests: Mutex<Vec<Request>>,
    replies: Mutex<VecDeque<AsyncRawResponse>>,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request or POST replay"))
    }
}

fn setup(
    status: u16,
    body: &str,
    retry_after: Option<&str>,
    audience: Audience,
) -> (ApiClient, Arc<Wire>, Arc<Credential>) {
    let mut headers = Headers::new();
    if let Some(value) = retry_after {
        headers.insert("retry-after", value.to_owned());
    }
    let wire = Arc::new(Wire {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(VecDeque::from([AsyncRawResponse::from_bytes(
            StatusCode::from(status),
            headers,
            body.as_bytes().to_vec(),
        )])),
    });
    let credential = Arc::new(Credential::default());
    let http = Arc::new(
        HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default()).unwrap(),
    );
    let api = ApiClient::new(
        credential.clone(),
        CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap(),
        Arc::new(fabric_cli_resources::ResourceHttpExecutor(http)),
    );
    (api, wire, credential)
}

fn limit() -> NonZeroUsize {
    NonZeroUsize::new(16_384).unwrap()
}

fn relevance_request() -> DiscoveryRequest {
    DiscoveryRequest::new(
        "sales & margin + growth",
        5,
        DiscoveryScope::Tenant,
        vec![ArtifactType::Report],
    )
    .unwrap()
}

fn relevance_client(api: ApiClient) -> RelevanceClient {
    RelevanceClient::from_api(api, EndpointBinding::public(ServiceOrigin::RelevanceEngine)).unwrap()
}

#[test]
fn concrete_relevance_diagnostics_survive_approval_and_scalar_or_null_delivery() {
    struct ReadPolicy;
    impl ApprovalPolicy for ReadPolicy {
        type Error = &'static str;
        fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), Self::Error> {
            if operation.operation_id() == "catalog.search"
                && operation.effects() == [Effect::RemoteRead, Effect::ModelExecution]
            {
                Ok(())
            } else {
                Err("Unexpected operation or effects.")
            }
        }
    }

    const TENANT: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    for (projection, expected_data) in [("length(hits)", json!(0)), ("`null`", Value::Null)] {
        let body = json!({
            "queryId":"11111111-2222-4333-8444-555555555555",
            "artifacts":[],
            "diagnostics":{"semanticRanking":true,"futureDiagnostic":17}
        })
        .to_string();
        let (api, wire, _) = setup(200, &body, None, Audience::Fabric);
        let provider = RelevanceProvider::new(relevance_client(api), Guid::new(TENANT).unwrap());
        let command = parse_search([
            "search",
            "/",
            "--mode",
            "relevance",
            "--text",
            "sales",
            "--project",
            projection,
        ])
        .unwrap();
        let request = sdk_request(&command, TENANT).unwrap();
        let prepared = prepare_search(&request, "/").unwrap();
        let approved = prepared.authorize(&mut ReadPolicy).unwrap();
        let operation = SearchOperation::new(None, Some(&provider));
        let receipt = execute_approved_search(&approved, &request, &operation, pending())
            .now_or_never()
            .unwrap()
            .unwrap();
        let envelope = serde_json::to_value(receipt).unwrap();
        let sources = envelope["meta"]["sources"].clone();
        let source = sources
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["source"] == "relevance")
            .unwrap();
        assert_eq!(
            source["details"]["nativeDiagnostics"]["semanticRanking"],
            true
        );
        assert_eq!(
            source["details"]["nativeDiagnostics"]["futureDiagnostic"],
            17
        );
        let mut output = Vec::new();
        SearchDelivery::preflight(&command, &std::env::temp_dir())
            .unwrap()
            .deliver(envelope, &mut output)
            .unwrap();
        let rendered: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(rendered["data"], expected_data);
        assert_eq!(rendered["meta"]["sources"], sources);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn catalog_wire_preserves_actual_measurements_and_does_not_hydrate_hits() {
    let body = r#"{"value":[],"continuationToken":"next"}"#;
    let (api, wire, credential) = setup(200, body, None, Audience::Fabric);
    let client = CatalogClient::new(api);
    let request =
        CatalogRequest::initial(Some("sales".into()), CatalogFilter::default(), Some(5)).unwrap();
    let observation = client
        .search_v2_observed(&request, limit(), pending())
        .now_or_never()
        .unwrap()
        .unwrap();
    assert_eq!(observation.response_bytes, body.len());
    assert_eq!(observation.http_status, 200);
    assert_eq!(observation.page.next_token.as_deref(), Some("next"));
    let sent = wire.requests.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].url().as_str(),
        "https://api.fabric.microsoft.com/v1/catalog/search"
    );
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);
}

#[test]
fn relevance_wire_uses_the_direct_route_and_shared_authentication_without_replay() {
    let body = r#"{"queryId":"11111111-2222-4333-8444-555555555555","artifacts":[]}"#;
    let (api, wire, _) = setup(200, body, None, Audience::Fabric);
    let client = relevance_client(api);
    let observation = client
        .discover_observed(&relevance_request(), limit(), pending())
        .now_or_never()
        .unwrap()
        .unwrap();
    assert_eq!(observation.response_bytes, body.len());
    assert_eq!(observation.http_status, 200);
    let sent = wire.requests.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].url().path(),
        "/relevanceengine/v1/discoverArtifacts"
    );
    assert_eq!(sent[0].url().host_str(), Some("api.powerbi.com"));
    let query: Vec<_> = sent[0]
        .url()
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    assert_eq!(
        query,
        vec![(
            "searchText".to_owned(),
            "sales & margin + growth".to_owned()
        )]
    );
    let payload: Value = serde_json::from_slice(&azure_core::Bytes::from(sent[0].body())).unwrap();
    assert_eq!(
        payload,
        json!({"discoveryScope":{"queryScope":"Global","allowedArtifactTypes":[3]},"top":5,"userIntent":"search-intent"})
    );
}

#[test]
fn both_search_clients_retain_throttling_evidence_without_post_retries() {
    let body = r#"{"errorCode":"TooManyRequests","message":"synthetic throttle"}"#;
    let (api, wire, _) = setup(429, body, Some("7"), Audience::Fabric);
    let failure = CatalogClient::new(api)
        .search_v2_observed(
            &CatalogRequest::initial(Some("sales".into()), CatalogFilter::default(), None).unwrap(),
            limit(),
            pending(),
        )
        .now_or_never()
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.http_status, Some(429));
    assert_eq!(failure.retry_after_seconds, Some(7));
    assert_eq!(failure.response_bytes, Some(body.len()));
    assert_eq!(wire.requests.lock().unwrap().len(), 1);

    let (api, wire, _) = setup(429, body, Some("9"), Audience::Fabric);
    let failure = relevance_client(api)
        .discover_observed(&relevance_request(), limit(), pending())
        .now_or_never()
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.http_status, Some(429));
    assert_eq!(failure.retry_after_seconds, Some(9));
    assert_eq!(failure.response_bytes, Some(body.len()));
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[test]
fn mismatched_relevance_audience_is_rejected_before_credentials_or_send() {
    let (api, wire, credential) = setup(200, "{}", None, Audience::OneLake);
    let failure = relevance_client(api)
        .discover_observed(&relevance_request(), limit(), pending())
        .now_or_never()
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.http_status, None);
    assert_eq!(failure.response_bytes, None);
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());
}
