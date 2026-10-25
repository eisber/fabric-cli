// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public SDK -> shared HTTP executor -> synthetic HTTP lifecycle contracts.

#[path = "../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_sdk_jobs::{JobError, JobLimits, JobTarget, JobType, WaitOptions};
use serde_json::{Value, json};
use std::{
    future::{Future, pending, poll_fn},
    num::NonZeroUsize,
    sync::{Arc, atomic::Ordering},
    task::Poll,
};
use support::*;

fn cases() -> Value {
    serde_json::from_str(include_str!("../../../tests/compatibility/jobs/cases.json")).unwrap()
}

fn target() -> JobTarget {
    JobTarget::new("ws", "item", JobType::Pipeline).unwrap()
}

#[tokio::test]
async fn all_source_wire_routes_execute_through_the_shared_pipeline() {
    for case in cases()["wire"].as_array().unwrap() {
        let name = case["function"].as_str().unwrap();
        let body = match name {
            "get_item_job_instance" => r#"{"id":"run","status":"InProgress"}"#,
            "list_item_schedules" | "list_item_runs" => r#"{"value":[{"id":"one"}]}"#,
            _ => r#"{"id":"sch"}"#,
        };
        let http = ScriptedHttp::new([if name == "run_on_demand_item_job" {
            accepted()
        } else {
            response(case["status"].as_u64().unwrap() as u16, body, &[])
        }]);
        let client = sdk(http.clone(), Arc::new(TestTimer::default()));
        let target = target();
        match name {
            "run_on_demand_item_job" => {
                let handle = client
                    .submit(&target, Some(json!({"parameters":{"n":2}})), pending())
                    .await
                    .unwrap();
                assert_eq!(handle.id(), "run");
                assert!(format!("{handle:?}").contains("not completed"));
            }
            "get_item_job_instance" => {
                client.status(&target, "run", pending()).await.unwrap();
            }
            "cancel_item_job_instance" => {
                client.cancel(&target, "run", pending()).await.unwrap();
            }
            "create_item_schedule" => {
                client
                    .create_schedule(
                        &target,
                        &json!({"configuration":{"type":"Cron"},"enabled":false}),
                        pending(),
                    )
                    .await
                    .unwrap();
            }
            "update_item_schedule" => {
                client
                    .update_schedule(
                        &target,
                        "sch",
                        &json!({"configuration":{"type":"Cron"},"enabled":true}),
                        pending(),
                    )
                    .await
                    .unwrap();
            }
            "get_item_schedule" => {
                client
                    .get_schedule(&target, "sch", pending())
                    .await
                    .unwrap();
            }
            "list_item_schedules" => {
                assert_eq!(
                    client.list(&target, true, pending()).await.unwrap().value,
                    [json!({"id":"one"})]
                );
            }
            "list_item_runs" => {
                assert_eq!(
                    client.list(&target, false, pending()).await.unwrap().value,
                    [json!({"id":"one"})]
                );
            }
            "remove_item_schedule" => {
                client
                    .remove_schedule(&target, "sch", pending())
                    .await
                    .unwrap();
            }
            _ => panic!("uncovered source wire case"),
        }
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "{name}");
        let request = &requests[0];
        assert_eq!(
            request.url().as_str(),
            format!(
                "https://api.fabric.microsoft.com/v1/{}",
                case["uri"].as_str().unwrap()
            ),
            "{name}"
        );
        assert_eq!(
            request.method().to_string().to_lowercase(),
            case["method"],
            "{name}"
        );
        let body = match request.body() {
            azure_core::http::Body::Bytes(bytes) => bytes.as_ref(),
            _ => panic!("expected bounded byte body"),
        };
        if let Some(expected) = case.get("payload") {
            assert_eq!(
                serde_json::from_slice::<Value>(body).unwrap(),
                serde_json::from_str::<Value>(expected.as_str().unwrap()).unwrap()
            );
        } else {
            assert!(body.is_empty());
        }
    }
}

#[tokio::test]
async fn independent_python_wait_cases_preserve_sleep_states_and_timeout_accounting() {
    for case in cases()["wait"].as_array().unwrap() {
        let headers: Vec<_> = case["headers"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap()))
            .collect();
        let reply_headers: Vec<_> = case
            .get("response_headers")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.as_str(), v.as_str().unwrap()))
                    .collect()
            })
            .unwrap_or_default();
        let mut replies = vec![response(
            202,
            "",
            &[(
                "Location",
                "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/jobs/instances/run",
            )]
            .into_iter()
            .chain(headers.iter().copied())
            .collect::<Vec<_>>(),
        )];
        for status in case["statuses"].as_array().unwrap() {
            replies.push(response(
                200,
                &json!({"id":"run","status":status}).to_string(),
                &reply_headers,
            ));
        }
        let http = ScriptedHttp::new(replies);
        let timer = Arc::new(TestTimer::default());
        let client = sdk(http.clone(), timer.clone());
        let handle = client.submit(&target(), None, pending()).await.unwrap();
        let mut observed = Vec::new();
        let result = client
            .wait(
                &handle,
                WaitOptions {
                    timeout: case.get("timeout").and_then(Value::as_i64),
                    polling_interval: case.get("custom").and_then(Value::as_u64),
                },
                |status| {
                    observed.push(status.status().to_owned());
                    Ok(())
                },
                pending(),
            )
            .await;
        match case.get("error").and_then(Value::as_str) {
            Some("JobFailed") => assert!(matches!(result, Err(JobError::Failed { .. }))),
            Some("TimeoutError") => assert!(matches!(result, Err(JobError::TimedOut { .. }))),
            None => {
                result.unwrap();
            }
            _ => panic!("uncovered wait error"),
        }
        let expected_waits: Vec<_> = case["waits"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| **v != json!(0))
            .cloned()
            .collect();
        assert_eq!(
            json!(*timer.waits.lock().unwrap()),
            json!(expected_waits),
            "{}",
            case["name"]
        );
        assert_eq!(
            http.requests.lock().unwrap().len() as u64 - 1,
            case["requests"].as_u64().unwrap()
        );
        assert_eq!(json!(observed), case["statuses"]);
    }
}

#[tokio::test]
async fn invalid_or_cross_resource_locations_are_not_completed_or_pollable() {
    for location in [
        "",
        "https://evil.invalid/v1/workspaces/ws/items/item/jobs/instances/run",
        "https://api.fabric.microsoft.com/v1/workspaces/other/items/item/jobs/instances/run",
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/jobs/instances/",
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/jobs/instances/run?secret=x",
        "https://api.fabric.microsoft.com/v1/operations/not-a-job",
    ] {
        let http = ScriptedHttp::new([response(202, "", &[("location", location)])]);
        assert!(matches!(
            sdk(http, Arc::new(TestTimer::default()))
                .submit(&target(), None, pending())
                .await,
            Err(JobError::InvalidResponse)
        ));
    }
}

#[tokio::test]
async fn mutation_statuses_transport_loss_and_throttling_are_never_replayed() {
    for status in [200, 201, 204, 400, 429, 503] {
        let http = ScriptedHttp::new([response(
            status,
            r#"{"errorCode":"Rejected","message":"synthetic"}"#,
            &[("retry-after", "0")],
        )]);
        let result = sdk(http.clone(), Arc::new(TestTimer::default()))
            .submit(&target(), None, pending())
            .await;
        assert!(result.is_err());
        assert_eq!(http.requests.lock().unwrap().len(), 1);
    }
    let http = ScriptedHttp::new([]);
    http.fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        sdk(http.clone(), Arc::new(TestTimer::default()))
            .submit(&target(), None, pending())
            .await,
        Err(JobError::Execution(_))
    ));
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn service_error_codes_follow_source_precedence_and_keep_wire_details() {
    for (status, headers, expected) in [
        (
            401,
            vec![
                ("x-ms-public-api-error-code", "PolicyDenied"),
                ("x-ms-error-code", "other"),
            ],
            "PolicyDenied",
        ),
        (
            403,
            vec![("x-ms-error-code", "HeaderDenied")],
            "HeaderDenied",
        ),
        (404, vec![], "NotFound"),
        (409, vec![], "EntityConflict"),
    ] {
        let http = ScriptedHttp::new([response(
            status,
            r#"{"errorCode":"EntityConflict","message":"synthetic","requestId":"offline"}"#,
            &headers,
        )]);
        match sdk(http, Arc::new(TestTimer::default()))
            .status(&target(), "run", pending())
            .await
            .unwrap_err()
        {
            JobError::Http {
                code,
                service_error,
                ..
            } => {
                assert_eq!(code, expected);
                assert_eq!(
                    service_error.unwrap().additional_fields["requestId"],
                    "offline"
                );
            }
            error => panic!("wrong error: {error:?}"),
        }
    }
}

#[tokio::test]
async fn cancelled_credential_and_pending_http_work_never_become_success() {
    let credential = Arc::new(TestCredential::default());
    let http = ScriptedHttp::new([accepted()]);
    let client = client(
        credential.clone(),
        http.clone(),
        Arc::new(TestTimer::default()),
        JobLimits::default(),
    );
    assert!(matches!(
        client.submit(&target(), None, async {}).await,
        Err(JobError::Cancelled)
    ));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(http.requests.lock().unwrap().is_empty());

    http.pending.store(true, Ordering::SeqCst);
    let mut first = true;
    let cancel = poll_fn(move |cx| {
        if first {
            first = false;
            cx.waker().wake_by_ref();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    });
    assert!(matches!(
        client.submit(&target(), None, cancel).await,
        Err(JobError::Cancelled)
    ));
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn pending_job_poll_can_be_dropped_without_remote_cancel() {
    let http = ScriptedHttp::new([accepted()]);
    let timer = Arc::new(TestTimer::default());
    let client = sdk(http.clone(), timer.clone());
    let handle = client.submit(&target(), None, pending()).await.unwrap();
    timer.pending_wait.store(true, Ordering::SeqCst);
    let mut work = Box::pin(client.wait(&handle, WaitOptions::default(), |_| Ok(()), pending()));
    poll_fn(|cx| {
        assert!(work.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(work);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn credential_deadline_response_bound_and_malformed_status_fail_explicitly() {
    let credential = Arc::new(TestCredential::default());
    credential.failed.store(true, Ordering::SeqCst);
    let http = ScriptedHttp::new([]);
    assert!(matches!(
        client(
            credential,
            http.clone(),
            Arc::new(TestTimer::default()),
            JobLimits::default()
        )
        .status(&target(), "run", pending())
        .await,
        Err(JobError::Credential)
    ));
    assert!(http.requests.lock().unwrap().is_empty());
    let timer = Arc::new(TestTimer::default());
    timer.expired.store(true, Ordering::SeqCst);
    assert!(matches!(
        sdk(http.clone(), timer)
            .status(&target(), "run", pending())
            .await,
        Err(JobError::Execution(
            fabric_sdk_jobs::JobExecutionError::TimedOut
        ))
    ));
    let http = ScriptedHttp::new([response(200, r#"{"status":"InProgress"}"#, &[])]);
    let limits = JobLimits {
        max_response_bytes: NonZeroUsize::new(2).unwrap(),
        ..JobLimits::default()
    };
    assert!(matches!(
        client(
            Arc::new(TestCredential::default()),
            http,
            Arc::new(TestTimer::default()),
            limits
        )
        .status(&target(), "run", pending())
        .await,
        Err(JobError::Execution(
            fabric_sdk_jobs::JobExecutionError::ResponseLimit
        ))
    ));
    for body in [
        "null",
        "{}",
        r#"{"status":null}"#,
        r#"{"status":2}"#,
        "not-json",
    ] {
        let http = ScriptedHttp::new([response(200, body, &[])]);
        assert!(matches!(
            sdk(http, Arc::new(TestTimer::default()))
                .status(&target(), "run", pending())
                .await,
            Err(JobError::InvalidResponse)
        ));
    }
}

#[test]
fn source_resource_map_is_explicit() {
    for (kind, expected) in [
        ("SparkJobDefinition", "sparkjob"),
        ("Notebook", "RunNotebook"),
        ("DataPipeline", "Pipeline"),
        ("Lakehouse", "TableMaintenance"),
        ("GraphModel", "refreshGraph"),
    ] {
        assert_eq!(JobType::from_item_type(kind).unwrap().as_str(), expected);
    }
}

#[tokio::test]
async fn job_lists_reuse_the_shared_pager_through_real_http_with_a_token_per_page() {
    for case in cases()["paging"].as_array().unwrap() {
        let replies = case["pages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|page| {
                let headers: Vec<_> = page
                    .get("headers")
                    .and_then(Value::as_object)
                    .map(|values| {
                        values
                            .iter()
                            .map(|(key, value)| (key.as_str(), value.as_str().unwrap()))
                            .collect()
                    })
                    .unwrap_or_default();
                response(200, &page["body"].to_string(), &headers)
            })
            .collect::<Vec<_>>();
        let credential = Arc::new(TestCredential::default());
        let http = ScriptedHttp::new(replies);
        let client = client(
            credential.clone(),
            http.clone(),
            Arc::new(TestTimer::default()),
            JobLimits::default(),
        );
        let result = client.list(&target(), false, pending()).await.unwrap();
        assert_eq!(json!(result.value), case["expected"]);
        assert_eq!(json!(result.pages_received), case["pages_received"]);
        assert_eq!(
            credential.calls.load(Ordering::SeqCst),
            result.pages_received
        );
        let queries: Vec<_> = http
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.url().query().unwrap_or("").to_owned())
            .collect();
        assert_eq!(json!(queries), case["queries"]);
    }
}

#[tokio::test]
async fn later_job_list_errors_and_bounds_retain_partial_progress_without_success() {
    let http = ScriptedHttp::new([
        response(
            200,
            r#"{"value":[{"id":"one"}],"continuationToken":"next"}"#,
            &[],
        ),
        response(403, r#"{"errorCode":"No","message":"synthetic"}"#, &[]),
    ]);
    let service = sdk(http, Arc::new(TestTimer::default()));
    match service.list(&target(), false, pending()).await.unwrap_err() {
        JobError::Paging(error) => {
            assert_eq!(error.partial.pages_received, 1);
            assert_eq!(error.partial.value, [json!({"id":"one"})]);
            assert!(matches!(
                error.kind,
                fabric_sdk_jobs::PagingErrorKind::Fetch(JobError::Http { status: 403, .. })
            ));
        }
        error => panic!("wrong partial failure: {error:?}"),
    }
    let http = ScriptedHttp::new([response(
        200,
        r#"{"value":[{"id":"one"}],"continuationToken":"next"}"#,
        &[],
    )]);
    let client = client(
        Arc::new(TestCredential::default()),
        http,
        Arc::new(TestTimer::default()),
        JobLimits {
            max_pages: NonZeroUsize::new(1).unwrap(),
            ..JobLimits::default()
        },
    );
    assert!(
        matches!(client.list(&target(),false,pending()).await,Err(JobError::Paging(error)) if matches!(error.kind,fabric_sdk_jobs::PagingErrorKind::PageLimit))
    );
}
#[test]
fn route_guards_and_polling_validation_are_explicit() {
    for kind in ["Report", "SemanticModel", "Workspace", "notebook"] {
        assert!(JobType::from_item_type(kind).is_none());
    }
    for id in [
        "", ".", "..", "a/b", "a\\b", "a%2fb", "a?b", "a#b", "a b", "a\nb",
    ] {
        assert!(JobTarget::new("ws", id, JobType::Pipeline).is_err());
    }
    assert!(
        WaitOptions {
            timeout: Some(10),
            polling_interval: Some(10)
        }
        .validate()
        .is_err()
    );
    assert!(
        WaitOptions {
            timeout: Some(0),
            polling_interval: None
        }
        .validate()
        .is_ok()
    );
}
