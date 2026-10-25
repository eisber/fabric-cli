// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! External consumer of additive facade delegation and unchanged shared types.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::Audience;
use fabric_sdk::{
    Client, CredentialSelection, Page, TokenCredential, capacities::*, models::FabricServiceError,
};
use std::{
    collections::BTreeMap,
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
            "offline-facade-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}
struct Executor;
impl CapacityExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: CapacityRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<CapacityResponse, CapacityExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(
                request.url().as_str(),
                "https://api.fabric.microsoft.com/v1/capacities"
            );
            assert_eq!(request.max_response_bytes().get(), 73);
            assert_eq!(request.token().token.secret(), "offline-facade-only");
            Ok(CapacityResponse {
                status: 200,
                body: br#"{"value":[{"id":"opaque","displayName":"Case"}]}"#.to_vec(),
                headers: BTreeMap::new(),
            })
        })
    }
}

#[tokio::test]
async fn facade_forwards_credential_selection_limits_and_reexports() {
    let credential = Arc::new(Credential::default());
    let selection = CredentialSelection::new("account", "tenant", Audience::Fabric).unwrap();
    let client = Client::new(credential.clone(), selection.clone());
    let direct = CapacityClient::new(client.credential().clone(), client.selection().clone());
    assert_eq!(direct.selection(), &selection);
    assert!(Arc::ptr_eq(direct.credential(), client.credential()));
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    let limits = CapacityListLimits {
        max_response_bytes: std::num::NonZeroUsize::new(73).unwrap(),
        ..CapacityListLimits::default()
    };
    let list = client
        .list_capacities(&Executor, limits, pending())
        .await
        .unwrap();
    let raw: fabric_sdk::models::Capacity = list.value[0].clone();
    let _: fabric_sdk_capacities::Capacity = raw.clone();
    let page = Page {
        value: vec![raw],
        continuation_token: None,
        continuation_uri: None,
    };
    let _: fabric_sdk_capacities::Page<Capacity> = page;
    let error = FabricServiceError {
        error_code: "Code".into(),
        message: "message".into(),
        additional_fields: BTreeMap::new(),
    };
    let _: fabric_sdk_capacities::FabricServiceError = error;
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);
    let error = client
        .list_capacities(&Executor, limits, std::future::ready(()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, CapacityListErrorKind::Cancelled);
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);
}
