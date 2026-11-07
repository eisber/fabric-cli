// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The CLI opt-in policy must not weaken typed SDK readers or bounded paging.

#[path = "../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_sdk_jobs::{
    JobCommandRoute, JobError, JobTarget, JobType, PagingErrorKind, WaitOptions,
};
use serde_json::json;
use std::{future::pending, sync::Arc};
use support::*;

fn target() -> JobTarget {
    JobTarget::new("ws", "item", JobType::Notebook).unwrap()
}

#[tokio::test]
async fn raw_status_retains_source_values_but_typed_sdk_status_remains_strict() {
    for body in [r#"{"id":"run"}"#, r#"{"status":null}"#, "null", "[1,false]"] {
        let http = ScriptedHttp::new([response(200, body, &[]), response(200, body, &[])]);
        let sdk = sdk(http, Arc::new(TestTimer::default()));
        assert!(matches!(
            sdk.status(&target(), "run", pending()).await,
            Err(JobError::InvalidResponse)
        ));
        let raw = sdk
            .response_for_command(
                &target(),
                JobCommandRoute::Status("run"),
                None,
                true,
                None,
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&raw.body).unwrap(),
            serde_json::from_str::<serde_json::Value>(body).unwrap()
        );
    }
}

#[tokio::test]
async fn false_list_shapes_and_empty_first_objects_are_only_opt_in() {
    for body in [
        r#"{"value":null}"#,
        r#"{"value":false}"#,
        r#"{"value":0}"#,
        r#"{"value":""}"#,
        r#"{"value":{}}"#,
        r#"{"value":[{},null,1]}"#,
    ] {
        let http = ScriptedHttp::new([response(200, body, &[]), response(200, body, &[])]);
        let sdk = sdk(http, Arc::new(TestTimer::default()));
        assert!(sdk.list(&target(), false, pending()).await.is_err());
        let rows = sdk
            .list_for_command(&target(), false, None, pending())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            rows.value,
            if body.contains("[{}") {
                vec![json!({}), json!(null), json!(1)]
            } else {
                vec![]
            }
        );
    }
}

#[tokio::test]
async fn later_noncompleted_page_retains_partial_rows_and_never_succeeds() {
    let http = ScriptedHttp::new([
        response(
            200,
            r#"{"value":[{"id":"one"}],"continuationToken":"next"}"#,
            &[],
        ),
        response(204, "", &[]),
    ]);
    let sdk = sdk(http, Arc::new(TestTimer::default()));
    let Err(JobError::Paging(error)) = sdk
        .list_for_command(&target(), false, None, pending())
        .await
    else {
        panic!("later page must fail")
    };
    assert!(matches!(error.kind, PagingErrorKind::UnexpectedStatus(204)));
    assert_eq!(error.partial.value, [json!({"id":"one"})]);
    assert_eq!(error.partial.pages_received, 1);
}

#[tokio::test]
async fn source_non200_poll_continues_and_nonstring_status_reaches_unknown_observer() {
    for value in [json!(null), json!(false), json!(7), json!([]), json!({})] {
        let body = json!({"status":value}).to_string();
        let http = ScriptedHttp::new([
            accepted(),
            response(204, "", &[]),
            response(200, &body, &[]),
        ]);
        let timer = Arc::new(TestTimer::default());
        let sdk = sdk(http.clone(), timer.clone());
        let handle = sdk.submit(&target(), None, pending()).await.unwrap();
        let result = sdk
            .wait_for_command(&handle, WaitOptions::default(), |_| Ok(()), None, pending())
            .await
            .unwrap();
        assert_eq!(result.state(), fabric_sdk_jobs::JobState::Unknown);
        assert_eq!(result.status_value(), &value);
        assert_eq!(http.requests.lock().unwrap().len(), 3);
    }
}
