// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Official native credential selection through the real verifier and HTTP engine.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::Secret,
    http::{AsyncRawResponse, HttpClient, Method, Request, Url, headers::Headers},
};
use fabric_identity::headless::{HeadlessCredential, HeadlessInput, ServiceIdentity};
use fabric_sdk::{
    Audience, CredentialSelection, RequestContext, TokenCredential,
    notebook_runtime::{NotebookRuntime, content::ContentLimits},
    runtime::{NativeDeadline, NativeEndpoint, NativeError, NativeExecutor},
};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor,
    notebook_workload::{
        NativeNotebookExecutor, NotebookEnvironment, NotebookOrigin, NotebookReadScope,
        NotebookWorkloadSelection,
    },
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::pending,
    sync::{Arc, Mutex},
    time::Duration,
};

#[path = "../../../../tests/fixtures/notebook-identity/fixture.rs"]
mod fixture;
use fixture::*;

#[derive(Debug)]
struct Wire {
    replies: Mutex<VecDeque<Value>>,
    requests: Mutex<Vec<Request>>,
}
#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        let body = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unplanned native HTTP or replay");
        Ok(AsyncRawResponse::from_bytes(
            200.into(),
            Headers::new(),
            serde_json::to_vec(&body).unwrap(),
        ))
    }
}
struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(pending())
    }
}
fn token_response(value: String) -> Value {
    json!({"access_token": value, "token_type": "Bearer", "expires_in": 3500})
}
fn wire(replies: impl IntoIterator<Item = Value>) -> Arc<Wire> {
    Arc::new(Wire {
        replies: Mutex::new(replies.into_iter().collect()),
        requests: Mutex::new(Vec::new()),
    })
}
fn input() -> HeadlessInput {
    HeadlessInput::ClientSecret {
        identity: ServiceIdentity::new(TENANT, CLIENT).unwrap(),
        secret: Secret::new("synthetic-offline-client-secret"),
    }
}
fn adapter(credential: Arc<dyn TokenCredential>, wire: Arc<Wire>) -> NativeNotebookExecutor {
    let owner = CredentialSelection::new(CLIENT, TENANT, Audience::Fabric).unwrap();
    let selected = NotebookWorkloadSelection::new(
        owner.clone(),
        NotebookEnvironment::Production,
        NotebookOrigin::new(FABRIC).unwrap(),
        NotebookOrigin::new(CLUSTER).unwrap(),
        NotebookOrigin::new(WORKLOAD).unwrap(),
        NotebookReadScope::new(CAPACITY, WORKSPACE, NOTEBOOK).unwrap(),
    )
    .unwrap();
    let http = HttpExecutor::new(wire, Arc::new(Timer), ExecutionOptions::default()).unwrap();
    NativeNotebookExecutor::new(
        RequestContext::new(credential, owner),
        http,
        selected,
        NativeDeadline::new(Duration::from_secs(60)).unwrap(),
    )
    .unwrap()
}
fn request() -> fabric_sdk::runtime::NativeRequest {
    NotebookRuntime::new(
        NativeEndpoint::new(Url::parse(WORKLOAD).unwrap()).unwrap(),
        CAPACITY,
        WORKSPACE,
        NOTEBOOK,
        ContentLimits::default(),
        4096,
        10,
    )
    .unwrap()
    .content_get_request()
    .unwrap()
}

#[tokio::test]
async fn official_selected_credential_signed_proofs_and_mwc_request_execute_end_to_end_offline() {
    let pbi = token(PBI);
    let wire = wire([
        token_response(token(FABRIC)),
        jwks("offline-key"),
        token_response(pbi.clone()),
        json!({"backendUrl": CLUSTER}),
        grant(),
        json!({"notebook": "offline"}),
    ]);
    let credential = HeadlessCredential::new_with_pbi_bootstrap(input(), wire.clone()).unwrap();
    assert!(wire.requests.lock().unwrap().is_empty());
    let executor = adapter(credential, wire.clone());
    assert_eq!(
        request().execute(&executor, 4096).await.unwrap().status,
        200
    );
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    for (index, scope) in [
        (0, "api.fabric.microsoft.com"),
        (2, "analysis.windows.net%2Fpowerbi%2Fapi"),
    ] {
        assert_eq!(requests[index].method(), Method::Post);
        assert_eq!(
            requests[index].url().as_str(),
            format!("https://login.microsoftonline.com/{TENANT}/oauth2/v2.0/token")
        );
        let body = String::from_utf8(Bytes::from(requests[index].body()).to_vec()).unwrap();
        assert!(body.contains(&format!("scope=https%3A%2F%2F{scope}%2F.default")));
        assert!(body.contains(&format!("client_id={CLIENT}")));
        assert!(body.contains("grant_type=client_credentials"));
    }
    assert!(
        requests[1]
            .headers()
            .get_str(&"authorization".into())
            .is_err()
    );
    assert_eq!(
        requests[3]
            .headers()
            .get_str(&"authorization".into())
            .unwrap(),
        format!("Bearer {pbi}")
    );
    assert_eq!(
        requests[4]
            .headers()
            .get_str(&"authorization".into())
            .unwrap(),
        format!("Bearer {pbi}")
    );
    assert_eq!(
        requests[5]
            .headers()
            .get_str(&"authorization".into())
            .unwrap(),
        format!("MwcToken {MWC}")
    );
    assert_eq!(requests[3].url().path(), "/metadata/cluster");
    assert_eq!(
        requests[4].url().path(),
        "/metadata/v201606/generatemwctoken"
    );
    assert!(requests[5].url().path().ends_with("/content"));
}

#[tokio::test]
async fn native_default_credential_keeps_pbi_disabled_without_fallback_or_bootstrap() {
    let wire = wire([token_response(token(FABRIC)), jwks("offline-key")]);
    let credential = HeadlessCredential::new(input(), wire.clone()).unwrap();
    let executor = adapter(credential, wire.clone());
    assert_eq!(
        executor.execute(&request(), 4096).await.err(),
        Some(NativeError::InvalidResponse)
    );
    assert_eq!(wire.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn native_provider_claims_not_selection_labels_decide_account_and_tenant() {
    for (name, value, count) in [("tid", OTHER, 1), ("appid", OTHER, 2)] {
        let mut c = claims(FABRIC);
        c[name] = json!(value);
        let mut replies = vec![token_response(signed(&header("offline-key"), &c))];
        if count == 2 {
            replies.push(jwks("offline-key"));
        }
        let wire = wire(replies);
        let credential = HeadlessCredential::new_with_pbi_bootstrap(input(), wire.clone()).unwrap();
        let executor = adapter(credential, wire.clone());
        assert_eq!(
            executor.execute(&request(), 4096).await.err(),
            Some(NativeError::InvalidResponse)
        );
        assert_eq!(wire.requests.lock().unwrap().len(), count);
    }
}
