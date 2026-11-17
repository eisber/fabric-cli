// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use super::*;
use async_trait::async_trait;
use azure_core::{
    Bytes,
    error::ErrorKind,
    http::{AsyncRawResponse, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use futures_util::{future::pending, stream};
use std::{collections::VecDeque, sync::Mutex};

struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(async move {
            if delay == ExecutionOptions::default().timeout {
                pending::<()>().await;
            }
        })
    }
}

#[derive(Debug)]
struct Wire {
    replies: Mutex<VecDeque<AsyncRawResponse>>,
    requests: Mutex<Vec<Request>>,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected replay"))
    }
}

fn setup(replies: impl IntoIterator<Item = AsyncRawResponse>) -> (HttpExecutor, Arc<Wire>) {
    let wire = Arc::new(Wire {
        replies: Mutex::new(replies.into_iter().collect()),
        requests: Mutex::default(),
    });
    (
        HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default()).unwrap(),
        wire,
    )
}

fn response(status: u16, body: &[u8]) -> AsyncRawResponse {
    AsyncRawResponse::from_bytes(StatusCode::from(status), Headers::new(), body.to_vec())
}

fn request<'a>(url: &'a Url, token: &'a AccessToken, method: Method) -> HttpRequest<'a> {
    HttpRequest {
        origin: ServiceOrigin::Fabric,
        method,
        url,
        token,
        body: b"exact \x00\xff",
        headers: &[],
        max_response_bytes: NonZeroUsize::new(4).unwrap(),
    }
}

#[tokio::test]
async fn neutral_trait_preserves_requests_responses_and_get_only_retries() {
    for (method, statuses, count, status) in [
        (Method::Get, vec![503, 200], 2, 200),
        (Method::Post, vec![503], 1, 503),
        (Method::Patch, vec![429], 1, 429),
    ] {
        let (engine, wire) = setup(statuses.into_iter().map(|status| response(status, b"raw")));
        let executor: &dyn HttpExecution = &engine;
        let url = Url::parse("https://api.fabric.microsoft.com/v1/neutral").unwrap();
        let token = AccessToken::new("offline", OffsetDateTime::UNIX_EPOCH);
        let result = executor
            .execute_http(request(&url, &token, method), None)
            .await
            .unwrap();
        assert_eq!(result.status, status);
        assert_eq!(result.body, b"raw");
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), count);
        for sent in requests.iter() {
            assert_eq!(sent.method(), method);
            assert_eq!(sent.url(), &url);
            assert_eq!(Bytes::from(sent.body()).as_ref(), b"exact \x00\xff");
        }
    }
}

#[derive(Default)]
struct Progress {
    records: Mutex<Vec<(Vec<u8>, bool)>>,
    omitted: Mutex<bool>,
    fail: bool,
}

impl ResponseProgress for Progress {
    fn record(&self, response: &HttpResponse, complete: bool) -> Result<(), ExecutionError> {
        self.records
            .lock()
            .unwrap()
            .push((response.body.clone(), complete));
        if self.fail {
            Err(ExecutionError::InvalidInput)
        } else {
            Ok(())
        }
    }

    fn body_omitted(&self, _: &HttpResponse) -> Result<(), ExecutionError> {
        *self.omitted.lock().unwrap() = true;
        Ok(())
    }
}

#[tokio::test]
async fn neutral_progress_preserves_partial_failure_overflow_and_recorder_failure() {
    for (chunks, fail, expected, records, omitted) in [
        (
            vec![
                Ok(Bytes::from_static(b"raw")),
                Err(azure_core::Error::with_message(ErrorKind::Io, "private")),
            ],
            false,
            ExecutionError::Transport,
            vec![(vec![], false), (b"raw".to_vec(), false)],
            false,
        ),
        (
            vec![
                Ok(Bytes::from_static(b"raw")),
                Ok(Bytes::from_static(b"xx")),
            ],
            false,
            ExecutionError::ResponseLimit,
            vec![(vec![], false), (b"raw".to_vec(), false)],
            true,
        ),
        (
            vec![Ok(Bytes::from_static(b"raw"))],
            true,
            ExecutionError::InvalidInput,
            vec![(vec![], false)],
            false,
        ),
    ] {
        let raw = AsyncRawResponse::new(
            StatusCode::Accepted,
            Headers::new(),
            Box::pin(stream::iter(chunks)),
        );
        let (engine, wire) = setup([raw]);
        let trace = ExecutionTrace::default();
        let engine = engine.with_trace(trace.clone());
        let progress = Progress {
            fail,
            ..Default::default()
        };
        let url = Url::parse("https://api.fabric.microsoft.com/v1/neutral").unwrap();
        let token = AccessToken::new("offline", OffsetDateTime::UNIX_EPOCH);
        assert_eq!(
            engine
                .execute_http(request(&url, &token, Method::Post), Some(&progress))
                .await
                .unwrap_err(),
            expected
        );
        assert_eq!(*progress.records.lock().unwrap(), records);
        assert_eq!(*progress.omitted.lock().unwrap(), omitted);
        assert_eq!(trace.snapshot().status, Some(202));
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn neutral_seam_rejects_origin_and_token_mismatch_before_dispatch() {
    let (engine, wire) = setup([]);
    let token = AccessToken::new("offline", OffsetDateTime::UNIX_EPOCH);
    let wrong = Url::parse("https://management.azure.com/subscriptions").unwrap();
    assert_eq!(
        engine
            .execute_http(request(&wrong, &token, Method::Get), None)
            .await
            .unwrap_err(),
        ExecutionError::InvalidInput
    );
    let url = Url::parse("https://api.fabric.microsoft.com/v1/neutral").unwrap();
    let invalid = AccessToken::new("", OffsetDateTime::UNIX_EPOCH);
    assert_eq!(
        engine
            .execute_http(request(&url, &invalid, Method::Get), None)
            .await
            .unwrap_err(),
        ExecutionError::InvalidInput
    );
    assert!(wire.requests.lock().unwrap().is_empty());
}
