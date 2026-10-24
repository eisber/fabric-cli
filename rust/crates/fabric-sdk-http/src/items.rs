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
    use fabric_sdk_items::{ItemErrorKind, ItemExecutionError, ItemGetOptions, ItemListOptions};
    use std::{future::pending, num::NonZeroUsize, sync::Arc};

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
}
