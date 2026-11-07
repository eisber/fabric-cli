// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public metadata operation consumers with offline credentials and transport.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    TokenCredential,
    items::{ItemErrorKind, ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse},
    metadata::{MetadataError, MetadataHost, MetadataLimits, MetadataTarget},
    workspaces::{
        WorkspaceExecutionError, WorkspaceExecutor, WorkspaceListErrorKind, WorkspaceListLimits,
        WorkspaceMetadataErrorKind, WorkspaceRequest, WorkspaceResponse,
    },
};
use fabric_sdk_workspaces::WorkspaceClient;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
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
    calls: Mutex<Vec<Vec<String>>>,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.calls
            .lock()
            .unwrap()
            .push(scopes.iter().map(|value| (*value).to_owned()).collect());
        Ok(AccessToken::new(
            "offline-metadata-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

struct DropGuard<'a>(&'a AtomicUsize);

impl Drop for DropGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct Executor {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<(String, usize)>>,
    wait: bool,
    started: tokio::sync::Notify,
    dropped: AtomicUsize,
}

impl Executor {
    fn response(status: u16, body: &[u8]) -> Self {
        Self {
            replies: Mutex::new(VecDeque::from([Reply {
                status,
                body: body.to_vec(),
            }])),
            ..Self::default()
        }
    }

    fn record(&self, url: &str, bytes: usize) {
        self.requests.lock().unwrap().push((url.to_owned(), bytes));
    }

    fn next(&self) -> Reply {
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra request")
    }

    async fn wait_if_requested(&self) {
        if self.wait {
            let _guard = DropGuard(&self.dropped);
            self.started.notify_one();
            pending::<()>().await;
        }
    }
}

impl ItemExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            self.record(request.url().as_str(), request.max_response_bytes().get());
            self.wait_if_requested().await;
            let reply = self.next();
            Ok(ItemResponse {
                status: reply.status,
                body: reply.body,
                headers: BTreeMap::new(),
            })
        })
    }
}

impl WorkspaceExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: WorkspaceRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            self.record(request.url().as_str(), request.max_response_bytes().get());
            self.wait_if_requested().await;
            let reply = self.next();
            Ok(WorkspaceResponse {
                status: reply.status,
                body: reply.body,
                headers: BTreeMap::new(),
            })
        })
    }
}

fn client(credential: Arc<Credential>, audience: Audience) -> Client {
    Client::new(
        credential,
        CredentialSelection::new("offline", "tenant", audience).unwrap(),
    )
}

fn limits(result: usize) -> MetadataLimits {
    MetadataLimits {
        max_response_bytes: NonZeroUsize::new(4096).unwrap(),
        max_result_bytes: NonZeroUsize::new(result).unwrap(),
    }
}

fn item() -> MetadataTarget<'static> {
    MetadataTarget::Item {
        workspace_id: "ws",
        item_id: "item",
    }
}

fn workspace() -> MetadataTarget<'static> {
    MetadataTarget::Workspace { workspace_id: "ws" }
}

fn data() -> Vec<u8> {
    br#"{"future":{"id":null,"n":1.0},"id":"item","displayName":"A","type":null,"empty":[]}"#
        .to_vec()
}

#[tokio::test]
async fn item_reads_one_generic_endpoint_and_preserves_full_metadata() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone(), Audience::Fabric);
    let executor = Executor::response(200, &data());
    let host = MetadataHost {
        client: &client,
        items: &executor,
        workspaces: &executor,
    };
    let read = host.read(item(), limits(4096), pending()).await.unwrap();
    assert_eq!(read.target(), item());
    assert_eq!(read.json_bytes(), data());
    assert_eq!(read.data()["future"]["id"], Value::Null);
    assert_eq!(
        executor.requests.lock().unwrap().as_slice(),
        &[(
            "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item".into(),
            4096
        )]
    );
    assert_eq!(
        credential.calls.lock().unwrap().as_slice(),
        &[vec!["https://api.fabric.microsoft.com/.default".to_owned()]]
    );
    let debug = format!("{read:?}");
    assert!(!debug.contains("future") && !debug.contains("offline-metadata-only"));
}

#[tokio::test]
async fn workspace_reads_primary_only_and_matches_focused_sdk() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone(), Audience::Fabric);
    let body = br#"{"id":"ws","displayName":"Workspace","future":null}"#;
    let executor = Executor::response(200, body);
    let host = MetadataHost {
        client: &client,
        items: &executor,
        workspaces: &executor,
    };
    let read = host
        .read(workspace(), limits(4096), pending())
        .await
        .unwrap();
    assert_eq!(read.json_bytes(), body);
    assert_eq!(
        executor.requests.lock().unwrap().as_slice(),
        &[(
            "https://api.fabric.microsoft.com/v1/workspaces/ws".into(),
            4096
        )]
    );
    let direct = WorkspaceClient::with_context(client.context().clone());
    let direct_executor = Executor::response(200, body);
    let metadata = direct
        .get_workspace_primary_metadata(
            &direct_executor,
            "ws",
            WorkspaceListLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(read.data(), metadata.as_value());
    assert_eq!(direct_executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn exact_serialized_limit_succeeds_and_one_less_fails_without_truncation() {
    for (cap, success) in [(data().len(), true), (data().len() - 1, false)] {
        let client = client(Arc::new(Credential::default()), Audience::Fabric);
        let executor = Executor::response(200, &data());
        let host = MetadataHost {
            client: &client,
            items: &executor,
            workspaces: &executor,
        };
        let read = host.read(item(), limits(cap), pending()).await;
        if success {
            assert_eq!(read.unwrap().json_bytes(), data());
        } else {
            assert!(matches!(read, Err(MetadataError::BudgetExceeded)));
        }
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn response_budget_is_enforced_before_json_decoding() {
    let client = client(Arc::new(Credential::default()), Audience::Fabric);
    let executor = Executor::response(200, &data());
    let host = MetadataHost {
        client: &client,
        items: &executor,
        workspaces: &executor,
    };
    let mut bounds = limits(4096);
    bounds.max_response_bytes = NonZeroUsize::new(data().len() - 1).unwrap();
    let error = host.read(item(), bounds, pending()).await.unwrap_err();
    assert!(
        matches!(error, MetadataError::Item(error) if error.kind == ItemErrorKind::ResponseLimit)
    );
}

#[tokio::test]
async fn invalid_identifiers_and_audience_fail_before_credentials_or_http() {
    for target in [
        MetadataTarget::Workspace {
            workspace_id: "../ws",
        },
        MetadataTarget::Item {
            workspace_id: "ws",
            item_id: "item?getDefinition",
        },
    ] {
        let credential = Arc::new(Credential::default());
        let client = client(credential.clone(), Audience::Fabric);
        let executor = Executor::default();
        let host = MetadataHost {
            client: &client,
            items: &executor,
            workspaces: &executor,
        };
        assert!(host.read(target, limits(4096), pending()).await.is_err());
        assert!(credential.calls.lock().unwrap().is_empty());
        assert!(executor.requests.lock().unwrap().is_empty());
    }
    for target in [item(), workspace()] {
        let credential = Arc::new(Credential::default());
        let client = client(credential.clone(), Audience::OneLake);
        let executor = Executor::default();
        let host = MetadataHost {
            client: &client,
            items: &executor,
            workspaces: &executor,
        };
        assert!(host.read(target, limits(4096), pending()).await.is_err());
        assert!(credential.calls.lock().unwrap().is_empty());
        assert!(executor.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn failed_and_incomplete_reads_are_not_metadata_success() {
    for status in [202, 204, 401, 403, 404, 429, 500] {
        for target in [item(), workspace()] {
            let client = client(Arc::new(Credential::default()), Audience::Fabric);
            let executor =
                Executor::response(status, br#"{"errorCode":"Denied","message":"private"}"#);
            let host = MetadataHost {
                client: &client,
                items: &executor,
                workspaces: &executor,
            };
            let error = host
                .read(target, limits(4096), pending())
                .await
                .unwrap_err();
            if let MetadataError::Item(ref error) = error
                && let ItemErrorKind::Http { status: actual, .. } = error.kind
            {
                assert_eq!(actual, status);
            }
            assert!(!error.to_string().contains("private"));
            assert_eq!(executor.requests.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn malformed_objects_and_continuations_fail_without_extra_reads() {
    for body in [
        json!({}),
        json!({"id":"item","displayName":"A","continuationToken":"next"}),
        json!({"id":"item","displayName":null}),
        json!([]),
    ] {
        for target in [item(), workspace()] {
            let client = client(Arc::new(Credential::default()), Audience::Fabric);
            let executor = Executor::response(200, &serde_json::to_vec(&body).unwrap());
            let host = MetadataHost {
                client: &client,
                items: &executor,
                workspaces: &executor,
            };
            assert!(host.read(target, limits(4096), pending()).await.is_err());
            assert_eq!(executor.requests.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn pre_cancelled_reads_do_not_acquire_credentials() {
    for target in [item(), workspace()] {
        let credential = Arc::new(Credential::default());
        let client = client(credential.clone(), Audience::Fabric);
        let executor = Executor::default();
        let host = MetadataHost {
            client: &client,
            items: &executor,
            workspaces: &executor,
        };
        assert!(host.read(target, limits(4096), ready(())).await.is_err());
        assert!(credential.calls.lock().unwrap().is_empty());
        assert!(executor.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn in_flight_cancellation_drops_the_one_read_without_followups() {
    for target in [item(), workspace()] {
        let client = client(Arc::new(Credential::default()), Audience::Fabric);
        let executor = Executor {
            wait: true,
            ..Executor::default()
        };
        let host = MetadataHost {
            client: &client,
            items: &executor,
            workspaces: &executor,
        };
        let (send, receive) = tokio::sync::oneshot::channel::<()>();
        let cancellation = async {
            let _ = receive.await;
        };
        let work = host.read(target, limits(4096), cancellation);
        let stop = async {
            executor.started.notified().await;
            send.send(()).unwrap();
        };
        let (result, ()) = tokio::join!(work, stop);
        match result.unwrap_err() {
            MetadataError::Item(error) => assert_eq!(error.kind, ItemErrorKind::Cancelled),
            MetadataError::Workspace(error) => assert_eq!(
                error.kind,
                WorkspaceMetadataErrorKind::Read(WorkspaceListErrorKind::Cancelled),
            ),
            other => panic!("unexpected failure: {other}"),
        }
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
        assert_eq!(executor.dropped.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn configured_endpoint_is_preserved_without_changing_audience() {
    let credential = Arc::new(Credential::default());
    let context = RequestContext::with_endpoint(
        credential.clone(),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        EndpointBinding::with_host(ServiceOrigin::Fabric, "daily.fabric.microsoft.com").unwrap(),
    );
    let client = Client::with_context(context);
    let executor = Executor::response(200, &data());
    let host = MetadataHost {
        client: &client,
        items: &executor,
        workspaces: &executor,
    };
    host.read(item(), limits(4096), pending()).await.unwrap();
    assert_eq!(
        executor.requests.lock().unwrap()[0].0,
        "https://daily.fabric.microsoft.com/v1/workspaces/ws/items/item"
    );
    assert_eq!(
        credential.calls.lock().unwrap()[0],
        vec!["https://api.fabric.microsoft.com/.default".to_owned()]
    );
}
