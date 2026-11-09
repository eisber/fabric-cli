// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic checks of the concrete shared-API adapter, not live acceptance.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    http::Method,
    time::OffsetDateTime,
};
use fabric_core::{Audience, CredentialSelection, EndpointBinding, ServiceOrigin, TokenCredential};
use fabric_sdk_api::{ApiClient, ApiError, Executor, Origin, Request};
use fabric_sdk_relevance::{
    ArtifactType, DISCOVER_ARTIFACTS_PATH, DiscoveryCoverage, DiscoveryRequest, DiscoveryScope,
    RelevanceClient, RelevanceError, RelevanceObservationError,
};
use fabric_sdk_workspaces::workspaces::{WorkspaceExecutionError, WorkspaceResponse};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{Future, pending, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // This verifies inherited workspace policy, not RE deployment acceptance.
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        Ok(AccessToken::new("offline-only", OffsetDateTime::UNIX_EPOCH))
    }
}

struct SharedExecutor {
    supported: bool,
    requests: Mutex<Vec<Request>>,
    response: Mutex<Option<WorkspaceResponse>>,
}

impl Executor for SharedExecutor {
    fn supports(&self, request: &Request) -> bool {
        self.supported && request.origin() == Origin::RelevanceEngine
    }

    fn execute<'a>(
        &'a self,
        request: &'a Request,
        token: &'a AccessToken,
        _: NonZeroUsize,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(token.token.secret(), "offline-only");
            self.requests.lock().unwrap().push(request.clone());
            Ok(self.response.lock().unwrap().take().unwrap())
        })
    }
}

fn response(status: u16, body: Value) -> WorkspaceResponse {
    WorkspaceResponse {
        status,
        headers: BTreeMap::new(),
        body: serde_json::to_vec(&body).unwrap(),
    }
}

fn success() -> WorkspaceResponse {
    response(
        200,
        json!({"queryId":"12345678-1234-1234-1234-1234567890ab","artifacts":[]}),
    )
}

fn setup(
    audience: Audience,
    supported: bool,
    response: WorkspaceResponse,
) -> (ApiClient, Arc<Credential>, Arc<SharedExecutor>) {
    let credential = Arc::new(Credential::default());
    let executor = Arc::new(SharedExecutor {
        supported,
        requests: Mutex::default(),
        response: Mutex::new(Some(response)),
    });
    let api = ApiClient::new(
        credential.clone(),
        CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap(),
        executor.clone(),
    );
    (api, credential, executor)
}

fn request() -> DiscoveryRequest {
    DiscoveryRequest::new(
        " revenue & café + %/? ",
        5,
        DiscoveryScope::Tenant,
        vec![ArtifactType::Report, ArtifactType::SemanticModel],
    )
    .unwrap()
}

fn limit() -> NonZeroUsize {
    NonZeroUsize::new(1024 * 1024).unwrap()
}

#[tokio::test]
async fn concrete_adapter_uses_exact_finite_origin_post_and_once_encoded_query() {
    let response = success();
    let actual_bytes = response.body.len();
    let (api, credential, executor) = setup(Audience::Fabric, true, response);
    let client =
        RelevanceClient::from_api(api, EndpointBinding::public(ServiceOrigin::RelevanceEngine))
            .unwrap();
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    let input = request();
    let observation = client
        .discover_observed(&input, limit(), pending())
        .await
        .unwrap();
    assert_eq!(observation.result.coverage, DiscoveryCoverage::RankedTopK);
    assert_eq!(observation.http_status, 200);
    assert_eq!(observation.response_bytes, actual_bytes);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let sent = &requests[0];
    assert_eq!(sent.origin(), Origin::RelevanceEngine);
    assert_eq!(sent.method(), Method::Post);
    assert_eq!(sent.url().host_str(), Some("api.powerbi.com"));
    assert_eq!(sent.url().path(), DISCOVER_ARTIFACTS_PATH);
    assert_eq!(
        sent.url().query_pairs().into_owned().collect::<Vec<_>>(),
        input.query()
    );
    assert_eq!(
        serde_json::from_slice::<Value>(sent.body()).unwrap(),
        input.body()
    );
    assert_eq!(
        serde_json::from_slice::<Value>(sent.body()).unwrap()["userIntent"],
        json!("search-intent")
    );
    assert!(sent.headers().is_empty());
}

#[tokio::test]
async fn explicit_reviewed_host_is_preserved_without_changing_service_root() {
    let (api, _, executor) = setup(Audience::Fabric, true, success());
    let endpoint = EndpointBinding::with_host(
        ServiceOrigin::RelevanceEngine,
        "dailyapi.fabric.microsoft.com",
    )
    .unwrap();
    let client = RelevanceClient::from_api_with_endpoint(api, endpoint).unwrap();
    client
        .discover(&request(), limit(), pending())
        .await
        .unwrap();
    let requests = executor.requests.lock().unwrap();
    assert_eq!(
        requests[0].url().host_str(),
        Some("dailyapi.fabric.microsoft.com")
    );
    assert_eq!(requests[0].url().path(), DISCOVER_ARTIFACTS_PATH);
}

#[tokio::test]
async fn wrong_service_audience_unsupported_executor_and_precancellation_never_acquire_tokens() {
    for service in [
        ServiceOrigin::PowerBi,
        ServiceOrigin::Fabric,
        ServiceOrigin::Arm,
    ] {
        let (api, credential, executor) = setup(Audience::Fabric, true, success());
        assert!(matches!(
            RelevanceClient::from_api_with_endpoint(api, EndpointBinding::public(service)),
            Err(RelevanceError::Api(ApiError::InvalidAudience))
        ));
        assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
        assert!(executor.requests.lock().unwrap().is_empty());
    }
    for (audience, supported, expected) in [
        (Audience::OneLake, true, ApiError::InvalidAudience),
        (Audience::Fabric, false, ApiError::UnsupportedRequest),
    ] {
        let (api, credential, executor) = setup(audience, supported, success());
        let client =
            RelevanceClient::from_api(api, EndpointBinding::public(ServiceOrigin::RelevanceEngine))
                .unwrap();
        assert_eq!(
            client.discover(&request(), limit(), pending()).await,
            Err(RelevanceError::Api(expected))
        );
        assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
        assert!(executor.requests.lock().unwrap().is_empty());
    }
    let (api, credential, executor) = setup(Audience::Fabric, true, success());
    let client =
        RelevanceClient::from_api(api, EndpointBinding::public(ServiceOrigin::RelevanceEngine))
            .unwrap();
    assert_eq!(
        client.discover(&request(), limit(), ready(())).await,
        Err(RelevanceError::Api(ApiError::Cancelled))
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn shared_adapter_returns_failures_without_replay_or_status_coercion() {
    let mut throttle = response(429, json!({"error":{"code":"SyntheticThrottle"}}));
    throttle.headers.insert("Retry-After".into(), "11".into());
    let actual_bytes = throttle.body.len();
    let (api, credential, executor) = setup(Audience::Fabric, true, throttle);
    let client =
        RelevanceClient::from_api(api, EndpointBinding::public(ServiceOrigin::RelevanceEngine))
            .unwrap();
    let observation: RelevanceObservationError = client
        .discover_observed(&request(), limit(), pending())
        .await
        .unwrap_err();
    assert_eq!(observation.http_status, Some(429));
    assert_eq!(observation.response_bytes, Some(actual_bytes));
    assert_eq!(observation.retry_after_seconds, Some(11));
    let RelevanceError::Service(failure) = observation.error else {
        panic!("expected source-backed HTTP failure");
    };
    assert_eq!(failure.status, 429);
    assert_eq!(failure.retry_after.as_deref(), Some("11"));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn shared_pre_token_and_response_limit_failures_do_not_invent_observations() {
    for (audience, bound, error, expected_calls) in [
        (Audience::OneLake, limit(), ApiError::InvalidAudience, 0),
        (
            Audience::Fabric,
            NonZeroUsize::new(1).unwrap(),
            ApiError::Limit,
            1,
        ),
    ] {
        let (api, credential, executor) = setup(audience, true, success());
        let client =
            RelevanceClient::from_api(api, EndpointBinding::public(ServiceOrigin::RelevanceEngine))
                .unwrap();
        let failure = client
            .discover_observed(&request(), bound, pending())
            .await
            .unwrap_err();
        assert_eq!(failure.error, RelevanceError::Api(error));
        assert_eq!(failure.http_status, None);
        assert_eq!(failure.response_bytes, None);
        assert_eq!(failure.retry_after_seconds, None);
        assert_eq!(credential.calls.load(Ordering::SeqCst), expected_calls);
        assert_eq!(executor.requests.lock().unwrap().len(), expected_calls);
    }
}
