// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standalone public consumer: no facade, identity backend, transport, or CLI.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    error::ErrorKind,
    time::OffsetDateTime,
};
use fabric_core::Audience;
use fabric_sdk_capacities::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::{Pin, pin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

const ENDPOINT: &str = "https://api.fabric.microsoft.com/v1/capacities";
const TOKEN: &str = "offline-only-not-a-real-secret";

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
    fail: bool,
    fail_at: Option<usize>,
    pending: bool,
    drops: AtomicUsize,
}

struct DropGuard<'a>(&'a AtomicUsize);
impl Drop for DropGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.pending {
            let _guard = DropGuard(&self.drops);
            pending::<()>().await;
        }
        if self.fail || self.fail_at == Some(call) {
            return Err(azure_core::Error::with_message(
                ErrorKind::Other,
                "provider-sensitive",
            ));
        }
        Ok(AccessToken::new(TOKEN, OffsetDateTime::UNIX_EPOCH))
    }
}

#[derive(Default)]
struct Executor {
    replies: Mutex<VecDeque<Result<CapacityResponse, CapacityExecutionError>>>,
    urls: Mutex<Vec<String>>,
    pending: bool,
    drops: AtomicUsize,
    cancel: Option<(Arc<AtomicBool>, usize)>,
}

impl Executor {
    fn new(replies: impl IntoIterator<Item = CapacityResponse>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().map(Ok).collect()),
            ..Self::default()
        }
    }
    fn calls(&self) -> usize {
        self.urls.lock().unwrap().len()
    }
}

impl CapacityExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: CapacityRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<CapacityResponse, CapacityExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            assert_eq!(request.content_type(), "application/json");
            assert_eq!(request.token().token.secret(), TOKEN);
            assert!(!format!("{request:?}").contains(TOKEN));
            assert!(!format!("{request:?}").contains("opaque"));
            self.urls.lock().unwrap().push(request.url().to_string());
            if let Some((signal, index)) = &self.cancel
                && self.calls() == *index
            {
                signal.store(true, Ordering::SeqCst);
            }
            if self.pending {
                let _guard = DropGuard(&self.drops);
                pending::<()>().await;
            }
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request")
        })
    }
}

fn sdk(credential: Arc<Credential>, audience: Audience) -> CapacityClient {
    CapacityClient::new(
        credential,
        CredentialSelection::new("account", "tenant", audience).unwrap(),
    )
}
fn row(id: &str) -> Value {
    json!({"id":id,"displayName":"b/Research\u{65e5}","state":"futureSTATE","region":null})
}
fn response(status: u16, body: Value) -> CapacityResponse {
    CapacityResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}
fn page(ids: &[&str], token: Option<&str>) -> CapacityResponse {
    let mut body = json!({"value": ids.iter().map(|id| row(id)).collect::<Vec<_>>()});
    if let Some(token) = token {
        body["continuationToken"] = json!(token);
    }
    response(200, body)
}
fn limit(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}
fn cancelled(signal: &AtomicBool) -> impl Future<Output = ()> + '_ {
    poll_fn(move |_| {
        if signal.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
}

#[tokio::test]
async fn ordered_pages_duplicates_case_unknowns_null_and_python_token_encoding() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::new([
        page(&["z", "a"], Some("opaque+/= %~*?\u{65e5}")),
        page(&[], Some("second")),
        page(&["a"], None),
    ]);
    let client = sdk(credential.clone(), Audience::Fabric);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(!format!("{client:?}").contains(TOKEN));
    let list = client
        .list_capacities(&executor, CapacityListLimits::default(), pending())
        .await
        .unwrap();
    assert_eq!(
        list.value.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["z", "a", "a"]
    );
    assert_eq!(list.pages_received, 3);
    assert!(list.next_token.is_none());
    assert_eq!(list.value[0].display_name, "b/Research\u{65e5}");
    assert_eq!(list.value[0].display_name(), "b/Research\u{65e5}");
    assert_eq!(list.value[0].state(), Some(&json!("futureSTATE")));
    assert_eq!(list.value[0].region(), Some(&Value::Null));
    assert_eq!(list.value[0].sku(), None);
    assert_eq!(serde_json::to_value(&list.value[0]).unwrap(), row("z"));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        *executor.urls.lock().unwrap(),
        [
            ENDPOINT.to_owned(),
            format!("{ENDPOINT}?continuationToken=opaque%2B%2F%3D+%25~%2A%3F%E6%97%A5"),
            format!("{ENDPOINT}?continuationToken=second"),
        ]
    );
    let raw = json!({"id":"opaque","displayName":"UPPER","future":{"nested":[1,null]},"sku":false});
    let capacity: Capacity = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(capacity.sku(), Some(&Value::Bool(false)));
    assert_eq!(serde_json::to_value(capacity).unwrap(), raw);
    let minimal: Capacity = serde_json::from_value(json!({"id":"","displayName":""})).unwrap();
    assert!(minimal.id.is_empty());
    assert!(minimal.display_name.is_empty());
}

#[tokio::test]
async fn empty_pages_and_header_presence_override_even_malformed_body_token() {
    for token in ["", "header"] {
        let mut first = response(
            201,
            json!({
                "value":[], "continuationToken":42, "continuationUri":null
            }),
        );
        first
            .headers
            .insert("X-MS-ConTinuation".into(), token.into());
        let executor = Executor::new([first, page(&[], None)]);
        let list = sdk(Arc::default(), Audience::Fabric)
            .list_capacities(&executor, CapacityListLimits::default(), pending())
            .await
            .unwrap();
        assert!(list.value.is_empty());
        assert_eq!(list.pages_received, if token.is_empty() { 1 } else { 2 });
        assert_eq!(executor.calls(), list.pages_received);
    }
    for token in [Value::Null, json!("")] {
        let executor = Executor::new([response(
            200,
            json!({
                "value":[], "continuationToken":token, "continuationUri":42
            }),
        )]);
        assert!(
            sdk(Arc::default(), Audience::Fabric)
                .list_capacities(&executor, CapacityListLimits::default(), pending())
                .await
                .is_ok()
        );
    }
}

#[tokio::test]
async fn successful_201_followup_is_not_silently_omitted() {
    let mut second = page(&["b"], None);
    second.status = 201;
    let executor = Executor::new([page(&["a"], Some("next")), second]);
    let list = sdk(Arc::default(), Audience::Fabric)
        .list_capacities(&executor, CapacityListLimits::default(), pending())
        .await
        .unwrap();
    assert_eq!(
        list.value
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(list.pages_received, 2);
    assert!(list.next_token.is_none());
}

#[tokio::test]
async fn malformed_rows_pages_and_selected_tokens_fail_without_empty_success() {
    for body in [
        json!({}),
        json!({"value":null}),
        json!({"value":{}}),
        json!({"value":[null]}),
        json!({"value":[{}]}),
        json!({"value":[{"id":"x"}]}),
        json!({"value":[{"id":null,"displayName":"x"}]}),
        json!({"value":[{"id":"x","displayName":3}]}),
        json!({"value":[],"continuationToken":42}),
        json!({"value":[],"continuationToken":false}),
    ] {
        let executor = Executor::new([page(&["good"], Some("next")), response(200, body)]);
        let error = sdk(Arc::default(), Audience::Fabric)
            .list_capacities(&executor, CapacityListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, CapacityListErrorKind::InvalidResponse);
        assert_eq!(error.partial.pages_received, 1);
        assert_eq!(error.partial.value[0].id, "good");
        assert_eq!(error.partial.next_token.as_deref(), Some("next"));
    }
}

#[tokio::test]
async fn source_service_error_precedence_and_structured_details() {
    for (status, headers, expected) in [
        (401, vec![], "Unauthorized"),
        (403, vec![], "Forbidden"),
        (
            403,
            vec![
                ("X-MS-Public-API-Error-Code", "Public"),
                ("x-ms-error-code", "Other"),
            ],
            "Public",
        ),
        (
            403,
            vec![
                ("x-ms-public-api-error-code", ""),
                ("X-MS-Error-Code", "Other"),
            ],
            "Other",
        ),
        (
            404,
            vec![("x-ms-public-api-error-code", "Ignored")],
            "NotFound",
        ),
        (429, vec![], "BodyCode"),
        (400, vec![], "BodyCode"),
    ] {
        let body = json!({"errorCode":"BodyCode","message":"sensitive service text","requestId":"id","details":[1]});
        let mut rejected = response(status, body.clone());
        rejected.headers = headers
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        let executor = Executor::new([page(&["good"], Some("next")), rejected]);
        let error = sdk(Arc::default(), Audience::Fabric)
            .list_capacities(&executor, CapacityListLimits::default(), pending())
            .await
            .unwrap_err();
        match &error.kind {
            CapacityListErrorKind::Http {
                status: actual,
                code,
                service_error,
            } => {
                assert_eq!(*actual, status);
                assert_eq!(code, expected);
                assert_eq!(
                    serde_json::to_value(service_error.as_ref().unwrap()).unwrap(),
                    body
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(error.partial.value.len(), 1);
        assert!(!error.to_string().contains("sensitive"));
    }
    for status in [202, 204] {
        let executor = Executor::new([response(status, json!({"value":[]}))]);
        assert_eq!(
            sdk(Arc::default(), Audience::Fabric)
                .list_capacities(&executor, CapacityListLimits::default(), pending())
                .await
                .unwrap_err()
                .kind,
            CapacityListErrorKind::UnsupportedStatus(status)
        );
    }
    let executor = Executor::new([CapacityResponse {
        status: 500,
        body: b"not JSON".to_vec(),
        headers: BTreeMap::new(),
    }]);
    assert!(matches!(sdk(Arc::default(),Audience::Fabric)
        .list_capacities(&executor,CapacityListLimits::default(),pending()).await.unwrap_err().kind,
        CapacityListErrorKind::Http{code,service_error:None,..} if code=="UnknownError"));
}

#[tokio::test]
async fn fixed_active_uri_validation_terminal_ignore_and_cycle_limit() {
    for uri in [
        "http://api.fabric.microsoft.com/v1/capacities",
        "https://other.invalid/v1/capacities",
        "https://api.fabric.microsoft.com:444/v1/capacities",
        "https://user@api.fabric.microsoft.com/v1/capacities",
        "https://api.fabric.microsoft.com/v1/workspaces",
        "https://api.fabric.microsoft.com/v1/capacities/child",
        "https://api.fabric.microsoft.com/v1/capacities#fragment",
        "/v1/capacities",
    ] {
        for active in [true, false] {
            let executor = Executor::new([response(
                200,
                json!({
                    "value":[row("x")],"continuationToken":if active{Some("next")}else{None},
                    "continuationUri":uri
                }),
            )]);
            let result = sdk(Arc::default(), Audience::Fabric)
                .list_capacities(&executor, CapacityListLimits::default(), pending())
                .await;
            if active {
                let error = result.unwrap_err();
                assert_eq!(error.kind, CapacityListErrorKind::InvalidContinuation);
                assert_eq!(error.partial.pages_received, 1);
            } else {
                assert!(result.is_ok());
            }
            assert_eq!(executor.calls(), 1);
        }
    }
    let executor = Executor::new([
        response(
            200,
            json!({"value":[],"continuationToken":"a","continuationUri":format!("{ENDPOINT}?evil=ignored")}),
        ),
        page(&["x"], Some("a")),
    ]);
    let error = sdk(Arc::default(), Audience::Fabric)
        .list_capacities(&executor, CapacityListLimits::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, CapacityListErrorKind::RepeatedContinuation);
    assert_eq!(error.partial.pages_received, 2);
    assert_eq!(
        executor.urls.lock().unwrap()[1],
        format!("{ENDPOINT}?continuationToken=a")
    );
}

#[tokio::test]
async fn exact_and_plus_one_page_item_and_response_bounds() {
    let defaults = CapacityListLimits::default();
    assert_eq!(
        (
            defaults.max_pages.get(),
            defaults.max_items.get(),
            defaults.max_response_bytes.get()
        ),
        (100, 100_000, 4 * 1024 * 1024)
    );
    let limits = CapacityListLimits {
        max_pages: limit(2),
        max_items: limit(2),
        ..defaults
    };
    let executor = Executor::new([page(&["a"], Some("next")), page(&["b"], None)]);
    assert_eq!(
        sdk(Arc::default(), Audience::Fabric)
            .list_capacities(&executor, limits, pending())
            .await
            .unwrap()
            .value
            .len(),
        2
    );
    for (second, kind) in [
        (
            page(&["b"], Some("third")),
            CapacityListErrorKind::PageLimit,
        ),
        (page(&["b", "c"], None), CapacityListErrorKind::ItemLimit),
    ] {
        let executor = Executor::new([page(&["a"], Some("next")), second]);
        let error = sdk(Arc::default(), Audience::Fabric)
            .list_capacities(&executor, limits, pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(
            error.partial.pages_received,
            if kind == CapacityListErrorKind::PageLimit {
                2
            } else {
                1
            }
        );
        assert_eq!(executor.calls(), 2);
    }
    let bytes = page(&[], None).body.len();
    for bound in [bytes, bytes - 1] {
        let executor = Executor::new([page(&[], None)]);
        let result = sdk(Arc::default(), Audience::Fabric)
            .list_capacities(
                &executor,
                CapacityListLimits {
                    max_response_bytes: limit(bound),
                    ..defaults
                },
                pending(),
            )
            .await;
        if bound == bytes {
            assert_eq!(result.unwrap().pages_received, 1);
        } else {
            assert_eq!(
                result.unwrap_err().kind,
                CapacityListErrorKind::ResponseLimit
            );
        }
    }
}

#[tokio::test]
async fn actual_default_boundaries_are_exercised_not_only_smaller_proxies() {
    let limits = CapacityListLimits::default();
    for over in [false, true] {
        let replies = (0..100).map(|index| {
            let token = if index < 99 || over {
                Some(format!("page-{index}"))
            } else {
                None
            };
            page(&[], token.as_deref())
        });
        let executor = Executor::new(replies);
        let result = sdk(Arc::default(), Audience::Fabric)
            .list_capacities(&executor, limits, pending())
            .await;
        if over {
            let error = result.unwrap_err();
            assert_eq!(error.kind, CapacityListErrorKind::PageLimit);
            assert_eq!(error.partial.pages_received, 100);
        } else {
            assert_eq!(result.unwrap().pages_received, 100);
        }
        assert_eq!(executor.calls(), 100);
    }
    for over in [false, true] {
        let replies=(0..100).map(|index| {
            let count=if over&&index==99 {1001}else{1000};
            let token=if index<99 {Some(format!("page-{index}"))}else{None};
            response(200,json!({
                "value":(0..count).map(|_|json!({"id":"x","displayName":"X"})).collect::<Vec<_>>(),
                "continuationToken":token
            }))
        });
        let executor = Executor::new(replies);
        let result = sdk(Arc::default(), Audience::Fabric)
            .list_capacities(&executor, limits, pending())
            .await;
        if over {
            let error = result.unwrap_err();
            assert_eq!(error.kind, CapacityListErrorKind::ItemLimit);
            assert_eq!(error.partial.value.len(), 99_000);
            assert_eq!(error.partial.pages_received, 99);
        } else {
            assert_eq!(result.unwrap().value.len(), 100_000);
        }
    }
    for extra in [0, 1] {
        let mut reply = page(&[], None);
        reply
            .body
            .resize(limits.max_response_bytes.get() + extra, b' ');
        let executor = Executor::new([reply]);
        let result = sdk(Arc::default(), Audience::Fabric)
            .list_capacities(&executor, limits, pending())
            .await;
        if extra == 0 {
            assert_eq!(result.unwrap().pages_received, 1);
        } else {
            assert_eq!(
                result.unwrap_err().kind,
                CapacityListErrorKind::ResponseLimit
            );
        }
    }
}

#[tokio::test]
async fn invalid_audience_and_credential_and_executor_failures_are_explicit() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::default();
    assert_eq!(
        sdk(credential.clone(), Audience::Arm)
            .list_capacities(&executor, CapacityListLimits::default(), pending())
            .await
            .unwrap_err()
            .kind,
        CapacityListErrorKind::InvalidAudience
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert_eq!(executor.calls(), 0);
    let credential = Arc::new(Credential {
        fail: true,
        ..Credential::default()
    });
    let error = sdk(credential, Audience::Fabric)
        .list_capacities(&executor, CapacityListLimits::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, CapacityListErrorKind::Credential);
    assert!(!error.to_string().contains("provider-sensitive"));
    let credential = Arc::new(Credential {
        fail_at: Some(2),
        ..Credential::default()
    });
    let executor = Executor::new([page(&["a"], Some("next"))]);
    let error = sdk(credential, Audience::Fabric)
        .list_capacities(&executor, CapacityListLimits::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, CapacityListErrorKind::Credential);
    assert_eq!(error.partial.value[0].id, "a");
    assert_eq!(error.partial.next_token.as_deref(), Some("next"));
    assert_eq!(executor.calls(), 1);
    for failure in [
        CapacityExecutionError::Failed,
        CapacityExecutionError::TimedOut,
        CapacityExecutionError::Cancelled,
        CapacityExecutionError::RetriesExhausted,
        CapacityExecutionError::ResponseLimit,
    ] {
        let executor = Executor::new([page(&["a"], Some("next"))]);
        executor.replies.lock().unwrap().push_back(Err(failure));
        let error = sdk(Arc::default(), Audience::Fabric)
            .list_capacities(&executor, CapacityListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            if failure == CapacityExecutionError::ResponseLimit {
                CapacityListErrorKind::ResponseLimit
            } else {
                CapacityListErrorKind::Execution(failure)
            }
        );
        assert_eq!(error.partial.pages_received, 1);
    }
}

#[tokio::test]
async fn cancellation_pre_ready_response_and_between_ready_pages_preserves_committed_progress() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::default();
    assert_eq!(
        sdk(credential.clone(), Audience::Fabric)
            .list_capacities(&executor, CapacityListLimits::default(), ready(()))
            .await
            .unwrap_err()
            .kind,
        CapacityListErrorKind::Cancelled
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    for index in [1, 2] {
        let signal = Arc::new(AtomicBool::new(false));
        let executor = Executor {
            cancel: Some((signal.clone(), index)),
            ..Executor::new([page(&["a"], Some("next")), page(&["b"], None)])
        };
        let error = sdk(Arc::default(), Audience::Fabric)
            .list_capacities(&executor, CapacityListLimits::default(), cancelled(&signal))
            .await
            .unwrap_err();
        assert_eq!(error.kind, CapacityListErrorKind::Cancelled);
        assert_eq!(error.partial.pages_received, index - 1);
        assert_eq!(error.partial.value.len(), index - 1);
    }
}

#[test]
fn in_flight_credential_and_executor_are_dropped_on_cancel_or_future_drop() {
    for credential_pending in [true, false] {
        for explicit_cancel in [true, false] {
            let credential = Arc::new(Credential {
                pending: credential_pending,
                ..Credential::default()
            });
            let executor = Executor {
                pending: !credential_pending,
                ..Executor::default()
            };
            let client = sdk(credential.clone(), Audience::Fabric);
            let signal = AtomicBool::new(false);
            {
                let mut future = pin!(client.list_capacities(
                    &executor,
                    CapacityListLimits::default(),
                    cancelled(&signal)
                ));
                let mut cx = Context::from_waker(Waker::noop());
                assert!(future.as_mut().poll(&mut cx).is_pending());
                if explicit_cancel {
                    signal.store(true, Ordering::SeqCst);
                    let Poll::Ready(Err(error)) = future.as_mut().poll(&mut cx) else {
                        panic!("cancel must win")
                    };
                    assert_eq!(error.kind, CapacityListErrorKind::Cancelled);
                }
            }
            assert_eq!(
                credential.drops.load(Ordering::SeqCst),
                usize::from(credential_pending)
            );
            assert_eq!(
                executor.drops.load(Ordering::SeqCst),
                usize::from(!credential_pending)
            );
        }
    }
}
