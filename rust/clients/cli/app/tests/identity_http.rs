// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline CLI-layer composition of the actual SDK binding adapter and HTTP executor.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use azure_identity::Executor;
use fabric_core::{
    ApiRequest, ApiResponse, Audience, CancellationToken, CoreError, ErrorKind, HttpExecutor,
    HttpTransport, OperationContext,
};
use fabric_identity::{AzureCliSession, Binding, BoundCredential};
use fabric_sdk_http::{HttpClient, SystemClock};
use std::{
    ffi::OsStr,
    io,
    process::Output,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Debug, Default)]
struct PendingProcess {
    calls: AtomicUsize,
    dropped: Arc<AtomicBool>,
}

struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl Executor for PendingProcess {
    async fn run(&self, _: &OsStr, _: &[&OsStr]) -> io::Result<Output> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _drop = DropFlag(self.dropped.clone());
        std::future::pending().await
    }
}

struct NeverTransport;
#[async_trait]
impl HttpTransport for NeverTransport {
    async fn send(&self, _: ApiRequest, _: &OperationContext) -> Result<ApiResponse, CoreError> {
        panic!("credential failure/cancellation must precede HTTP")
    }
}

fn pending_client() -> (HttpClient, Arc<PendingProcess>) {
    let process = Arc::new(PendingProcess::default());
    let clock = Arc::new(SystemClock);
    let session = Arc::new(AzureCliSession::new(process.clone(), clock.clone()).unwrap());
    let binding = Binding {
        tenant: "00000000-0000-0000-0000-000000000001".into(),
        principal: "00000000-0000-0000-0000-000000000002".into(),
    };
    // The composition boundary accepts the SDK trait, not an identity-specific protocol.
    let credential: Arc<dyn TokenCredential> =
        Arc::new(BoundCredential::new(session, binding).unwrap());
    (
        HttpClient::new(credential, Arc::new(NeverTransport), clock),
        process,
    )
}

fn request() -> ApiRequest {
    ApiRequest {
        method: "GET".into(),
        url: format!("{}/items", Audience::Fabric.base_url())
            .parse()
            .unwrap(),
        headers: Default::default(),
        body: Vec::new(),
    }
}

#[tokio::test]
async fn real_http_executor_cancels_the_bound_sdk_process_future() {
    let (client, process) = pending_client();
    let context = OperationContext::new(Duration::from_secs(5), CancellationToken::new());
    let signal = context.cancellation.clone();
    let cancel = async {
        tokio::task::yield_now().await;
        signal.cancel();
    };
    let (result, ()) = tokio::join!(
        client.execute(request(), Audience::Fabric, &context),
        cancel,
    );
    assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(process.calls.load(Ordering::SeqCst), 1);
    assert!(process.dropped.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn real_http_executor_deadline_drops_sdk_work_and_expired_context_starts_nothing() {
    let (client, process) = pending_client();
    let context = OperationContext::new(Duration::from_secs(5), CancellationToken::new());
    assert_eq!(
        client
            .execute(request(), Audience::Fabric, &context)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Timeout,
    );
    assert!(process.dropped.load(Ordering::SeqCst));
    let expired = OperationContext {
        deadline: Instant::now() - Duration::from_secs(1),
        cancellation: CancellationToken::new(),
    };
    assert_eq!(
        client
            .execute(request(), Audience::Fabric, &expired)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Timeout,
    );
    assert_eq!(process.calls.load(Ordering::SeqCst), 1);
}
