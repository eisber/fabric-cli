// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public shared-HTTP capacity consumer with fake HTTP, credential, and timer.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_core::Audience;
use fabric_sdk::{Client, CredentialSelection, TokenCredential, capacities::*};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use std::{
    collections::VecDeque,
    future::{pending, poll_fn},
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
}
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|scope| (*scope).to_owned()).collect());
        Ok(AccessToken::new(
            "offline-capacity-http",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}
#[derive(Default)]
struct Timer {
    waits: Mutex<Vec<Duration>>,
    expired: bool,
}
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        self.waits.lock().unwrap().push(delay);
        Box::pin(poll_fn(move |_| {
            if delay == Duration::from_secs(240) && !self.expired {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        }))
    }
}
#[derive(Debug)]
struct Http {
    replies: Mutex<VecDeque<AsyncRawResponse>>,
    requests: Mutex<Vec<Request>>,
}
#[async_trait]
impl HttpClient for Http {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP replay"))
    }
}
fn reply(status: u16, body: &str, headers: &[(&str, &str)]) -> AsyncRawResponse {
    let mut values = Headers::new();
    for &(key, value) in headers {
        values.insert(key.to_owned(), value.to_owned());
    }
    AsyncRawResponse::from_bytes(StatusCode::from(status), values, body.as_bytes().to_vec())
}
fn scripted_http(replies: impl IntoIterator<Item = AsyncRawResponse>) -> Arc<Http> {
    Arc::new(Http {
        replies: Mutex::new(replies.into_iter().collect()),
        requests: Mutex::new(vec![]),
    })
}
fn sdk() -> Client {
    Client::new(
        Arc::new(Credential::default()),
        CredentialSelection::new("account", "tenant", Audience::Fabric).unwrap(),
    )
}
fn executor(http: Arc<Http>, timer: Arc<Timer>) -> HttpExecutor {
    HttpExecutor::new(http, timer, ExecutionOptions::default()).unwrap()
}

#[tokio::test]
async fn adapter_uses_shared_execution_for_pages_headers_query_and_credential() {
    let http = scripted_http([
        reply(
            200,
            r#"{"value":[{"id":"x","displayName":"A"}],"continuationToken":"body"}"#,
            &[("X-MS-Continuation", "opaque+/= ~*")],
        ),
        reply(200, r#"{"value":[]}"#, &[]),
    ]);
    let list = sdk()
        .list_capacities(
            &executor(http.clone(), Arc::default()),
            CapacityListLimits::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(list.pages_received, 2);
    assert_eq!(list.value[0].display_name, "A");
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].url().as_str(),
        "https://api.fabric.microsoft.com/v1/capacities"
    );
    assert_eq!(
        requests[1].url().query(),
        Some("continuationToken=opaque%2B%2F%3D+~%2A")
    );
    assert_eq!(requests[1].method(), azure_core::http::Method::Get);
    assert_eq!(
        requests[0]
            .headers()
            .get_str(&"authorization".into())
            .unwrap(),
        "Bearer offline-capacity-http"
    );
    assert_eq!(
        requests[0]
            .headers()
            .get_str(&"content-type".into())
            .unwrap(),
        "application/json"
    );
}

#[tokio::test]
async fn adapter_preserves_shared_429_and_gateway_retry_policy() {
    let http = scripted_http([
        reply(503, "", &[]),
        reply(200, r#"{"value":[]}"#, &[]),
        reply(429, "", &[]),
        reply(200, r#"{"value":[]}"#, &[]),
    ]);
    let timer = Arc::new(Timer::default());
    let exec = executor(http.clone(), timer.clone());
    for _ in 0..2 {
        assert!(
            sdk()
                .list_capacities(&exec, CapacityListLimits::default(), pending())
                .await
                .is_ok()
        );
    }
    assert_eq!(http.requests.lock().unwrap().len(), 4);
    assert_eq!(
        *timer.waits.lock().unwrap(),
        [
            Duration::from_secs(240),
            Duration::from_secs(240),
            Duration::from_secs(10)
        ]
    );
    let http = scripted_http([
        reply(429, "", &[]),
        reply(429, "", &[]),
        reply(429, "", &[]),
        reply(429, "", &[]),
    ]);
    let timer = Arc::new(Timer::default());
    let error = sdk()
        .list_capacities(
            &executor(http.clone(), timer.clone()),
            CapacityListLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        CapacityListErrorKind::Execution(CapacityExecutionError::RetriesExhausted)
    );
    assert_eq!(http.requests.lock().unwrap().len(), 4);
    assert_eq!(
        timer
            .waits
            .lock()
            .unwrap()
            .iter()
            .filter(|&&wait| wait == Duration::from_secs(10))
            .count(),
        4
    );
}

#[tokio::test]
async fn adapter_preserves_typed_service_failure_and_partial_progress() {
    let http = scripted_http([
        reply(
            200,
            r#"{"value":[{"id":"a","displayName":"A"}],"continuationToken":"next"}"#,
            &[],
        ),
        reply(
            403,
            r#"{"errorCode":"Body","message":"secret service text","requestId":"id"}"#,
            &[("X-MS-Public-API-Error-Code", "Header")],
        ),
    ]);
    let error = sdk()
        .list_capacities(
            &executor(http, Arc::default()),
            CapacityListLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&error.kind,CapacityListErrorKind::Http{status:403,code,service_error:Some(error)} if code=="Header"&&error.additional_fields["requestId"]=="id")
    );
    assert_eq!(error.partial.value[0].id, "a");
    assert_eq!(error.partial.pages_received, 1);
    assert!(!error.to_string().contains("secret"));
}

#[tokio::test]
async fn adapter_streaming_exact_plus_one_and_deadline_map_without_policy_copy() {
    let body = r#"{"value":[]}"#;
    for bound in [body.len(), body.len() - 1] {
        let http = scripted_http([reply(200, body, &[])]);
        let result = sdk()
            .list_capacities(
                &executor(http, Arc::default()),
                CapacityListLimits {
                    max_response_bytes: NonZeroUsize::new(bound).unwrap(),
                    ..CapacityListLimits::default()
                },
                pending(),
            )
            .await;
        if bound == body.len() {
            assert!(result.is_ok());
        } else {
            assert_eq!(
                result.unwrap_err().kind,
                CapacityListErrorKind::ResponseLimit
            );
        }
    }
    let http = scripted_http([]);
    let timer = Arc::new(Timer {
        expired: true,
        ..Timer::default()
    });
    let error = sdk()
        .list_capacities(
            &executor(http.clone(), timer),
            CapacityListLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        CapacityListErrorKind::Execution(CapacityExecutionError::TimedOut)
    );
    assert!(http.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn immutable_source_goldens_drive_public_http_consumer_without_rewriting_safety_exceptions() {
    use serde_json::{Value, json};
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/capacities/cases.json"
    ))
    .unwrap();
    assert_eq!(
        fixtures["source_commit"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    let mut compared = 0;
    let mut safety_exceptions = 0;
    for case in fixtures["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["rust_parity"] == true)
    {
        let name = case["name"].as_str().unwrap();
        let scripts = case["responses"].as_array().unwrap();
        let http = scripted_http(scripts.iter().map(|script| {
            let headers: Vec<(&str, &str)> = script["headers"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str().unwrap()))
                .collect();
            let body = script
                .get("raw_body")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| serde_json::to_string(&script["body"]).unwrap());
            reply(
                u16::try_from(script["status"].as_u64().unwrap()).unwrap(),
                &body,
                &headers,
            )
        }));
        let credential = Arc::new(Credential::default());
        let client = Client::new(
            credential.clone(),
            CredentialSelection::new("account", "tenant", Audience::Fabric).unwrap(),
        );
        let timer = Arc::new(Timer::default());
        let result = client
            .list_capacities(
                &executor(http.clone(), timer.clone()),
                CapacityListLimits::default(),
                pending(),
            )
            .await;
        let safety = match name {
            "consumer_body_tokens_three_pages_empty_middle_opaque_encoding" => {
                let error = result.unwrap_err();
                assert_eq!(error.kind, CapacityListErrorKind::InvalidContinuation);
                assert_eq!(error.partial.value.len(), 1);
                assert_eq!(error.partial.next_token.as_deref(), Some("a+/= ?&\u{e9}"));
                Some((1, 1))
            }
            "rate_limit_exhaustion_sleeps_even_after_fourth_response"
            | "consumer_rate_limit_recovers_on_fourth_attempt" => {
                assert_eq!(
                    result.unwrap_err().kind,
                    CapacityListErrorKind::Execution(CapacityExecutionError::Failed)
                );
                // Existing shared HTTP rejects malformed Retry-After instead of Python's fallback.
                Some((2, 1))
            }
            _ => {
                match result {
                    Ok(list) => {
                        assert!(case["expected"].get("error_code").is_none(), "{name}");
                        assert_eq!(
                            serde_json::to_value(&list.value).unwrap(),
                            case["expected"]["value"],
                            "{name}"
                        );
                        assert!(list.next_token.is_none(), "{name}");
                    }
                    Err(error) => match error.kind {
                        CapacityListErrorKind::Http { code, .. } => assert_eq!(
                            code,
                            case["expected"]["error_code"].as_str().unwrap(),
                            "{name}"
                        ),
                        other => panic!("{name}: unexpected {other:?}"),
                    },
                }
                compared += 1;
                None
            }
        };
        let requests = http.requests.lock().unwrap();
        let actual:Vec<Value>=requests.iter().map(|request|json!({
            "method":request.method().as_str().to_ascii_lowercase(),"url":request.url().as_str()
        })).collect();
        let scopes = credential.scopes.lock().unwrap();
        if let Some((request_count, scope_count)) = safety {
            safety_exceptions += 1;
            assert_eq!(
                actual,
                case["expected"]["requests"].as_array().unwrap()[..request_count],
                "{name}"
            );
            assert_eq!(
                serde_json::to_value(&scopes[..]).unwrap(),
                json!(case["expected"]["scopes"].as_array().unwrap()[..scope_count]),
                "{name}"
            );
        } else {
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                case["expected"]["requests"],
                "{name}"
            );
            assert_eq!(
                serde_json::to_value(&*scopes).unwrap(),
                case["expected"]["scopes"],
                "{name}"
            );
            let waits: Vec<u64> = timer
                .waits
                .lock()
                .unwrap()
                .iter()
                .filter(|&&delay| delay != Duration::from_secs(240))
                .map(Duration::as_secs)
                .collect();
            assert_eq!(json!(waits), case["expected"]["sleeps"], "{name}");
            assert!(http.replies.lock().unwrap().is_empty(), "{name}");
        }
    }
    assert_eq!((compared, safety_exceptions), (17, 3));
}
