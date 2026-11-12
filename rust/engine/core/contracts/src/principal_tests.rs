// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use super::*;
use async_trait::async_trait;
use std::{
    collections::VecDeque,
    future::pending,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Waker},
};

#[path = "../../../../tests/fixtures/notebook-identity/fixture.rs"]
mod fixture;
use fixture::{CLIENT, FABRIC, PBI, TENANT, claims, header, jwks, signed, token};

#[derive(Debug)]
struct Credential {
    fabric: String,
    pbi: String,
}

impl Credential {
    fn new() -> Self {
        Self {
            fabric: token(FABRIC),
            pbi: token(PBI),
        }
    }
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<crate::TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        let value = match scopes {
            [scope] if *scope == Audience::Fabric.scope() => &self.fabric,
            [scope] if *scope == Audience::PowerBiBootstrap.scope() => &self.pbi,
            _ => panic!("unexpected credential scope"),
        };
        Ok(AccessToken::new(
            value.clone(),
            OffsetDateTime::now_utc() + azure_core::time::Duration::seconds(3500),
        ))
    }
}

enum Reply {
    Keys,
    Failure,
    Pending,
}

struct Source {
    replies: Mutex<VecDeque<Reply>>,
    requests: AtomicUsize,
    dropped: AtomicUsize,
}

impl Source {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        })
    }
}

impl EntraJwksSource for Source {
    fn fetch<'a>(
        &'a self,
        endpoint: &'a EntraJwksEndpoint,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, PrincipalError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(
                endpoint.url().as_str(),
                format!("https://login.microsoftonline.com/{TENANT}/discovery/keys")
            );
            self.requests.fetch_add(1, Ordering::SeqCst);
            let reply = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected JWKS fetch or replay");
            match reply {
                Reply::Keys => Ok(serde_json::to_vec(&jwks("offline-key")).unwrap()),
                Reply::Failure => Err(PrincipalError::Keys),
                Reply::Pending => {
                    let _guard = Dropped(&self.dropped);
                    pending().await
                }
            }
        })
    }
}

struct Dropped<'a>(&'a AtomicUsize);
impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn verifier(source: Arc<Source>) -> EntraJwtVerifier {
    EntraJwtVerifier::new(
        CredentialSelection::new(CLIENT, TENANT, Audience::Fabric).unwrap(),
        source,
    )
    .unwrap()
}

async fn expire_document(verifier: &EntraJwtVerifier) {
    let mut caches = verifier.documents.lock().await;
    let now = Instant::now();
    caches[0]
        .document
        .as_mut()
        .expect("verified document")
        .expires = now;
    caches[0].retry_after = Some(now);
}

#[tokio::test]
async fn cold_jwks_failure_throttles_before_another_fetch_and_recovers_after_interval() {
    let source = Source::new([Reply::Failure, Reply::Keys]);
    let verifier = verifier(source.clone());
    let credential = Credential::new();
    assert_eq!(
        verifier.acquire_pbi(&credential).await.err(),
        Some(PrincipalError::Keys)
    );
    assert_eq!(
        verifier.acquire_pbi(&credential).await.err(),
        Some(PrincipalError::Keys)
    );
    assert_eq!(source.requests.load(Ordering::SeqCst), 1);
    {
        let mut caches = verifier.documents.lock().await;
        assert!(caches[0].document.is_none());
        caches[0].retry_after = Some(Instant::now());
    }
    assert!(verifier.acquire_pbi(&credential).await.is_ok());
    assert_eq!(source.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cancelled_cold_jwks_fetch_keeps_attempt_without_a_cached_document() {
    let source = Source::new([Reply::Pending]);
    let verifier = verifier(source.clone());
    let credential = Credential::new();
    let mut acquisition = Box::pin(verifier.acquire_pbi(&credential));
    assert!(
        acquisition
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    drop(acquisition);
    assert_eq!(source.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(
        verifier.acquire_pbi(&credential).await.err(),
        Some(PrincipalError::Keys)
    );
    assert_eq!(source.requests.load(Ordering::SeqCst), 1);
    assert!(verifier.documents.lock().await[0].document.is_none());
}

#[tokio::test]
async fn expired_cache_failure_never_uses_stale_keys_or_retries_without_backoff() {
    let source = Source::new([Reply::Keys, Reply::Failure, Reply::Keys]);
    let verifier = verifier(source.clone());
    let credential = Credential::new();
    assert!(verifier.acquire_pbi(&credential).await.is_ok());
    expire_document(&verifier).await;
    assert_eq!(
        verifier.acquire_pbi(&credential).await.err(),
        Some(PrincipalError::Keys)
    );
    assert_eq!(
        verifier.acquire_pbi(&credential).await.err(),
        Some(PrincipalError::Keys)
    );
    assert_eq!(source.requests.load(Ordering::SeqCst), 2);
    {
        let mut caches = verifier.documents.lock().await;
        assert!(Instant::now() >= caches[0].document.as_ref().unwrap().expires);
        caches[0].retry_after = Some(Instant::now());
    }
    assert!(verifier.acquire_pbi(&credential).await.is_ok());
    assert_eq!(source.requests.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn cancelled_expired_cache_fetch_does_not_restore_stale_keys_or_clear_attempt() {
    let source = Source::new([Reply::Keys, Reply::Pending]);
    let verifier = verifier(source.clone());
    let credential = Credential::new();
    assert!(verifier.acquire_pbi(&credential).await.is_ok());
    expire_document(&verifier).await;
    let mut acquisition = Box::pin(verifier.acquire_pbi(&credential));
    assert!(
        acquisition
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    drop(acquisition);
    assert_eq!(source.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(
        verifier.acquire_pbi(&credential).await.err(),
        Some(PrincipalError::Keys)
    );
    assert_eq!(source.requests.load(Ordering::SeqCst), 2);
    assert!(
        Instant::now()
            >= verifier.documents.lock().await[0]
                .document
                .as_ref()
                .unwrap()
                .expires
    );
}

#[tokio::test]
async fn valid_cached_keys_remain_usable_after_failed_key_miss_refresh_without_replay() {
    let source = Source::new([Reply::Keys, Reply::Failure]);
    let verifier = verifier(source.clone());
    let credential = Credential::new();
    assert!(verifier.acquire_pbi(&credential).await.is_ok());
    let missing_key = Credential {
        fabric: signed(&header("rotated-key"), &claims(FABRIC)),
        pbi: token(PBI),
    };
    assert_eq!(
        verifier.acquire_pbi(&missing_key).await.err(),
        Some(PrincipalError::Keys)
    );
    assert_eq!(
        verifier.acquire_pbi(&missing_key).await.err(),
        Some(PrincipalError::Keys)
    );
    assert!(verifier.acquire_pbi(&credential).await.is_ok());
    assert_eq!(source.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn valid_cached_keys_survive_cancelled_key_miss_refresh_without_replay() {
    let source = Source::new([Reply::Keys, Reply::Pending]);
    let verifier = verifier(source.clone());
    let credential = Credential::new();
    assert!(verifier.acquire_pbi(&credential).await.is_ok());
    let missing_key = Credential {
        fabric: signed(&header("rotated-key"), &claims(FABRIC)),
        pbi: token(PBI),
    };
    let mut acquisition = Box::pin(verifier.acquire_pbi(&missing_key));
    assert!(
        acquisition
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    drop(acquisition);
    assert_eq!(source.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(
        verifier.acquire_pbi(&missing_key).await.err(),
        Some(PrincipalError::Keys)
    );
    assert!(verifier.acquire_pbi(&credential).await.is_ok());
    assert_eq!(source.requests.load(Ordering::SeqCst), 2);
}
