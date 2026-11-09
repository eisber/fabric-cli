// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use azure_core::http::Method;
use fabric_sdk::items::{ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse};
use fabric_sdk_http::{ExecutionError, ExecutionFuture, HttpExecutor, PublicOrigin};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationRequest, OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use std::time::Duration;

pub(super) struct DeployHttp<'a>(pub &'a HttpExecutor);

fn headers() -> [(String, String); 2] {
    [
        (
            "user-agent".into(),
            format!(
                "ms-fabric-cli/{} (deploy; ms-fabric-cicd/1.4.0)",
                env!("CARGO_PKG_VERSION")
            ),
        ),
        (
            "content-type".into(),
            "application/json; charset=utf-8".into(),
        ),
    ]
}

fn operation_error(error: ExecutionError) -> OperationExecutionError {
    match error {
        ExecutionError::TimedOut => OperationExecutionError::TimedOut,
        ExecutionError::ResponseLimit => OperationExecutionError::ResponseLimit,
        ExecutionError::RetriesExhausted => OperationExecutionError::RetriesExhausted,
        ExecutionError::InvalidInput | ExecutionError::Transport | ExecutionError::RetryDelay => {
            OperationExecutionError::Failed
        }
    }
}

impl OperationExecutor for DeployHttp<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> ExecutionFuture<'a, Result<OperationResponse, OperationExecutionError>> {
        Box::pin(async move {
            let method = match request.method() {
                ResourceMethod::Get => Method::Get,
                ResourceMethod::Post => Method::Post,
                ResourceMethod::Patch => Method::Patch,
                ResourceMethod::Delete => Method::Delete,
            };
            self.0
                .execute_bound(
                    PublicOrigin::Fabric,
                    method,
                    request.url(),
                    request.token(),
                    request.body(),
                    &headers(),
                    request.max_response_bytes(),
                )
                .await
                .map(|response| OperationResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                })
                .map_err(operation_error)
        })
    }
    fn wait(&self, delay: Duration) -> ExecutionFuture<'_, Result<(), OperationExecutionError>> {
        Box::pin(async move { self.0.wait_for(delay).await.map_err(operation_error) })
    }
}

impl ItemExecutor for DeployHttp<'_> {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> ExecutionFuture<'a, Result<ItemResponse, ItemExecutionError>> {
        Box::pin(async move {
            self.0
                .execute_bound(
                    PublicOrigin::Fabric,
                    Method::Get,
                    request.url(),
                    request.token(),
                    &[],
                    &headers(),
                    request.max_response_bytes(),
                )
                .await
                .map(|response| ItemResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                })
                .map_err(|error| match error {
                    ExecutionError::TimedOut => ItemExecutionError::TimedOut,
                    ExecutionError::ResponseLimit => ItemExecutionError::ResponseLimit,
                    ExecutionError::RetriesExhausted => ItemExecutionError::RetriesExhausted,
                    ExecutionError::InvalidInput
                    | ExecutionError::Transport
                    | ExecutionError::RetryDelay => ItemExecutionError::Failed,
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use azure_core::{
        credentials::{AccessToken, TokenCredential, TokenRequestOptions},
        http::{AsyncRawResponse, Body, HttpClient, Request, StatusCode, headers::Headers},
        time::OffsetDateTime,
    };
    use fabric_sdk::{Audience, Client, CredentialSelection, items::ItemListOptions};
    use fabric_sdk_http::{ExecutionOptions, ExecutionTimer};
    use std::{
        future::pending,
        sync::{Arc, Mutex},
    };

    #[derive(Debug)]
    struct Credential;

    #[async_trait]
    impl TokenCredential for Credential {
        async fn get_token(
            &self,
            scopes: &[&str],
            _: Option<TokenRequestOptions<'_>>,
        ) -> azure_core::Result<AccessToken> {
            assert_eq!(scopes, [Audience::Fabric.scope()]);
            Ok(AccessToken::new(
                "offline-deploy-only",
                OffsetDateTime::UNIX_EPOCH,
            ))
        }
    }

    #[derive(Debug, Default)]
    struct Http(Mutex<Vec<Request>>);

    #[async_trait]
    impl HttpClient for Http {
        async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
            assert_eq!(
                request.headers().get_str(&"user-agent".into()).unwrap(),
                format!(
                    "ms-fabric-cli/{} (deploy; ms-fabric-cicd/1.4.0)",
                    env!("CARGO_PKG_VERSION")
                )
            );
            assert_eq!(
                request.headers().get_str(&"content-type".into()).unwrap(),
                "application/json; charset=utf-8"
            );
            assert_eq!(
                request.headers().get_str(&"authorization".into()).unwrap(),
                "Bearer offline-deploy-only"
            );
            self.0.lock().unwrap().push(request.clone());
            Ok(AsyncRawResponse::from_bytes(
                StatusCode::Ok,
                Headers::new(),
                br#"{"value":[]}"#.to_vec(),
            ))
        }
    }

    struct Timer;
    impl ExecutionTimer for Timer {
        fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
            Box::pin(pending())
        }
    }

    #[tokio::test]
    async fn actual_shared_http_requests_bind_cli_user_agent_and_source_content_type() {
        let http = Arc::new(Http::default());
        let shared =
            HttpExecutor::new(http.clone(), Arc::new(Timer), ExecutionOptions::default()).unwrap();
        let executor = DeployHttp(&shared);
        let client = Client::new(
            Arc::new(Credential),
            CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        );
        client
            .list_items(
                &executor,
                "11111111-1111-1111-1111-111111111111",
                ItemListOptions::default(),
                pending(),
            )
            .await
            .unwrap();
        let requests = http.0.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0].body(), Body::Bytes(bytes) if bytes.is_empty()));
    }

    #[tokio::test]
    async fn deployment_metadata_adapter_preserves_public_daily_and_msit_contexts() {
        use fabric_sdk::{EndpointBinding, RequestContext, ServiceOrigin};
        for host in [
            "api.fabric.microsoft.com",
            "dailyapi.fabric.microsoft.com",
            "msitapi.fabric.microsoft.com",
        ] {
            let endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap();
            let http = Arc::new(Http::default());
            let shared =
                HttpExecutor::new(http.clone(), Arc::new(Timer), ExecutionOptions::default())
                    .unwrap()
                    .with_endpoint(endpoint.clone());
            let executor = DeployHttp(&shared);
            let client = Client::with_context(RequestContext::with_endpoint(
                Arc::new(Credential),
                CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
                endpoint,
            ));
            client
                .list_items(
                    &executor,
                    "11111111-1111-1111-1111-111111111111",
                    ItemListOptions::default(),
                    pending(),
                )
                .await
                .unwrap();
            let requests = http.0.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].url().host_str(), Some(host));
            assert_eq!(
                requests[0].url().path(),
                "/v1/workspaces/11111111-1111-1111-1111-111111111111/items"
            );
        }
    }
}
