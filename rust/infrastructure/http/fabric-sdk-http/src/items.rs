// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Item adapter using the private shared HTTP engine, without policy duplication.

use crate::{ExecutionError, ExecutionFuture, HttpExecutor};
use azure_core::http::Method;
use fabric_sdk_items::{ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse};

impl ItemExecutor for HttpExecutor {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> ExecutionFuture<'a, Result<ItemResponse, ItemExecutionError>> {
        Box::pin(async move {
            self.execute(
                Method::Get,
                request.url(),
                request.token(),
                &[],
                request.max_response_bytes(),
            )
            .await
            .map(|response| ItemResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
            .map_err(|error| match error {
                ExecutionError::TimedOut => ItemExecutionError::TimedOut,
                ExecutionError::ResponseLimit => ItemExecutionError::ResponseLimit,
                ExecutionError::RetriesExhausted => ItemExecutionError::RetriesExhausted,
                ExecutionError::InvalidInput
                | ExecutionError::Transport
                | ExecutionError::RetryDelay => ItemExecutionError::Failed,
            })
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::tests::{ScriptedClient, TestTimer, executor, response, sdk};
    use fabric_sdk_items::{
        ItemCollection, ItemErrorKind, ItemExecutionError, ItemGetOptions, ItemListOptions,
    };
    use std::{
        future::{Future, pending, poll_fn},
        num::NonZeroUsize,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        task::{Context, Poll, Waker},
    };

    #[tokio::test]
    async fn items_use_shared_get_retries_headers_and_facade() {
        let raw = ScriptedClient::new([
            response(503, "", &[]),
            response(
                200,
                r#"{"value":[{"id":"a","displayName":"A"}],"continuationToken":"opaque+/="}"#,
                &[],
            ),
            response(200, r#"{"value":[]}"#, &[]),
            response(502, "", &[]),
            response(
                200,
                r#"{"id":"a","displayName":"A","type":"FutureType","extra":null}"#,
                &[],
            ),
        ]);
        let exec = executor(raw.clone(), Arc::new(TestTimer::default()));
        let client = sdk();
        let list = client
            .list_items(&exec, "ws", ItemListOptions::default(), pending())
            .await
            .unwrap();
        assert_eq!(list.pages_received, 2);
        assert_eq!(list.value.len(), 1);
        let item = client
            .get_item(&exec, "ws", "a", ItemGetOptions::default(), pending())
            .await
            .unwrap();
        assert_eq!(item.item_type(), Some("FutureType"));
        let requests = raw.requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        assert!(
            requests
                .iter()
                .all(|request| request.method() == azure_core::http::Method::Get)
        );
        assert_eq!(
            requests[2].url().as_str(),
            "https://api.fabric.microsoft.com/v1/workspaces/ws/items?continuationToken=opaque%2B%2F%3D"
        );
        for request in requests.iter() {
            assert_eq!(
                request.headers().get_str(&"content-type".into()).unwrap(),
                "application/json"
            );
            assert!(
                request
                    .headers()
                    .get_str(&"authorization".into())
                    .unwrap()
                    .starts_with("Bearer ")
            );
        }
    }

    #[tokio::test]
    async fn items_preserve_shared_limits_deadline_and_retry_failure() {
        let raw = ScriptedClient::new([response(200, r#"{"id":"a","displayName":"A"}"#, &[])]);
        let exec = executor(raw, Arc::new(TestTimer::default()));
        let error = sdk()
            .get_item(
                &exec,
                "ws",
                "a",
                ItemGetOptions {
                    max_response_bytes: NonZeroUsize::new(1).unwrap(),
                    ..Default::default()
                },
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ItemErrorKind::ResponseLimit);

        let raw = ScriptedClient::new((0..4).map(|_| response(503, "", &[])));
        let exec = executor(raw.clone(), Arc::new(TestTimer::default()));
        let error = sdk()
            .list_items(&exec, "ws", ItemListOptions::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            ItemErrorKind::Execution(ItemExecutionError::RetriesExhausted)
        );
        assert_eq!(raw.requests.lock().unwrap().len(), 4);

        let raw = ScriptedClient::new([]);
        let timer = Arc::new(TestTimer::default());
        timer
            .expired
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let exec = executor(raw.clone(), timer);
        let error = sdk()
            .get_item(&exec, "ws", "a", ItemGetOptions::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            ItemErrorKind::Execution(ItemExecutionError::TimedOut)
        );
        assert!(raw.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn metadata_facade_preserves_http_body_order_through_shared_retry_and_bounds() {
        let body = r#"{"future":{"z":null,"a":[1,1.0]},"displayName":"N","id":"i","type":null}"#;
        let raw = ScriptedClient::new([response(503, "", &[]), response(200, body, &[])]);
        let exec = executor(raw.clone(), Arc::new(TestTimer::default()));
        let metadata = sdk()
            .get_item_metadata(
                &exec,
                "ws",
                "i",
                ItemGetOptions {
                    max_response_bytes: NonZeroUsize::new(body.len()).unwrap(),
                    ..Default::default()
                },
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(serde_json::to_string(&metadata).unwrap(), body);
        assert_eq!(raw.requests.lock().unwrap().len(), 2);

        let raw = ScriptedClient::new([response(200, &format!("{body} "), &[])]);
        let exec = executor(raw, Arc::new(TestTimer::default()));
        assert_eq!(
            sdk()
                .get_item_metadata(
                    &exec,
                    "ws",
                    "i",
                    ItemGetOptions {
                        max_response_bytes: NonZeroUsize::new(body.len()).unwrap(),
                        ..Default::default()
                    },
                    pending(),
                )
                .await
                .unwrap_err()
                .kind,
            ItemErrorKind::ResponseLimit
        );

        let raw = ScriptedClient::new((0..4).map(|_| response(503, "", &[])));
        let exec = executor(raw.clone(), Arc::new(TestTimer::default()));
        assert_eq!(
            sdk()
                .get_item_metadata(&exec, "ws", "i", ItemGetOptions::default(), pending())
                .await
                .unwrap_err()
                .kind,
            ItemErrorKind::Execution(ItemExecutionError::RetriesExhausted)
        );
        assert_eq!(raw.requests.lock().unwrap().len(), 4);

        let raw = ScriptedClient::new([]);
        let timer = Arc::new(TestTimer::default());
        timer
            .expired
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let exec = executor(raw.clone(), timer);
        assert_eq!(
            sdk()
                .get_item_metadata(&exec, "ws", "i", ItemGetOptions::default(), pending())
                .await
                .unwrap_err()
                .kind,
            ItemErrorKind::Execution(ItemExecutionError::TimedOut)
        );
        assert!(raw.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn specialized_metadata_reuses_http_retries_query_and_status_precedence() {
        let body = r#"{"before":null,"displayName":"N","id":"i","type":17}"#;
        let raw = ScriptedClient::new([response(503, "", &[]), response(200, body, &[])]);
        let exec = executor(raw.clone(), Arc::new(TestTimer::default()));
        let metadata = sdk()
            .get_item_metadata_from_collection(
                &exec,
                "ws",
                ItemCollection::GraphQuerySet,
                "i",
                ItemGetOptions {
                    query: vec![("q".into(), "*~ +".into())],
                    ..Default::default()
                },
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(serde_json::to_string(&metadata).unwrap(), body);
        {
            let requests = raw.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            for request in requests.iter() {
                assert_eq!(
                    request.url().as_str(),
                    "https://api.fabric.microsoft.com/v1/workspaces/ws/GraphQuerySets/i?q=%2A~+%2B"
                );
                assert_eq!(request.method(), azure_core::http::Method::Get);
                assert!(request.headers().get_str(&"authorization".into()).is_ok());
            }
        }
        for (status, expected) in [(401, "HeaderCode"), (403, "HeaderCode"), (404, "NotFound")] {
            let raw = ScriptedClient::new([response(
                status,
                r#"{"errorCode":"BodyCode","message":"private"}"#,
                &[("x-ms-public-api-error-code", "HeaderCode")],
            )]);
            let exec = executor(raw.clone(), Arc::new(TestTimer::default()));
            let error = sdk()
                .get_item_metadata_from_collection(
                    &exec,
                    "ws",
                    ItemCollection::GraphQlApi,
                    "i",
                    ItemGetOptions::default(),
                    pending(),
                )
                .await
                .unwrap_err();
            assert!(
                matches!(error.kind, ItemErrorKind::Http { status: actual, ref code, .. }
                if actual == status && code == expected)
            );
            assert!(!error.to_string().contains("private"));
            assert_eq!(
                raw.requests.lock().unwrap()[0].url().as_str(),
                "https://api.fabric.microsoft.com/v1/workspaces/ws/graphqlapis/i"
            );
        }
    }

    struct BodyDrop(Arc<AtomicBool>);
    impl Drop for BodyDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn specialized_metadata_stream_limit_and_inflight_cancel_drop_shared_http_body() {
        use azure_core::{
            Bytes,
            http::{AsyncRawResponse, StatusCode, headers::Headers},
        };
        for cancelled in [false, true] {
            let polls = Arc::new(AtomicUsize::new(0));
            let dropped = Arc::new(AtomicBool::new(false));
            let body_drop = BodyDrop(dropped.clone());
            let count = polls.clone();
            let body = futures_util::stream::poll_fn(move |_| {
                let _keep_until_drop = &body_drop;
                count.fetch_add(1, Ordering::SeqCst);
                if cancelled {
                    Poll::Pending
                } else {
                    Poll::Ready(Some(Ok(Bytes::from_static(b"x"))))
                }
            });
            let raw = ScriptedClient::new([AsyncRawResponse::new(
                StatusCode::Ok,
                Headers::new(),
                Box::pin(body),
            )]);
            let exec = executor(raw.clone(), Arc::new(TestTimer::default()));
            let client = sdk();
            let stop = AtomicBool::new(false);
            let signal = poll_fn(|_| {
                if stop.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            });
            let mut future = Box::pin(client.get_item_metadata_from_collection(
                &exec,
                "ws",
                ItemCollection::Notebook,
                "i",
                ItemGetOptions {
                    max_response_bytes: NonZeroUsize::new(5).unwrap(),
                    ..Default::default()
                },
                signal,
            ));
            if cancelled {
                assert!(
                    future
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                        .is_pending()
                );
                stop.store(true, Ordering::SeqCst);
            }
            let error = future.await.unwrap_err();
            assert_eq!(
                error.kind,
                if cancelled {
                    ItemErrorKind::Cancelled
                } else {
                    ItemErrorKind::ResponseLimit
                }
            );
            assert!(dropped.load(Ordering::SeqCst));
            assert_eq!(polls.load(Ordering::SeqCst), if cancelled { 1 } else { 6 });
            let requests = raw.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(
                requests[0].url().as_str(),
                "https://api.fabric.microsoft.com/v1/workspaces/ws/notebooks/i"
            );
        }
    }
}
