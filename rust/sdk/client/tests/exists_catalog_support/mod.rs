// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::exists_support::{self, ITEM, TENANT, WS};
use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    http::Method,
    time::OffsetDateTime,
};
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, ServiceOrigin, TokenCredential,
    api::{ApiClient, ExecutionError, Executor, Request, Response},
    exists::{
        CatalogBackend, ExistsLimits, ExistsReport, ExistsRequest, ExistsTarget, MetadataSelector,
        prepare_exists,
    },
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

pub(crate) const TABLE_HOST: &str = "onelake.table.fabric.microsoft.com";
pub(crate) const PREFIX: &str = "configured/catalog-prefix";

#[derive(Debug, Default)]
pub(crate) struct StorageCredential(pub(crate) AtomicUsize);
#[async_trait]
impl TokenCredential for StorageCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://storage.azure.com/.default"]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "offline-catalog-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

pub(crate) fn config(prefix: &str) -> Value {
    json!({
        "defaults": {},
        "overrides": {"prefix": prefix},
        "endpoints": [
            "HEAD /v1/{prefix}/namespaces/{namespace}",
            "HEAD /v1/{prefix}/namespaces/{namespace}/tables/{table}"
        ]
    })
}
pub(crate) fn response(status: u16, body: Vec<u8>) -> Response {
    Response {
        status,
        body,
        headers: BTreeMap::new(),
    }
}
pub(crate) fn configuration(value: Value) -> Response {
    response(200, serde_json::to_vec(&value).unwrap())
}
pub(crate) fn parent() -> exists_support::Reply {
    exists_support::reply(
        200,
        json!({"id":ITEM,"workspaceId":WS,"displayName":"Lake","type":"Lakehouse"}),
    )
}
pub(crate) fn target(table: bool) -> ExistsTarget {
    if table {
        ExistsTarget::CatalogTable {
            workspace: MetadataSelector::id(WS).unwrap(),
            lakehouse: MetadataSelector::id(ITEM).unwrap(),
            namespace: "dbo".into(),
            table: "Orders".into(),
        }
    } else {
        ExistsTarget::CatalogNamespace {
            workspace: MetadataSelector::id(WS).unwrap(),
            lakehouse: MetadataSelector::id(ITEM).unwrap(),
            namespace: "dbo".into(),
        }
    }
}

#[derive(Default)]
pub(crate) struct CatalogExecutor {
    pub(crate) replies: Mutex<VecDeque<Result<Response, ExecutionError>>>,
    pub(crate) requests: Mutex<Vec<(Method, String, usize)>>,
    pub(crate) stall_at: Option<usize>,
    pub(crate) started: tokio::sync::Notify,
    pub(crate) drops: AtomicUsize,
}
struct Guard<'a>(&'a AtomicUsize);
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl Executor for CatalogExecutor {
    fn supports(&self, request: &Request) -> bool {
        assert_eq!(request.origin(), fabric_sdk::api::Origin::OneLake);
        assert!(request.headers().is_empty());
        assert!(request.body().is_empty());
        true
    }
    fn execute<'a>(
        &'a self,
        request: &'a Request,
        _: &'a AccessToken,
        limit: NonZeroUsize,
    ) -> Pin<Box<dyn Future<Output = Result<Response, ExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            let count = {
                let mut requests = self.requests.lock().unwrap();
                requests.push((request.method(), request.url().to_string(), limit.get()));
                requests.len()
            };
            if self.stall_at == Some(count) {
                let _guard = Guard(&self.drops);
                self.started.notify_one();
                pending::<()>().await;
            }
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected catalog request")
        })
    }
}

pub(crate) struct CatalogFixture {
    pub(crate) metadata: exists_support::Fixture,
    pub(crate) storage: Arc<StorageCredential>,
    pub(crate) executor: Arc<CatalogExecutor>,
    pub(crate) backend: CatalogBackend,
}
impl CatalogFixture {
    pub(crate) fn new(replies: Vec<Response>) -> Self {
        Self::with_executor(
            vec![parent()],
            CatalogExecutor {
                replies: Mutex::new(replies.into_iter().map(Ok).collect()),
                ..CatalogExecutor::default()
            },
        )
    }
    pub(crate) fn with_executor(
        metadata: Vec<exists_support::Reply>,
        executor: CatalogExecutor,
    ) -> Self {
        let storage = Arc::new(StorageCredential::default());
        let executor = Arc::new(executor);
        let backend = CatalogBackend::new(
            ApiClient::new(
                storage.clone(),
                CredentialSelection::new("offline", TENANT, Audience::OneLake).unwrap(),
                executor.clone(),
            ),
            EndpointBinding::with_host(ServiceOrigin::OneLake, TABLE_HOST).unwrap(),
        )
        .unwrap();
        Self {
            metadata: exists_support::Fixture::new(metadata),
            storage,
            executor,
            backend,
        }
    }
    pub(crate) async fn run(&self, target: ExistsTarget, limits: ExistsLimits) -> ExistsReport {
        let request = ExistsRequest::new(TENANT, target).unwrap();
        let prepared = prepare_exists(&request).unwrap();
        let approved = prepared.authorize(&mut exists_support::ReadPolicy).unwrap();
        self.metadata
            .host()
            .with_catalog(&self.backend)
            .execute(&approved, &request, limits, pending())
            .await
            .unwrap()
    }
}
