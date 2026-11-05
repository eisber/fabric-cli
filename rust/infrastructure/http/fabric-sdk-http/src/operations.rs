// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{ExecutionError, ExecutionFuture, HttpExecutor, ServiceOrigin};
use azure_core::http::Method;
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationRequest, OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use std::time::Duration;

impl OperationExecutor for HttpExecutor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> ExecutionFuture<'a, Result<OperationResponse, OperationExecutionError>> {
        Box::pin(async move {
            if self.endpoint().origin() != ServiceOrigin::Fabric {
                return Err(operation_error(ExecutionError::InvalidInput));
            }
            let method = match request.method() {
                ResourceMethod::Get => Method::Get,
                ResourceMethod::Post => Method::Post,
                ResourceMethod::Patch => Method::Patch,
                ResourceMethod::Delete => Method::Delete,
            };
            let response = if let Some(accept) = request.accept() {
                self.execute_bound(
                    ServiceOrigin::Fabric,
                    method,
                    request.url(),
                    request.token(),
                    request.body(),
                    &[("accept".into(), accept.as_str().into())],
                    request.max_response_bytes(),
                )
                .await
            } else {
                self.execute_fabric(
                    method,
                    request.url(),
                    request.token(),
                    request.body(),
                    request.max_response_bytes(),
                )
                .await
            };
            response
                .map(|response| OperationResponse {
                    status: response.status,
                    body: response.body,
                    headers: response.headers,
                })
                .map_err(operation_error)
        })
    }

    fn wait(&self, delay: Duration) -> ExecutionFuture<'_, Result<(), OperationExecutionError>> {
        Box::pin(async move { self.wait_for(delay).await.map_err(operation_error) })
    }
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

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;
    use crate::tests::{ScriptedClient, TestTimer, executor, response};
    use async_trait::async_trait;
    use azure_core::{
        credentials::{AccessToken, TokenRequestOptions},
        time::OffsetDateTime,
    };
    use fabric_core::{Audience, CredentialSelection, TokenCredential};
    use fabric_sdk_operations::{
        FabricOperation, OperationAccept, OperationLimits, OperationSession,
    };
    use fabric_sdk_workspaces::mutations::CompletionPolicy;
    use std::{future::pending, sync::Arc};

    const ID: &str = "11111111-1111-1111-1111-111111111111";

    #[derive(Debug)]
    struct Credential;

    #[async_trait]
    impl TokenCredential for Credential {
        async fn get_token(
            &self,
            _: &[&str],
            _: Option<TokenRequestOptions<'_>>,
        ) -> azure_core::Result<AccessToken> {
            Ok(AccessToken::new(
                "offline-operations",
                OffsetDateTime::UNIX_EPOCH,
            ))
        }
    }
}

#[tokio::test]
async fn finite_accept_reaches_initial_and_result_http_but_not_state_polling() {
    for accept in [
        None,
        Some(OperationAccept::ArrowV1),
        Some(OperationAccept::ArrowV2),
    ] {
        let initial_url = format!("https://api.fabric.microsoft.com/v1/operations/{ID}");
        let result_url = format!("{initial_url}/result");
        let client = ScriptedClient::new([
            response(
                202,
                "",
                &[
                    ("x-ms-operation-id", ID),
                    ("Location", &initial_url),
                    ("Retry-After", "0"),
                ],
            ),
            response(
                200,
                r#"{"status":"Succeeded"}"#,
                &[("Location", &result_url)],
            ),
            response(
                200,
                "exact-result",
                &[("Content-Type", "application/vnd.apache.arrow.stream")],
            ),
        ]);
        let credential = Credential;
        let selection = CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap();
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor(client.clone(), Arc::new(TestTimer::default())),
            OperationLimits::default(),
            pending(),
        );
        let operation = FabricOperation::new(
            ResourceMethod::Post,
            "/v1/workspaces/ws/dataflows/item/executeQuery",
            br#"{"queryName":"exact"}"#.to_vec(),
            CompletionPolicy::WaitForResult,
        )
        .unwrap();
        let operation = match accept {
            Some(accept) => operation.with_accept(accept),
            None => operation,
        };
        assert_eq!(
            session.execute(operation).await.unwrap().body,
            b"exact-result"
        );
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        for (index, request) in requests.iter().enumerate() {
            let expected = if index == 1 {
                None
            } else {
                accept.map(OperationAccept::as_str)
            };
            assert_eq!(request.headers().get_str(&"accept".into()).ok(), expected);
            assert_eq!(
                request.headers().get_str(&"content-type".into()).unwrap(),
                "application/json"
            );
            assert!(request.headers().get_str(&"authorization".into()).is_ok());
        }
    }

    #[tokio::test]
    async fn arrow_negotiation_never_weakens_the_fabric_executor_binding() {
        for origin in [
            ServiceOrigin::Arm,
            ServiceOrigin::PowerBi,
            ServiceOrigin::OneLake,
        ] {
            for accept in [
                None,
                Some(OperationAccept::ArrowV1),
                Some(OperationAccept::ArrowV2),
            ] {
                let client = ScriptedClient::new([]);
                let http = executor(client.clone(), Arc::new(TestTimer::default()))
                    .with_endpoint(fabric_core::EndpointBinding::public(origin));
                let credential = Credential;
                let selection =
                    CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap();
                let mut session = OperationSession::new(
                    &credential,
                    &selection,
                    http,
                    OperationLimits::default(),
                    pending(),
                );
                let operation = FabricOperation::new(
                    ResourceMethod::Post,
                    "/v1/workspaces/ws/dataflows/item/executeQuery",
                    br#"{"queryName":"exact"}"#.to_vec(),
                    CompletionPolicy::WaitForResult,
                )
                .unwrap();
                let operation = match accept {
                    Some(accept) => operation.with_accept(accept),
                    None => operation,
                };
                assert!(session.execute(operation).await.is_err());
                assert!(client.requests.lock().unwrap().is_empty());
            }
        }
    }
}
