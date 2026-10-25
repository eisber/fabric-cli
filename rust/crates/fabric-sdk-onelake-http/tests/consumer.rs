// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real focused SDK -> adapter -> shared HTTP, using synthetic credentials/network.

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    error::ErrorKind,
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_core::{Audience, CredentialSelection};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_onelake::{
    OneLakeClient, OneLakeError, OneLakeExecutionError, OneLakeLimits, OneLakePath,
};
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use futures_util::stream;
use serde_json::json;
use std::{
    collections::VecDeque,
    future::{Future, pending, poll_fn},
    num::NonZeroUsize,
    pin::pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(240);

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
    unavailable: bool,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://storage.azure.com/.default"]);
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.unavailable {
            return Err(azure_core::Error::with_message(
                ErrorKind::Other,
                "synthetic credential failure",
            ));
        }
        Ok(AccessToken::new(
            "synthetic-onelake-http",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug, Default)]
struct Network {
    replies: Mutex<VecDeque<azure_core::Result<AsyncRawResponse>>>,
    requests: Mutex<Vec<Request>>,
    pending: bool,
    dropped: AtomicBool,
}

struct DropFlag<'a>(&'a AtomicBool);
impl Drop for DropFlag<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl HttpClient for Network {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        if self.pending {
            let _guard = DropFlag(&self.dropped);
            return pending().await;
        }
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP request/replay")
    }
}

#[derive(Default)]
struct Timer {
    waits: Mutex<Vec<Duration>>,
    expired: AtomicBool,
}
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay != TIMEOUT {
            self.waits.lock().unwrap().push(delay);
        }
        Box::pin(poll_fn(move |_| {
            if delay == TIMEOUT && !self.expired.load(Ordering::SeqCst) {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        }))
    }
}

fn response(status: u16, body: &[u8], headers: &[(&str, &str)]) -> AsyncRawResponse {
    let mut result = Headers::new();
    for &(key, value) in headers {
        result.insert(key.to_owned(), value.to_owned());
    }
    AsyncRawResponse::from_bytes(StatusCode::from(status), result, body.to_vec())
}

fn setup(
    replies: Vec<AsyncRawResponse>,
) -> (
    OneLakeClient,
    OneLakeHttpExecutor,
    Arc<Network>,
    Arc<Credential>,
    Arc<Timer>,
) {
    let credential = Arc::new(Credential::default());
    let client = OneLakeClient::new(
        credential.clone(),
        CredentialSelection::new("synthetic-account", "synthetic-tenant", Audience::OneLake)
            .unwrap(),
    );
    let network = Arc::new(Network {
        replies: Mutex::new(replies.into_iter().map(Ok).collect()),
        ..Network::default()
    });
    let timer = Arc::new(Timer::default());
    let adapter = OneLakeHttpExecutor::new(Arc::new(
        HttpExecutor::new(network.clone(), timer.clone(), ExecutionOptions::default()).unwrap(),
    ));
    (client, adapter, network, credential, timer)
}

fn path(relative: &str) -> OneLakePath {
    OneLakePath::new("workspace", "item", relative).unwrap()
}

#[tokio::test]
async fn actual_table_read_dependency_chain_lists_pages_reads_delta_bytes_and_heads_source() {
    let delta = b"{\"metaData\":{\"schemaString\":\"{\\\"type\\\":\\\"struct\\\"}\"}}\n";
    let (client, adapter, network, credential, _) = setup(vec![
        response(200, br#"{"paths":[{"name":"item/Tables/t/_delta_log/00000.json","isDirectory":false}]}"#, &[("x-ms-continuation", "opaque+/=")]),
        response(200, br#"{"paths":[{"name":"item/Tables/t/_delta_log/00001.json","contentLength":"73","future":{"keep":true}}]}"#, &[]),
        response(200, delta, &[("content-type", "application/json")]),
        response(200, b"", &[("content-length", "42"), ("content-type", "text/csv"), ("etag", "synthetic-etag")]),
    ]);
    let rows = client
        .list(
            &adapter,
            &path("Tables/t/_delta_log"),
            false,
            OneLakeLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(rows.pages_received, 2);
    assert_eq!(rows.paths[0]["name"], "item/Tables/t/_delta_log/00000.json");
    assert_eq!(rows.paths[1]["future"], json!({"keep":true}));
    let bytes = client
        .read(
            &adapter,
            &path("Tables/t/_delta_log/00001.json"),
            OneLakeLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(bytes, delta);
    let headers = client
        .properties(
            &adapter,
            &path("Files/source data.csv"),
            OneLakeLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(headers["content-length"], "42");
    assert_eq!(headers["content-type"], "text/csv");
    assert_eq!(headers["etag"], "synthetic-etag");
    let requests = network.requests.lock().unwrap();
    assert_eq!(
        requests.iter().map(Request::method).collect::<Vec<_>>(),
        [Method::Get, Method::Get, Method::Get, Method::Head]
    );
    assert_eq!(
        requests[0].url().as_str(),
        "https://onelake.dfs.fabric.microsoft.com/workspace/?recursive=false&resource=filesystem&directory=item%2FTables%2Ft%2F_delta_log&getShortcutMetadata=true"
    );
    assert!(
        requests[1]
            .url()
            .as_str()
            .ends_with("&continuationToken=opaque%2B%2F%3D")
    );
    assert_eq!(
        requests[2].url().path(),
        "/workspace/item/Tables/t/_delta_log/00001.json"
    );
    assert_eq!(
        requests[3].url().path(),
        "/workspace/item/Files/source%20data.csv"
    );
    for request in requests.iter() {
        assert_eq!(
            request.url().host_str(),
            Some("onelake.dfs.fabric.microsoft.com")
        );
        assert_eq!(
            request.headers().get_str(&"authorization".into()).unwrap(),
            "Bearer synthetic-onelake-http"
        );
        assert_eq!(
            request.headers().get_str(&"content-type".into()).unwrap(),
            "application/json"
        );
        assert!(Bytes::from(request.body()).is_empty());
    }
    assert_eq!(credential.calls.load(Ordering::SeqCst), 4);
    assert!(!format!("{adapter:?}").contains("synthetic-onelake-http"));
}

#[tokio::test]
async fn real_streaming_byte_cap_accepts_exact_and_rejects_one_more_without_empty_success() {
    for (chunks, expected) in [
        (
            vec![Ok(Bytes::from_static(b"ab")), Ok(Bytes::from_static(b"cd"))],
            Ok(b"abcd".to_vec()),
        ),
        (
            vec![
                Ok(Bytes::from_static(b"ab")),
                Ok(Bytes::from_static(b"cde")),
            ],
            Err(OneLakeError::Execution(
                OneLakeExecutionError::ResponseLimit,
            )),
        ),
    ] {
        let response = AsyncRawResponse::new(
            StatusCode::Ok,
            Headers::new(),
            Box::pin(stream::iter(chunks)),
        );
        let (client, adapter, network, _, _) = setup(vec![response]);
        let result = client
            .read(
                &adapter,
                &path("Files/data"),
                OneLakeLimits {
                    max_response_bytes: NonZeroUsize::new(4).unwrap(),
                    ..OneLakeLimits::default()
                },
                pending(),
            )
            .await;
        assert_eq!(result, expected);
        assert_eq!(network.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn get_retries_share_existing_policy_but_head_is_single_attempt() {
    let (client, adapter, network, credential, timer) = setup(vec![
        response(503, b"temporary", &[]),
        response(200, b"content", &[]),
        response(503, b"temporary", &[]),
    ]);
    assert_eq!(
        client
            .read(
                &adapter,
                &path("Files/data"),
                OneLakeLimits::default(),
                pending()
            )
            .await
            .unwrap(),
        b"content"
    );
    let error = client
        .properties(
            &adapter,
            &path("Files/data"),
            OneLakeLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        OneLakeError::Http {
            status: 503,
            code: None
        }
    );
    assert_eq!(
        network
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(Request::method)
            .collect::<Vec<_>>(),
        [Method::Get, Method::Get, Method::Head]
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 2);
    assert!(timer.waits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn exhausted_get_retries_and_lost_response_do_not_install_another_retry_stack() {
    let (client, adapter, network, credential, timer) =
        setup((0..4).map(|_| response(503, b"", &[])).collect());
    assert_eq!(
        client
            .read(
                &adapter,
                &path("Files/data"),
                OneLakeLimits::default(),
                pending()
            )
            .await
            .unwrap_err(),
        OneLakeError::Execution(OneLakeExecutionError::RetriesExhausted)
    );
    assert_eq!(network.requests.lock().unwrap().len(), 4);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *timer.waits.lock().unwrap(),
        [Duration::from_secs(2), Duration::from_secs(4)]
    );
    let (client, adapter, network, _, _) = setup(vec![]);
    network
        .replies
        .lock()
        .unwrap()
        .push_back(Err(azure_core::Error::with_message(
            ErrorKind::Other,
            "synthetic lost response",
        )));
    assert_eq!(
        client
            .read(
                &adapter,
                &path("Files/data"),
                OneLakeLimits::default(),
                pending()
            )
            .await
            .unwrap_err(),
        OneLakeError::Execution(OneLakeExecutionError::Failed)
    );
    assert_eq!(network.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn service_forbidden_and_not_found_remain_distinct_with_storage_error_codes() {
    for status in [401, 403, 404] {
        let (client, adapter, network, _, _) = setup(vec![response(
            status,
            b"untrusted diagnostic",
            &[("x-ms-error-code", "SyntheticStorageError")],
        )]);
        let error = client
            .list(
                &adapter,
                &path("Files"),
                false,
                OneLakeLimits::default(),
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            OneLakeError::Http {
                status,
                code: Some("SyntheticStorageError".into())
            }
        );
        assert!(!error.to_string().contains("untrusted diagnostic"));
        assert_eq!(network.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn wrong_selection_or_unavailable_credential_never_dispatches_or_falls_back() {
    let (_, adapter, network, credential, _) = setup(vec![]);
    let client = OneLakeClient::new(
        credential.clone(),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    );
    assert_eq!(
        client
            .read(
                &adapter,
                &path("Files/data"),
                OneLakeLimits::default(),
                pending()
            )
            .await
            .unwrap_err(),
        OneLakeError::InvalidAudience
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    let unavailable = Arc::new(Credential {
        unavailable: true,
        ..Credential::default()
    });
    let client = OneLakeClient::new(
        unavailable.clone(),
        CredentialSelection::new("offline", "tenant", Audience::OneLake).unwrap(),
    );
    assert_eq!(
        client
            .read(
                &adapter,
                &path("Files/data"),
                OneLakeLimits::default(),
                pending()
            )
            .await
            .unwrap_err(),
        OneLakeError::Credential
    );
    assert_eq!(unavailable.calls.load(Ordering::SeqCst), 1);
    assert!(network.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn deadline_uses_the_existing_timer_before_network_work() {
    let (client, adapter, network, _, timer) = setup(vec![]);
    timer.expired.store(true, Ordering::SeqCst);
    assert_eq!(
        client
            .read(
                &adapter,
                &path("Files/data"),
                OneLakeLimits::default(),
                pending()
            )
            .await
            .unwrap_err(),
        OneLakeError::Execution(OneLakeExecutionError::TimedOut)
    );
    assert!(network.requests.lock().unwrap().is_empty());
}

#[test]
fn cancellation_drops_the_inflight_shared_exchange_and_never_commits_a_response() {
    let credential = Arc::new(Credential::default());
    let client = OneLakeClient::new(
        credential,
        CredentialSelection::new("offline", "tenant", Audience::OneLake).unwrap(),
    );
    let network = Arc::new(Network {
        pending: true,
        ..Network::default()
    });
    let adapter = OneLakeHttpExecutor::new(Arc::new(
        HttpExecutor::new(
            network.clone(),
            Arc::new(Timer::default()),
            ExecutionOptions::default(),
        )
        .unwrap(),
    ));
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&cancelled);
    let resource = path("Files/data");
    let future = client.read(
        &adapter,
        &resource,
        OneLakeLimits::default(),
        poll_fn(move |_| {
            if signal.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }),
    );
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    assert!(future.as_mut().poll(&mut context).is_pending());
    assert_eq!(network.requests.lock().unwrap().len(), 1);
    cancelled.store(true, Ordering::SeqCst);
    assert_eq!(
        future.as_mut().poll(&mut context),
        Poll::Ready(Err(OneLakeError::Cancelled))
    );
    assert!(network.dropped.load(Ordering::SeqCst));
}
