// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline public published-read and authenticated executor contracts.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin, TokenCredential,
};
use fabric_sdk_dataagent_ontology::{
    private::Limits,
    public::PublicClient,
    result::{ErrorKind, Outcome},
    scope::*,
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationRequest, OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::json;
use std::{
    collections::BTreeMap,
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

fn id(number: u8) -> Guid {
    Guid::new(format!("11111111-2222-3333-4444-{number:012}")).unwrap()
}

#[derive(Debug, Default)]
struct Credential(AtomicUsize);
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken {
            token: "offline-public-fixture".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}

#[derive(Clone)]
struct Executor {
    seen: Arc<Mutex<Vec<String>>>,
    response: OperationResponse,
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.method(), ResourceMethod::Get);
            assert!(request.body().is_empty());
            assert_eq!(request.token().token.secret(), "offline-public-fixture");
            self.seen.lock().unwrap().push(request.url().to_string());
            Ok(self.response.clone())
        })
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("public read must not install a poller")
    }
}

fn sdk(credential: Arc<Credential>, audience: Audience) -> PublicClient {
    let selection =
        CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap();
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap();
    PublicClient::new(
        RequestContext::with_endpoint(credential, selection, endpoint),
        AgentScope {
            workspace_id: id(1),
            agent_id: id(2),
        },
        Limits::default(),
    )
    .unwrap()
}

fn executor(status: u16, body: serde_json::Value) -> Executor {
    Executor {
        seen: Arc::new(Mutex::new(Vec::new())),
        response: OperationResponse {
            status,
            body: serde_json::to_vec(&body).unwrap(),
            headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
        },
    }
}

#[tokio::test]
async fn public_published_routes_use_existing_authenticated_policy_and_explicit_endpoint() {
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential.clone(), Audience::Fabric);
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    let executor = executor(200, json!({"value": []}));
    let Outcome::Completed(list) = sdk
        .execute(sdk.list_published().unwrap(), executor.clone(), pending())
        .await
        .unwrap()
    else {
        panic!("not completed")
    };
    assert!(list.value.value.is_empty());
    assert!(list.effects.may_migrate_configuration);
    let root = "returned+/= %2F";
    sdk.execute(
        sdk.published_elements(id(3), Some(root)).unwrap(),
        executor.clone(),
        pending(),
    )
    .await
    .unwrap();
    let seen = executor.seen.lock().unwrap();
    assert_eq!(
        seen[0],
        format!(
            "https://dailyapi.fabric.microsoft.com/v1/workspaces/{}/dataagents/{}/ontologies",
            id(1),
            id(2)
        )
    );
    assert_eq!(
        seen[1],
        format!(
            "https://dailyapi.fabric.microsoft.com/v1/workspaces/{}/dataagents/{}/ontologies/{}/elements?rootId=returned%2B%2F%3D%20%252F",
            id(1),
            id(2),
            id(3)
        )
    );
    assert!(
        seen.iter()
            .all(|url| !url.contains("/staging/") && !url.contains("/responses"))
    );
    assert_eq!(credential.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn public_failures_and_malformed_rows_never_fall_back_to_private_or_runtime() {
    let sdk = sdk(Arc::new(Credential::default()), Audience::Fabric);
    let rejected = executor(
        403,
        json!({"error": {"code": "native-denied", "message": "source data"}}),
    );
    let error = sdk
        .execute(sdk.list_published().unwrap(), rejected.clone(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Forbidden);
    assert!(error.public_error.unwrap().raw_service_error.is_some());
    assert_eq!(rejected.seen.lock().unwrap().len(), 1);
    let malformed = executor(200, json!({"value": [{}]}));
    let error = sdk
        .execute(sdk.list_published().unwrap(), malformed.clone(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidResponse);
    assert!(error.response.is_some());
    assert_eq!(malformed.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn public_audience_mismatch_precedes_credential_access_and_dispatch() {
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential.clone(), Audience::OneLake);
    let executor = executor(200, json!({"value": []}));
    let error = sdk
        .execute(sdk.list_published().unwrap(), executor.clone(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Execution);
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(executor.seen.lock().unwrap().is_empty());
}
