// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public shared collector contracts; no HTTP, credential or runtime creation.

use azure_core::http::Url;
use fabric_sdk_paging::{PageResponse, PagingErrorKind, PagingLimits, TokenPager};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{pending, poll_fn},
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    task::Poll,
};

const ROUTE: &str = "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/jobs/instances";

fn pager(limits: PagingLimits) -> TokenPager {
    TokenPager::new(Url::parse(ROUTE).unwrap(), limits).unwrap()
}
fn page(body: Value) -> PageResponse {
    PageResponse {
        status: 200,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

#[tokio::test]
async fn independent_source_pages_preserve_token_precedence_encoding_order_and_terminal_uris() {
    let cases: Value =
        serde_json::from_str(include_str!("../../../tests/compatibility/jobs/cases.json")).unwrap();
    for case in cases["paging"].as_array().unwrap() {
        let queue = Arc::new(Mutex::new(
            case["pages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    let mut result = page(p["body"].clone());
                    if let Some(headers) = p.get("headers").and_then(Value::as_object) {
                        result.headers = headers
                            .iter()
                            .map(|(k, v)| (k.clone(), v.as_str().unwrap().into()))
                            .collect();
                    }
                    result
                })
                .collect::<VecDeque<_>>(),
        ));
        let urls = Arc::new(Mutex::new(Vec::new()));
        let result = pager(PagingLimits::default())
            .collect(
                |request| {
                    urls.lock()
                        .unwrap()
                        .push(request.url().query().unwrap_or("").to_owned());
                    let response = queue
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("unexpected continuation");
                    async move { Ok::<_, &'static str>(response) }
                },
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(
            json!(urls.lock().unwrap().clone()),
            case["queries"],
            "{}",
            case["name"]
        );
        assert_eq!(json!(result.value), case["expected"]);
        assert_eq!(json!(result.pages_received), case["pages_received"]);
        assert!(result.next_token.is_none());
    }
}

#[tokio::test]
async fn later_fetch_failures_retain_only_full_committed_pages() {
    let mut calls = 0;
    let error = pager(PagingLimits::default())
        .collect(
            |_| {
                calls += 1;
                let result = if calls == 1 {
                    Ok(page(json!({"value":[{"id":1}],"continuationToken":"next"})))
                } else {
                    Err("synthetic fetch failure")
                };
                async move { result }
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind,
        PagingErrorKind::Fetch("synthetic fetch failure")
    ));
    assert_eq!(error.partial.value, vec![json!({"id":1})]);
    assert_eq!(error.partial.pages_received, 1);
    assert_eq!(error.partial.next_token.as_deref(), Some("next"));
}

#[tokio::test]
async fn page_row_byte_and_repeated_token_limits_are_explicit_incomplete_errors() {
    let limits = PagingLimits {
        max_pages: NonZeroUsize::new(1).unwrap(),
        ..PagingLimits::default()
    };
    let error = pager(limits)
        .collect(
            |_| async { Ok::<_, ()>(page(json!({"value":[{"id":1}],"continuationToken":"next"}))) },
            pending(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error.kind, PagingErrorKind::PageLimit));
    assert_eq!(error.partial.pages_received, 1);
    let limits = PagingLimits {
        max_items: NonZeroUsize::new(1).unwrap(),
        ..PagingLimits::default()
    };
    let error = pager(limits)
        .collect(
            |_| async { Ok::<_, ()>(page(json!({"value":[1,2]}))) },
            pending(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error.kind, PagingErrorKind::ItemLimit));
    assert_eq!(error.partial.pages_received, 0);
    assert!(error.partial.value.is_empty());
    let limits = PagingLimits {
        max_response_bytes: NonZeroUsize::new(2).unwrap(),
        ..PagingLimits::default()
    };
    let error = pager(limits)
        .collect(
            |request| async move {
                assert_eq!(request.max_response_bytes().get(), 2);
                Ok::<_, ()>(page(json!({"value":[]})))
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error.kind, PagingErrorKind::ResponseLimit));
    let error = pager(PagingLimits::default())
        .collect(
            |_| async { Ok::<_, ()>(page(json!({"value":[1],"continuationToken":"same"}))) },
            pending(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error.kind, PagingErrorKind::RepeatedContinuation));
    assert_eq!(error.partial.pages_received, 2);
}

#[tokio::test]
async fn active_continuation_uri_is_bound_to_exact_origin_and_raw_route() {
    for uri in [
        "https://evil.invalid/v1/workspaces/ws/items/item/jobs/instances",
        "https://api.fabric.microsoft.com/v1/workspaces/other/items/item/jobs/instances",
        "https://api.fabric.microsoft.com:444/v1/workspaces/ws/items/item/jobs/instances",
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/jobs/x/../instances",
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/jobs/instances#fragment",
    ] {
        let error = pager(PagingLimits::default())
            .collect(
                |_| async move {
                    Ok::<_, ()>(page(
                        json!({"value":[1],"continuationToken":"next","continuationUri":uri}),
                    ))
                },
                pending(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error.kind, PagingErrorKind::InvalidContinuation));
        assert_eq!(error.partial.pages_received, 1);
    }
}

#[tokio::test]
async fn cancellation_wins_ready_pages_and_retains_prior_progress() {
    let mut called = false;
    let error = pager(PagingLimits::default())
        .collect(
            |_| {
                called = true;
                async { Ok::<_, ()>(page(json!({"value":[]}))) }
            },
            async {},
        )
        .await
        .unwrap_err();
    assert!(matches!(error.kind, PagingErrorKind::Cancelled));
    assert!(!called);
    let mut first = true;
    let cancelled = poll_fn(move |cx| {
        if first {
            first = false;
            cx.waker().wake_by_ref();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    });
    let error = pager(PagingLimits::default())
        .collect(
            |_| async { Ok::<_, ()>(page(json!({"value":[1],"continuationToken":"next"}))) },
            cancelled,
        )
        .await
        .unwrap_err();
    assert!(matches!(error.kind, PagingErrorKind::Cancelled));
    assert_eq!(error.partial.pages_received, 1);
}

#[tokio::test]
async fn malformed_pages_noncompleted_statuses_and_initial_repeated_token_are_not_successes() {
    for body in [
        json!(null),
        json!({}),
        json!({"value":null}),
        json!({"value":[],"continuationToken":true}),
    ] {
        let error = pager(PagingLimits::default())
            .collect(|_| async { Ok::<_, ()>(page(body.clone())) }, pending())
            .await
            .unwrap_err();
        assert!(matches!(error.kind, PagingErrorKind::InvalidResponse));
    }
    for status in [201, 202, 204, 403] {
        let error = pager(PagingLimits::default())
            .collect(
                |_| async {
                    let mut response = page(json!({"value":[]}));
                    response.status = status;
                    Ok::<_, ()>(response)
                },
                pending(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error.kind,PagingErrorKind::UnexpectedStatus(s) if s==status));
    }
    let mut base = Url::parse(ROUTE).unwrap();
    base.set_query(Some("type=first&type=second&continuationToken=initial"));
    let error = TokenPager::new(base, PagingLimits::default())
        .unwrap()
        .collect(
            |request| async move {
                assert_eq!(
                    request.url().query(),
                    Some("type=second&continuationToken=initial")
                );
                Ok::<_, ()>(page(json!({"value":[1],"continuationToken":"initial"})))
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error.kind, PagingErrorKind::RepeatedContinuation));
    assert_eq!(error.partial.pages_received, 1);
}
