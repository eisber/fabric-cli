// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::Audience;
use fabric_sdk_folders::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[derive(Debug, Default)]
pub struct Credential {
    pub calls: AtomicUsize,
    pub wait: bool,
    pub fail: bool,
    pub dropped: AtomicBool,
}
pub struct DropGuard<'a>(pub &'a AtomicBool);
impl Drop for DropGuard<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        let _guard = DropGuard(&self.dropped);
        if self.wait {
            pending::<()>().await;
        }
        if self.fail {
            return Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Other,
                "private-provider-diagnostic",
            ));
        }
        Ok(AccessToken::new(
            "offline-folder-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}
pub fn selection() -> CredentialSelection {
    CredentialSelection::new("test-account", "test-tenant", Audience::Fabric).unwrap()
}
pub fn client(credential: Arc<Credential>) -> FolderClient {
    FolderClient::new(credential, selection())
}

#[derive(Default)]
pub struct Executor {
    pub replies: Mutex<VecDeque<Result<FolderResponse, FolderExecutionError>>>,
    pub requests: Mutex<Vec<String>>,
    pub wait: bool,
    pub dropped: AtomicBool,
    pub cancel_on_return: Option<Arc<AtomicBool>>,
}
impl Executor {
    pub fn new(replies: impl IntoIterator<Item = FolderResponse>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().map(Ok).collect()),
            ..Default::default()
        }
    }
}
impl FolderExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: FolderRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<FolderResponse, FolderExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            assert_eq!(request.content_type(), "application/json");
            assert_eq!(request.token().token.secret(), "offline-folder-only");
            let debug = format!("{request:?}");
            assert!(!debug.contains("offline-folder-only"));
            assert!(!debug.contains("continuationToken"));
            assert!(!debug.contains("workspaces/"));
            self.requests
                .lock()
                .unwrap()
                .push(request.url().to_string());
            let _guard = DropGuard(&self.dropped);
            if self.wait {
                pending::<()>().await;
            }
            if let Some(flag) = &self.cancel_on_return {
                flag.store(true, Ordering::SeqCst);
            }
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected executor replay")
        })
    }
}
pub fn response(body: Value) -> FolderResponse {
    FolderResponse {
        status: 200,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}
pub fn page(next: Option<&str>) -> FolderResponse {
    response(json!({"value":[{"id":"a","displayName":"A"}],"continuationToken":next}))
}
pub fn fixture_reply(reply: &Value) -> FolderResponse {
    FolderResponse {
        status: u16::try_from(reply["status"].as_u64().unwrap()).unwrap(),
        body: reply.get("text").map_or_else(
            || serde_json::to_vec(&reply["body"]).unwrap(),
            |text| text.as_str().unwrap().as_bytes().to_vec(),
        ),
        headers: reply
            .get("headers")
            .map(|headers| {
                headers
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
                    .collect()
            })
            .unwrap_or_default(),
    }
}
pub fn query(case: &Value) -> Vec<(String, FolderQueryValue)> {
    case.get("query")
        .map(|pairs| {
            pairs
                .as_array()
                .unwrap()
                .iter()
                .map(|pair| {
                    (
                        pair[0].as_str().unwrap().to_owned(),
                        serde_json::from_value(pair[1].clone()).unwrap(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}
pub fn options(case: &Value) -> FolderListOptions {
    FolderListOptions {
        query: query(case),
        recursive: case
            .get("recursive")
            .map(|value| serde_json::from_value(value.clone()).unwrap()),
        root_folder_id: case
            .get("root_folder_id")
            .map(|value| serde_json::from_value(value.clone()).unwrap()),
        ..Default::default()
    }
}
