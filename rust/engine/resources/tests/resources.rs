// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline injected resource paging and resolution tests.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use fabric_core::{
    ApiRequest, ApiResponse, Audience, CancellationToken, CoreError, Endpoint, ErrorKind,
    HttpExecutor, MAX_RESPONSE_BYTES, OperationContext,
};
use fabric_resources::{Selector, list, request, resolve};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
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
    OperationContext::new(
        Instant::now(),
        Duration::from_secs(240),
        CancellationToken::new(),
    )
    .unwrap()
}

#[tokio::test]
async fn http_errors_preserve_only_validated_service_details() {
    let response = ApiResponse {
        status: 400,
        headers: BTreeMap::from([(
            "x-ms-request-id".into(),
            "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE".into(),
        )]),
        body: json!({
            "error": {
                "code": "NestedFailure",
                "message": "Item\u{1b} unavailable https://example.invalid/path?token=SECRET"
            },
            "errorCode": "OuterFailure",
            "requestId": "00000000-0000-0000-0000-000000000001"
        })
        .to_string()
        .into_bytes(),
    };
    let error = request(
        &fake(vec![response]),
        &Endpoint::public(Audience::Fabric),
        &context(),
        "GET",
        "workspaces",
        Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Service);
    assert_eq!(error.service_code(), Some("NestedFailure"));
    assert!(error.message.contains("HTTP 400"));
    assert!(error.message.contains("Item unavailable"));
    assert!(error.message.contains("https://example.invalid/path"));
    assert!(
        error
            .message
            .contains("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
    );
    assert!(!error.message.contains('\u{1b}'));
    assert!(!error.message.contains("SECRET"));

    let response = ApiResponse {
        status: 404,
        headers: BTreeMap::new(),
        body: json!({
            "errorCode": "NotFound",
            "message": "No workspace",
            "requestId": "00000000-0000-0000-0000-000000000001"
        })
        .to_string()
        .into_bytes(),
    };
    let error = request(
        &fake(vec![response]),
        &Endpoint::public(Audience::Fabric),
        &context(),
        "GET",
        "workspaces",
        Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.service_code(), Some("NotFound"));
    assert!(error.message.contains("No workspace"));
    assert!(
        error
            .message
            .contains("00000000-0000-0000-0000-000000000001")
    );
}

#[tokio::test]
async fn http_errors_reject_invalid_identifiers_and_use_header_fallback() {
    let invalid = ApiResponse {
        status: 400,
        headers: BTreeMap::new(),
        body: json!({
            "errorCode": "invalid/SECRET",
            "requestId": "invalid/SECRET",
            "message": "\u{1b}"
        })
        .to_string()
        .into_bytes(),
    };
    let error = request(
        &fake(vec![invalid]),
        &Endpoint::public(Audience::Fabric),
        &context(),
        "GET",
        "workspaces",
        Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.service_code(), None);
    assert!(error.message.contains("invalid service error code omitted"));
    assert!(!error.message.contains("SECRET"));

    let response = ApiResponse {
        status: 404,
        headers: BTreeMap::from([("x-ms-public-api-error-code".into(), "NotFound".into())]),
        body: b"not JSON".to_vec(),
    };
    let error = request(
        &fake(vec![response]),
        &Endpoint::public(Audience::Fabric),
        &context(),
        "GET",
        "workspaces",
        Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.service_code(), Some("NotFound"));
    assert!(error.message.contains("HTTP 404"));
    assert!(!error.message.contains("not JSON"));
}

#[tokio::test]
async fn ids_skip_http_and_page_cycles_fail_without_partial_results() {
    let executor = fake(vec![]);
    let selector = Selector::parse("{00000000-0000-0000-0000-000000000001}").unwrap();
    assert_eq!(
        resolve(
            &executor,
            &fabric_sdk_http::SystemClock,
            &Endpoint::public(Audience::Fabric),
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
            &Endpoint::public(Audience::Fabric),
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
            &Endpoint::public(Audience::Fabric),
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
            &Endpoint::public(Audience::Fabric),
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
        &Endpoint::public(Audience::Fabric),
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
            &Endpoint::public(Audience::Fabric),
            &context(),
            "workspaces",
        )
        .await;
        assert_eq!(result.is_ok(), succeeds);
    }
}
