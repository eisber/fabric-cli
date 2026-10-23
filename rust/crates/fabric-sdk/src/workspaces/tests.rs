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
    future::{pending, ready},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

const TEST_TOKEN: &str = "offline-test-only-not-a-real-token";

#[derive(Debug, Default)]
struct TestCredential {
    scopes: Mutex<Vec<Vec<String>>>,
    fail: bool,
    pending: bool,
    dropped: AtomicUsize,
}

struct DropCounter<'a>(&'a AtomicUsize);

impl Drop for DropCounter<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl TokenCredential for TestCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|s| (*s).to_owned()).collect());
        if self.fail {
            return Err(azure_core::Error::with_message(
                ErrorKind::Other,
                "sensitive provider diagnostic",
            ));
        }
        if self.pending {
            let _guard = DropCounter(&self.dropped);
            pending::<()>().await;
        }
        Ok(AccessToken::new(TEST_TOKEN, OffsetDateTime::UNIX_EPOCH))
    }
}

#[derive(Default)]
struct TestExecutor {
    replies: Mutex<VecDeque<Result<WorkspaceResponse, WorkspaceExecutionError>>>,
    urls: Mutex<Vec<String>>,
    pending: bool,
    dropped: AtomicUsize,
}

impl TestExecutor {
    fn new(replies: impl IntoIterator<Item = WorkspaceResponse>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().map(Ok).collect()),
            ..Self::default()
        }
    }

    fn calls(&self) -> usize {
        self.urls.lock().unwrap().len()
    }
}

impl WorkspaceExecutor for TestExecutor {
    fn execute<'a>(
        &'a self,
        request: WorkspaceRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            assert_eq!(request.content_type(), "application/json");
            assert_eq!(request.token().token.secret(), TEST_TOKEN);
            assert!(!format!("{request:?}").contains(TEST_TOKEN));
            assert!(!format!("{request:?}").contains("opaque"));
            self.urls
                .lock()
                .unwrap()
                .push(request.url().as_str().to_owned());
            if self.pending {
                let _guard = DropCounter(&self.dropped);
                pending::<()>().await;
            }
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected executor request")
        })
    }
}

fn client(credential: Arc<TestCredential>, audience: Audience) -> Client {
    Client::new(
        credential,
        CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap(),
    )
}

fn workspace(id: &str) -> Value {
    json!({"id": id, "displayName": "Research/\u{65e5}\u{672c}", "type": "Workspace"})
}

fn response(status: u16, body: Value) -> WorkspaceResponse {
    WorkspaceResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

fn page(ids: &[&str], token: Option<&str>) -> WorkspaceResponse {
    let mut body = json!({"value": ids.iter().map(|id| workspace(id)).collect::<Vec<_>>()});
    if let Some(token) = token {
        body["continuationToken"] = json!(token);
    }
    response(200, body)
}

#[tokio::test(flavor = "current_thread")]
async fn ordered_pages_preserve_duplicates_unicode_and_opaque_tokens() {
    let credential = Arc::new(TestCredential::default());
    let executor = TestExecutor::new([
        page(&["first", "same"], Some("opaque+/= token")),
        page(&["same"], Some("second")),
        page(&["last"], None),
    ]);
    let result = client(credential.clone(), Audience::Fabric)
        .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
        .await
        .unwrap();
    assert_eq!(
        result
            .value
            .iter()
            .map(|w| w.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "same", "same", "last"]
    );
    assert_eq!(result.pages_received, 3);
    assert_eq!(result.next_token, None);
    assert_eq!(result.value[0].display_name, "Research/\u{65e5}\u{672c}");
    let urls = executor.urls.lock().unwrap();
    assert_eq!(urls[0], WORKSPACES_URL);
    assert_eq!(
        urls[1],
        format!("{WORKSPACES_URL}?continuationToken=opaque%2B%2F%3D+token")
    );
    assert_eq!(
        credential.scopes.lock().unwrap().as_slice(),
        [
            vec![Audience::Fabric.scope()],
            vec![Audience::Fabric.scope()],
            vec![Audience::Fabric.scope()],
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_header_is_case_insensitive_and_overrides_body_even_when_empty() {
    for header_token in ["header", ""] {
        let mut first = page(&["a"], Some("body"));
        first
            .headers
            .insert("X-MS-Continuation".to_owned(), header_token.to_owned());
        let executor = TestExecutor::new([first, page(&[], None)]);
        let result = client(Arc::default(), Audience::Fabric)
            .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
            .await
            .unwrap();
        assert_eq!(
            result.pages_received,
            if header_token.is_empty() { 1 } else { 2 }
        );
        if !header_token.is_empty() {
            assert!(executor.urls.lock().unwrap()[1].ends_with("continuationToken=header"));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn empty_null_or_absent_token_terminates_without_following_uri() {
    for token in [None, Some(Value::Null), Some(json!(""))] {
        for uri in [
            WORKSPACES_URL,
            "https://foreign.example/v1/workspaces",
            "not a URI",
        ] {
            let credential = Arc::new(TestCredential::default());
            let mut body = json!({"value": [], "continuationUri": uri});
            if let Some(token) = &token {
                body["continuationToken"] = token.clone();
            }
            let executor = TestExecutor::new([response(201, body)]);
            let result = client(credential.clone(), Audience::Fabric)
                .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
                .await
                .unwrap();
            assert_eq!(result.pages_received, 1);
            assert_eq!(result.next_token, None);
            assert_eq!(executor.calls(), 1);
            assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        }
    }
}

#[test]
fn default_limits_match_the_owner_approved_contract() {
    let limits = WorkspaceListLimits::default();
    assert_eq!(limits.max_pages.get(), 100);
    assert_eq!(limits.max_items.get(), 100_000);
    assert_eq!(limits.max_response_bytes.get(), 4 * 1024 * 1024);
}

#[test]
fn workspace_list_future_can_be_sent_to_a_caller_runtime() {
    fn assert_send(_: impl Send) {}

    let client = client(Arc::default(), Audience::Fabric);
    let executor = TestExecutor::default();
    assert_send(client.list_workspaces(&executor, WorkspaceListLimits::default(), pending()));
}

#[tokio::test(flavor = "current_thread")]
async fn sdk_raw_boundary_uses_the_independent_python_cases_without_rewriting_them() {
    let reference: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/workspaces/cases.json"
    ))
    .unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 8);
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let scripted = case["responses"].as_array().unwrap();
        let replies = scripted.iter().map(|script| {
            let mut reply = response(
                u16::try_from(script["status"].as_u64().unwrap()).unwrap(),
                script["body"].clone(),
            );
            if let Some(headers) = script["headers"].as_object() {
                reply.headers = headers
                    .iter()
                    .map(|(key, value)| (key.clone(), value.as_str().unwrap().to_owned()))
                    .collect();
            }
            reply
        });
        let credential = Arc::new(TestCredential::default());
        let executor = TestExecutor::new(replies);
        let result = client(credential.clone(), Audience::Fabric)
            .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
            .await;
        match id {
            "token_pagination" => {
                let error = result.unwrap_err();
                assert_eq!(error.kind, WorkspaceListErrorKind::InvalidContinuation);
                assert_eq!(error.partial.pages_received, 1);
                assert_eq!(error.partial.value[0].id, "z");
                assert_eq!(error.partial.next_token.as_deref(), Some("page+/= 2"));
                assert_eq!(executor.calls(), 1);
            }
            "missing_value" => {
                assert_eq!(
                    result.unwrap_err().kind,
                    WorkspaceListErrorKind::InvalidResponse
                );
            }
            "forbidden_mapping" | "fabric_error" => {
                let error = result.unwrap_err();
                let WorkspaceListErrorKind::Http {
                    status,
                    code,
                    service_error,
                } = error.kind
                else {
                    panic!("expected typed HTTP failure for {id}");
                };
                assert_eq!(u64::from(status), scripted[0]["status"].as_u64().unwrap());
                assert_eq!(
                    code,
                    case["expected"]["exception"]["code"].as_str().unwrap()
                );
                assert_eq!(
                    serde_json::to_value(service_error.unwrap()).unwrap(),
                    scripted[0]["body"],
                );
            }
            _ => {
                let result = result.unwrap();
                let ids = result
                    .value
                    .iter()
                    .map(|workspace| workspace.id.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(json!(ids), case["expected"]["api_ids"], "{id}");
                assert_eq!(
                    serde_json::to_value(&result.value).unwrap(),
                    scripted[0]["body"]["value"],
                    "unfiltered service fields must remain unchanged for {id}",
                );
                assert_eq!(result.pages_received, 1);
                assert_eq!(result.next_token, None);
            }
        }
        let urls = executor.urls.lock().unwrap();
        if id == "token_pagination" {
            assert_eq!(urls.as_slice(), [WORKSPACES_URL]);
        } else {
            assert_eq!(json!(*urls), case["expected"]["requests"], "{id}");
            assert!(executor.replies.lock().unwrap().is_empty(), "{id}");
        }
        let scopes = credential.scopes.lock().unwrap();
        assert_eq!(scopes.len(), urls.len(), "{id}");
        assert!(
            scopes
                .iter()
                .all(|scope| scope == &[Audience::Fabric.scope()]),
            "{id}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn fabric_audience_is_required_before_credential_or_execution() {
    for audience in [Audience::OneLake, Audience::Arm] {
        let credential = Arc::new(TestCredential::default());
        let executor = TestExecutor::default();
        let error = client(credential.clone(), audience)
            .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, WorkspaceListErrorKind::InvalidAudience);
        assert!(credential.scopes.lock().unwrap().is_empty());
        assert_eq!(executor.calls(), 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn credential_failure_is_explicit_sanitized_and_has_no_fallback() {
    let credential = Arc::new(TestCredential {
        fail: true,
        ..Default::default()
    });
    let executor = TestExecutor::default();
    let error = client(credential.clone(), Audience::Fabric)
        .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::Credential);
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    assert_eq!(executor.calls(), 0);
    assert!(!format!("{error:?} {error}").contains("sensitive"));
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_pages_are_failures_and_retain_prior_progress() {
    for body in [
        b"not json".to_vec(),
        b"null".to_vec(),
        b"{}".to_vec(),
        br#"{"value":null}"#.to_vec(),
        br#"{"value":[{}]}"#.to_vec(),
        br#"{"value":[],"continuationToken":false}"#.to_vec(),
    ] {
        let executor = TestExecutor::new([
            page(&["a"], Some("next")),
            WorkspaceResponse {
                status: 200,
                body,
                headers: BTreeMap::new(),
            },
        ]);
        let error = client(Arc::default(), Audience::Fabric)
            .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, WorkspaceListErrorKind::InvalidResponse);
        assert_eq!(error.partial.value.len(), 1);
        assert_eq!(error.partial.pages_received, 1);
        assert_eq!(error.partial.next_token.as_deref(), Some("next"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn http_failures_preserve_python_code_precedence_without_echoing_messages() {
    for (status, expected) in [
        (401, "PublicCode"),
        (403, "PublicCode"),
        (404, "NotFound"),
        (400, "BodyCode"),
    ] {
        let mut reply = response(
            status,
            json!({
                "errorCode": "BodyCode", "message": "untrusted service message",
                "requestId": "offline-id", "moreDetails": [{"errorCode": "Detail", "message": "detail"}],
            }),
        );
        reply.headers.insert(
            "X-MS-Public-API-Error-Code".to_owned(),
            "PublicCode".to_owned(),
        );
        reply
            .headers
            .insert("x-ms-error-code".to_owned(), "OtherCode".to_owned());
        let executor = TestExecutor::new([reply]);
        let error = client(Arc::default(), Audience::Fabric)
            .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        let WorkspaceListErrorKind::Http {
            code,
            service_error,
            ..
        } = &error.kind
        else {
            panic!("{error:?}")
        };
        assert_eq!(code, expected);
        assert_eq!(
            service_error.as_ref().unwrap().additional_fields["requestId"],
            "offline-id"
        );
        assert!(!error.to_string().contains("untrusted"));
    }
    for (status, expected) in [
        (401, "Unauthorized"),
        (403, "Forbidden"),
        (500, "UnknownError"),
        (429, "UnknownError"),
        (302, "UnknownError"),
    ] {
        let executor = TestExecutor::new([response(status, json!(null))]);
        let error = client(Arc::default(), Audience::Fabric)
            .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            WorkspaceListErrorKind::Http {
                status,
                code: expected.to_owned(),
                service_error: None
            }
        );
        assert_eq!(executor.calls(), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn auth_error_header_fallback_matches_python() {
    for status in [401, 403] {
        let mut reply = response(status, json!({}));
        reply
            .headers
            .insert("x-ms-public-api-error-code".to_owned(), String::new());
        reply
            .headers
            .insert("X-MS-Error-Code".to_owned(), "FallbackCode".to_owned());
        let executor = TestExecutor::new([reply]);
        let error = client(Arc::default(), Audience::Fabric)
            .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        let WorkspaceListErrorKind::Http { code, .. } = error.kind else {
            panic!()
        };
        assert_eq!(code, "FallbackCode");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn incomplete_error_details_do_not_discard_a_valid_service_error_code() {
    for body in [
        json!({"errorCode": "InvalidWorkspaceRequest"}),
        json!({"errorCode": "InvalidWorkspaceRequest", "message": null}),
        json!({"errorCode": "InvalidWorkspaceRequest", "message": 123}),
    ] {
        let executor = TestExecutor::new([response(400, body)]);
        let error = client(Arc::default(), Audience::Fabric)
            .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            WorkspaceListErrorKind::Http {
                status: 400,
                code: "InvalidWorkspaceRequest".to_owned(),
                service_error: None,
            }
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn accepted_or_empty_success_is_not_a_workspace_listing() {
    for status in [202, 204] {
        let executor = TestExecutor::new([response(status, json!({"value": []}))]);
        let error = client(Arc::default(), Audience::Fabric)
            .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            WorkspaceListErrorKind::UnsupportedStatus(status)
        );
        assert_eq!(error.partial.pages_received, 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn execution_errors_remain_distinct_and_incomplete() {
    for kind in [
        WorkspaceExecutionError::Failed,
        WorkspaceExecutionError::TimedOut,
        WorkspaceExecutionError::Cancelled,
        WorkspaceExecutionError::RetriesExhausted,
    ] {
        let executor = TestExecutor::new([page(&["a"], Some("next"))]);
        executor.replies.lock().unwrap().push_back(Err(kind));
        let error = client(Arc::default(), Audience::Fabric)
            .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, WorkspaceListErrorKind::Execution(kind));
        assert_eq!(error.partial.value.len(), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn page_bound_is_exact_and_terminal_at_the_bound_is_successful() {
    let limits = WorkspaceListLimits {
        max_pages: NonZeroUsize::new(1).unwrap(),
        ..Default::default()
    };
    let executor = TestExecutor::new([page(&["a"], Some("next"))]);
    let error = client(Arc::default(), Audience::Fabric)
        .list_workspaces(&executor, limits, pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::PageLimit);
    assert_eq!(error.partial.pages_received, 1);
    assert_eq!(executor.calls(), 1);
    let executor = TestExecutor::new([page(&["a"], None)]);
    assert!(
        client(Arc::default(), Audience::Fabric)
            .list_workspaces(&executor, limits, pending())
            .await
            .is_ok()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_token_stops_without_a_third_request() {
    let executor = TestExecutor::new([page(&["a"], Some("repeat")), page(&["b"], Some("repeat"))]);
    let error = client(Arc::default(), Audience::Fabric)
        .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::RepeatedContinuation);
    assert_eq!(error.partial.pages_received, 2);
    assert_eq!(executor.calls(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn item_and_response_bounds_do_not_commit_an_oversized_page() {
    let executor = TestExecutor::new([page(&["a"], Some("next")), page(&["b", "c"], None)]);
    let limits = WorkspaceListLimits {
        max_items: NonZeroUsize::new(2).unwrap(),
        ..Default::default()
    };
    let error = client(Arc::default(), Audience::Fabric)
        .list_workspaces(&executor, limits, pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::ItemLimit);
    assert_eq!(error.partial.value.len(), 1);
    assert_eq!(error.partial.pages_received, 1);
    let executor = TestExecutor::new([page(&[], None)]);
    let limits = WorkspaceListLimits {
        max_response_bytes: NonZeroUsize::new(1).unwrap(),
        ..Default::default()
    };
    let error = client(Arc::default(), Audience::Fabric)
        .list_workspaces(&executor, limits, pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::ResponseLimit);
    assert_eq!(error.partial.pages_received, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn cumulative_item_count_at_the_exact_bound_succeeds() {
    let executor = TestExecutor::new([page(&["a"], Some("next")), page(&["b", "c"], None)]);
    let limits = WorkspaceListLimits {
        max_items: NonZeroUsize::new(3).unwrap(),
        ..Default::default()
    };
    let result = client(Arc::default(), Audience::Fabric)
        .list_workspaces(&executor, limits, pending())
        .await
        .unwrap();
    assert_eq!(result.value.len(), 3);
    assert_eq!(result.pages_received, 2);
    assert_eq!(result.next_token, None);
    assert_eq!(executor.calls(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn response_byte_count_succeeds_at_the_bound_and_fails_one_byte_over() {
    let reply = page(&["a"], None);
    let bytes = reply.body.len();
    for limit in [bytes, bytes - 1] {
        let executor = TestExecutor::new([reply.clone()]);
        let limits = WorkspaceListLimits {
            max_response_bytes: NonZeroUsize::new(limit).unwrap(),
            ..Default::default()
        };
        let result = client(Arc::default(), Audience::Fabric)
            .list_workspaces(&executor, limits, pending())
            .await;
        if limit == bytes {
            let result = result.unwrap();
            assert_eq!(result.value.len(), 1);
            assert_eq!(result.pages_received, 1);
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.kind, WorkspaceListErrorKind::ResponseLimit);
            assert!(error.partial.value.is_empty());
            assert_eq!(error.partial.pages_received, 0);
        }
        assert_eq!(executor.calls(), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_continuation_origins_and_paths_never_get_credentials_or_requests() {
    for uri in [
        "https://evil.example/v1/workspaces",
        "https://api.fabric.microsoft.com.evil.example/v1/workspaces",
        "http://api.fabric.microsoft.com/v1/workspaces",
        "https://api.fabric.microsoft.com:444/v1/workspaces",
        "https://user:password@api.fabric.microsoft.com/v1/workspaces",
        "/v1/workspaces",
        "https://api.fabric.microsoft.com/v1/workspaces#fragment",
        "https://api.fabric.microsoft.com/v1/items",
        "https://api.fabric.microsoft.com/v1/%77orkspaces",
        "file:///v1/workspaces",
    ] {
        let credential = Arc::new(TestCredential::default());
        let executor = TestExecutor::new([response(
            200,
            json!({
                "value": [workspace("a")], "continuationToken": "next", "continuationUri": uri,
            }),
        )]);
        let error = client(credential.clone(), Audience::Fabric)
            .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            WorkspaceListErrorKind::InvalidContinuation,
            "{uri}"
        );
        assert_eq!(error.partial.pages_received, 1);
        assert_eq!(executor.calls(), 1);
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn valid_uri_is_validated_but_token_drives_the_next_request() {
    let executor = TestExecutor::new([
        response(
            200,
            json!({"value": [], "continuationToken": "actual",
            "continuationUri": "https://API.FABRIC.MICROSOFT.COM:443/v1/workspaces?continuationToken=ignored"}),
        ),
        page(&[], None),
    ]);
    client(Arc::default(), Audience::Fabric)
        .list_workspaces(&executor, WorkspaceListLimits::default(), pending())
        .await
        .unwrap();
    assert!(executor.urls.lock().unwrap()[1].ends_with("continuationToken=actual"));
}

fn cancel_after(mut polls: usize) -> impl Future<Output = ()> + Send {
    poll_fn(move |cx| {
        polls -= 1;
        if polls == 0 {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_before_execution_has_no_side_effects() {
    let credential = Arc::new(TestCredential::default());
    let executor = TestExecutor::default();
    let error = client(credential.clone(), Audience::Fabric)
        .list_workspaces(&executor, WorkspaceListLimits::default(), ready(()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::Cancelled);
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert_eq!(executor.calls(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_drops_in_flight_credential_and_executor_futures() {
    let credential = Arc::new(TestCredential {
        pending: true,
        ..Default::default()
    });
    let executor = TestExecutor::default();
    let error = client(credential.clone(), Audience::Fabric)
        .list_workspaces(&executor, WorkspaceListLimits::default(), cancel_after(3))
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::Cancelled);
    assert_eq!(credential.dropped.load(Ordering::SeqCst), 1);
    assert_eq!(executor.calls(), 0);
    let executor = TestExecutor {
        pending: true,
        ..Default::default()
    };
    let error = client(Arc::default(), Audience::Fabric)
        .list_workspaces(&executor, WorkspaceListLimits::default(), cancel_after(4))
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::Cancelled);
    assert_eq!(executor.dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_is_checked_between_synchronously_ready_pages() {
    let executor = TestExecutor::new([page(&["a"], Some("next")), page(&["b"], None)]);
    let error = client(Arc::default(), Audience::Fabric)
        .list_workspaces(&executor, WorkspaceListLimits::default(), cancel_after(5))
        .await
        .unwrap_err();
    assert_eq!(error.kind, WorkspaceListErrorKind::Cancelled);
    assert_eq!(error.partial.pages_received, 1);
    assert_eq!(error.partial.next_token.as_deref(), Some("next"));
    assert_eq!(executor.calls(), 1);
}
