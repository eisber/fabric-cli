// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! External folder consumer of the same shared HTTP engine as existing services.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_core::Audience;
use fabric_sdk::{Client, CredentialSelection, TokenCredential, folders::*};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use std::{
    collections::VecDeque,
    future::{pending, poll_fn},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
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
            "offline-folder-http",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}
#[derive(Default)]
struct Timer {
    waits: Mutex<Vec<Duration>>,
    expired: AtomicBool,
}
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        self.waits.lock().unwrap().push(delay);
        Box::pin(poll_fn(move |_| {
            if delay == Duration::from_secs(240) && !self.expired.load(Ordering::SeqCst) {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        }))
    }
}
#[derive(Debug)]
struct Http {
    replies: Mutex<VecDeque<AsyncRawResponse>>,
    requests: Mutex<Vec<Request>>,
}
#[async_trait]
impl HttpClient for Http {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP replay"))
    }
}
fn reply(status: u16, body: &str, headers: &[(&str, &str)]) -> AsyncRawResponse {
    let mut values = Headers::new();
    for &(key, value) in headers {
        values.insert(key.to_owned(), value.to_owned());
    }
    AsyncRawResponse::from_bytes(StatusCode::from(status), values, body.as_bytes().to_vec())
}
fn http(replies: impl IntoIterator<Item = AsyncRawResponse>) -> Arc<Http> {
    Arc::new(Http {
        replies: Mutex::new(replies.into_iter().collect()),
        requests: Mutex::new(vec![]),
    })
}
fn sdk(credential: Arc<Credential>) -> Client {
    Client::new(
        credential,
        CredentialSelection::new("account", "tenant", Audience::Fabric).unwrap(),
    )
}
fn executor(http: Arc<Http>, timer: Arc<Timer>) -> HttpExecutor {
    HttpExecutor::new(http, timer, ExecutionOptions::default()).unwrap()
}

#[tokio::test]
async fn folder_list_and_get_use_shared_headers_query_scope_gateway_and_429_retries() {
    let raw = http([
        reply(503, "", &[]),
        reply(
            200,
            r#"{"value":[{"id":"a","displayName":"A"}],"continuationToken":"body"}"#,
            &[("X-MS-Continuation", "header+/= ~*")],
        ),
        reply(200, r#"{"value":[]}"#, &[]),
        reply(429, "", &[]),
        reply(
            200,
            r#"{"id":"a","displayName":"A","parentFolderId":null}"#,
            &[],
        ),
    ]);
    let timer = Arc::new(Timer::default());
    let credential = Arc::new(Credential::default());
    let client = sdk(credential.clone());
    let exec = executor(raw.clone(), timer.clone());
    let list = client
        .list_folders(&exec, "ws", Default::default(), pending())
        .await
        .unwrap();
    assert_eq!(list.pages_received, 2);
    assert_eq!(list.value[0].id, "a");
    let folder = client
        .get_folder(&exec, "ws", "a", Default::default(), pending())
        .await
        .unwrap();
    assert_eq!(folder.parent_folder_id(), Some(&serde_json::Value::Null));
    assert_eq!(credential.0.load(Ordering::SeqCst), 3);
    let requests = raw.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests[2].url().as_str(),
        "https://api.fabric.microsoft.com/v1/workspaces/ws/folders?recursive=True&continuationToken=header%2B%2F%3D+~%2A"
    );
    for request in requests.iter() {
        assert_eq!(request.method(), azure_core::http::Method::Get);
        assert_eq!(
            request.headers().get_str(&"content-type".into()).unwrap(),
            "application/json"
        );
        assert_eq!(
            request.headers().get_str(&"authorization".into()).unwrap(),
            "Bearer offline-folder-http"
        );
    }
    assert!(
        timer
            .waits
            .lock()
            .unwrap()
            .contains(&Duration::from_secs(10))
    );
}

#[tokio::test]
async fn folder_adapter_preserves_service_failure_with_real_partial_progress() {
    let raw = http([
        reply(
            200,
            r#"{"value":[{"id":"a","displayName":"A"}],"continuationToken":"next"}"#,
            &[],
        ),
        reply(403, "", &[("x-ms-public-api-error-code", "Denied")]),
    ]);
    let error = sdk(Arc::default())
        .list_folders(
            &executor(raw, Arc::default()),
            "ws",
            Default::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error.kind,FolderErrorKind::Http{status:403,ref code,..} if code=="Denied"));
    assert_eq!(error.partial.pages_received, 1);
    assert_eq!(error.partial.value[0].id, "a");
    assert_eq!(error.partial.next_token.as_deref(), Some("next"));
}

#[tokio::test]
async fn folder_adapter_propagates_shared_deadline_and_retry_budget() {
    let raw = http([]);
    let timer = Arc::new(Timer::default());
    timer.expired.store(true, Ordering::SeqCst);
    let error = sdk(Arc::default())
        .get_folder(
            &executor(raw.clone(), timer),
            "ws",
            "f",
            Default::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        FolderErrorKind::Execution(FolderExecutionError::TimedOut)
    );
    assert!(raw.requests.lock().unwrap().is_empty());
    let raw = http((0..4).map(|_| reply(503, "", &[])));
    let error = sdk(Arc::default())
        .list_folders(
            &executor(raw.clone(), Arc::default()),
            "ws",
            Default::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        FolderErrorKind::Execution(FolderExecutionError::RetriesExhausted)
    );
    assert_eq!(raw.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn folder_adapter_enforces_default_4mib_streaming_bound_exact_and_plus_one() {
    let bound = 4 * 1024 * 1024;
    for (list, extra) in [(true, 0), (true, 1), (false, 0), (false, 1)] {
        let base = if list {
            r#"{"value":[],"padding":""}"#
        } else {
            r#"{"id":"f","displayName":"F","padding":""}"#
        };
        let body = base.replace(
            "\"padding\":\"\"",
            &format!("\"padding\":\"{}\"", "x".repeat(bound - base.len() + extra)),
        );
        let raw = http([reply(200, &body, &[])]);
        let client = sdk(Arc::default());
        let exec = executor(raw, Arc::default());
        let result = if list {
            client
                .list_folders(&exec, "ws", Default::default(), pending())
                .await
                .map(|_| ())
                .map_err(|error| error.kind)
        } else {
            client
                .get_folder(&exec, "ws", "f", Default::default(), pending())
                .await
                .map(|_| ())
                .map_err(|error| error.kind)
        };
        if extra == 0 {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err(), FolderErrorKind::ResponseLimit);
        }
    }
}

#[tokio::test]
async fn folder_adapter_cancellation_drops_inflight_response_stream() {
    let dropped = Arc::new(AtomicBool::new(false));
    struct DropGuard(Arc<AtomicBool>);
    impl Drop for DropGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let guard = DropGuard(dropped.clone());
    let stream = futures_util::stream::poll_fn(move |_| {
        let _keep = &guard;
        Poll::<Option<azure_core::Result<azure_core::Bytes>>>::Pending
    });
    let raw = http([AsyncRawResponse::new(
        StatusCode::Ok,
        Headers::new(),
        Box::pin(stream),
    )]);
    let client = sdk(Arc::default());
    let exec = executor(raw.clone(), Arc::default());
    let cancel = AtomicBool::new(false);
    let cancelled = poll_fn(|_| {
        if cancel.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let mut call = std::pin::pin!(client.list_folders(&exec, "ws", Default::default(), cancelled));
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    use std::future::Future;
    assert!(call.as_mut().poll(&mut cx).is_pending());
    cancel.store(true, Ordering::SeqCst);
    let Poll::Ready(Err(error)) = call.as_mut().poll(&mut cx) else {
        panic!("cancellation not ready")
    };
    assert_eq!(error.kind, FolderErrorKind::Cancelled);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(raw.requests.lock().unwrap().len(), 1);
}
