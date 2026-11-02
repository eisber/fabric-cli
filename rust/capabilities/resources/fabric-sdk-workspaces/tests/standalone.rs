// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Uses only the focused SDK's public API, without a dependency on the facade.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    error::ErrorKind,
};
use fabric_core::Audience;
use fabric_sdk_workspaces::{
    CredentialSelection, TokenCredential, WorkspaceClient, WorkspaceExecutionError,
    WorkspaceExecutor, WorkspaceListErrorKind, WorkspaceListLimits, WorkspaceRequest,
    WorkspaceResponse,
};
use std::{
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug, Default)]
struct UnavailableCredential {
    calls: AtomicUsize,
}

#[async_trait]
impl TokenCredential for UnavailableCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        Err(azure_core::Error::with_message(
            ErrorKind::Other,
            "offline-only",
        ))
    }
}

struct UnexpectedExecutor;

impl WorkspaceExecutor for UnexpectedExecutor {
    fn execute<'a>(
        &'a self,
        _request: WorkspaceRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>
    {
        panic!("unavailable credentials must prevent execution")
    }
}

#[tokio::test(flavor = "current_thread")]
async fn focused_sdk_is_usable_without_the_facade_or_identity_backend() {
    let credential = Arc::new(UnavailableCredential::default());
    let selection =
        CredentialSelection::new("offline-account", "offline-tenant", Audience::Fabric).unwrap();
    let client = WorkspaceClient::new(credential.clone(), selection.clone());
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert_eq!(client.selection(), &selection);
    let error = client
        .list_workspaces(
            &UnexpectedExecutor,
            WorkspaceListLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::Credential);
    assert!(error.partial.value.is_empty());
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
}
