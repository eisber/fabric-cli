// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public SDK configuration exercised through the existing HTTP wire adapter.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, Url, headers::Headers},
    time::OffsetDateTime,
};
use fabric_core::{
    Audience, AuthenticatedExecution, CredentialSelection, EndpointBinding, RequestAuthentication,
    RequestContext, RequestError, ServiceOrigin, TokenCredential,
};
use fabric_sdk::Client;
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_items::{ItemClient, ItemErrorKind, ItemGetOptions, ItemListOptions};
use fabric_sdk_operations::{
    FabricOperation, OperationErrorKind, OperationLimits, OperationSession,
};
use fabric_sdk_workspaces::{
    WorkspaceClient, WorkspaceListErrorKind, WorkspaceListLimits,
    mutations::{CompletionPolicy, ResourceMethod, ResourceOperation},
};
use std::{
    collections::VecDeque,
    future::{pending, ready},
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

const OPERATION: &str = "22222222-2222-2222-2222-222222222222";

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|scope| (*scope).to_owned()).collect());
        Ok(AccessToken::new(
            "offline-endpoint-context",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(async move {
            if delay == Duration::from_secs(240) {
                pending::<()>().await;
            }
        })
    }
}

#[derive(Debug)]
struct Wire {
    responses: Mutex<VecDeque<AsyncRawResponse>>,
    requests: Mutex<Vec<Request>>,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request or mutation replay"))
    }
}

fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> AsyncRawResponse {
    let mut values = Headers::new();
    for &(name, value) in headers {
        values.insert(name.to_owned(), value.to_owned());
    }
    AsyncRawResponse::from_bytes(status.into(), values, body.as_bytes().to_vec())
}

fn wire(responses: impl IntoIterator<Item = AsyncRawResponse>) -> Arc<Wire> {
    Arc::new(Wire {
        responses: Mutex::new(responses.into_iter().collect()),
        requests: Mutex::new(Vec::new()),
    })
}

fn context(credential: Arc<dyn TokenCredential>, endpoint: EndpointBinding) -> RequestContext {
    let selection = CredentialSelection::new("account", "tenant", endpoint.audience()).unwrap();
    RequestContext::with_endpoint(credential, selection, endpoint)
}

fn executor(wire: Arc<Wire>, endpoint: EndpointBinding) -> HttpExecutor {
    HttpExecutor::new(wire, Arc::new(Timer), ExecutionOptions::default())
        .unwrap()
        .with_endpoint(endpoint)
}

#[tokio::test]
async fn public_and_daily_context_reach_workspace_item_continuation_and_lro_wire_requests() {
    for host in ["api.fabric.microsoft.com", "dailyapi.fabric.microsoft.com"] {
        let endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap();
        let root = format!("https://{host}");
        let credential = Arc::new(Credential::default());
        let erased: Arc<dyn TokenCredential> = credential.clone();
        let context = context(erased.clone(), endpoint.clone());
        let workspaces = WorkspaceClient::with_context(context.clone());
        let items = ItemClient::with_context(context.clone());
        let facade = Client::with_context(context.clone());
        assert!(Arc::ptr_eq(workspaces.credential(), &erased));
        assert!(Arc::ptr_eq(items.credential(), &erased));
        assert!(Arc::ptr_eq(facade.credential(), &erased));
        let location = format!("{root}/v1/operations/{OPERATION}");
        let result = format!("{location}/result");
        let raw = wire([
            response(
                200,
                &format!(
                    r#"{{"value":[{{"id":"ws","displayName":"Workspace"}}],"continuationToken":"next+/=","continuationUri":"{root}/v1/workspaces?continuationToken=next"}}"#
                ),
                &[],
            ),
            response(200, r#"{"value":[]}"#, &[]),
            response(200, r#"{"id":"ws","displayName":"Workspace"}"#, &[]),
            response(200, r#"{"value":[]}"#, &[]),
            response(200, "{}", &[]),
            response(200, r#"{"value":[]}"#, &[]),
            response(
                200,
                &format!(
                    r#"{{"value":[{{"id":"i","displayName":"Item","type":"Notebook"}}],"continuationToken":"item+/=","continuationUri":"{root}/v1/workspaces/ws/items?continuationToken=item"}}"#
                ),
                &[],
            ),
            response(200, r#"{"value":[]}"#, &[]),
            response(
                200,
                r#"{"id":"i","displayName":"Item","type":"Notebook"}"#,
                &[],
            ),
            response(
                202,
                "{}",
                &[
                    ("x-ms-operation-id", OPERATION),
                    ("Location", &location),
                    ("Retry-After", "0"),
                ],
            ),
            response(200, r#"{"status":"Succeeded"}"#, &[("Location", &result)]),
            response(200, r#"{"id":"completed"}"#, &[]),
        ]);
        let http = executor(raw.clone(), endpoint);
        let list = workspaces
            .list_workspaces(&http, WorkspaceListLimits::default(), pending())
            .await
            .unwrap();
        assert_eq!(list.pages_received, 2);
        let metadata = facade
            .get_workspace_metadata(&http, "ws", WorkspaceListLimits::default(), pending())
            .await
            .unwrap();
        assert_eq!(metadata.enrichments.len(), 3);
        let list = facade
            .list_items(&http, "ws", ItemListOptions::default(), pending())
            .await
            .unwrap();
        assert_eq!(list.pages_received, 2);
        assert_eq!(
            items
                .get_item(&http, "ws", "i", ItemGetOptions::default(), pending())
                .await
                .unwrap()
                .id,
            "i"
        );
        let operation = ResourceOperation::new(
            ResourceMethod::Get,
            &["workspaces", "ws"],
            None,
            CompletionPolicy::PythonSynchronous,
        )
        .unwrap();
        let mut session =
            OperationSession::with_context(&context, http, OperationLimits::default(), pending());
        let completed = session
            .execute(FabricOperation::from_resource(&operation).unwrap())
            .await
            .unwrap();
        assert_eq!(completed.body, br#"{"id":"completed"}"#);
        let requests = raw.requests.lock().unwrap();
        let paths: Vec<_> = requests
            .iter()
            .map(|request| request.url().path())
            .collect();
        assert_eq!(
            paths,
            [
                "/v1/workspaces",
                "/v1/workspaces",
                "/v1/workspaces/ws",
                "/v1/workspaces/ws/managedPrivateEndpoints",
                "/v1/workspaces/ws/spark/settings",
                "/v1/workspaces/ws/roleAssignments",
                "/v1/workspaces/ws/items",
                "/v1/workspaces/ws/items",
                "/v1/workspaces/ws/items/i",
                "/v1/workspaces/ws",
                &format!("/v1/operations/{OPERATION}"),
                &format!("/v1/operations/{OPERATION}/result"),
            ]
        );
        assert_eq!(
            requests[1].url().query(),
            Some("continuationToken=next%2B%2F%3D")
        );
        assert_eq!(
            requests[7].url().query(),
            Some("continuationToken=item%2B%2F%3D")
        );
        assert!(requests.iter().all(|request| {
            request.url().host_str() == Some(host)
                && request.method() == Method::Get
                && request
                    .headers()
                    .get_str(&"authorization".into())
                    .is_ok_and(|value| value == "Bearer offline-endpoint-context")
        }));
        assert_eq!(
            *credential.scopes.lock().unwrap(),
            vec![vec![Audience::Fabric.scope().to_owned()]; 12]
        );
        assert!(raw.responses.lock().unwrap().is_empty());
    }
}

struct BoundRead<'a> {
    http: &'a HttpExecutor,
    origin: ServiceOrigin,
}

impl AuthenticatedExecution for BoundRead<'_> {
    type Response = fabric_sdk_workspaces::WorkspaceResponse;
    type Error = fabric_sdk_http::ExecutionError;

    fn execute<'a>(
        &'a mut self,
        url: &'a Url,
        token: &'a AccessToken,
    ) -> ExecutionFuture<'a, Result<Self::Response, Self::Error>> {
        Box::pin(async move {
            self.http
                .execute_bound(
                    self.origin,
                    Method::Get,
                    url,
                    token,
                    &[],
                    &[],
                    NonZeroUsize::new(1024).unwrap(),
                )
                .await
        })
    }
}

#[tokio::test]
async fn all_four_explicit_hostname_configuration_shapes_keep_their_native_scope_and_prefix() {
    for (origin, host, path, scope) in [
        (
            ServiceOrigin::Fabric,
            "dailyapi.fabric.microsoft.com",
            "/v1/workspaces",
            Audience::Fabric,
        ),
        (
            ServiceOrigin::OneLake,
            "onelake.dfs.fabric.microsoft.com",
            "/ws/item/Files/test",
            Audience::OneLake,
        ),
        (
            ServiceOrigin::Arm,
            "management.azure.com",
            "/subscriptions/test?api-version=1",
            Audience::Arm,
        ),
        (
            ServiceOrigin::PowerBi,
            "api.powerbi.com",
            "/v1.0/myorg/groups",
            Audience::Fabric,
        ),
    ] {
        let endpoint = EndpointBinding::with_host(origin, host).unwrap();
        let credential = Arc::new(Credential::default());
        let context = context(credential.clone(), endpoint.clone());
        let raw = wire([response(200, "{}", &[])]);
        let http = executor(raw.clone(), endpoint.clone());
        let url = endpoint.url(path).unwrap();
        context
            .authentication()
            .execute(
                origin,
                &url,
                &mut BoundRead {
                    http: &http,
                    origin,
                },
                std::pin::pin!(pending()),
            )
            .await
            .unwrap();
        assert_eq!(raw.requests.lock().unwrap()[0].url(), &url);
        assert_eq!(
            *credential.scopes.lock().unwrap(),
            [vec![scope.scope().to_owned()]]
        );
        assert_eq!(endpoint.api_path(), origin.root_path());
    }
}

#[tokio::test]
async fn selected_binding_rejects_foreign_continuations_before_another_token_or_send() {
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap();
    let credential = Arc::new(Credential::default());
    let context = context(credential.clone(), endpoint.clone());
    let raw = wire([
        response(
            200,
            r#"{"value":[],"continuationToken":"next","continuationUri":"https://api.fabric.microsoft.com/v1/workspaces?continuationToken=next"}"#,
            &[],
        ),
        response(
            200,
            r#"{"value":[],"continuationToken":"next","continuationUri":"https://api.fabric.microsoft.com/v1/workspaces/ws/items?continuationToken=next"}"#,
            &[],
        ),
    ]);
    let http = executor(raw.clone(), endpoint);
    let error = WorkspaceClient::with_context(context.clone())
        .list_workspaces(&http, Default::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::InvalidContinuation);
    let error = ItemClient::with_context(context)
        .list_items(&http, "ws", Default::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ItemErrorKind::InvalidContinuation);
    assert_eq!(raw.requests.lock().unwrap().len(), 2);
    assert_eq!(credential.scopes.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn origin_audience_and_cancellation_guards_run_before_credential_or_transport() {
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap();
    let credential = Arc::new(Credential::default());
    let context = context(credential.clone(), endpoint.clone());
    let raw = wire([]);
    let http = executor(raw.clone(), endpoint.clone());
    let foreign = Url::parse("https://api.fabric.microsoft.com/v1/workspaces").unwrap();
    let error = context
        .authentication()
        .execute(
            ServiceOrigin::Fabric,
            &foreign,
            &mut BoundRead {
                http: &http,
                origin: ServiceOrigin::Fabric,
            },
            std::pin::pin!(pending()),
        )
        .await
        .unwrap_err();
    assert_eq!(error, RequestError::InvalidEndpoint);
    let wrong = CredentialSelection::new("account", "tenant", Audience::Arm).unwrap();
    let url = endpoint.url("/v1/workspaces").unwrap();
    let error = RequestAuthentication::new(credential.as_ref(), &wrong, &endpoint)
        .execute(
            ServiceOrigin::Fabric,
            &url,
            &mut BoundRead {
                http: &http,
                origin: ServiceOrigin::Fabric,
            },
            std::pin::pin!(pending()),
        )
        .await
        .unwrap_err();
    assert_eq!(error, RequestError::InvalidAudience);
    let error = context
        .authentication()
        .execute(
            ServiceOrigin::Fabric,
            &url,
            &mut BoundRead {
                http: &http,
                origin: ServiceOrigin::Fabric,
            },
            std::pin::pin!(ready(())),
        )
        .await
        .unwrap_err();
    assert_eq!(error, RequestError::Cancelled);
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(raw.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unconfigured_http_executor_remains_public_only() {
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap();
    let raw = wire([]);
    let http =
        HttpExecutor::new(raw.clone(), Arc::new(Timer), ExecutionOptions::default()).unwrap();
    let error = WorkspaceClient::with_context(context(Arc::new(Credential::default()), endpoint))
        .list_workspaces(&http, Default::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        WorkspaceListErrorKind::Execution(fabric_sdk_workspaces::WorkspaceExecutionError::Failed)
    );
    assert!(raw.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn daily_mutation_keeps_existing_no_replay_and_uncertain_side_effect_evidence() {
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap();
    let credential = Arc::new(Credential::default());
    let context = context(credential.clone(), endpoint.clone());
    let raw = wire([response(429, r#"{"errorCode":"Throttled"}"#, &[])]);
    let http = executor(raw.clone(), endpoint);
    let mut session =
        OperationSession::with_context(&context, http, OperationLimits::default(), pending());
    let operation = FabricOperation::new(
        ResourceMethod::Post,
        "/v1/workspaces",
        br#"{"displayName":"Offline"}"#.to_vec(),
        CompletionPolicy::WaitForResult,
    )
    .unwrap();
    let error = session.execute(operation).await.unwrap_err();
    assert!(matches!(
        error.kind,
        OperationErrorKind::Http { status: 429, .. }
    ));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(raw.requests.lock().unwrap().len(), 1);
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
}

struct ExpiredTimer;

impl ExecutionTimer for ExpiredTimer {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(ready(()))
    }
}

#[tokio::test]
async fn daily_request_keeps_existing_overall_deadline_before_wire_dispatch() {
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap();
    let raw = wire([]);
    let http = HttpExecutor::new(
        raw.clone(),
        Arc::new(ExpiredTimer),
        ExecutionOptions::default(),
    )
    .unwrap()
    .with_endpoint(endpoint.clone());
    let error = WorkspaceClient::with_context(context(Arc::new(Credential::default()), endpoint))
        .list_workspaces(&http, Default::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        WorkspaceListErrorKind::Execution(fabric_sdk_workspaces::WorkspaceExecutionError::TimedOut)
    );
    assert!(raw.requests.lock().unwrap().is_empty());
}
