// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Downstream source-compatibility checks without crate-private or cfg(test) helpers.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, Page, TokenCredential,
    models::{FabricServiceError, Workspace},
    workspaces::{
        WorkspaceExecutionError, WorkspaceExecutor, WorkspaceList, WorkspaceListError,
        WorkspaceListErrorKind, WorkspaceListLimits, WorkspaceRequest, WorkspaceResponse,
    },
};
use fabric_sdk_workspaces::WorkspaceClient;
use serde_json::json;
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
};

#[derive(Debug, Default)]
struct OfflineCredential {
    scopes: Mutex<Vec<Vec<String>>>,
}

#[async_trait]
impl TokenCredential for OfflineCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|scope| (*scope).to_owned()).collect());
        Ok(AccessToken::new(
            "offline-facade-test-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Default)]
struct ScriptedExecutor {
    replies: Mutex<VecDeque<Result<WorkspaceResponse, WorkspaceExecutionError>>>,
    requests: Mutex<Vec<(String, usize)>>,
    pending: bool,
    dropped: AtomicUsize,
}

impl WorkspaceExecutor for ScriptedExecutor {
    fn execute<'a>(
        &'a self,
        request: WorkspaceRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            assert_eq!(request.content_type(), "application/json");
            assert_eq!(request.token().token.secret(), "offline-facade-test-only");
            self.requests.lock().unwrap().push((
                request.url().to_string(),
                request.max_response_bytes().get(),
            ));
            if self.pending {
                let _guard = Dropped(&self.dropped);
                pending::<()>().await;
            }
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request")
        })
    }
}

struct Dropped<'a>(&'a AtomicUsize);

impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn selection(audience: Audience) -> CredentialSelection {
    CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap()
}

fn page(id: &str, token: Option<&str>) -> WorkspaceResponse {
    WorkspaceResponse {
        status: 200,
        body: serde_json::to_vec(&json!({
            "value": [{
                "id": id, "displayName": "A/B", "type": "FutureType",
                "capacityId": null, "futureField": {"value": true},
            }],
            "continuationToken": token,
        }))
        .unwrap(),
        headers: BTreeMap::new(),
    }
}

async fn compare(
    replies: Vec<Result<WorkspaceResponse, WorkspaceExecutionError>>,
    limits: WorkspaceListLimits,
) -> (
    Result<WorkspaceList, WorkspaceListError>,
    Vec<(String, usize)>,
) {
    let credential = Arc::new(OfflineCredential::default());
    let facade = Client::new(credential.clone(), selection(Audience::Fabric));
    let direct = WorkspaceClient::new(credential.clone(), selection(Audience::Fabric));
    let facade_executor = ScriptedExecutor {
        replies: Mutex::new(replies.clone().into()),
        ..Default::default()
    };
    let direct_executor = ScriptedExecutor {
        replies: Mutex::new(replies.into()),
        ..Default::default()
    };
    let through_facade = facade
        .list_workspaces(&facade_executor, limits, pending())
        .await;
    let through_direct = direct
        .list_workspaces(&direct_executor, limits, pending())
        .await;
    assert_eq!(through_facade, through_direct);
    let facade_requests = facade_executor.requests.lock().unwrap().clone();
    assert_eq!(facade_requests, *direct_executor.requests.lock().unwrap());
    let scopes = credential.scopes.lock().unwrap();
    assert_eq!(scopes.len(), facade_requests.len() * 2);
    assert!(
        scopes
            .iter()
            .all(|scope| scope == &[Audience::Fabric.scope()])
    );
    (through_facade, facade_requests)
}

#[test]
fn facade_paths_reexport_the_same_wire_and_operation_types() {
    let workspace: Workspace = serde_json::from_value(json!({
        "id": "opaque", "displayName": "A/B", "type": "FutureType", "capacityId": null,
    }))
    .unwrap();
    let direct: fabric_sdk_workspaces::Workspace = workspace;
    let page = Page {
        value: vec![direct],
        continuation_token: None,
        continuation_uri: None,
    };
    let _: fabric_sdk_workspaces::Page<fabric_sdk_workspaces::Workspace> = page;
    let error = FabricServiceError {
        error_code: "Denied".to_owned(),
        message: "untrusted".to_owned(),
        additional_fields: BTreeMap::new(),
    };
    let _: fabric_sdk_workspaces::FabricServiceError = error;
    let facade_error = WorkspaceListError {
        kind: WorkspaceListErrorKind::Cancelled,
        partial: WorkspaceList::default(),
    };
    let _: fabric_sdk_workspaces::WorkspaceListError = facade_error;
}

#[test]
fn construction_clone_accessors_and_debug_keep_credentials_injected_and_redacted() {
    let credential: Arc<dyn TokenCredential> = Arc::new(OfflineCredential::default());
    let selection = selection(Audience::Fabric);
    let facade = Client::new(credential.clone(), selection.clone()).clone();
    let direct = WorkspaceClient::new(credential.clone(), selection.clone()).clone();
    assert!(Arc::ptr_eq(facade.credential(), &credential));
    assert!(Arc::ptr_eq(direct.credential(), &credential));
    assert_eq!(facade.selection(), &selection);
    assert_eq!(direct.selection(), &selection);
    assert!(format!("{facade:?}").starts_with("Client {"));
    assert!(format!("{direct:?}").starts_with("WorkspaceClient {"));
    assert!(!format!("{facade:?} {direct:?}").contains("OfflineCredential"));
}

#[tokio::test(flavor = "current_thread")]
async fn facade_and_focused_client_preserve_identical_paging_requests_and_fields() {
    let (result, requests) = compare(
        vec![Ok(page("a", Some("opaque+/= next"))), Ok(page("a", None))],
        WorkspaceListLimits::default(),
    )
    .await;
    let result = result.unwrap();
    assert_eq!(
        result
            .value
            .iter()
            .map(|w| w.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "a"]
    );
    assert_eq!(result.value[0].workspace_type, "FutureType");
    assert_eq!(
        result.value[0].additional_fields["capacityId"],
        serde_json::Value::Null
    );
    assert_eq!(result.pages_received, 2);
    assert_eq!(result.next_token, None);
    assert_eq!(
        requests[1].0,
        "https://api.fabric.microsoft.com/v1/workspaces?continuationToken=opaque%2B%2F%3D+next"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn facade_preserves_error_details_partial_results_and_limit_arguments() {
    let mut invalid = page("unused", None);
    invalid.status = 403;
    invalid.body = br#"{"errorCode":"BodyCode","message":"untrusted"}"#.to_vec();
    invalid.headers.insert(
        "x-ms-public-api-error-code".to_owned(),
        "HeaderCode".to_owned(),
    );
    let scripts = [
        (
            Ok(invalid),
            WorkspaceListErrorKind::Http {
                status: 403,
                code: "HeaderCode".to_owned(),
                service_error: Some(Box::new(FabricServiceError {
                    error_code: "BodyCode".to_owned(),
                    message: "untrusted".to_owned(),
                    additional_fields: BTreeMap::new(),
                })),
            },
        ),
        (
            Err(WorkspaceExecutionError::TimedOut),
            WorkspaceListErrorKind::Execution(WorkspaceExecutionError::TimedOut),
        ),
        (
            Ok(WorkspaceResponse {
                status: 200,
                body: b"{}".to_vec(),
                headers: BTreeMap::new(),
            }),
            WorkspaceListErrorKind::InvalidResponse,
        ),
    ];
    let limits = WorkspaceListLimits {
        max_pages: NonZeroUsize::new(4).unwrap(),
        max_items: NonZeroUsize::new(8).unwrap(),
        max_response_bytes: NonZeroUsize::new(2048).unwrap(),
    };
    for (second, expected) in scripts {
        let (result, requests) = compare(vec![Ok(page("a", Some("next"))), second], limits).await;
        let error = result.unwrap_err();
        assert_eq!(error.kind, expected);
        assert_eq!(error.partial.value.len(), 1);
        assert_eq!(error.partial.pages_received, 1);
        assert_eq!(error.partial.next_token.as_deref(), Some("next"));
        assert!(requests.iter().all(|(_, bytes)| *bytes == 2048));
    }
    let limits = WorkspaceListLimits {
        max_pages: NonZeroUsize::new(1).unwrap(),
        ..limits
    };
    let (result, requests) = compare(vec![Ok(page("a", Some("next")))], limits).await;
    assert_eq!(result.unwrap_err().kind, WorkspaceListErrorKind::PageLimit);
    assert_eq!(requests.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn facade_does_not_acquire_credentials_for_invalid_audiences_or_pre_cancelled_calls() {
    for audience in [Audience::OneLake, Audience::Arm] {
        let credential = Arc::new(OfflineCredential::default());
        let facade = Client::new(credential.clone(), selection(audience));
        let executor = ScriptedExecutor::default();
        let error = facade
            .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, WorkspaceListErrorKind::InvalidAudience);
        assert!(credential.scopes.lock().unwrap().is_empty());
        assert!(executor.requests.lock().unwrap().is_empty());
    }
    let credential = Arc::new(OfflineCredential::default());
    let facade = Client::new(credential.clone(), selection(Audience::Fabric));
    let executor = ScriptedExecutor::default();
    let error = facade
        .list_workspaces(&executor, WorkspaceListLimits::default(), ready(()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::Cancelled);
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_a_pending_facade_call_drops_the_in_flight_executor() {
    let credential = Arc::new(OfflineCredential::default());
    let facade = Client::new(credential, selection(Audience::Fabric));
    let executor = ScriptedExecutor {
        pending: true,
        ..Default::default()
    };
    let mut operation =
        Box::pin(facade.list_workspaces(&executor, WorkspaceListLimits::default(), pending()));
    poll_fn(|cx| {
        assert!(operation.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
    assert_eq!(executor.dropped.load(Ordering::SeqCst), 0);
    drop(operation);
    assert_eq!(executor.dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn facade_future_remains_send_after_delegation() {
    fn assert_send(_: impl Send) {}
    let facade = Client::new(
        Arc::new(OfflineCredential::default()),
        selection(Audience::Fabric),
    );
    let executor = ScriptedExecutor::default();
    assert_send(facade.list_workspaces(&executor, WorkspaceListLimits::default(), pending()));
}

#[tokio::test(flavor = "current_thread")]
async fn metadata_facade_matches_focused_sdk_with_complete_ordered_enrichments() {
    let credential = Arc::new(OfflineCredential::default());
    let facade = Client::new(credential.clone(), selection(Audience::Fabric));
    let direct = WorkspaceClient::new(credential.clone(), selection(Audience::Fabric));
    let replies = [
        json!({"id":"w","displayName":"W","unknown":null}),
        json!({"value":[{"future":1}],"continuationToken":"next"}),
        json!({"value":[]}),
        json!({"spark":false}),
        json!({"value":[]}),
    ]
    .map(|value| {
        Ok(WorkspaceResponse {
            status: 200,
            body: serde_json::to_vec(&value).unwrap(),
            headers: BTreeMap::new(),
        })
    });
    let executor = || ScriptedExecutor {
        replies: Mutex::new(replies.clone().into()),
        ..Default::default()
    };
    let left = executor();
    let right = executor();
    let through_facade = facade
        .get_workspace_metadata(&left, "w", WorkspaceListLimits::default(), pending())
        .await
        .unwrap();
    let through_direct = direct
        .get_workspace_metadata(&right, "w", WorkspaceListLimits::default(), pending())
        .await
        .unwrap();
    assert_eq!(through_facade, through_direct);
    assert_eq!(
        *left.requests.lock().unwrap(),
        *right.requests.lock().unwrap()
    );
    assert_eq!(through_facade.enrichments.len(), 3);
    let _: fabric_sdk::workspaces::WorkspaceMetadata = through_direct.metadata;
    assert_eq!(credential.scopes.lock().unwrap().len(), 10);
}
