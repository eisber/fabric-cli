// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, TokenCredential,
    exists::{
        ExistsHost, ExistsLimits, ExistsReport, ExistsRequest, ExistsTarget, MetadataSelector,
        prepare_exists,
    },
    items::{ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse},
    v2::{ApprovalPolicy, Effect, PreparedOperation},
    workspaces::{WorkspaceExecutionError, WorkspaceExecutor, WorkspaceRequest, WorkspaceResponse},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

pub(crate) const TENANT: &str = "aaaaaaaa-0000-1111-2222-bbbbbbbbbbbb";
pub(crate) const WS: &str = "bbbbbbbb-1111-2222-3333-cccccccccccc";
pub(crate) const ITEM: &str = "cccccccc-2222-3333-4444-dddddddddddd";
pub(crate) const OTHER: &str = "dddddddd-3333-4444-5555-eeeeeeeeeeee";

#[derive(Debug, Default)]
pub(crate) struct Credential(pub(crate) AtomicUsize);
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "offline-exists-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
    pub(crate) headers: BTreeMap<String, String>,
}
pub(crate) fn reply(status: u16, value: Value) -> Reply {
    Reply {
        status,
        body: serde_json::to_vec(&value).unwrap(),
        headers: BTreeMap::new(),
    }
}
pub(crate) fn item_data() -> Value {
    json!({"id":ITEM,"workspaceId":WS,"displayName":"Note","type":"Notebook"})
}
pub(crate) fn workspace_data() -> Value {
    json!({"id":WS,"displayName":"Sales","type":"Workspace"})
}

#[derive(Default)]
pub(crate) struct Executor {
    pub(crate) replies: Mutex<VecDeque<Reply>>,
    pub(crate) requests: Mutex<Vec<String>>,
    pub(crate) stall: bool,
    pub(crate) started: tokio::sync::Notify,
    pub(crate) drops: AtomicUsize,
}

struct Guard<'a>(&'a AtomicUsize);
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Executor {
    pub(crate) fn new(replies: Vec<Reply>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            ..Self::default()
        }
    }
    async fn response(&self, url: &str) -> Reply {
        self.requests.lock().unwrap().push(url.into());
        if self.stall {
            let _guard = Guard(&self.drops);
            self.started.notify_one();
            pending::<()>().await;
        }
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra Exists request")
    }
}

impl ItemExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            let response = self.response(request.url().as_str()).await;
            Ok(ItemResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
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
            let response = self.response(request.url().as_str()).await;
            Ok(WorkspaceResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
        })
    }
}

pub(crate) struct ReadPolicy;
impl ApprovalPolicy for ReadPolicy {
    type Error = ();
    fn authorize(&mut self, operation: &PreparedOperation) -> Result<(), ()> {
        assert_eq!(operation.effects(), [Effect::RemoteRead]);
        assert!(operation.preconditions().is_empty());
        Ok(())
    }
}

pub(crate) struct Fixture {
    pub(crate) credential: Arc<Credential>,
    pub(crate) client: Client,
    pub(crate) executor: Executor,
}
impl Fixture {
    pub(crate) fn new(replies: Vec<Reply>) -> Self {
        let credential = Arc::new(Credential::default());
        let client = Client::new(
            credential.clone(),
            CredentialSelection::new("offline", TENANT, Audience::Fabric).unwrap(),
        );
        Self {
            credential,
            client,
            executor: Executor::new(replies),
        }
    }
    pub(crate) fn host(&self) -> ExistsHost<'_> {
        ExistsHost {
            client: &self.client,
            items: &self.executor,
            workspaces: &self.executor,
        }
    }
    pub(crate) async fn run(&self, target: ExistsTarget, limits: ExistsLimits) -> ExistsReport {
        let request = ExistsRequest::new(TENANT, target).unwrap();
        let prepared = prepare_exists(&request).unwrap();
        let approved = prepared.authorize(&mut ReadPolicy).unwrap();
        self.host()
            .execute(&approved, &request, limits, pending())
            .await
            .unwrap()
    }
}
pub(crate) fn item_target() -> ExistsTarget {
    ExistsTarget::Item {
        workspace: MetadataSelector::id(WS).unwrap(),
        item: MetadataSelector::id(ITEM).unwrap(),
        item_type: Some("Notebook".into()),
    }
}
pub(crate) fn named_item() -> ExistsTarget {
    ExistsTarget::Item {
        workspace: MetadataSelector::id(WS).unwrap(),
        item: MetadataSelector::name("Note").unwrap(),
        item_type: Some("Notebook".into()),
    }
}
