// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline injected resource paging and resolution tests.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use fabric_core::{
    ApiRequest, ApiResponse, Audience, CancellationToken, CoreError, HttpExecutor,
    MAX_RESPONSE_BYTES, OperationContext,
};
use fabric_resources::{Selector, list, resolve};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
    time::Duration,
};

struct Fake {
    replies: Mutex<VecDeque<ApiResponse>>,
    requests: Mutex<Vec<ApiRequest>>,
}

#[async_trait]
impl HttpExecutor for Fake {
    async fn execute(
        &self,
        request: ApiRequest,
        _: Audience,
        _: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        self.requests.lock().unwrap().push(request);
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request"))
    }
}

fn response(body: Vec<u8>) -> ApiResponse {
    ApiResponse {
        status: 200,
        headers: BTreeMap::new(),
        body,
    }
}

fn fake(replies: Vec<ApiResponse>) -> Fake {
    Fake {
        replies: Mutex::new(replies.into()),
        requests: Mutex::new(Vec::new()),
    }
}

fn context() -> OperationContext {
    OperationContext::new(Duration::from_secs(240), CancellationToken::new())
}

#[tokio::test]
async fn ids_skip_http_and_page_cycles_fail_without_partial_results() {
    let executor = fake(vec![]);
    let selector = Selector::parse("{00000000-0000-0000-0000-000000000001}").unwrap();
    assert_eq!(
        resolve(
            &executor,
            &fabric_sdk_http::SystemClock,
            &context(),
            "workspaces",
            &selector
        )
        .await
        .unwrap(),
        "00000000-0000-0000-0000-000000000001"
    );
    assert!(executor.requests.lock().unwrap().is_empty());

    let page = br#"{"value":[],"continuationToken":"next"}"#.to_vec();
    let executor = fake(vec![response(page.clone()), response(page)]);
    assert!(
        list(
            &executor,
            &fabric_sdk_http::SystemClock,
            &context(),
            "workspaces"
        )
        .await
        .is_err()
    );
    assert_eq!(executor.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn malformed_page_and_pre_cancelled_scan_fail() {
    let executor = fake(vec![response(br#"{"value":{} }"#.to_vec())]);
    assert!(
        list(
            &executor,
            &fabric_sdk_http::SystemClock,
            &context(),
            "workspaces"
        )
        .await
        .is_err()
    );
    let executor = fake(vec![]);
    let context = context();
    context.cancellation.cancel();
    assert!(
        list(
            &executor,
            &fabric_sdk_http::SystemClock,
            &context,
            "workspaces"
        )
        .await
        .is_err()
    );
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn empty_continuation_token_ends_scan() {
    let executor = fake(vec![response(
        br#"{"value":[],"continuationToken":""}"#.to_vec(),
    )]);
    let result = list(
        &executor,
        &fabric_sdk_http::SystemClock,
        &context(),
        "workspaces",
    )
    .await
    .unwrap();
    assert_eq!(result, json!([]));
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

fn page(value: Value, token: Option<&str>) -> ApiResponse {
    let mut body = json!({"value": [value]});
    if let Some(token) = token {
        body["continuationToken"] = json!(token);
    }
    response(serde_json::to_vec(&body).unwrap())
}

#[tokio::test]
async fn aggregate_limit_is_inclusive_and_applied_before_projection() {
    let overhead = serde_json::to_vec(&json!([{"v":""},{"v":""}]))
        .unwrap()
        .len();
    let available = MAX_RESPONSE_BYTES - overhead;
    let left = available / 2;
    let right = available - left;
    for (extra, succeeds) in [(0, true), (1, false)] {
        let a = json!({"v": "a".repeat(left)});
        let b = json!({"v": "b".repeat(right + extra)});
        assert_eq!(
            serde_json::to_vec(&json!([a.clone(), b.clone()]))
                .unwrap()
                .len(),
            MAX_RESPONSE_BYTES + extra
        );
        let executor = fake(vec![page(a, Some("next")), page(b, None)]);
        let result = list(
            &executor,
            &fabric_sdk_http::SystemClock,
            &context(),
            "workspaces",
        )
        .await;
        assert_eq!(result.is_ok(), succeeds);
    }
}
