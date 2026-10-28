// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Independent SDK consumer with explicit native transport selection.
//!
//! This offline example cancels before execution. The synthetic expired
//! credential is test-only and must never be used against a service.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_sdk::{
    Client, CredentialSelection,
    workspaces::{WorkspaceListErrorKind, WorkspaceListLimits},
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_transport::{
    NativeHttpClient, ProxyPolicy, TokioTimer, TransportOptions, TrustPolicy,
};
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Debug)]
struct SyntheticCredential(AtomicUsize);

#[async_trait]
impl TokenCredential for SyntheticCredential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "expired-synthetic-only",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

async fn consume() -> Result<(), Box<dyn std::error::Error>> {
    let credential = Arc::new(SyntheticCredential(AtomicUsize::new(0)));
    let sdk = Client::new(
        credential.clone(),
        CredentialSelection::new("offline", "synthetic", fabric_core::Audience::Fabric)?,
    );
    let client = NativeHttpClient::new(TransportOptions {
        connect_timeout: Duration::from_secs(10),
        read_timeout: Duration::from_secs(30),
        write_timeout: Duration::from_secs(30),
        pool_idle_timeout: Duration::from_secs(60),
        max_idle_per_host: NonZeroUsize::new(4).unwrap(),
        trust: TrustPolicy::Platform {
            additional_roots: Vec::new(),
        },
        proxy: ProxyPolicy::None,
    })?;
    let executor = HttpExecutor::new(
        Arc::new(client),
        Arc::new(TokioTimer),
        ExecutionOptions::default(),
    )?;
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    let result = sdk
        .list_workspaces(
            &executor,
            WorkspaceListLimits::default(),
            std::future::ready(()),
        )
        .await
        .unwrap_err();
    assert_eq!(result.kind, WorkspaceListErrorKind::Cancelled);
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    consume().await
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn public_sdk_consumer_composes_native_transport_without_ambient_credentials() {
        super::consume().await.unwrap();
    }
}
