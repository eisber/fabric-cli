// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Scripted native-entry dispatch, delivery and executable exit regressions.
#![deny(unnameable_test_items)]

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use super::*;
use fabric_sdk_jobs::{Audience, CredentialSelection};
use serde_json::{Value, json};
use std::{
    future::{pending, poll_fn},
    task::Poll,
};
use support::{ScriptedHttp, TestCredential, TestTimer, response};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.GraphModel/runtime/graph";
const INSTANCE: &str = "33333333-3333-3333-3333-333333333333";
const LOCATION: &str = "https://api.fabric.microsoft.com/v1/workspaces/11111111-1111-1111-1111-111111111111/items/22222222-2222-2222-2222-222222222222/jobs/instances/33333333-3333-3333-3333-333333333333";

fn command(wait: bool) -> refresh_command::RefreshCommand {
    let mut args = vec![
        "refresh",
        TARGET,
        "--timeout",
        "120",
        "--preview",
        "--approve",
    ];
    if wait {
        args.push("--wait");
    }
    refresh_command::parse(args.into_iter().map(OsString::from)).unwrap()
}

fn clients(
    http: Arc<ScriptedHttp>,
    timer: Arc<TestTimer>,
) -> (Client, JobClient, Arc<HttpJobExecutor>) {
    let executor = Arc::new(HttpJobExecutor::new(
        fabric_sdk_http::HttpExecutor::new(
            http,
            timer,
            fabric_sdk_http::ExecutionOptions::default(),
        )
        .unwrap(),
    ));
    let jobs = JobClient::new(
        Arc::new(TestCredential::default()),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        executor.clone(),
        JobLimits::default(),
    );
    (Client::with_context(jobs.context().clone()), jobs, executor)
}

fn assert_root(bytes: &[u8], status: &str, job: bool) -> Value {
    let value: Value = serde_json::from_slice(bytes).unwrap();
    let schema = refresh::schemas().unwrap();
    for field in schema["operation"]["outputSchema"]["required"]
        .as_array()
        .unwrap()
    {
        assert!(
            value.get(field.as_str().unwrap()).is_some(),
            "Missing advertised root field {field}: {value}"
        );
    }
    assert_eq!(value["schemaVersion"], "0.1");
    assert_eq!(value["status"], status);
    assert_eq!(value["meta"]["operation"], refresh::OPERATION_ID);
    assert_eq!(value["meta"]["target"], TARGET);
    assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
    assert_eq!(value["meta"]["remoteCancelled"], false);
    assert_eq!(value["meta"]["remoteReplayed"], false);
    if job {
        assert_eq!(value["meta"]["handle"]["instanceId"], INSTANCE);
        assert_eq!(value["meta"]["accepted"], true);
    }
    assert!(
        value.get("evidence").is_none(),
        "Native receipt must not use the generic error wrapper"
    );
    value
}

#[tokio::test]
async fn native_failed_observation_preserves_advertised_root_and_nonzero_exit() {
    let http = ScriptedHttp::new([
        response(202, "", &[("location", LOCATION), ("retry-after", "60")]),
        response(
            200,
            r#"{"status":"Failed","failureReason":{"errorCode":"Synthetic"}}"#,
            &[],
        ),
    ]);
    let (client, jobs, executor) = clients(http.clone(), Arc::new(TestTimer::default()));
    let delivery =
        refresh_command::dispatch(&command(true), &client, &jobs, executor.as_ref(), pending())
            .await
            .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let result = Ok(finish_delivery(delivery, 0, &mut stdout, &mut stderr));
    let exit = super::super::finish(result, &mut stderr);
    assert_eq!(exit, 1);
    assert!(stdout.is_empty());
    let value = assert_root(&stderr, "failed", true);
    assert_eq!(value["meta"]["lastState"], "Failed");
    assert_eq!(value["meta"]["responseStatus"], 202);
    assert_eq!(value["error"]["code"], "ObservationIncomplete");
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == azure_core::http::Method::Post)
            .count(),
        1
    );
}

#[tokio::test]
async fn native_timeout_and_cancel_after_acceptance_keep_root_handle_and_uncertainty() {
    for reason in [1, 2] {
        let http = ScriptedHttp::new([response(
            202,
            "",
            &[("location", LOCATION), ("retry-after", "60")],
        )]);
        let timer = Arc::new(TestTimer::default());
        timer.pending_wait.store(true, Ordering::SeqCst);
        let (client, jobs, executor) = clients(http.clone(), timer.clone());
        let interruption = AtomicU8::new(0);
        let cancelled = poll_fn(|cx| {
            if timer.waits.lock().unwrap().is_empty() {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                interruption.store(reason, Ordering::SeqCst);
                Poll::Ready(())
            }
        });
        let delivery =
            refresh_command::dispatch(&command(true), &client, &jobs, executor.as_ref(), cancelled)
                .await
                .unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = Ok(finish_delivery(
            delivery,
            interruption.load(Ordering::SeqCst),
            &mut stdout,
            &mut stderr,
        ));
        let exit = super::super::finish(result, &mut stderr);
        assert_eq!(exit, 1);
        assert!(stdout.is_empty());
        let value = assert_root(&stderr, "unknown", true);
        assert_eq!(
            value["meta"]["interruption"],
            if reason == 1 { "TimedOut" } else { "Cancelled" }
        );
        assert_eq!(value["error"]["code"], "Cancelled");
        assert_eq!(
            http.requests.lock().unwrap().len(),
            1,
            "No early poll, replay or remote cancellation"
        );
    }
}

#[tokio::test]
async fn native_positive_submission_and_completion_still_use_root_stdout() {
    for (status, state, expected) in [
        (200, None, "completed"),
        (202, None, "accepted"),
        (202, Some("Completed"), "completed"),
    ] {
        let mut replies = vec![response(
            status,
            "",
            if status == 202 {
                &[("location", LOCATION), ("retry-after", "60")]
            } else {
                &[]
            },
        )];
        if let Some(state) = state {
            replies.push(response(200, &json!({"status":state}).to_string(), &[]));
        }
        let http = ScriptedHttp::new(replies);
        let (client, jobs, executor) = clients(http, Arc::new(TestTimer::default()));
        let delivery = refresh_command::dispatch(
            &command(state.is_some()),
            &client,
            &jobs,
            executor.as_ref(),
            pending(),
        )
        .await
        .unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = Ok(finish_delivery(delivery, 0, &mut stdout, &mut stderr));
        assert_eq!(super::super::finish(result, &mut stderr), 0);
        assert!(stderr.is_empty());
        assert_root(&stdout, expected, status == 202);
    }
}

struct FailOnce {
    bytes: Vec<u8>,
    on_flush: bool,
    failed: bool,
}

impl Write for FailOnce {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.on_flush && !self.failed {
            self.failed = true;
            return Err(io::Error::other("synthetic native writer failure"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.on_flush && !self.failed {
            self.failed = true;
            return Err(io::Error::other("synthetic native flush failure"));
        }
        Ok(())
    }
}

#[tokio::test]
async fn native_failed_receipt_writer_retains_root_cause_and_handle_on_delivery_failure() {
    let http = ScriptedHttp::new([
        response(202, "", &[("location", LOCATION), ("retry-after", "60")]),
        response(200, r#"{"status":"Failed"}"#, &[]),
    ]);
    let (client, jobs, executor) = clients(http.clone(), Arc::new(TestTimer::default()));
    let delivery =
        refresh_command::dispatch(&command(true), &client, &jobs, executor.as_ref(), pending())
            .await
            .unwrap();
    let mut stdout = Vec::new();
    let mut stderr = FailOnce {
        bytes: Vec::new(),
        on_flush: false,
        failed: false,
    };
    let exit = finish_delivery(delivery, 0, &mut stdout, &mut stderr);
    assert_eq!(super::super::finish(Ok(exit), &mut stderr), 1);
    assert!(stdout.is_empty());
    let value = assert_root(&stderr.bytes, "unknown", true);
    assert_eq!(value["meta"]["lastState"], "Failed");
    assert_eq!(value["error"]["code"], "OutputFailure");
    assert_eq!(value["error"]["source"]["code"], "ObservationIncomplete");
    assert_eq!(value["error"]["outcomeStatus"], "failed");
    assert_eq!(http.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn native_stdout_flush_failure_keeps_accepted_root_receipt_without_replay() {
    let http = ScriptedHttp::new([response(
        202,
        "",
        &[("location", LOCATION), ("retry-after", "60")],
    )]);
    let (client, jobs, executor) = clients(http.clone(), Arc::new(TestTimer::default()));
    let delivery = refresh_command::dispatch(
        &command(false),
        &client,
        &jobs,
        executor.as_ref(),
        pending(),
    )
    .await
    .unwrap();
    let mut stdout = FailOnce {
        bytes: Vec::new(),
        on_flush: true,
        failed: false,
    };
    let mut stderr = Vec::new();
    let exit = finish_delivery(delivery, 0, &mut stdout, &mut stderr);
    assert_eq!(super::super::finish(Ok(exit), &mut stderr), 1);
    assert_root(&stdout.bytes, "accepted", true);
    let value = assert_root(&stderr, "unknown", true);
    assert_eq!(value["error"]["code"], "OutputFailure");
    assert_eq!(value["error"]["outcomeStatus"], "accepted");
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[test]
fn native_predispatch_failures_keep_generic_error_and_original_exit() {
    for exit in [1, 2] {
        let mut stderr = Vec::new();
        assert_eq!(
            super::super::finish(
                Err(failed(
                    "PreflightFailed",
                    "No native request was submitted.",
                    exit
                )),
                &mut stderr
            ),
            exit,
        );
        let value: Value = serde_json::from_slice(&stderr).unwrap();
        assert_eq!(value["error"]["code"], "PreflightFailed");
        assert!(value["evidence"].is_null());
        assert!(value.get("schemaVersion").is_none());
    }
}
