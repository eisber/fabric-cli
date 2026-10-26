// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use azure_core::http::Method;
use fabric_sdk_api::{
    Origin, Paging, QueryDispatchTrace, RawRequestOptions, Request as ApiRequest, RequestBounds,
};
use std::{
    future::Future,
    pin::pin,
    sync::atomic::AtomicBool,
    task::{Context, Poll, Waker},
};

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../../tests/compatibility/catalog/query_dispatch_cases.json"
    ))
    .unwrap()
}

fn pairs(value: &Value) -> Vec<(String, String)> {
    value
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter())
        .map(|(key, value)| (key.clone(), value.as_str().unwrap().into()))
        .collect()
}

fn read_request(case: &Value) -> ApiRequest {
    ApiRequest::raw(RawRequestOptions {
        origin: Origin::Fabric,
        method: Method::Get,
        endpoint: case["uri"].as_str().unwrap().into(),
        query: pairs(&case["initial"]),
        headers: Vec::new(),
        body: Vec::new(),
        bounds: RequestBounds::default(),
    })
    .unwrap()
}

#[derive(Debug)]
struct FailCredential {
    calls: AtomicUsize,
    fail_at: usize,
}

#[async_trait]
impl TokenCredential for FailCredential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.fail_at {
            return Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Other,
                "synthetic",
            ));
        }
        Ok(AccessToken {
            token: "offline-query-trace".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}

fn configured(wire: Arc<dyn HttpClient>, credential: Arc<dyn TokenCredential>) -> ApiClient {
    let executor =
        Arc::new(HttpExecutor::new(wire, Arc::new(Timer), ExecutionOptions::default()).unwrap());
    ApiClient::new(
        credential,
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        Arc::new(fabric_cli_resources::ResourceHttpExecutor(executor)),
    )
}

#[tokio::test]
async fn original_query_dictionary_lifetime_is_reconstructed_only_by_an_explicit_caller_from_actual_read_dispatch()
 {
    let fixture = fixture();
    for case in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["name"] != "page-two-interrupt")
    {
        let trace = QueryDispatchTrace::default();
        let mut replies: Vec<_> = case["pages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|page| {
                let headers: Vec<_> = page["headers"]
                    .as_object()
                    .into_iter()
                    .flat_map(|object| object.iter())
                    .map(|(key, value)| (key.as_str(), value.as_str().unwrap()))
                    .collect();
                reply(
                    page["status"].as_u64().unwrap() as u16,
                    &page["body"].to_string(),
                    &headers,
                )
            })
            .collect();
        if !case["followup"].is_null() {
            replies.push(reply(200, "{}", &[]));
        }
        let wire = Arc::new(Wire {
            requests: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into()),
        });
        let api = configured(
            wire.clone(),
            Arc::new(FailCredential {
                calls: AtomicUsize::new(0),
                fail_at: case["fail_credential_call"].as_u64().unwrap_or(u64::MAX) as usize,
            }),
        )
        .with_query_dispatch_trace(trace.clone());
        let result = api
            .collect(
                read_request(case),
                "value",
                Paging::Query,
                Limits::default(),
                pending(),
            )
            .await;
        let expected = &case["expected"];
        if expected["error"].is_null() {
            assert!(result.unwrap().next_token.is_none(), "{}", case["name"]);
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.partial.pages, 1, "{}", case["name"]);
            if case["name"] == "page-two-credential-error" {
                assert_eq!(error.error, ApiError::Credential);
            } else {
                assert_eq!(
                    error.error,
                    ApiError::Http {
                        status: 403,
                        code: "Denied".into()
                    }
                );
            }
        }
        let dispatched = trace.last_dispatch().expect("at least one actual read");
        assert_eq!(
            dispatched.encoded_query(),
            expected["last_dispatched_query"].as_str(),
            "{}",
            case["name"]
        );
        let decoded: Vec<_> = dispatched
            .query_pairs()
            .map(|(key, value)| json!([key, value]))
            .collect();
        assert_eq!(
            json!(decoded),
            expected["last_dispatched_pairs"],
            "{}",
            case["name"]
        );
        assert_eq!(dispatched.origin(), Origin::Fabric);
        assert_eq!(dispatched.method(), Method::Get);
        let requests: Vec<_> = wire.requests.lock().unwrap().iter().map(|request|
            json!({"method": request.method().to_string(), "url": request.url().as_str()})).collect();
        assert_eq!(
            json!(requests),
            expected["read_requests"],
            "{}",
            case["name"]
        );

        if !case["followup"].is_null() {
            // This is the explicit compatibility caller, not SDK/global state:
            // absent request_params has no persistent dictionary to carry.
            let followup = &case["followup"];
            let query = match followup["reset"].as_str().unwrap() {
                "keep" if !case["initial"].is_null() => dispatched
                    .query_pairs()
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect(),
                "replace" => pairs(&followup["params"]),
                _ => Vec::new(),
            };
            let request = ApiRequest::raw(RawRequestOptions {
                origin: Origin::Fabric,
                method: Method::Post,
                endpoint: followup["uri"].as_str().unwrap().into(),
                query,
                headers: Vec::new(),
                body: Vec::new(),
                bounds: RequestBounds::default(),
            })
            .unwrap();
            api.send(&request, NonZeroUsize::new(4096).unwrap(), pending())
                .await
                .unwrap();
            let requests = wire.requests.lock().unwrap();
            let last = requests.last().unwrap();
            assert_eq!(
                json!({"method": last.method().to_string(), "url": last.url().as_str()}),
                expected["followup_request"],
                "{}",
                case["name"]
            );
        }
    }
}

#[tokio::test]
async fn catalog_trace_reports_the_last_sent_token_not_the_unconsumed_response_token_and_never_implicitly_carries_queries()
 {
    let trace = QueryDispatchTrace::default();
    let query = vec![("filter".into(), "original".into())];
    let (api, wire, _) = setup(vec![
        reply(
            200,
            r#"{"domains":[],"continuationToken":"dispatched +/*~"}"#,
            &[],
        ),
        reply(
            200,
            r#"{"domains":[{"id":"domain"}],"continuationToken":"UNCONSUMED"}"#,
            &[],
        ),
        reply(200, r#"{"domains":[]}"#, &[]),
    ]);
    let client = CatalogClient::new(api).with_query_dispatch_trace(trace.clone());
    let error = client
        .list_hidden_with_query(
            &HiddenCollection::Domains,
            &query,
            Limits {
                max_pages: NonZeroUsize::new(2).unwrap(),
                ..Limits::default()
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.error, ApiError::Limit);
    assert_eq!(error.partial.next_token.as_deref(), Some("UNCONSUMED"));
    let observed = trace.last_dispatch().unwrap();
    assert_eq!(
        observed.encoded_query(),
        Some("filter=original&continuationToken=dispatched+%2B%2F%2A~")
    );
    assert!(!format!("{observed:?} {trace:?}").contains("dispatched"));
    assert_eq!(query, vec![("filter".into(), "original".into())]);
    client
        .list_hidden(&HiddenCollection::Domains, Limits::default(), pending())
        .await
        .unwrap();
    assert_eq!(trace.last_dispatch().unwrap().encoded_query(), None);
    assert_eq!(wire.requests.lock().unwrap()[2].url().query(), None);
    assert_eq!(
        observed.encoded_query(),
        Some("filter=original&continuationToken=dispatched+%2B%2F%2A~")
    );
    trace.clear();
    assert!(trace.last_dispatch().is_none());
    assert!(QueryDispatchTrace::default().last_dispatch().is_none());
}

struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct PendingSecondPage {
    requests: Mutex<Vec<Request>>,
    first: Mutex<Option<AsyncRawResponse>>,
    dropped: Arc<AtomicBool>,
}

#[async_trait]
impl HttpClient for PendingSecondPage {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        let first = self.first.lock().unwrap().take();
        if let Some(response) = first {
            return Ok(response);
        }
        let _drop = Dropped(self.dropped.clone());
        pending().await
    }
}

#[tokio::test]
async fn page_two_inflight_cancellation_retains_actual_dispatch_and_partial_progress_without_an_unreceived_token()
 {
    let fixture = fixture();
    let case = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "page-two-interrupt")
        .unwrap();
    let trace = QueryDispatchTrace::default();
    let dropped = Arc::new(AtomicBool::new(false));
    let wire = Arc::new(PendingSecondPage {
        requests: Mutex::new(Vec::new()),
        first: Mutex::new(Some(reply(200, &case["pages"][0]["body"].to_string(), &[]))),
        dropped: dropped.clone(),
    });
    let api = configured(wire.clone(), Arc::new(Credential::default()))
        .with_query_dispatch_trace(trace.clone());
    let request = read_request(case);
    let (send, receive) = tokio::sync::oneshot::channel::<()>();
    let mut future = pin!(
        api.collect(request, "value", Paging::Query, Limits::default(), async {
            let _ = receive.await;
        })
    );
    let mut context = Context::from_waker(Waker::noop());
    assert!(trace.last_dispatch().is_none());
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending)); // Shared pager yields after page one.
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending)); // Page two reaches HTTP.
    assert_eq!(wire.requests.lock().unwrap().len(), 2);
    send.send(()).unwrap();
    let Poll::Ready(Err(error)) = future.as_mut().poll(&mut context) else {
        panic!("cancel must fail explicitly");
    };
    assert_eq!(error.error, ApiError::Cancelled);
    assert_eq!(error.partial.pages, 1);
    assert_eq!(
        trace.last_dispatch().unwrap().encoded_query(),
        case["expected"]["last_dispatched_query"].as_str()
    );
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn predispatch_validation_credential_failure_and_ready_cancellation_leave_the_caller_snapshot_unchanged()
 {
    let trace = QueryDispatchTrace::default();
    let (base, wire, _) = setup(vec![reply(200, "{}", &[])]);
    let api = base.with_query_dispatch_trace(trace.clone());
    let request = ApiRequest::fabric(Method::Get, &["synthetic"])
        .unwrap()
        .with_query(&[("previous".into(), "value".into())])
        .unwrap();
    api.send(&request, NonZeroUsize::new(100).unwrap(), pending())
        .await
        .unwrap();
    let original = trace.last_dispatch().unwrap();
    assert_eq!(
        api.send(&request, NonZeroUsize::new(100).unwrap(), ready(()))
            .await
            .unwrap_err(),
        ApiError::Cancelled
    );
    assert_eq!(trace.last_dispatch().unwrap(), original);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
    let (base, wire, _) = setup(Vec::new());
    let api = base.with_query_dispatch_trace(trace.clone());
    let invalid = ApiRequest::raw(RawRequestOptions {
        origin: Origin::Arm,
        method: Method::Get,
        endpoint: "synthetic".into(),
        query: Vec::new(),
        headers: Vec::new(),
        body: Vec::new(),
        bounds: RequestBounds::default(),
    })
    .unwrap();
    assert_eq!(
        api.send(&invalid, NonZeroUsize::new(100).unwrap(), pending())
            .await
            .unwrap_err(),
        ApiError::InvalidAudience
    );
    assert_eq!(trace.last_dispatch().unwrap(), original);
    let api = configured(
        wire.clone(),
        Arc::new(FailCredential {
            calls: AtomicUsize::new(0),
            fail_at: 1,
        }),
    )
    .with_query_dispatch_trace(trace.clone());
    assert_eq!(
        api.send(&request, NonZeroUsize::new(100).unwrap(), pending())
            .await
            .unwrap_err(),
        ApiError::Credential
    );
    assert_eq!(trace.last_dispatch().unwrap(), original);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn between_page_cancellation_does_not_observe_the_unpolled_next_request() {
    let trace = QueryDispatchTrace::default();
    let (base, wire, _) = setup(vec![reply(
        200,
        r#"{"domains":[{"id":"a"}],"continuationToken":"UNCONSUMED"}"#,
        &[],
    )]);
    let client = CatalogClient::new(base).with_query_dispatch_trace(trace.clone());
    let target = HiddenCollection::Domains;
    let query = vec![("initial".into(), "value".into())];
    let (send, receive) = tokio::sync::oneshot::channel::<()>();
    let mut future =
        pin!(
            client.list_hidden_with_query(&target, &query, Limits::default(), async {
                let _ = receive.await;
            })
        );
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
    send.send(()).unwrap();
    let Poll::Ready(Err(error)) = future.as_mut().poll(&mut context) else {
        panic!("cancel must retain partial failure");
    };
    assert_eq!(error.error, ApiError::Cancelled);
    assert_eq!(error.partial.pages, 1);
    assert_eq!(error.partial.next_token.as_deref(), Some("UNCONSUMED"));
    assert_eq!(
        trace.last_dispatch().unwrap().encoded_query(),
        Some("initial=value")
    );
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn original_byte_caps_and_default_client_clones_keep_their_existing_behavior() {
    let body = r#"{"domains":[{"id":"a"}]}"#;
    for plus_one in [false, true] {
        let trace = QueryDispatchTrace::default();
        let (base, wire, _) = setup(vec![reply(
            200,
            &format!("{body}{}", if plus_one { " " } else { "" }),
            &[],
        )]);
        let client = CatalogClient::new(base).with_query_dispatch_trace(trace.clone());
        let result = client
            .list_hidden_with_query(
                &HiddenCollection::Domains,
                &[("query".into(), "opaque +/*~".into())],
                Limits {
                    max_response_bytes: NonZeroUsize::new(body.len()).unwrap(),
                    ..Limits::default()
                },
                pending(),
            )
            .await;
        if plus_one {
            assert_eq!(
                result.unwrap_err().error,
                ApiError::Execution(fabric_sdk_api::ExecutionError::ResponseLimit)
            );
        } else {
            assert_eq!(result.unwrap().rows.len(), 1);
        }
        let actual = trace.last_dispatch().unwrap();
        assert_eq!(actual.url(), wire.requests.lock().unwrap()[0].url());
    }
    let trace = QueryDispatchTrace::default();
    let (base, wire, _) = setup(vec![reply(200, "{}", &[]), reply(200, "{}", &[])]);
    let traced = base.clone().with_query_dispatch_trace(trace.clone());
    let query = ApiRequest::fabric(Method::Get, &["synthetic"])
        .unwrap()
        .with_query(&[("only".into(), "opted-in".into())])
        .unwrap();
    traced
        .send(&query, NonZeroUsize::new(100).unwrap(), pending())
        .await
        .unwrap();
    let last = trace.last_dispatch().unwrap();
    let untraced = ApiRequest::fabric(Method::Get, &["synthetic"]).unwrap();
    base.send(&untraced, NonZeroUsize::new(100).unwrap(), pending())
        .await
        .unwrap();
    assert_eq!(trace.last_dispatch().unwrap(), last);
    assert_eq!(wire.requests.lock().unwrap()[1].url().query(), None);
}

#[tokio::test]
async fn opted_in_shared_reconciliation_observes_actual_gets_without_carrying_the_initial_query() {
    let trace = QueryDispatchTrace::default();
    let (base, wire, _) = setup(vec![
        reply(
            202,
            r#"{"accepted":true}"#,
            &[
                ("x-ms-operation-id", "22222222-2222-2222-2222-222222222222"),
                ("Retry-After", "0"),
            ],
        ),
        reply(200, r#"{"status":"Completed"}"#, &[]),
    ]);
    let client = base.with_query_dispatch_trace(trace.clone());
    let request = ApiRequest::fabric(Method::Post, &["synthetic"])
        .unwrap()
        .with_query(&[("initial".into(), "only".into())])
        .unwrap();
    let response = client
        .send_completed(&request, NonZeroUsize::new(4096).unwrap(), pending())
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    let observed = trace.last_dispatch().unwrap();
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(observed.method(), Method::Get);
    assert_eq!(observed.url(), requests[1].url());
    assert_eq!(observed.encoded_query(), None);
    assert_eq!(requests[0].url().query(), Some("initial=only"));
}
