// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Direct table-consumer contract without a CLI, facade, or identity backend.

#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_sdk_jobs::{JobState, JobTarget, JobType, WaitOptions};
use serde_json::{Value, json};
use std::{future::pending, sync::Arc};
use support::*;

#[tokio::test]
async fn caller_supplied_table_execution_data_reaches_the_real_shared_executor() {
    let http = ScriptedHttp::new([
        accepted(),
        response(200, r#"{"id":"run","status":"InProgress"}"#, &[]),
        response(200, r#"{"id":"run","status":"Completed"}"#, &[]),
    ]);
    let client = sdk(http.clone(), Arc::new(TestTimer::default()));
    let table = JobTarget::new("ws", "item", JobType::TableMaintenance).unwrap();
    let execution = json!({"tableName":"orders","optimizeSettings":{"vOrder":true}});
    let handle = client
        .submit(&table, Some(execution.clone()), pending())
        .await
        .unwrap();
    let state = client
        .wait(&handle, WaitOptions::default(), |_| Ok(()), pending())
        .await
        .unwrap();
    assert_eq!(state.state(), JobState::Completed);
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].url().query(), Some("jobType=TableMaintenance"));
    match requests[0].body() {
        azure_core::http::Body::Bytes(bytes) => assert_eq!(
            serde_json::from_slice::<Value>(bytes).unwrap(),
            json!({"executionData":execution})
        ),
        _ => panic!("expected bounded request bytes"),
    }
    assert_eq!(
        requests[1].url().path(),
        "/v1/workspaces/ws/items/item/jobs/instances/run"
    );
}

#[tokio::test]
async fn table_consumer_can_explicitly_cancel_and_wait_for_confirmed_cancellation() {
    let http = ScriptedHttp::new([
        response(202, "", &[]),
        response(200, r#"{"id":"run","status":"Cancelled"}"#, &[]),
    ]);
    let client = sdk(http.clone(), Arc::new(TestTimer::default()));
    let table = JobTarget::new("ws", "item", JobType::TableMaintenance).unwrap();
    let accepted_cancel = client.cancel(&table, "run", pending()).await.unwrap();
    let terminal = client
        .wait(
            &accepted_cancel,
            WaitOptions::default(),
            |_| Ok(()),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(terminal.state(), JobState::Cancelled);
    assert_eq!(http.requests.lock().unwrap().len(), 2);
}
