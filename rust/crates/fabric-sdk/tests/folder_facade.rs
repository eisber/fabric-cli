// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! An external caller consumes the facade and focused folder model as one type.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::Audience;
use fabric_sdk::{Client, CredentialSelection, TokenCredential, folders::*};
use std::{
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

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
        Ok(AccessToken::new(
            "offline-facade-folder",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}
struct Executor;
impl FolderExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: FolderRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<FolderResponse, FolderExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.token().token.secret(), "offline-facade-folder");
            let body = if request.url().path().ends_with("/folders") {
                assert_eq!(
                    request.url().query(),
                    Some("rootFolderId=None&recursive=True")
                );
                r#"{"value":[{"id":"folder","displayName":"F","parentFolderId":null}]}"#
            } else {
                assert!(request.url().query().is_none());
                r#"{"id":"folder","displayName":"F","parentFolderId":null}"#
            };
            Ok(FolderResponse {
                status: 200,
                body: body.as_bytes().to_vec(),
                headers: Default::default(),
            })
        })
    }
}

#[tokio::test]
async fn facade_list_get_share_provider_selection_and_focused_wire_types() {
    let credential = Arc::new(Credential::default());
    let selection = CredentialSelection::new("account", "tenant", Audience::Fabric).unwrap();
    let sdk = Client::new(credential.clone(), selection.clone());
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert_eq!(sdk.selection(), &selection);
    let list = sdk
        .list_folders(
            &Executor,
            "ws",
            FolderListOptions {
                root_folder_id: Some(FolderQueryValue::None),
                ..Default::default()
            },
            pending(),
        )
        .await
        .unwrap();
    let folder: fabric_sdk_folders::Folder = sdk
        .get_folder(&Executor, "ws", "folder", Default::default(), pending())
        .await
        .unwrap();
    let same: fabric_sdk::models::Folder = list.value[0].clone();
    assert_eq!(same, folder);
    assert_eq!(folder.parent_folder_id(), Some(&serde_json::Value::Null));
    assert_eq!(credential.0.load(Ordering::SeqCst), 2);
}
