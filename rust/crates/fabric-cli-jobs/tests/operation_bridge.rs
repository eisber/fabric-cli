// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Concrete shared operation sessions, not a fake completed response adapter.

#[path = "../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_cli_jobs::HttpJobExecutor;
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_jobs::{
    Audience, CredentialSelection, JobError, JobOperationReconciler, JobResponse, JobTarget,
    JobType, SharedJobOperations,
};
use fabric_sdk_operations::{OperationErrorKind, OperationExecutionError, OperationLimits};
use serde_json::json;
use std::{
    collections::BTreeMap,
    future::{pending, poll_fn},
    num::NonZeroUsize,
    sync::Arc,
    task::Poll,
};
use support::*;

const OPERATION: &str = "11111111-1111-1111-1111-111111111111";

fn accepted_operation() -> JobResponse {
    JobResponse {
        status: 202,
        body: Vec::new(),
        headers: BTreeMap::from([
            ("x-ms-operation-id".into(), OPERATION.into()),
            (
                "Location".into(),
                format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}"),
            ),
        ]),
    }
}

fn bridge(
    http: Arc<ScriptedHttp>,
    timer: Arc<TestTimer>,
    limits: OperationLimits,
) -> SharedJobOperations {
    SharedJobOperations::new(
        Arc::new(TestCredential::default()),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        Arc::new(HttpJobExecutor::new(
            HttpExecutor::new(http, timer, ExecutionOptions::default()).unwrap(),
        )),
        limits,
    )
}

#[tokio::test]
async fn accepted_cancel_reconciles_through_real_shared_get_and_result_without_replay() {
    let http = ScriptedHttp::new([
        response(
            202,
            "",
            &[
                ("x-ms-operation-id", OPERATION),
                (
                    "Location",
                    "https://api.fabric.microsoft.com/v1/operations/11111111-1111-1111-1111-111111111111",
                ),
            ],
        ),
        response(
            200,
            r#"{"status":"Succeeded"}"#,
            &[(
                "Location",
                "https://api.fabric.microsoft.com/v1/operations/11111111-1111-1111-1111-111111111111/result",
            )],
        ),
        response(200, r#"{"id":"completed-operation-result"}"#, &[]),
    ]);
    let timer = Arc::new(TestTimer::default());
    let operations = bridge(http.clone(), timer.clone(), OperationLimits::default());
    let sdk = sdk(http.clone(), timer);
    let result = sdk
        .cancel_for_wait(
            &JobTarget::new("ws", "item", JobType::TableMaintenance).unwrap(),
            "run",
            Some(&operations),
            pending(),
        )
        .await
        .unwrap();
    assert!(result.is_none());
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method(), azure_core::http::Method::Post);
    assert!(
        requests[0]
            .url()
            .path()
            .ends_with("/jobs/instances/run/cancel")
    );
    for request in &requests[1..] {
        assert_eq!(request.method(), azure_core::http::Method::Get);
    }
    assert!(requests[1].url().path().ends_with(OPERATION));
    assert!(requests[2].url().path().ends_with("/result"));
}

#[tokio::test]
async fn terminal_failure_and_cancel_preserve_exact_shared_error_and_raw_data() {
    for status in ["Failed", "Cancelled"] {
        for raw in [
            json!(null),
            json!("provider detail"),
            json!(["nested", true, 1.0]),
            json!({"code":"Nested","message":"provider detail","nested":[null,true,1.0]}),
        ] {
            let http = ScriptedHttp::new([response(
                200,
                &json!({"status":status,"error":raw}).to_string(),
                &[],
            )]);
            let operations = bridge(
                http.clone(),
                Arc::new(TestTimer::default()),
                OperationLimits::default(),
            );
            let error = operations
                .reconcile(&accepted_operation())
                .await
                .unwrap_err();
            let JobError::Operation(error) = error else {
                panic!("lossy error mapping");
            };
            assert!(
                matches!(&error.kind,OperationErrorKind::OperationFailed{status:actual,..} if actual==status)
            );
            assert_eq!(error.raw_service_error.as_deref(), Some(&raw));
            assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
            assert!(error.mutation_may_have_occurred);
            assert!(!error.to_string().contains("provider detail"));
            assert_eq!(http.requests.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn http_failure_retains_headers_code_raw_body_operation_id_and_uncertainty() {
    let raw = json!({"errorCode":"BodyCode","message":"body message","detail":{"unchanged":true}});
    let http = ScriptedHttp::new([response(
        403,
        &raw.to_string(),
        &[("x-ms-public-api-error-code", "HeaderCode")],
    )]);
    let operations = bridge(
        http,
        Arc::new(TestTimer::default()),
        OperationLimits::default(),
    );
    let JobError::Operation(error) = operations
        .reconcile(&accepted_operation())
        .await
        .unwrap_err()
    else {
        panic!("lossy error");
    };
    assert!(
        matches!(&error.kind,OperationErrorKind::Http{status:403,code:Some(code),..} if code=="HeaderCode")
    );
    assert_eq!(error.raw_service_error.as_deref(), Some(&raw));
    assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
    assert!(error.mutation_may_have_occurred);
}

#[tokio::test]
async fn immediate_and_inflight_cancellation_keep_known_operation_and_drop_get() {
    for immediately in [true, false] {
        let http = ScriptedHttp::new([]);
        http.pending
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let operations = bridge(
            http.clone(),
            Arc::new(TestTimer::default()),
            OperationLimits::default(),
        );
        let mut first = true;
        let cancelled = poll_fn(move |cx| {
            if !immediately && first {
                first = false;
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        });
        let JobError::Operation(error) = operations
            .reconcile_with_cancellation(&accepted_operation(), Box::pin(cancelled))
            .await
            .unwrap_err()
        else {
            panic!("lost cancellation evidence");
        };
        assert_eq!(error.kind, OperationErrorKind::Cancelled);
        assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
        assert!(error.mutation_may_have_occurred);
        assert!(http.requests.lock().unwrap().len() <= 1);
    }
}

#[tokio::test]
async fn bounds_invalid_link_transport_and_no_result_policy_remain_shared_classifications() {
    let http = ScriptedHttp::new([]);
    let operations = bridge(
        http.clone(),
        Arc::new(TestTimer::default()),
        OperationLimits {
            max_requests: NonZeroUsize::new(1).unwrap(),
            ..OperationLimits::default()
        },
    );
    let JobError::Operation(error) = operations
        .reconcile(&accepted_operation())
        .await
        .unwrap_err()
    else {
        panic!("lost bound");
    };
    assert_eq!(error.kind, OperationErrorKind::Limit);
    assert!(http.requests.lock().unwrap().is_empty());
    let http = ScriptedHttp::new([]);
    http.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let operations = bridge(
        http.clone(),
        Arc::new(TestTimer::default()),
        OperationLimits::default(),
    );
    let JobError::Operation(error) = operations
        .reconcile(&accepted_operation())
        .await
        .unwrap_err()
    else {
        panic!("lost transport");
    };
    assert_eq!(
        error.kind,
        OperationErrorKind::Execution(OperationExecutionError::Failed)
    );
    assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    let http = ScriptedHttp::new([
        response(
            200,
            r#"{"status":"Completed"}"#,
            &[(
                "Location",
                "https://api.fabric.microsoft.com/v1/operations/11111111-1111-1111-1111-111111111111/result",
            )],
        ),
        response(
            400,
            r#"{"errorCode":"OperationHasNoResult","message":"synthetic"}"#,
            &[],
        ),
    ]);
    let operations = bridge(
        http.clone(),
        Arc::new(TestTimer::default()),
        OperationLimits::default(),
    );
    assert_eq!(
        operations
            .reconcile(&accepted_operation())
            .await
            .unwrap()
            .status,
        200
    );
    assert_eq!(http.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn cancel_for_wait_cancellation_after_post_keeps_real_operation_evidence() {
    let http = ScriptedHttp::new([response(202, "", &[("x-ms-operation-id", OPERATION)])]);
    let timer = Arc::new(TestTimer::default());
    let operations = bridge(http.clone(), timer.clone(), OperationLimits::default());
    let service = sdk(http.clone(), timer);
    let cancelled = poll_fn(|cx| {
        if http.requests.lock().unwrap().is_empty() {
            cx.waker().wake_by_ref();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    });
    let result = service
        .cancel_for_wait(
            &JobTarget::new("ws", "item", JobType::Pipeline).unwrap(),
            "run",
            Some(&operations),
            cancelled,
        )
        .await;
    let Err(JobError::Operation(error)) = result else {
        panic!("cancellation evidence lost");
    };
    assert_eq!(error.kind, OperationErrorKind::Cancelled);
    assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn ordinary_job_location_bypasses_generic_engine_and_invalid_link_keeps_uncertainty() {
    let http = ScriptedHttp::new([accepted()]);
    let timer = Arc::new(TestTimer::default());
    let operations = bridge(http.clone(), timer.clone(), OperationLimits::default());
    let service = sdk(http.clone(), timer);
    assert!(
        service
            .cancel_for_wait(
                &JobTarget::new("ws", "item", JobType::Pipeline).unwrap(),
                "run",
                Some(&operations),
                pending()
            )
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    let mut accepted = accepted_operation();
    accepted.headers.insert(
        "Location".into(),
        "https://evil.invalid/operations/id".into(),
    );
    let result = operations.reconcile(&accepted).await;
    let Err(JobError::Operation(error)) = result else {
        panic!("invalid link evidence lost");
    };
    assert_eq!(error.kind, OperationErrorKind::InvalidLink);
    assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}
