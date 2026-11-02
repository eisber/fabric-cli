// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public standalone folder consumer and independent source compatibility tests.

mod support;
use fabric_core::Audience;
use fabric_sdk_folders::*;
use serde_json::{Value, json};
use std::{
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
};
use support::*;

fn fixtures() -> Value {
    serde_json::from_str(include_str!(
        "../../../../tests/compatibility/folders/cases.json"
    ))
    .unwrap()
}
fn limits(pages: usize, items: usize) -> FolderListOptions {
    FolderListOptions {
        limits: FolderListLimits {
            max_pages: NonZeroUsize::new(pages).unwrap(),
            max_items: NonZeroUsize::new(items).unwrap(),
            ..Default::default()
        },
        ..Default::default()
    }
}
async fn run_case(case: &Value, exec: &Executor) -> Result<FolderList, FolderListError> {
    let sdk = client(Arc::default());
    let ws = case
        .get("workspace_id")
        .and_then(Value::as_str)
        .unwrap_or("ws");
    if case["operation"] == "list" {
        sdk.list_folders(exec, ws, options(case), pending()).await
    } else {
        sdk.get_folder(
            exec,
            ws,
            case.get("folder_id").and_then(Value::as_str).unwrap_or("f"),
            FolderGetOptions {
                query: query(case),
                ..Default::default()
            },
            pending(),
        )
        .await
        .map(|folder| FolderList {
            value: vec![folder],
            pages_received: 1,
            next_token: None,
        })
        .map_err(|error| FolderListError {
            kind: error.kind,
            partial: FolderList::default(),
        })
    }
}

#[tokio::test]
async fn literal_source_cases_compare_exact_query_route_rows_scope_and_error_precedence() {
    let manifest = fixtures();
    assert_eq!(
        manifest["source_commit"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    assert_eq!(manifest["scope"], Audience::Fabric.scope());
    let cases = manifest["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 26);
    for case in cases {
        let exec = Executor::new(
            case["responses"]
                .as_array()
                .unwrap()
                .iter()
                .map(fixture_reply),
        );
        let actual = run_case(case, &exec).await;
        let expected = &case["expected"];
        if let Some(rows) = expected.get("rows") {
            let actual = actual.unwrap();
            assert_eq!(
                actual.pages_received,
                expected
                    .get("pages")
                    .map_or(1, |pages| usize::try_from(pages.as_u64().unwrap()).unwrap()),
                "{}",
                case["id"]
            );
            assert_eq!(actual.next_token, None, "{}", case["id"]);
            assert_eq!(
                serde_json::to_value(actual.value).unwrap(),
                *rows,
                "{}",
                case["id"]
            );
        } else {
            let error = actual.unwrap_err();
            assert_eq!(
                serde_json::to_value(error.partial.value).unwrap(),
                expected
                    .get("partial_rows")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
                "{}",
                case["id"]
            );
            assert_eq!(
                error.partial.pages_received,
                expected
                    .get("pages")
                    .map_or(0, |pages| usize::try_from(pages.as_u64().unwrap()).unwrap()),
                "{}",
                case["id"]
            );
            assert!(
                matches!(error.kind,FolderErrorKind::Http{code,..} if code==expected["error_code"].as_str().unwrap()),
                "{}",
                case["id"]
            );
        }
        let expected_urls: Vec<_> = expected["urls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|url| format!("https://api.fabric.microsoft.com{}", url.as_str().unwrap()))
            .collect();
        assert_eq!(
            *exec.requests.lock().unwrap(),
            expected_urls,
            "{}",
            case["id"]
        );
    }
}

#[tokio::test]
async fn raw_python_quirks_are_separate_typed_safety_exceptions() {
    let manifest = fixtures();
    let cases = manifest["python_only"].as_array().unwrap();
    assert_eq!(cases.len(), 13);
    let mut checked = 0;
    for case in cases {
        let Some(error) = case["rust_error"].as_str() else {
            continue;
        };
        let exec = Executor::new(
            case["responses"]
                .as_array()
                .unwrap()
                .iter()
                .map(fixture_reply),
        );
        assert!(
            format!("{:?}", run_case(case, &exec).await.unwrap_err().kind).starts_with(error),
            "{}",
            case["id"]
        );
        checked += 1;
    }
    assert_eq!(checked, 12);
}

#[test]
fn wire_model_preserves_opaque_ids_parent_presence_null_and_unknown_values() {
    for parent in [
        None,
        Some(Value::Null),
        Some(json!("parent")),
        Some(json!(17)),
        Some(json!({"future":true})),
    ] {
        let mut value =
            json!({"id":"not-a-uuid","displayName":" A/\u{65e5} ","future":[null,1,true]});
        if let Some(parent) = parent {
            value["parentFolderId"] = parent;
        }
        let folder: Folder = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(folder.parent_folder_id(), value.get("parentFolderId"));
        assert_eq!(serde_json::to_value(folder).unwrap(), value);
    }
    for value in [
        json!({}),
        json!({"id":null,"displayName":"F"}),
        json!({"id":1,"displayName":"F"}),
        json!({"id":"f","displayName":false}),
    ] {
        assert!(serde_json::from_value::<Folder>(value).is_err());
    }
}

#[test]
fn query_value_api_explicitly_rejects_unrepresentable_python_objects() {
    for value in [
        json!(1.25),
        json!([]),
        json!({}),
        json!(18446744073709551615_u64),
    ] {
        assert!(serde_json::from_value::<FolderQueryValue>(value).is_err());
    }
}

#[tokio::test]
async fn invalid_identifiers_and_recursive_injection_fail_before_auth() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let exec = Executor::default();
    for id in ["", ".", "..", "a/b", "a\\b", "x?y", "x#y", "x\n"] {
        assert_eq!(
            sdk.list_folders(&exec, id, Default::default(), pending())
                .await
                .unwrap_err()
                .kind,
            FolderErrorKind::InvalidIdentifier
        );
        assert_eq!(
            sdk.get_folder(&exec, "ws", id, Default::default(), pending())
                .await
                .unwrap_err()
                .kind,
            FolderErrorKind::InvalidIdentifier
        );
    }
    for value in ["x=y", "false&extra=true", "x?y", "x#y", "x\r"] {
        assert_eq!(
            sdk.list_folders(
                &exec,
                "ws",
                FolderListOptions {
                    recursive: Some(value.into()),
                    ..Default::default()
                },
                pending()
            )
            .await
            .unwrap_err()
            .kind,
            FolderErrorKind::InvalidQuery
        );
    }
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(exec.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn encoded_ids_cannot_become_traversal_segments() {
    let exec = Executor::new([response(json!({"id":"f","displayName":"F"}))]);
    client(Arc::default())
        .get_folder(&exec, "ws %2F", "%2e%2e", Default::default(), pending())
        .await
        .unwrap();
    assert_eq!(
        exec.requests.lock().unwrap()[0],
        "https://api.fabric.microsoft.com/v1/workspaces/ws%20%252F/folders/%252e%252e"
    );
}

#[tokio::test]
async fn invalid_audience_and_provider_error_never_fallback_or_leak() {
    let credential = Arc::new(Credential::default());
    let sdk = FolderClient::new(
        credential.clone(),
        CredentialSelection::new("a", "t", Audience::Arm).unwrap(),
    );
    let exec = Executor::default();
    assert_eq!(
        sdk.list_folders(&exec, "ws", Default::default(), pending())
            .await
            .unwrap_err()
            .kind,
        FolderErrorKind::InvalidAudience
    );
    assert_eq!(
        sdk.get_folder(&exec, "ws", "f", Default::default(), pending())
            .await
            .unwrap_err()
            .kind,
        FolderErrorKind::InvalidAudience
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    let credential = Arc::new(Credential {
        fail: true,
        ..Default::default()
    });
    let error = client(credential.clone())
        .list_folders(&exec, "ws", Default::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, FolderErrorKind::Credential);
    assert!(!error.to_string().contains("private-provider-diagnostic"));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert!(exec.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn malformed_pages_gets_and_tokens_never_become_empty_success() {
    let sdk = client(Arc::default());
    for body in [
        json!({}),
        json!(null),
        json!({"value":null}),
        json!({"value":{}}),
        json!({"value":[{"id":"x"}]}),
        json!({"value":[],"continuationToken":false}),
    ] {
        let error = sdk
            .list_folders(
                &Executor::new([response(body)]),
                "ws",
                Default::default(),
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, FolderErrorKind::InvalidResponse);
        assert_eq!(error.partial, FolderList::default());
    }
    for body in [
        json!(null),
        json!([]),
        json!({"id":"f"}),
        json!({"id":"f","displayName":"F","continuationToken":7}),
    ] {
        assert_eq!(
            sdk.get_folder(
                &Executor::new([response(body)]),
                "ws",
                "f",
                Default::default(),
                pending()
            )
            .await
            .unwrap_err()
            .kind,
            FolderErrorKind::InvalidResponse
        );
    }
}

#[tokio::test]
async fn active_uri_matrix_is_validated_without_following_it() {
    let sdk = client(Arc::default());
    for uri in [
        "http://api.fabric.microsoft.com/v1/workspaces/ws/folders",
        "https://api.fabric.microsoft.com:444/v1/workspaces/ws/folders",
        "https://user@api.fabric.microsoft.com/v1/workspaces/ws/folders",
        "https://api.fabric.microsoft.com/v1/workspaces/other/folders",
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items",
        "https://api.fabric.microsoft.com/v1/workspaces/ws/folders#fragment",
        "https://api.fabric.microsoft.com/v1/workspaces/ws/other/../folders",
        "https://api.fabric.microsoft.com/v1/workspaces/ws/%66olders",
        "/v1/workspaces/ws/folders",
    ] {
        let exec = Executor::new([response(
            json!({"value":[],"continuationToken":"next","continuationUri":uri}),
        )]);
        let error = sdk
            .list_folders(&exec, "ws", Default::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, FolderErrorKind::InvalidContinuation, "{uri}");
        assert_eq!(error.partial.pages_received, 1);
        assert_eq!(error.partial.next_token.as_deref(), Some("next"));
        assert_eq!(exec.requests.lock().unwrap().len(), 1);
    }
    let exec = Executor::new([
        response(
            json!({"value":[],"continuationToken":"next","continuationUri":"https://api.fabric.microsoft.com:443/v1/workspaces/ws/folders?evil=ignored"}),
        ),
        page(None),
    ]);
    sdk.list_folders(&exec, "ws", Default::default(), pending())
        .await
        .unwrap();
    assert_eq!(
        exec.requests.lock().unwrap()[1],
        "https://api.fabric.microsoft.com/v1/workspaces/ws/folders?recursive=True&continuationToken=next"
    );
}

#[tokio::test]
async fn cycles_include_initial_tokens_and_prior_requested_tokens() {
    for (initial, tokens, expected_pages) in [
        (Some("same"), vec![Some("same")], 1),
        (None, vec![Some("a"), Some("b"), Some("a")], 3),
    ] {
        let exec = Executor::new(tokens.into_iter().map(page));
        let options = FolderListOptions {
            query: initial
                .into_iter()
                .map(|t| ("continuationToken".to_owned(), t.into()))
                .collect(),
            ..Default::default()
        };
        let error = client(Arc::default())
            .list_folders(&exec, "ws", options, pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind, FolderErrorKind::RepeatedContinuation);
        assert_eq!(error.partial.pages_received, expected_pages);
        assert_eq!(exec.requests.lock().unwrap().len(), expected_pages);
    }
}

#[tokio::test]
async fn default_page_limit_exact_100_and_plus_one_are_real_bounds() {
    assert_eq!(FolderListLimits::default().max_pages.get(), 100);
    let credential = Arc::new(Credential::default());
    for extra in [false, true] {
        let exec=Executor::new((0..100).map(|i|page(if i==99 && !extra {None}else{Some("placeholder")})).enumerate().map(|(i,mut response)| {
            if i<99 || extra {response.body=serde_json::to_vec(&json!({"value":[{"id":"a","displayName":"A"}],"continuationToken":format!("token-{i}")})).unwrap();}
            response
        }));
        let result = client(credential.clone())
            .list_folders(&exec, "ws", Default::default(), pending())
            .await;
        let progress = if extra {
            let error = result.unwrap_err();
            assert_eq!(error.kind, FolderErrorKind::PageLimit);
            assert_eq!(error.partial.next_token.as_deref(), Some("token-99"));
            error.partial
        } else {
            result.unwrap()
        };
        assert_eq!(progress.pages_received, 100);
        assert_eq!(progress.value.len(), 100);
        assert_eq!(exec.requests.lock().unwrap().len(), 100);
    }
    assert_eq!(credential.calls.load(Ordering::SeqCst), 200);
}

#[tokio::test]
async fn default_row_limit_exact_100000_and_plus_one_are_real_bounds() {
    assert_eq!(FolderListLimits::default().max_items.get(), 100_000);
    for count in [100_000, 100_001] {
        let exec = Executor::new([response(
            json!({"value":vec![json!({"id":"a","displayName":"A"});count]}),
        )]);
        let result = client(Arc::default())
            .list_folders(&exec, "ws", Default::default(), pending())
            .await;
        if count == 100_000 {
            assert_eq!(result.unwrap().value.len(), count);
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.kind, FolderErrorKind::ItemLimit);
            assert_eq!(error.partial, FolderList::default());
        }
    }
}

#[tokio::test]
async fn a_row_limit_never_commits_only_a_prefix_of_a_page() {
    let exec = Executor::new([
        page(Some("next")),
        response(json!({"value":[{"id":"b","displayName":"B"},{"id":"c","displayName":"C"}]})),
    ]);
    let error = client(Arc::default())
        .list_folders(&exec, "ws", limits(3, 2), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, FolderErrorKind::ItemLimit);
    assert_eq!(error.partial.value.len(), 1);
    assert_eq!(error.partial.pages_received, 1);
    assert_eq!(error.partial.next_token.as_deref(), Some("next"));
}

#[tokio::test]
async fn default_byte_bound_exact_4mib_and_plus_one_covers_list_and_get() {
    let bound = 4 * 1024 * 1024;
    assert_eq!(FolderGetOptions::default().max_response_bytes.get(), bound);
    for list in [false, true] {
        let base = if list {
            r#"{"value":[],"padding":""}"#
        } else {
            r#"{"id":"f","displayName":"F","padding":""}"#
        };
        for extra in [0, 1] {
            let body = base.replace(
                "\"padding\":\"\"",
                &format!("\"padding\":\"{}\"", "x".repeat(bound - base.len() + extra)),
            );
            assert_eq!(body.len(), bound + extra);
            let exec = Executor::new([FolderResponse {
                status: 200,
                body: body.into_bytes(),
                headers: Default::default(),
            }]);
            let sdk = client(Arc::default());
            let result = if list {
                sdk.list_folders(&exec, "ws", Default::default(), pending())
                    .await
                    .map(|_| ())
                    .map_err(|e| e.kind)
            } else {
                sdk.get_folder(&exec, "ws", "f", Default::default(), pending())
                    .await
                    .map(|_| ())
                    .map_err(|e| e.kind)
            };
            if extra == 0 {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err(), FolderErrorKind::ResponseLimit);
            }
        }
    }
}

#[tokio::test]
async fn service_errors_preserve_diagnostics_as_data_but_display_is_sanitized() {
    let mut reply =
        response(json!({"errorCode":"Body","message":"private-service-message","future":null}));
    reply.status = 403;
    reply
        .headers
        .insert("X-MS-Public-API-Error-Code".into(), "Public".into());
    let error = client(Arc::default())
        .get_folder(
            &Executor::new([reply]),
            "ws",
            "f",
            Default::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("private-service-message"));
    assert!(
        matches!(error.kind,FolderErrorKind::Http{code,service_error:Some(service),headers,..}
        if code=="Public" && service.message=="private-service-message" && service.additional_fields["future"]==Value::Null && headers["X-MS-Public-API-Error-Code"]=="Public")
    );
}

#[tokio::test]
async fn later_execution_and_malformed_failures_preserve_only_prior_full_pages() {
    for failure in [
        Err(FolderExecutionError::TimedOut),
        Err(FolderExecutionError::ResponseLimit),
        Ok(response(json!({"value":[{}]}))),
    ] {
        let exec = Executor::new([page(Some("next"))]);
        exec.replies.lock().unwrap().push_back(failure.clone());
        let error = client(Arc::default())
            .list_folders(&exec, "ws", Default::default(), pending())
            .await
            .unwrap_err();
        let expected = match failure {
            Err(FolderExecutionError::ResponseLimit) => FolderErrorKind::ResponseLimit,
            Err(e) => FolderErrorKind::Execution(e),
            Ok(_) => FolderErrorKind::InvalidResponse,
        };
        assert_eq!(error.kind, expected);
        assert_eq!(error.partial.value.len(), 1);
        assert_eq!(error.partial.pages_received, 1);
        assert_eq!(error.partial.next_token.as_deref(), Some("next"));
    }
}

fn cancellation(flag: &AtomicBool) -> impl Future<Output = ()> + Send + '_ {
    poll_fn(|_| {
        if flag.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
}

#[tokio::test]
async fn cancellation_before_and_simultaneously_ready_never_commits_success() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let exec = Executor::default();
    assert_eq!(
        sdk.list_folders(&exec, "ws", Default::default(), ready(()))
            .await
            .unwrap_err()
            .kind,
        FolderErrorKind::Cancelled
    );
    assert_eq!(
        sdk.get_folder(&exec, "ws", "f", Default::default(), ready(()))
            .await
            .unwrap_err()
            .kind,
        FolderErrorKind::Cancelled
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    let flag = Arc::new(AtomicBool::new(false));
    let exec = Executor {
        cancel_on_return: Some(flag.clone()),
        ..Executor::new([page(None)])
    };
    let error = sdk
        .list_folders(&exec, "ws", Default::default(), cancellation(&flag))
        .await
        .unwrap_err();
    assert_eq!(error.kind, FolderErrorKind::Cancelled);
    assert_eq!(error.partial, FolderList::default());
    assert!(exec.dropped.load(Ordering::SeqCst));
    flag.store(false, Ordering::SeqCst);
    let exec = Executor {
        cancel_on_return: Some(flag.clone()),
        ..Executor::new([response(json!({"id":"f","displayName":"F"}))])
    };
    assert_eq!(
        sdk.get_folder(&exec, "ws", "f", Default::default(), cancellation(&flag))
            .await
            .unwrap_err()
            .kind,
        FolderErrorKind::Cancelled
    );
    assert!(exec.dropped.load(Ordering::SeqCst));
}

#[test]
fn cancellation_drops_inflight_credential_and_executor_futures() {
    for (credential_wait, list) in [(false, true), (true, true), (false, false), (true, false)] {
        let flag = AtomicBool::new(false);
        let credential = Arc::new(Credential {
            wait: credential_wait,
            ..Default::default()
        });
        let sdk = client(credential.clone());
        let exec = Executor {
            wait: !credential_wait,
            ..Default::default()
        };
        let future = async {
            if list {
                sdk.list_folders(&exec, "ws", Default::default(), cancellation(&flag))
                    .await
                    .map(|_| ())
                    .map_err(|error| error.kind)
            } else {
                sdk.get_folder(&exec, "ws", "f", Default::default(), cancellation(&flag))
                    .await
                    .map(|_| ())
                    .map_err(|error| error.kind)
            }
        };
        let mut call = pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(call.as_mut().poll(&mut cx).is_pending());
        flag.store(true, Ordering::SeqCst);
        let Poll::Ready(Err(error)) = call.as_mut().poll(&mut cx) else {
            panic!("cancellation not ready")
        };
        assert_eq!(error, FolderErrorKind::Cancelled);
        if credential_wait {
            assert!(credential.dropped.load(Ordering::SeqCst));
            assert!(exec.requests.lock().unwrap().is_empty());
        } else {
            assert!(exec.dropped.load(Ordering::SeqCst));
        }
    }
}

#[test]
fn cancellation_between_ready_pages_retains_progress_and_does_not_start_next_auth() {
    let flag = AtomicBool::new(false);
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let exec = Executor::new([page(Some("next")), page(None)]);
    let mut call = pin!(sdk.list_folders(&exec, "ws", Default::default(), cancellation(&flag)));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(call.as_mut().poll(&mut cx).is_pending());
    flag.store(true, Ordering::SeqCst);
    let Poll::Ready(Err(error)) = call.as_mut().poll(&mut cx) else {
        panic!("cancellation not ready")
    };
    assert_eq!(error.kind, FolderErrorKind::Cancelled);
    assert_eq!(error.partial.pages_received, 1);
    assert_eq!(error.partial.next_token.as_deref(), Some("next"));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert_eq!(exec.requests.lock().unwrap().len(), 1);
}
