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
    OneLakeWriteLimits, OneLakeWriteOutcome, OneLakeWriteStep,
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
            code: None,
            raw_service_error: None,
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
                code: Some("SyntheticStorageError".into()),
                raw_service_error: None,
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

#[tokio::test]
async fn directory_create_delete_and_rename_use_exact_source_methods_queries_and_headers() {
    let (client, adapter, network, credential, _) = setup(vec![
        response(201, b"", &[]),
        response(202, b"", &[]),
        response(200, b"", &[]),
    ]);
    assert_eq!(
        client
            .create_directory(
                &adapter,
                &path("Files/new folder"),
                OneLakeWriteLimits::default(),
                pending()
            )
            .await
            .unwrap(),
        OneLakeWriteOutcome::Completed { status: 201 }
    );
    assert_eq!(
        client
            .delete_path(
                &adapter,
                &path("Files/new folder"),
                OneLakeWriteLimits::default(),
                pending()
            )
            .await
            .unwrap(),
        OneLakeWriteOutcome::Accepted
    );
    assert_eq!(
        client
            .rename_path(
                &adapter,
                &path("Files/source"),
                &path("Files/target"),
                OneLakeWriteLimits::default(),
                pending()
            )
            .await
            .unwrap(),
        OneLakeWriteOutcome::Completed { status: 200 }
    );
    let requests = network.requests.lock().unwrap();
    assert_eq!(
        requests.iter().map(Request::method).collect::<Vec<_>>(),
        [Method::Put, Method::Delete, Method::Put]
    );
    assert_eq!(
        requests[0].url().as_str(),
        "https://onelake.dfs.fabric.microsoft.com/workspace/item/Files/new%20folder/?resource=directory"
    );
    assert_eq!(
        requests[0]
            .headers()
            .get_str(&"if-none-match".into())
            .unwrap(),
        "*"
    );
    assert_eq!(
        requests[1].url().as_str(),
        "https://onelake.dfs.fabric.microsoft.com/workspace/item/Files/new%20folder/?recursive=true"
    );
    assert_eq!(requests[2].url().path(), "/workspace/item/Files/target");
    assert_eq!(
        requests[2]
            .headers()
            .get_str(&"x-ms-rename-source".into())
            .unwrap(),
        "workspace/item/Files/source"
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn upload_uses_exact_binary_bytes_media_type_and_byte_positions() {
    let (client, adapter, network, _, _) = setup(vec![
        response(201, b"", &[]),
        response(202, b"", &[]),
        response(200, b"", &[]),
    ]);
    let bytes = b"\x00\xff\xc3\xa9\n";
    let upload = client
        .upload_file(
            &adapter,
            &path("Files/data.bin"),
            bytes,
            "application/octet-stream",
            OneLakeWriteLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(upload.create.status(), 201);
    assert_eq!(upload.append, Some(OneLakeWriteOutcome::Accepted));
    assert_eq!(
        upload.flush,
        Some(OneLakeWriteOutcome::Completed { status: 200 })
    );
    let requests = network.requests.lock().unwrap();
    assert_eq!(
        requests.iter().map(Request::method).collect::<Vec<_>>(),
        [Method::Put, Method::Patch, Method::Patch]
    );
    assert_eq!(requests[0].url().query(), Some("resource=file"));
    assert!(requests[0].url().path().ends_with("/data.bin/"));
    assert_eq!(requests[1].url().query(), Some("action=append&position=0"));
    assert_eq!(Bytes::from(requests[1].body()).as_ref(), bytes);
    assert_eq!(
        requests[1]
            .headers()
            .get_str(&"content-type".into())
            .unwrap(),
        "application/octet-stream"
    );
    assert_eq!(
        requests[1]
            .headers()
            .get_str(&"x-ms-content-type".into())
            .unwrap(),
        "application/octet-stream"
    );
    assert_eq!(requests[2].url().query(), Some("action=flush&position=5"));
    assert!(Bytes::from(requests[2].body()).is_empty());
    assert_eq!(
        requests[2]
            .headers()
            .get_str(&"content-type".into())
            .unwrap(),
        "application/json"
    );
}

#[tokio::test]
async fn file_create_other_than_201_does_not_append_flush_or_invent_upload_completion() {
    for status in [200, 202, 204] {
        let (client, adapter, network, _, _) = setup(vec![response(status, b"", &[])]);
        let result = client
            .upload_file(
                &adapter,
                &path("Files/data.txt"),
                b"text",
                "text/plain",
                OneLakeWriteLimits::default(),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(result.create.status(), status);
        assert_eq!(result.append, None);
        assert_eq!(result.flush, None);
        assert_eq!(network.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn failed_upload_stages_retain_response_progress_and_possible_side_effects_without_replay() {
    for (replies, stage) in [
        (
            vec![response(201, b"", &[]), response(403, b"", &[])],
            vec![OneLakeWriteStep::CreateFile],
        ),
        (
            vec![
                response(201, b"", &[]),
                response(202, b"", &[]),
                response(403, b"", &[]),
            ],
            vec![OneLakeWriteStep::CreateFile, OneLakeWriteStep::Append],
        ),
    ] {
        let count = replies.len();
        let (client, adapter, network, _, _) = setup(replies);
        let error = client
            .upload_file(
                &adapter,
                &path("Files/data"),
                b"data",
                "text/plain",
                OneLakeWriteLimits::default(),
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            OneLakeError::Http {
                status: 403,
                code: None,
                raw_service_error: None,
            }
        );
        assert!(error.mutation_may_have_occurred);
        assert_eq!(error.completed_steps, stage);
        assert_eq!(network.requests.lock().unwrap().len(), count);
    }
}

#[tokio::test]
async fn lost_put_patch_and_delete_responses_are_not_replayed_by_either_sdk_or_shared_http() {
    for operation in ["create", "delete", "upload"] {
        let (client, adapter, network, _, _) = setup(if operation == "upload" {
            vec![response(201, b"", &[])]
        } else {
            Vec::new()
        });
        network
            .replies
            .lock()
            .unwrap()
            .push_back(Err(azure_core::Error::with_message(
                ErrorKind::Other,
                "synthetic uncertain write",
            )));
        let error = match operation {
            "create" => client
                .create_directory(
                    &adapter,
                    &path("Files/new"),
                    OneLakeWriteLimits::default(),
                    pending(),
                )
                .await
                .unwrap_err(),
            "delete" => client
                .delete_path(
                    &adapter,
                    &path("Files/new"),
                    OneLakeWriteLimits::default(),
                    pending(),
                )
                .await
                .unwrap_err(),
            _ => client
                .upload_file(
                    &adapter,
                    &path("Files/data"),
                    b"data",
                    "text/plain",
                    OneLakeWriteLimits::default(),
                    pending(),
                )
                .await
                .unwrap_err(),
        };
        assert_eq!(
            error.kind,
            OneLakeError::Execution(OneLakeExecutionError::Failed)
        );
        assert!(error.mutation_may_have_occurred);
        assert_eq!(
            network.requests.lock().unwrap().len(),
            if operation == "upload" { 2 } else { 1 }
        );
    }
}

#[tokio::test]
async fn unsafe_root_and_media_type_and_exact_file_bound_fail_before_any_storage_request() {
    let (client, adapter, network, credential, _) = setup(Vec::new());
    let error = client
        .delete_path(
            &adapter,
            &path(""),
            OneLakeWriteLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, OneLakeError::InvalidInput);
    assert!(!error.mutation_may_have_occurred);
    for content_type in ["text/plain\r\nx:bad", "text/\u{65e5}", ""] {
        let error = client
            .upload_file(
                &adapter,
                &path("Files/file"),
                b"data",
                content_type,
                OneLakeWriteLimits::default(),
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, OneLakeError::InvalidInput);
        assert!(!error.mutation_may_have_occurred);
    }
    let error = client
        .upload_file(
            &adapter,
            &path("Files/file"),
            b"12345",
            "text/plain",
            OneLakeWriteLimits {
                max_request_bytes: NonZeroUsize::new(4).unwrap(),
                ..OneLakeWriteLimits::default()
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, OneLakeError::Limit);
    assert!(!error.mutation_may_have_occurred);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(network.requests.lock().unwrap().is_empty());
    let (client, adapter, network, _, _) = setup(vec![
        response(201, b"", &[]),
        response(202, b"", &[]),
        response(200, b"", &[]),
    ]);
    client
        .upload_file(
            &adapter,
            &path("Files/file"),
            b"1234",
            "text/plain",
            OneLakeWriteLimits {
                max_request_bytes: NonZeroUsize::new(4).unwrap(),
                ..OneLakeWriteLimits::default()
            },
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(network.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn cancellation_before_write_credentials_reports_no_dispatched_mutation() {
    let (client, adapter, network, credential, _) = setup(Vec::new());
    let error = client
        .create_directory(
            &adapter,
            &path("Files/new"),
            OneLakeWriteLimits::default(),
            std::future::ready(()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, OneLakeError::Cancelled);
    assert!(!error.mutation_may_have_occurred);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(network.requests.lock().unwrap().is_empty());
}

#[test]
fn cancellation_of_an_inflight_write_drops_http_but_retains_possible_storage_effects() {
    let client = OneLakeClient::new(
        Arc::new(Credential::default()),
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
    let resource = path("Files/new");
    let future = client.create_directory(
        &adapter,
        &resource,
        OneLakeWriteLimits::default(),
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
    cancelled.store(true, Ordering::SeqCst);
    let Poll::Ready(Err(error)) = future.as_mut().poll(&mut context) else {
        panic!("cancelled write did not fail");
    };
    assert_eq!(error.kind, OneLakeError::Cancelled);
    assert!(error.mutation_may_have_occurred);
    assert!(network.dropped.load(Ordering::SeqCst));
    assert_eq!(network.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn four_mib_default_upload_bound_and_one_extra_byte_are_real_preflight_limits() {
    let exact = vec![b'x'; 4 * 1024 * 1024];
    let (client, adapter, network, credential, _) = setup(vec![
        response(201, b"", &[]),
        response(202, b"", &[]),
        response(200, b"", &[]),
    ]);
    client
        .upload_file(
            &adapter,
            &path("Files/exact"),
            &exact,
            "application/octet-stream",
            OneLakeWriteLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(
        Bytes::from(network.requests.lock().unwrap()[1].body()).len(),
        4 * 1024 * 1024
    );
    let count = credential.calls.load(Ordering::SeqCst);
    let extra = vec![b'x'; 4 * 1024 * 1024 + 1];
    let error = client
        .upload_file(
            &adapter,
            &path("Files/extra"),
            &extra,
            "application/octet-stream",
            OneLakeWriteLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, OneLakeError::Limit);
    assert!(!error.mutation_may_have_occurred);
    assert_eq!(credential.calls.load(Ordering::SeqCst), count);
    assert_eq!(network.requests.lock().unwrap().len(), 3);
}
