// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in argument, approval and output contracts remain offline.
#![deny(unnameable_test_items)]

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_cli::refresh_command::{self, Delivery, Policy};
use fabric_cli_jobs::HttpJobExecutor;
use fabric_sdk::Client;
use fabric_sdk_graph as refresh;
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_jobs::{Audience, CredentialSelection, JobClient, JobLimits};
use serde_json::json;
use std::{ffi::OsString, future::pending, io, sync::Arc};
use support::{ScriptedHttp, TestCredential, TestTimer, response};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.GraphModel/runtime/graph";
fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn schema_is_local_and_execution_requires_an_explicit_positive_deadline() {
    let command = refresh_command::parse(args(&["refresh", "--schema"])).unwrap();
    assert!(command.schema && command.path.is_none());
    assert!(refresh::schemas().unwrap()["bindings"].is_array());
    for values in [
        vec!["refresh", TARGET],
        vec!["refresh", TARGET, "--timeout", "0"],
        vec!["refresh", TARGET, "--timeout", "86401"],
        vec!["refresh", TARGET, "--timeout", "10", "--input", "body.json"],
        vec!["refresh", TARGET, "--timeout", "10", "--type", "Full"],
        vec!["refresh", "--schema", "--wait"],
        vec!["refresh", TARGET, "--timeout", "10", "--query", "MATCH"],
    ] {
        assert!(refresh_command::parse(args(&values)).is_err());
    }
}

#[test]
fn approval_and_preview_are_bound_to_the_exact_prepared_graph() {
    let prepared = refresh::prepare(TARGET).unwrap();
    for values in [
        vec!["refresh", TARGET, "--timeout", "10"],
        vec!["refresh", TARGET, "--timeout", "10", "--approve"],
        vec!["refresh", TARGET, "--timeout", "10", "--preview"],
    ] {
        let command = refresh_command::parse(args(&values)).unwrap();
        assert!(prepared.authorize(&mut Policy(&command)).is_err());
    }
    let command = refresh_command::parse(args(&[
        "refresh",
        TARGET,
        "--timeout",
        "10",
        "--approve",
        "--preview",
    ]))
    .unwrap();
    assert!(prepared.authorize(&mut Policy(&command)).is_ok());
    let other = refresh::prepare(&TARGET.replace("22222222", "33333333")).unwrap();
    assert!(other.authorize(&mut Policy(&command)).is_err());
}

struct Broken;
impl io::Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("synthetic"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn output_failure_retains_real_handle_and_does_not_claim_rollback() {
    let delivery = Delivery {
        value: json!({"status":"accepted","meta":{"handle":{"kind":"itemJobRun","instanceId":"native-job"},"mutationMayHaveOccurred":true,"remoteCancelled":false,"remoteReplayed":false}}),
        exit: 0,
    };
    let mut error = Vec::new();
    assert_eq!(delivery.write_to(&mut Broken, &mut error), 1);
    let value: serde_json::Value = serde_json::from_slice(&error).unwrap();
    assert_eq!(value["status"], "unknown");
    assert_eq!(value["meta"]["handle"]["instanceId"], "native-job");
    assert_eq!(value["meta"]["remoteCancelled"], false);
    assert_eq!(value["meta"]["remoteReplayed"], false);
}

#[test]
fn receipts_use_one_stream_and_one_line() {
    for exit in [0, 1] {
        let delivery = Delivery {
            value: json!({"status":if exit==0{"accepted"}else{"unknown"}}),
            exit,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(delivery.write_to(&mut out, &mut err), exit);
        let bytes = if exit == 0 {
            assert!(err.is_empty());
            out
        } else {
            assert!(out.is_empty());
            err
        };
        assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
    }
}

#[tokio::test]
async fn actual_frontend_dispatch_retains_acceptance_and_failed_observation() {
    let location = "https://api.fabric.microsoft.com/v1/workspaces/11111111-1111-1111-1111-111111111111/items/22222222-2222-2222-2222-222222222222/jobs/instances/33333333-3333-3333-3333-333333333333";
    for state in [
        None,
        Some("Completed"),
        Some("Failed"),
        Some("Deduped"),
        Some("FutureState"),
    ] {
        let mut responses = vec![response(
            202,
            "",
            &[("location", location), ("retry-after", "60")],
        )];
        if let Some(state) = state {
            responses.push(response(200, &json!({"status":state}).to_string(), &[]));
        }
        let http = ScriptedHttp::new(responses);
        let executor = Arc::new(HttpJobExecutor::new(
            HttpExecutor::new(
                http.clone(),
                Arc::new(TestTimer::default()),
                ExecutionOptions::default(),
            )
            .unwrap(),
        ));
        let jobs = JobClient::new(
            Arc::new(TestCredential::default()),
            CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
            executor.clone(),
            JobLimits::default(),
        );
        let client = Client::with_context(jobs.context().clone());
        let mut values = vec![
            "refresh",
            TARGET,
            "--timeout",
            "120",
            "--approve",
            "--preview",
        ];
        if state.is_some() {
            values.push("--wait");
        }
        let command = refresh_command::parse(args(&values)).unwrap();
        let delivery =
            refresh_command::dispatch(&command, &client, &jobs, executor.as_ref(), pending())
                .await
                .unwrap();
        assert_eq!(
            delivery.value["meta"]["handle"]["instanceId"],
            "33333333-3333-3333-3333-333333333333"
        );
        assert_eq!(delivery.value["meta"]["accepted"], true);
        assert_eq!(delivery.value["meta"]["remoteCancelled"], false);
        assert_eq!(delivery.value["meta"]["remoteReplayed"], false);
        match state {
            None => {
                assert_eq!(delivery.exit, 0);
                assert_eq!(delivery.value["status"], "accepted");
            }
            Some("Completed") => {
                assert_eq!(delivery.exit, 0);
                assert_eq!(delivery.value["status"], "completed");
            }
            Some("Failed" | "Deduped") => {
                assert_eq!(delivery.exit, 1);
                assert_eq!(delivery.value["status"], "failed");
            }
            _ => {
                assert_eq!(delivery.exit, 1);
                assert_eq!(delivery.value["status"], "unknown");
            }
        }
        let requests = http.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method() == azure_core::http::Method::Post)
                .count(),
            1
        );
    }
}

#[cfg(feature = "headless")]
#[test]
fn native_executable_graph_route_is_local_for_schema_and_refuses_missing_approval() {
    let binary = env!("CARGO_BIN_EXE_fab-cli-v2");
    let schema = std::process::Command::new(binary)
        .args(["refresh", "--schema"])
        .env_remove("FAB_RUST_AUTH_MODE")
        .output()
        .unwrap();
    assert!(schema.status.success());
    let value: serde_json::Value = serde_json::from_slice(&schema.stdout).unwrap();
    assert_eq!(value["operation"]["id"], refresh::OPERATION_ID);
    assert!(schema.stderr.is_empty());
    let refused = std::process::Command::new(binary)
        .args(["refresh", TARGET, "--timeout", "10"])
        .env_remove("FAB_RUST_AUTH_MODE")
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1));
    assert!(refused.stdout.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&refused.stderr).unwrap();
    assert_eq!(value["error"]["code"], "ApprovalRequired");
}
