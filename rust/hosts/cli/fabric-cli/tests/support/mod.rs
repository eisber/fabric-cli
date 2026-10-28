// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_sdk::{
    Client, CredentialSelection, TokenCredential,
    capacities::{CapacityExecutionError, CapacityExecutor, CapacityRequest, CapacityResponse},
    items::{ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse},
    workspaces::{WorkspaceExecutionError, WorkspaceExecutor, WorkspaceRequest, WorkspaceResponse},
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    pin::{Pin, pin},
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

#[derive(Debug, Default)]
pub(crate) struct Credential {
    pub(crate) scopes: Mutex<Vec<Vec<String>>>,
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
            .push(scopes.iter().map(|s| (*s).into()).collect());
        Ok(AccessToken::new(
            "offline-command-test",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Clone, Deserialize)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: Value,
    #[serde(default)]
    pub(crate) headers: BTreeMap<String, String>,
}

pub(crate) struct Backend {
    pub(crate) replies: Mutex<VecDeque<Reply>>,
    pub(crate) requests: Mutex<Vec<String>>,
}

impl Backend {
    pub(crate) fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::new(vec![]),
        }
    }

    fn take(&self, url: &str, token: &AccessToken) -> WorkspaceResponse {
        assert_eq!(token.token.secret(), "offline-command-test");
        self.requests.lock().unwrap().push(url.into());
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected SDK request");
        WorkspaceResponse {
            status: reply.status,
            body: match reply.body {
                Value::String(raw) => raw.into_bytes(),
                value => serde_json::to_vec(&value).unwrap(),
            },
            headers: reply.headers,
        }
    }
}

impl WorkspaceExecutor for Backend {
    fn execute<'a>(
        &'a self,
        request: WorkspaceRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            Ok(self.take(request.url().as_str(), request.token()))
        })
    }
}

impl ItemExecutor for Backend {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            let response = self.take(request.url().as_str(), request.token());
            Ok(ItemResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
        })
    }
}

impl CapacityExecutor for Backend {
    fn execute<'a>(
        &'a self,
        request: CapacityRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<CapacityResponse, CapacityExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            Ok(self.take(request.url().as_str(), request.token()))
        })
    }
}

pub(crate) fn sdk(credential: Arc<Credential>) -> Client {
    Client::new(
        credential,
        CredentialSelection::new(
            "offline-account",
            "offline-tenant",
            fabric_sdk::Audience::Fabric,
        )
        .unwrap(),
    )
}

pub(crate) fn complete<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("the offline stdlib test backend must complete without a runtime"),
    }
}

pub(crate) fn reply(body: Value) -> Reply {
    Reply {
        status: 200,
        body,
        headers: BTreeMap::new(),
    }
}
