// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline interoperability proof. This credential must never be used in production.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::{Audience, CredentialSelection};
use fabric_sdk::Client;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug, Default)]
struct TestOnlyCredential {
    calls: AtomicUsize,
}

#[async_trait]
impl TokenCredential for TestOnlyCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "offline-test-only-not-a-real-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

async fn prove_injection() -> Result<(), Box<dyn std::error::Error>> {
    let credential = Arc::new(TestOnlyCredential::default());
    let selection = CredentialSelection::new(
        "offline-test-account",
        "offline-test-tenant",
        Audience::Fabric,
    )?;
    let client = Client::new(credential.clone(), selection);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    let scope = client.selection().audience().scope();
    let token = client.credential().get_token(&[scope], None).await?;
    assert_eq!(token.token.secret(), "offline-test-only-not-a-real-token");
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert!(!format!("{client:?}").contains("TestOnlyCredential"));
    assert!(!format!("{token:?}").contains("offline-test-only-not-a-real-token"));
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    prove_injection().await?;
    println!("Offline credential injection verified; no service calls were made.");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test(flavor = "current_thread")]
    async fn sdk_accepts_an_independent_test_only_azure_credential() {
        super::prove_injection().await.unwrap();
    }
}
