// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use async_trait::async_trait;
use azure_core::{
    credentials::{TokenCredential, TokenRequestOptions},
    error::ErrorKind,
    time::OffsetDateTime,
};
use fabric_core::CredentialSelection;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::pending,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Waker},
};

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
    fail: bool,
    wait: bool,
    dropped: AtomicBool,
}

struct Dropped<'a>(&'a AtomicBool);
impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        let _guard = Dropped(&self.dropped);
        if self.wait {
            pending::<()>().await;
        }
        if self.fail {
            return Err(azure_core::Error::with_message(
                ErrorKind::Other,
                "provider-secret",
            ));
        }
        Ok(AccessToken {
            token: "offline-token-not-for-network".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}

fn client(credential: Arc<Credential>) -> ItemClient {
    ItemClient::new(
        credential,
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    )
}

#[derive(Default)]
struct Executor {
    responses: Mutex<VecDeque<Result<ItemResponse, ItemExecutionError>>>,
    requests: Mutex<Vec<Value>>,
    wait: bool,
    dropped: AtomicBool,
}
impl Executor {
    fn new(responses: impl IntoIterator<Item = ItemResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(Ok).collect()),
            ..Default::default()
        }
    }
}
impl ItemExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            assert_eq!(request.content_type(), "application/json");
            assert_eq!(
                request.token().token.secret(),
                "offline-token-not-for-network"
            );
            let debug = format!("{request:?}");
            assert!(!debug.contains("offline-token"));
            assert!(!debug.contains("continuationToken"));
            self.requests.lock().unwrap().push(json!({
                "method": request.method(), "url": request.url().as_str(),
                "scope": Audience::Fabric.scope()
            }));
            let _guard = Dropped(&self.dropped);
            if self.wait {
                pending::<()>().await;
            }
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request")
        })
    }
}
fn response(value: Value) -> ItemResponse {
    ItemResponse {
        status: 200,
        body: serde_json::to_vec(&value).unwrap(),
        headers: BTreeMap::new(),
    }
}
fn page(token: Option<&str>) -> ItemResponse {
    response(json!({"value":[{"id":"a","displayName":"A"}], "continuationToken":token}))
}
fn options(pages: usize, items: usize) -> ItemListOptions {
    ItemListOptions {
        limits: ItemListLimits {
            max_pages: NonZeroUsize::new(pages).unwrap(),
            max_items: NonZeroUsize::new(items).unwrap(),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn independent_python_cases_compare_exact_requests_rows_and_error_precedence() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/items/cases.json"
    ))
    .unwrap();
    assert_eq!(
        fixture["source_commit"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 30);
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let credential = Arc::new(Credential {
            fail: case.get("auth_error").is_some(),
            ..Default::default()
        });
        let sdk = client(credential.clone());
        let executor = Executor::new(case["responses"].as_array().unwrap().iter().map(|reply| {
            ItemResponse {
                status: u16::try_from(reply["status"].as_u64().unwrap()).unwrap(),
                body: serde_json::to_vec(&reply["body"]).unwrap(),
                headers: reply["headers"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(key, value)| (key.clone(), value.as_str().unwrap().to_owned()))
                    .collect(),
            }
        }));
        let query = case
            .get("query")
            .map(|query| {
                query
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|pair| {
                        (
                            pair[0].as_str().unwrap().to_owned(),
                            pair[1].as_str().unwrap().to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let workspace = case["workspace_id"].as_str().unwrap();
        let result = if case["operation"] == "list" {
            sdk.list_items(
                &executor,
                workspace,
                ItemListOptions {
                    query,
                    ..Default::default()
                },
                pending(),
            )
            .await
            .map(|list| list.value)
            .map_err(|error| error.kind)
        } else {
            sdk.get_item(
                &executor,
                workspace,
                case["item_id"].as_str().unwrap(),
                ItemGetOptions {
                    query,
                    ..Default::default()
                },
                pending(),
            )
            .await
            .map(|item| vec![item])
            .map_err(|error| error.kind)
        };
        // Python ignores these foreign URIs. Rust's explicit origin exception
        // fails before following the token; the independent goldens stay intact.
        let origin_exception = matches!(
            id,
            "list_three_pages_empty_middle" | "list_header_token_precedence_case_insensitive"
        );
        if origin_exception {
            assert_eq!(
                result.unwrap_err(),
                ItemErrorKind::InvalidContinuation,
                "{id}"
            );
            assert_eq!(
                executor.requests.lock().unwrap().as_slice(),
                &case["expected"]["requests"].as_array().unwrap()[..1],
                "{id}"
            );
            continue;
        }
        match result {
            Ok(rows) => assert_eq!(
                serde_json::to_value(rows).unwrap(),
                case["expected"]["rows"],
                "{id}"
            ),
            Err(ItemErrorKind::Http { code, .. }) => {
                assert_eq!(code, case["expected"]["error_code"], "{id}")
            }
            Err(ItemErrorKind::Credential) => assert!(case.get("auth_error").is_some(), "{id}"),
            Err(error) => panic!("{id}: unexpected {error:?}"),
        }
        assert_eq!(
            *executor.requests.lock().unwrap(),
            *case["expected"]["requests"].as_array().unwrap(),
            "{id}"
        );
        assert!(executor.responses.lock().unwrap().is_empty(), "{id}");
        assert_eq!(
            credential.calls.load(Ordering::SeqCst),
            case["expected"]["requests"].as_array().unwrap().len()
                + usize::from(case.get("auth_error").is_some()),
            "{id}"
        );
    }
}

#[tokio::test]
async fn safe_multi_page_traversal_preserves_queries_and_order_without_local_filtering() {
    let mut first = page(Some(" +/= \u{96ea}"));
    first.body = serde_json::to_vec(&json!({"value":[{"id":"a","displayName":"Zulu","type":"Report"}],
        "continuationToken":" +/= \u{96ea}",
        "continuationUri":"https://api.fabric.microsoft.com/v1/workspaces/ws/items?ignored=not-followed"})).unwrap();
    let exec = Executor::new([
        first,
        response(json!({"value":[], "continuationToken":"last"})),
        page(None),
    ]);
    let result = client(Arc::default())
        .list_items(
            &exec,
            "ws",
            ItemListOptions {
                query: vec![
                    ("type".into(), "Notebook".into()),
                    ("continuationToken".into(), "initial".into()),
                    ("type".into(), "Report".into()),
                ],
                ..Default::default()
            },
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.pages_received, 3);
    assert_eq!(
        result
            .value
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "a"]
    );
    assert_eq!(result.value[0].display_name, "Zulu");
    assert!(result.next_token.is_none());
    let requests = exec.requests.lock().unwrap();
    assert_eq!(
        requests[1]["url"],
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items?type=Report&continuationToken=+%2B%2F%3D+%E9%9B%AA"
    );
    assert_eq!(
        requests[2]["url"],
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items?type=Report&continuationToken=last"
    );
}

#[tokio::test]
async fn python_query_punctuation_golden_matches_without_whatwg_encoding_drift() {
    let case: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/items/query-encoding.json"
    ))
    .unwrap();
    let exec = Executor::new([response(json!({"value":[]}))]);
    client(Arc::default())
        .list_items(
            &exec,
            "ws",
            ItemListOptions {
                query: case["query"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|pair| {
                        (
                            pair[0].as_str().unwrap().into(),
                            pair[1].as_str().unwrap().into(),
                        )
                    })
                    .collect(),
                ..Default::default()
            },
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(
        *exec.requests.lock().unwrap(),
        *case["expected"]["requests"].as_array().unwrap()
    );
}

#[tokio::test]
async fn page_item_and_byte_bounds_are_exact_and_never_incomplete_success() {
    let sdk = client(Arc::default());
    for (responses, limits, expected_pages, expected_rows, kind) in [
        (vec![page(None)], options(1, 1), 1, 1, None),
        (
            vec![page(Some("next"))],
            options(1, 1),
            1,
            1,
            Some(ItemErrorKind::PageLimit),
        ),
        (
            vec![page(Some("next")), page(None)],
            options(2, 1),
            1,
            1,
            Some(ItemErrorKind::ItemLimit),
        ),
        (
            vec![page(Some("next")), page(None)],
            options(2, 2),
            2,
            2,
            None,
        ),
    ] {
        let exec = Executor::new(responses);
        let result = sdk.list_items(&exec, "ws", limits, pending()).await;
        let progress = match kind {
            None => result.unwrap(),
            Some(kind) => {
                let error = result.unwrap_err();
                assert_eq!(error.kind, kind);
                error.partial
            }
        };
        assert_eq!(progress.pages_received, expected_pages);
        assert_eq!(progress.value.len(), expected_rows);
    }
    let cap = ItemListLimits::default().max_response_bytes.get();
    assert_eq!(cap, 4 * 1024 * 1024);
    for extra in [0, 1] {
        let mut reply = response(json!({"value":[]}));
        reply.body.resize(cap + extra, b' ');
        let result = sdk
            .list_items(
                &Executor::new([reply]),
                "ws",
                ItemListOptions::default(),
                pending(),
            )
            .await;
        if extra == 0 {
            assert!(result.is_ok());
        } else {
            assert_eq!(result.unwrap_err().kind, ItemErrorKind::ResponseLimit);
        }
        let mut reply = response(json!({"id":"a","displayName":"A"}));
        reply.body.resize(cap + extra, b' ');
        let result = sdk
            .get_item(
                &Executor::new([reply]),
                "ws",
                "a",
                ItemGetOptions::default(),
                pending(),
            )
            .await;
        if extra == 0 {
            assert!(result.is_ok());
        } else {
            assert_eq!(result.unwrap_err().kind, ItemErrorKind::ResponseLimit);
        }
    }
}

#[tokio::test]
async fn default_hundred_pages_and_hundred_thousand_rows_boundaries_are_measured() {
    let sdk = client(Arc::default());
    assert_eq!(ItemListLimits::default().max_pages.get(), 100);
    assert_eq!(ItemListLimits::default().max_items.get(), 100_000);
    for more in [false, true] {
        let exec = Executor::new((0..100).map(|i| page(if i < 99 || more { Some("unused") } else { None })).enumerate().map(|(i, mut reply)| {
            reply.body = serde_json::to_vec(&json!({"value":[], "continuationToken": if i < 99 || more { Some(format!("token-{i}")) } else { None }})).unwrap();
            reply
        }));
        let result = sdk
            .list_items(&exec, "ws", ItemListOptions::default(), pending())
            .await;
        if more {
            let error = result.unwrap_err();
            assert_eq!(error.kind, ItemErrorKind::PageLimit);
            assert_eq!(error.partial.pages_received, 100);
        } else {
            assert_eq!(result.unwrap().pages_received, 100);
        }
        assert_eq!(exec.requests.lock().unwrap().len(), 100);
    }
    for extra in [0, 1] {
        let rows = vec![json!({"id":"a","displayName":""}); 100_000 + extra];
        let result = sdk
            .list_items(
                &Executor::new([response(json!({"value":rows}))]),
                "ws",
                ItemListOptions::default(),
                pending(),
            )
            .await;
        if extra == 0 {
            assert_eq!(result.unwrap().value.len(), 100_000);
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.kind, ItemErrorKind::ItemLimit);
            assert_eq!(error.partial.pages_received, 0);
        }
    }
}

#[tokio::test]
async fn cycles_foreign_origins_and_wrong_paths_fail_with_committed_progress() {
    let sdk = client(Arc::default());
    for uri in [
        "http://api.fabric.microsoft.com/v1/workspaces/ws/items",
        "https://evil.invalid/v1/workspaces/ws/items",
        "https://api.fabric.microsoft.com:444/v1/workspaces/ws/items",
        "https://user@api.fabric.microsoft.com/v1/workspaces/ws/items",
        "https://api.fabric.microsoft.com/v1/workspaces/other/items",
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items/a",
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items#fragment",
        "https://api.fabric.microsoft.com/v1/workspaces/ws/other/../items",
        "https://api.fabric.microsoft.com/v1/workspaces/ws/%2e/items",
        "/v1/workspaces/ws/items",
        "not a URL",
    ] {
        let exec = Executor::new([response(
            json!({"value":[{"id":"a","displayName":"A"}], "continuationToken":"next","continuationUri":uri}),
        )]);
        let error = sdk
            .list_items(&exec, "ws", ItemListOptions::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, ItemErrorKind::InvalidContinuation, "{uri}");
        assert_eq!(error.partial.value.len(), 1);
        assert_eq!(error.partial.next_token.as_deref(), Some("next"));
        assert_eq!(exec.requests.lock().unwrap().len(), 1);
    }
    for initial in [false, true] {
        let exec = Executor::new([page(Some("same")), page(Some("same"))]);
        let error = sdk
            .list_items(
                &exec,
                "ws",
                ItemListOptions {
                    query: if initial {
                        vec![("continuationToken".into(), "same".into())]
                    } else {
                        vec![]
                    },
                    ..Default::default()
                },
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ItemErrorKind::RepeatedContinuation);
        assert_eq!(error.partial.pages_received, if initial { 1 } else { 2 });
    }
}

#[tokio::test]
async fn malformed_responses_and_later_service_errors_never_become_empty_success() {
    let sdk = client(Arc::default());
    let error = sdk
        .get_item(
            &Executor::new([response(json!({
                "id":"a","displayName":"A","continuationToken":"next"
            }))]),
            "ws",
            "a",
            ItemGetOptions::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ItemErrorKind::InvalidContinuation);
    for value in [
        json!({}),
        json!({"value":null}),
        json!({"value":{}}),
        json!({"value":[{}]}),
        json!({"value":[{"id":null,"displayName":"A"}]}),
        json!({"value":[{"id":"a","displayName":1}]}),
        json!({"value":[],"continuationToken":false}),
        json!({"value":[],"continuationToken":42}),
        json!({"value":[],"continuationUri":42}),
    ] {
        let error = sdk
            .list_items(
                &Executor::new([response(value)]),
                "ws",
                ItemListOptions::default(),
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ItemErrorKind::InvalidResponse);
        assert_eq!(error.partial.pages_received, 0);
    }
    for value in [
        json!(null),
        json!({}),
        json!([]),
        json!({"id":"a","displayName":null}),
        json!({"id":"a","displayName":"A","continuationToken":false}),
    ] {
        assert_eq!(
            sdk.get_item(
                &Executor::new([response(value)]),
                "ws",
                "a",
                ItemGetOptions::default(),
                pending()
            )
            .await
            .unwrap_err()
            .kind,
            ItemErrorKind::InvalidResponse
        );
    }
    for status in [202, 204] {
        let mut reply = page(None);
        reply.status = status;
        assert_eq!(
            sdk.list_items(
                &Executor::new([reply.clone()]),
                "ws",
                ItemListOptions::default(),
                pending()
            )
            .await
            .unwrap_err()
            .kind,
            ItemErrorKind::UnsupportedStatus(status)
        );
        assert_eq!(
            sdk.get_item(
                &Executor::new([reply]),
                "ws",
                "a",
                ItemGetOptions::default(),
                pending()
            )
            .await
            .unwrap_err()
            .kind,
            ItemErrorKind::UnsupportedStatus(status)
        );
    }
    let mut created = page(None);
    created.status = 201;
    assert!(
        sdk.list_items(
            &Executor::new([created.clone()]),
            "ws",
            ItemListOptions::default(),
            pending()
        )
        .await
        .is_ok()
    );
    let error = sdk
        .list_items(
            &Executor::new([page(Some("next")), created]),
            "ws",
            ItemListOptions::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ItemErrorKind::UnsupportedStatus(201));
    assert_eq!(error.partial.pages_received, 1);
    assert_eq!(error.partial.next_token.as_deref(), Some("next"));
    for body in [b"".as_slice(), b"{", b"not JSON"] {
        let reply = ItemResponse {
            status: 200,
            body: body.to_vec(),
            headers: BTreeMap::new(),
        };
        assert_eq!(
            sdk.list_items(
                &Executor::new([reply.clone()]),
                "ws",
                ItemListOptions::default(),
                pending()
            )
            .await
            .unwrap_err()
            .kind,
            ItemErrorKind::InvalidResponse
        );
        assert_eq!(
            sdk.get_item(
                &Executor::new([reply]),
                "ws",
                "a",
                ItemGetOptions::default(),
                pending()
            )
            .await
            .unwrap_err()
            .kind,
            ItemErrorKind::InvalidResponse
        );
    }
    let mut bad = response(
        json!({"errorCode":"BodyCode","message":"untrusted secret","requestId":"body-id"}),
    );
    bad.status = 403;
    bad.headers
        .insert("X-MS-Public-Api-Error-Code".into(), "HeaderCode".into());
    bad.headers
        .insert("X-MS-Request-ID".into(), "header-id".into());
    let error = sdk
        .list_items(
            &Executor::new([page(Some("next")), bad]),
            "ws",
            ItemListOptions::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.partial.pages_received, 1);
    assert_eq!(error.partial.value.len(), 1);
    assert!(!error.to_string().contains("untrusted secret"));
    match error.kind {
        ItemErrorKind::Http {
            status,
            code,
            service_error,
            headers,
        } => {
            assert_eq!(status, 403);
            assert_eq!(code, "HeaderCode");
            assert_eq!(
                service_error.unwrap().additional_fields["requestId"],
                "body-id"
            );
            assert_eq!(headers["X-MS-Request-ID"], "header-id");
        }
        kind => panic!("{kind:?}"),
    }
}

#[tokio::test]
async fn path_segments_are_encoded_without_route_injection_and_invalid_ids_acquire_no_token() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let exec = Executor::new([response(json!({"id":"a","displayName":"A"}))]);
    sdk.get_item(
        &exec,
        "Ws \u{96ea}",
        "%2e%2e +@",
        ItemGetOptions::default(),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(
        exec.requests.lock().unwrap()[0]["url"],
        "https://api.fabric.microsoft.com/v1/workspaces/Ws%20%E9%9B%AA/items/%252e%252e%20+@"
    );
    for id in ["", ".", "..", "a/b", "a\\b", "a?b", "a#b", "a\nb", "\0"] {
        let before = credential.calls.load(Ordering::SeqCst);
        assert_eq!(
            sdk.list_items(&exec, id, ItemListOptions::default(), pending())
                .await
                .unwrap_err()
                .kind,
            ItemErrorKind::InvalidIdentifier
        );
        assert_eq!(
            sdk.get_item(&exec, "ws", id, ItemGetOptions::default(), pending())
                .await
                .unwrap_err()
                .kind,
            ItemErrorKind::InvalidIdentifier
        );
        assert_eq!(credential.calls.load(Ordering::SeqCst), before);
    }
}

#[tokio::test]
async fn auth_audience_and_execution_failures_are_explicit() {
    let credential = Arc::new(Credential::default());
    let wrong = ItemClient::new(
        credential.clone(),
        CredentialSelection::new("a", "t", Audience::Arm).unwrap(),
    );
    let exec = Executor::default();
    assert_eq!(
        wrong
            .list_items(&exec, "ws", ItemListOptions::default(), pending())
            .await
            .unwrap_err()
            .kind,
        ItemErrorKind::InvalidAudience
    );
    assert_eq!(
        wrong
            .get_item(&exec, "ws", "a", ItemGetOptions::default(), pending())
            .await
            .unwrap_err()
            .kind,
        ItemErrorKind::InvalidAudience
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    for failure in [
        ItemExecutionError::Failed,
        ItemExecutionError::TimedOut,
        ItemExecutionError::Cancelled,
        ItemExecutionError::RetriesExhausted,
        ItemExecutionError::ResponseLimit,
    ] {
        let exec = Executor {
            responses: Mutex::new([Err(failure), Err(failure)].into()),
            ..Default::default()
        };
        let kind = if failure == ItemExecutionError::ResponseLimit {
            ItemErrorKind::ResponseLimit
        } else {
            ItemErrorKind::Execution(failure)
        };
        assert_eq!(
            client(Arc::default())
                .list_items(&exec, "ws", ItemListOptions::default(), pending())
                .await
                .unwrap_err()
                .kind,
            kind
        );
        assert_eq!(
            client(Arc::default())
                .get_item(&exec, "ws", "a", ItemGetOptions::default(), pending())
                .await
                .unwrap_err()
                .kind,
            kind
        );
    }
}

#[tokio::test]
async fn cancellation_before_credentials_inflight_and_between_ready_pages_drops_resources() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let exec = Executor::default();
    assert_eq!(
        sdk.list_items(
            &exec,
            "ws",
            ItemListOptions::default(),
            std::future::ready(())
        )
        .await
        .unwrap_err()
        .kind,
        ItemErrorKind::Cancelled
    );
    assert_eq!(
        sdk.get_item(
            &exec,
            "ws",
            "a",
            ItemGetOptions::default(),
            std::future::ready(())
        )
        .await
        .unwrap_err()
        .kind,
        ItemErrorKind::Cancelled
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);

    let credential = Arc::new(Credential {
        wait: true,
        ..Default::default()
    });
    let sdk = client(credential.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let cancelled = poll_fn({
        let stop = stop.clone();
        move |_| {
            if stop.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    });
    let mut future = Box::pin(sdk.list_items(&exec, "ws", ItemListOptions::default(), cancelled));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(future.as_mut().poll(&mut cx).is_pending());
    stop.store(true, Ordering::SeqCst);
    assert_eq!(
        future
            .as_mut()
            .poll(&mut cx)
            .map(|result| result.unwrap_err().kind),
        Poll::Ready(ItemErrorKind::Cancelled)
    );
    assert!(credential.dropped.load(Ordering::SeqCst));

    let sdk = client(Arc::default());
    for get in [false, true] {
        let exec = Executor {
            wait: true,
            ..Default::default()
        };
        let stop = AtomicBool::new(false);
        let cancelled = poll_fn(|_| {
            if stop.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let mut future: Pin<Box<dyn Future<Output = ItemErrorKind>>> = if get {
            Box::pin(async {
                sdk.get_item(&exec, "ws", "a", ItemGetOptions::default(), cancelled)
                    .await
                    .unwrap_err()
                    .kind
            })
        } else {
            Box::pin(async {
                sdk.list_items(&exec, "ws", ItemListOptions::default(), cancelled)
                    .await
                    .unwrap_err()
                    .kind
            })
        };
        assert!(future.as_mut().poll(&mut cx).is_pending());
        stop.store(true, Ordering::SeqCst);
        assert_eq!(
            future.as_mut().poll(&mut cx),
            Poll::Ready(ItemErrorKind::Cancelled)
        );
        assert!(exec.dropped.load(Ordering::SeqCst));
    }
    let exec = Executor::new([page(Some("next")), page(None)]);
    let stop = AtomicBool::new(false);
    let cancelled = poll_fn(|_| {
        if stop.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let mut future = Box::pin(sdk.list_items(&exec, "ws", ItemListOptions::default(), cancelled));
    assert!(future.as_mut().poll(&mut cx).is_pending());
    assert_eq!(exec.requests.lock().unwrap().len(), 1);
    stop.store(true, Ordering::SeqCst);
    let Poll::Ready(Err(error)) = future.as_mut().poll(&mut cx) else {
        panic!("cancellation must win");
    };
    assert_eq!(error.kind, ItemErrorKind::Cancelled);
    assert_eq!(error.partial.pages_received, 1);
    assert_eq!(error.partial.next_token.as_deref(), Some("next"));
    assert_eq!(exec.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn metadata_and_legacy_get_share_status_payload_and_continuation_precedence() {
    let sdk = client(Arc::default());
    for (status, body, headers) in [
        (200, "", vec![]),
        (200, "{", vec![]),
        (200, "null", vec![]),
        (200, "[]", vec![]),
        (200, "{}", vec![]),
        (200, r#"{"id":null,"displayName":"N"}"#, vec![]),
        (200, r#"{"id":"i","displayName":1}"#, vec![]),
        (
            200,
            r#"{"id":"i","displayName":"N","continuationToken":true}"#,
            vec![],
        ),
        (
            200,
            r#"{"id":"i","displayName":"N","continuationToken":"next"}"#,
            vec![],
        ),
        (
            200,
            r#"{"id":"i","displayName":"N","continuationToken":true}"#,
            vec![("X-MS-Continuation", "")],
        ),
        (201, r#"{"extra":null,"displayName":"N","id":"i"}"#, vec![]),
        (202, "{}", vec![]),
        (204, "", vec![]),
        (401, "not JSON", vec![("X-MS-Error-Code", "Secondary")]),
        (
            403,
            r#"{"errorCode":"Ignored","message":"private"}"#,
            vec![
                ("X-MS-Public-Api-Error-Code", "Primary"),
                ("X-MS-Error-Code", "Secondary"),
            ],
        ),
        (404, r#"{"errorCode":"Ignored"}"#, vec![]),
        (
            400,
            r#"{"errorCode":"BodyCode","message":"private","requestId":"sensitive"}"#,
            vec![],
        ),
    ] {
        let reply = ItemResponse {
            status,
            body: body.as_bytes().to_vec(),
            headers: headers
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        };
        let old = sdk
            .get_item(
                &Executor::new([reply.clone()]),
                "ws",
                "i",
                ItemGetOptions::default(),
                pending(),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.kind);
        let metadata = sdk
            .get_item_metadata(
                &Executor::new([reply.clone()]),
                "ws",
                "i",
                ItemGetOptions::default(),
                pending(),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.kind);
        assert_eq!(metadata, old, "{status}: {body}");
        let specialized = sdk
            .get_item_metadata_from_collection(
                &Executor::new([reply]),
                "ws",
                ItemCollection::GraphQuerySet,
                "i",
                ItemGetOptions::default(),
                pending(),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.kind);
        assert_eq!(specialized, old, "{status}: {body}");
    }
}

#[tokio::test]
async fn metadata_get_keeps_query_identifier_identity_and_execution_policies() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let exec = Executor::new([response(json!({"extra":null,"displayName":"N","id":"i"}))]);
    sdk.get_item_metadata(
        &exec,
        "ws",
        "i",
        ItemGetOptions {
            query: vec![
                ("q".into(), "old".into()),
                ("x".into(), "*~ +".into()),
                ("q".into(), "new".into()),
            ],
            ..Default::default()
        },
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(
        exec.requests.lock().unwrap()[0]["url"],
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items/i?q=new&x=%2A~+%2B"
    );
    for (workspace, item) in [("", "i"), ("ws", "a/b"), ("ws", "..")] {
        assert_eq!(
            sdk.get_item_metadata(&exec, workspace, item, ItemGetOptions::default(), pending())
                .await
                .unwrap_err()
                .kind,
            ItemErrorKind::InvalidIdentifier
        );
    }
    let wrong = ItemClient::new(
        credential.clone(),
        CredentialSelection::new("a", "t", Audience::Arm).unwrap(),
    );
    assert_eq!(
        wrong
            .get_item_metadata(&exec, "ws", "i", ItemGetOptions::default(), pending())
            .await
            .unwrap_err()
            .kind,
        ItemErrorKind::InvalidAudience
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    let failing = client(Arc::new(Credential {
        fail: true,
        ..Default::default()
    }));
    let error = failing
        .get_item_metadata(&exec, "ws", "i", ItemGetOptions::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ItemErrorKind::Credential);
    assert!(!format!("{error:?}").contains("provider-secret"));
    for failure in [
        ItemExecutionError::Failed,
        ItemExecutionError::TimedOut,
        ItemExecutionError::Cancelled,
        ItemExecutionError::RetriesExhausted,
        ItemExecutionError::ResponseLimit,
    ] {
        let exec = Executor {
            responses: Mutex::new([Err(failure)].into()),
            ..Default::default()
        };
        let expected = if failure == ItemExecutionError::ResponseLimit {
            ItemErrorKind::ResponseLimit
        } else {
            ItemErrorKind::Execution(failure)
        };
        assert_eq!(
            sdk.get_item_metadata(&exec, "ws", "i", ItemGetOptions::default(), pending())
                .await
                .unwrap_err()
                .kind,
            expected
        );
    }
}

#[tokio::test]
async fn metadata_get_measures_exact_response_limit_and_one_extra_byte() {
    let sdk = client(Arc::default());
    for cap in [64, ItemGetOptions::default().max_response_bytes.get()] {
        for extra in [0, 1] {
            let mut reply = response(json!({"before":null,"displayName":"N","id":"i"}));
            reply.body.resize(cap + extra, b' ');
            let result = sdk
                .get_item_metadata(
                    &Executor::new([reply]),
                    "ws",
                    "i",
                    ItemGetOptions {
                        max_response_bytes: NonZeroUsize::new(cap).unwrap(),
                        ..Default::default()
                    },
                    pending(),
                )
                .await;
            if extra == 0 {
                assert_eq!(result.unwrap().id(), "i");
            } else {
                assert_eq!(result.unwrap_err().kind, ItemErrorKind::ResponseLimit);
            }
        }
    }
}

#[tokio::test]
async fn specialized_metadata_keeps_cancellation_identity_and_exact_byte_bounds() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let exec = Executor::default();
    assert_eq!(
        sdk.get_item_metadata_from_collection(
            &exec,
            "",
            ItemCollection::Notebook,
            "..",
            ItemGetOptions::default(),
            std::future::ready(()),
        )
        .await
        .unwrap_err()
        .kind,
        ItemErrorKind::Cancelled,
    );
    let wrong = ItemClient::new(
        credential.clone(),
        CredentialSelection::new("a", "t", Audience::Arm).unwrap(),
    );
    assert_eq!(
        wrong
            .get_item_metadata_from_collection(
                &exec,
                "ws",
                ItemCollection::Notebook,
                "i",
                ItemGetOptions::default(),
                pending(),
            )
            .await
            .unwrap_err()
            .kind,
        ItemErrorKind::InvalidAudience,
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    for during_auth in [true, false] {
        let credential = Arc::new(Credential {
            wait: during_auth,
            ..Default::default()
        });
        let sdk = client(credential.clone());
        let exec = Executor {
            wait: !during_auth,
            ..Default::default()
        };
        let stop = AtomicBool::new(false);
        let cancelled = poll_fn(|_| {
            if stop.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let mut future = Box::pin(sdk.get_item_metadata_from_collection(
            &exec,
            "ws",
            ItemCollection::Notebook,
            "i",
            ItemGetOptions::default(),
            cancelled,
        ));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(future.as_mut().poll(&mut cx).is_pending());
        stop.store(true, Ordering::SeqCst);
        assert_eq!(
            future
                .as_mut()
                .poll(&mut cx)
                .map(|result| result.unwrap_err().kind),
            Poll::Ready(ItemErrorKind::Cancelled),
        );
        assert!(credential.dropped.load(Ordering::SeqCst));
        assert_eq!(exec.dropped.load(Ordering::SeqCst), !during_auth);
        assert_eq!(
            exec.requests.lock().unwrap().len(),
            usize::from(!during_auth)
        );
    }
    for extra in [0, 1] {
        let mut reply = response(json!({"first":null,"displayName":"N","id":"i"}));
        reply.body.resize(64 + extra, b' ');
        let result = sdk
            .get_item_metadata_from_collection(
                &Executor::new([reply]),
                "ws",
                ItemCollection::VariableLibrary,
                "i",
                ItemGetOptions {
                    max_response_bytes: NonZeroUsize::new(64).unwrap(),
                    ..Default::default()
                },
                pending(),
            )
            .await;
        if extra == 0 {
            assert_eq!(
                result
                    .unwrap()
                    .as_value()
                    .as_object()
                    .unwrap()
                    .keys()
                    .next()
                    .unwrap(),
                "first"
            );
        } else {
            assert_eq!(result.unwrap_err().kind, ItemErrorKind::ResponseLimit);
        }
    }
}

#[tokio::test]
async fn metadata_cancellation_wins_before_and_during_credentials_or_execution() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let exec = Executor::default();
    assert_eq!(
        sdk.get_item_metadata(
            &exec,
            "ws",
            "i",
            ItemGetOptions::default(),
            std::future::ready(())
        )
        .await
        .unwrap_err()
        .kind,
        ItemErrorKind::Cancelled
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(exec.requests.lock().unwrap().is_empty());
    for during_auth in [true, false] {
        let credential = Arc::new(Credential {
            wait: during_auth,
            ..Default::default()
        });
        let sdk = client(credential.clone());
        let exec = Executor {
            wait: !during_auth,
            ..Default::default()
        };
        let stop = AtomicBool::new(false);
        let cancelled = poll_fn(|_| {
            if stop.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let mut future =
            Box::pin(sdk.get_item_metadata(&exec, "ws", "i", ItemGetOptions::default(), cancelled));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(future.as_mut().poll(&mut cx).is_pending());
        stop.store(true, Ordering::SeqCst);
        assert_eq!(
            future
                .as_mut()
                .poll(&mut cx)
                .map(|result| result.unwrap_err().kind),
            Poll::Ready(ItemErrorKind::Cancelled)
        );
        assert!(credential.dropped.load(Ordering::SeqCst));
        assert_eq!(exec.dropped.load(Ordering::SeqCst), !during_auth);
    }
}
