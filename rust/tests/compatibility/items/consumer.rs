// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::Audience;
use std::{
    future::{Future, pending},
    pin::Pin,
    sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}},
};

#[derive(Debug, Default)]
struct ConsumerCredential(AtomicUsize);

#[async_trait]
impl TokenCredential for ConsumerCredential {
    async fn get_token(&self, scopes: &[&str], _: Option<TokenRequestOptions<'_>>) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken {
            token: "offline-consumer-token".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}

#[derive(Default)]
struct ConsumerExecutor(Mutex<Vec<String>>);

impl ItemExecutor for ConsumerExecutor {
    fn execute<'a>(&'a self, request: ItemRequest<'a>) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(request.token().token.secret(), "offline-consumer-token");
            self.0.lock().unwrap().push(request.url().to_string());
            let body = if request.url().path().ends_with("/items") {
                br#"{"value":[{"id":"opaque","displayName":"Name","type":"Future","extra":null}]}"#.to_vec()
            } else {
                br#"{"id":"opaque","displayName":"Name","type":"Future","extra":null}"#.to_vec()
            };
            Ok(ItemResponse { status:200, body, headers:Default::default() })
        })
    }
}

#[tokio::test]
async fn public_consumer_lists_and_gets_with_the_same_injected_credential() {
    let credential = Arc::new(ConsumerCredential::default());
    let selection = CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap();
    let sdk = ConsumerClient::new(credential.clone(), selection.clone());
    assert_eq!(sdk.selection(), &selection);
    assert_eq!(credential.0.load(Ordering::SeqCst),0);
    let executor = ConsumerExecutor::default();
    let list = sdk.list_items(&executor,"ws",ItemListOptions::default(),pending()).await.unwrap();
    let item = sdk.get_item(&executor,"ws","opaque",ItemGetOptions::default(),pending()).await.unwrap();
    assert_eq!(list.value,[item]);
    assert_eq!(list.value[0].item_type(),Some("Future"));
    assert_eq!(list.value[0].additional_fields["extra"],serde_json::Value::Null);
    assert_eq!(credential.0.load(Ordering::SeqCst),2);
    assert_eq!(*executor.0.lock().unwrap(),[
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items",
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items/opaque"
    ]);
}
