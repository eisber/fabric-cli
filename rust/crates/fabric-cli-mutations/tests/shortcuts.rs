// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shortcut metadata really consumes Fabric backend, not the OneLake storage adapter.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_mutations::FabricMutationBackend;
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor, PublicOrigin,
};
use fabric_sdk_onelake::shortcuts::{ShortcutConflictPolicy, ShortcutMutations, ShortcutPath};
use fabric_sdk_operations::OperationLimits;
use fabric_sdk_workspaces::CredentialSelection;
use serde_json::json;
use std::{
    collections::VecDeque,
    future::pending,
    sync::{Arc, Mutex},
    time::Duration,
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
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        Ok(AccessToken::new(
            "offline-fabric-shortcut",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}
#[derive(Debug)]
struct Network {
    replies: Mutex<VecDeque<AsyncRawResponse>>,
    requests: Mutex<Vec<Request>>,
}
#[async_trait]
impl HttpClient for Network {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected shortcut request"))
    }
}
struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        assert_eq!(delay, Duration::from_secs(240));
        Box::pin(pending())
    }
}
fn response(status: u16, body: &str) -> AsyncRawResponse {
    AsyncRawResponse::from_bytes(
        StatusCode::from(status),
        Headers::new(),
        body.as_bytes().to_vec(),
    )
}

#[tokio::test]
async fn get_and_delete_use_encoded_metadata_route_with_fabric_scope_and_actual_acceptance() {
    let network = Arc::new(Network {
        replies: Mutex::new(VecDeque::from([
            response(200, r#"{"name":"Chosen","target":{"type":"OneLake"}}"#),
            response(202, ""),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let http = HttpExecutor::new(
        network.clone(),
        Arc::new(Timer),
        ExecutionOptions::default(),
    )
    .unwrap();
    let mut backend = FabricMutationBackend::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "tenant", PublicOrigin::Fabric.audience()).unwrap(),
        &http,
        OperationLimits::default(),
        pending(),
    );
    let path = ShortcutPath::new("ws", "item", "Files/a b", "Chosen\u{65e5}").unwrap();
    let value = ShortcutMutations::get(&mut backend, &path).await.unwrap();
    assert!(
        matches!(value,fabric_sdk_workspaces::mutations::ResourceOutcome::Completed { status:200,data:Some(data) } if data["name"]=="Chosen")
    );
    assert!(matches!(
        ShortcutMutations::delete(&mut backend, &path)
            .await
            .unwrap(),
        fabric_sdk_workspaces::mutations::ResourceOutcome::Accepted { .. }
    ));
    let requests = network.requests.lock().unwrap();
    assert_eq!(
        requests.iter().map(Request::method).collect::<Vec<_>>(),
        [Method::Get, Method::Delete]
    );
    assert_eq!(
        requests[0].url().as_str(),
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/shortcuts/Files/a%20b/Chosen%E6%97%A5"
    );
}

#[tokio::test]
async fn overwrite_only_is_a_finite_query_not_an_arbitrary_url_or_implicit_storage_write() {
    let network = Arc::new(Network {
        replies: Mutex::new(VecDeque::from([response(200, r#"{"name":"Chosen"}"#)])),
        requests: Mutex::new(Vec::new()),
    });
    let http = HttpExecutor::new(
        network.clone(),
        Arc::new(Timer),
        ExecutionOptions::default(),
    )
    .unwrap();
    let mut backend = FabricMutationBackend::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "tenant", PublicOrigin::Fabric.audience()).unwrap(),
        &http,
        OperationLimits::default(),
        pending(),
    );
    ShortcutMutations::create(&mut backend,"ws","item",json!({"name":"Chosen","path":"Files","target":{"oneLake":{"workspaceId":"target-ws","itemId":"target-item","path":"Files"}}}),ShortcutConflictPolicy::OverwriteOnly).await.unwrap();
    assert_eq!(
        network.requests.lock().unwrap()[0].url().query(),
        Some("shortcutConflictPolicy=OverwriteOnly")
    );
}

#[test]
fn unsafe_shortcut_routes_fail_before_any_provider_or_network_is_selected() {
    for (folder, name) in [
        ("Files/../secret", "Chosen"),
        ("Files", "../target"),
        ("", "Chosen"),
        ("Files/a\\b", "Chosen"),
    ] {
        assert!(ShortcutPath::new("ws", "item", folder, name).is_err());
    }
}
