// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real TABLE maintenance adapter -> generic jobs -> shared HTTP contracts.

#[path = "../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_sdk_jobs::WaitOptions;
use fabric_sdk_tables::*;
use serde_json::{Value, json};
use std::{
    future::{Future, pending, poll_fn, ready},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
};
use support::*;

fn table() -> TablePath {
    TablePath::new("ws", "item", "Tables/dbo/sales").unwrap()
}

#[tokio::test]
async fn actual_job_engine_submits_table_payload_then_observes_completion() {
    let http = ScriptedHttp::new([
        accepted(),
        response(
            200,
            r#"{"id":"run","status":"InProgress"}"#,
            &[("Retry-After", "2")],
        ),
        response(200, r#"{"id":"run","status":"Completed"}"#, &[]),
    ]);
    let timer = Arc::new(TestTimer::default());
    let client = sdk(http.clone(), timer.clone());
    let adapter = JobMaintenance {
        jobs: &client,
        wait: WaitOptions::default(),
        cancel_on_timeout: true,
        operations: None,
    };
    let table = table();
    let data = Maintenance::Optimize {
        vorder: true,
        zorder: Some(vec!["key".into()]),
    }
    .execution_data(&table);
    let mut events = Vec::new();
    let outcome = adapter
        .run_observed(
            &table,
            data.clone(),
            &mut |event| {
                events.push(event);
                Ok(())
            },
            Box::pin(pending()),
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        MaintenanceOutcome::Completed {
            instance_id: "run".into()
        }
    );
    assert_eq!(events.len(), 3);
    assert!(
        matches!(&events[0],MaintenanceEvent::Created {instance_id,timeout:None} if instance_id=="run")
    );
    assert!(matches!(&events[2],MaintenanceEvent::Status {status,..} if status=="Completed"));
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].url().query(), Some("jobType=TableMaintenance"));
    assert_eq!(
        requests[0].url().path(),
        "/v1/workspaces/ws/items/item/jobs/instances"
    );
    match requests[0].body() {
        azure_core::http::Body::Bytes(bytes) => assert_eq!(
            serde_json::from_slice::<Value>(bytes).unwrap(),
            json!({"executionData":data})
        ),
        _ => panic!("expected exact request bytes"),
    }
    assert_eq!(*timer.waits.lock().unwrap(), [10, 2]);
}

#[tokio::test]
async fn remote_terminal_states_are_not_fabricated_success_and_failed_rows_survive() {
    for state in ["Cancelled", "Deduped", "FutureState", "Failed"] {
        let http = ScriptedHttp::new([
            accepted(),
            response(
                200,
                &format!(r#"{{"id":"run","status":"{state}","failureReason":null}}"#),
                &[],
            ),
        ]);
        let client = sdk(http, Arc::new(TestTimer::default()));
        let adapter = JobMaintenance {
            jobs: &client,
            wait: WaitOptions::default(),
            cancel_on_timeout: true,
            operations: None,
        };
        let result = adapter
            .run(&table(), json!({"tableName":"sales"}), Box::pin(pending()))
            .await;
        if state == "Failed" {
            let error = result.unwrap_err();
            assert_eq!(error.operation_id.as_deref(), Some("run"));
            assert!(error.mutation_may_have_occurred);
            assert!(
                matches!(error.kind,TableErrorKind::Maintenance {code,content:Some(content),..} if code=="JobFailed" && content["status"]=="Failed")
            );
        } else {
            assert_eq!(
                result.unwrap(),
                MaintenanceOutcome::TerminalWithoutOutput {
                    instance_id: "run".into(),
                    status: state.into()
                }
            );
        }
    }
}

#[tokio::test]
async fn source_timeout_configuration_controls_explicit_remote_cancel() {
    for cancel_on_timeout in [false, true] {
        let mut replies = vec![accepted()];
        if cancel_on_timeout {
            replies.push(response(202, "", &[]));
        }
        let http = ScriptedHttp::new(replies);
        let client = sdk(http.clone(), Arc::new(TestTimer::default()));
        let adapter = JobMaintenance {
            jobs: &client,
            wait: WaitOptions {
                timeout: Some(0),
                polling_interval: None,
            },
            cancel_on_timeout,
            operations: None,
        };
        let outcome = adapter
            .run(&table(), json!({"tableName":"sales"}), Box::pin(pending()))
            .await
            .unwrap();
        assert_eq!(
            outcome,
            if cancel_on_timeout {
                MaintenanceOutcome::CancellationAccepted {
                    instance_id: "run".into(),
                }
            } else {
                MaintenanceOutcome::TimedOutWithoutCancellation {
                    instance_id: "run".into(),
                    elapsed_seconds: 0,
                }
            }
        );
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), if cancel_on_timeout { 2 } else { 1 });
        if cancel_on_timeout {
            assert_eq!(
                requests[1].url().path(),
                "/v1/workspaces/ws/items/item/jobs/instances/run/cancel"
            );
        }
    }
}

#[tokio::test]
async fn validation_and_pre_cancel_prevent_submission_and_post_cancel_retains_id() {
    let http = ScriptedHttp::new([accepted()]);
    let timer = Arc::new(TestTimer::default());
    let client = sdk(http.clone(), timer.clone());
    let mut adapter = JobMaintenance {
        jobs: &client,
        wait: WaitOptions {
            timeout: Some(2),
            polling_interval: Some(2),
        },
        cancel_on_timeout: true,
        operations: None,
    };
    assert!(
        adapter
            .run(&table(), json!({}), Box::pin(pending()))
            .await
            .is_err()
    );
    assert!(http.requests.lock().unwrap().is_empty());
    adapter.wait = WaitOptions::default();
    assert_eq!(
        adapter
            .run(&table(), json!({}), Box::pin(ready(())))
            .await
            .unwrap_err()
            .kind,
        TableErrorKind::Cancelled
    );
    assert!(http.requests.lock().unwrap().is_empty());
    timer.pending_wait.store(true, Ordering::SeqCst);
    let path = table();
    let mut future = Box::pin(adapter.run(&path, json!({}), Box::pin(pending())));
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(future);
    assert_eq!(
        http.requests.lock().unwrap().len(),
        1,
        "drop must not issue remote cancel"
    );
    let mut events = Vec::new();
    let http = ScriptedHttp::new([accepted()]);
    let client = sdk(http.clone(), Arc::new(TestTimer::default()));
    let adapter = JobMaintenance {
        jobs: &client,
        wait: WaitOptions::default(),
        cancel_on_timeout: true,
        operations: None,
    };
    let error = adapter
        .run_observed(
            &table(),
            json!({}),
            &mut |event| {
                events.push(event);
                Err(TableError::new(TableErrorKind::Cancelled))
            },
            Box::pin(pending()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.operation_id.as_deref(), Some("run"));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn generic_cancel_lro_is_explicit_dependency_not_fake_accepted_completion() {
    let http = ScriptedHttp::new([
        accepted(),
        response(
            202,
            "",
            &[
                (
                    "Location",
                    "https://api.fabric.microsoft.com/v1/operations/op",
                ),
                ("x-ms-operation-id", "op"),
            ],
        ),
    ]);
    let client = sdk(http.clone(), Arc::new(TestTimer::default()));
    let adapter = JobMaintenance {
        jobs: &client,
        wait: WaitOptions {
            timeout: Some(0),
            polling_interval: None,
        },
        cancel_on_timeout: true,
        operations: None,
    };
    let error = adapter
        .run(&table(), json!({}), Box::pin(pending()))
        .await
        .unwrap_err();
    assert!(matches!(error.kind,TableErrorKind::Maintenance {code,..} if code=="NotSupported"));
    assert_eq!(error.operation_id.as_deref(), Some("run"));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(http.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn local_cancellation_during_wait_keeps_run_evidence_and_never_remote_cancels() {
    let http = ScriptedHttp::new([accepted()]);
    let timer = Arc::new(TestTimer::default());
    timer.pending_wait.store(true, Ordering::SeqCst);
    let client = sdk(http.clone(), timer);
    let adapter = JobMaintenance {
        jobs: &client,
        wait: WaitOptions::default(),
        cancel_on_timeout: true,
        operations: None,
    };
    let stop = AtomicBool::new(false);
    let path = table();
    let cancelled = poll_fn(|_| {
        if stop.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let mut future = Box::pin(adapter.run(&path, json!({}), Box::pin(cancelled)));
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    stop.store(true, Ordering::SeqCst);
    let error = future.await.unwrap_err();
    assert_eq!(error.kind, TableErrorKind::Cancelled);
    assert_eq!(error.operation_id.as_deref(), Some("run"));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}
