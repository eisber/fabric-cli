// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The actual native handlers consume the same injected session and HTTP stack.

#![deny(unnameable_test_items)]

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use super::*;
use fabric_cli::{start_command, stop_command};
use std::{ffi::OsString, future::pending};
use support::{ScriptedHttp, TestCredential, TestTimer, response};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.MirroredDatabase/runtime/mirroring";

fn session(
    http: Arc<ScriptedHttp>,
    credential: Arc<dyn azure_core::credentials::TokenCredential>,
) -> NativeSession {
    let context = RequestContext::new(
        credential,
        CredentialSelection::new("synthetic-session", "synthetic-tenant", Audience::Fabric)
            .unwrap(),
    );
    let executor = Arc::new(
        HttpExecutor::new(
            http.clone(),
            Arc::new(TestTimer::default()),
            ExecutionOptions::default(),
        )
        .unwrap(),
    );
    NativeSession {
        context,
        storage_selection: CredentialSelection::new(
            "synthetic-session",
            "synthetic-tenant",
            Audience::OneLake,
        )
        .unwrap(),
        executor,
        service_http: http,
    }
}

#[tokio::test]
async fn native_stop_handler_maps_typed_parser_registration_and_executor() {
    for (state, yes, expected) in [
        ("Stopped", false, 1),
        ("Stopping", false, 1),
        ("Running", false, 1),
        ("Running", true, 3),
    ] {
        let http = ScriptedHttp::new([
            response(200, &serde_json::json!({"status":state}).to_string(), &[]),
            response(200, "", &[]),
            response(200, r#"{"status":"Stopped"}"#, &[]),
        ]);
        let mut args = vec!["stop", TARGET, "--timeout", "10"];
        if yes {
            args.push("--yes");
        }
        let command = stop_command::parse(args.into_iter().map(OsString::from)).unwrap();
        let delivery = v2_stop_main::execute(
            &command,
            session(http.clone(), Arc::new(TestCredential::default())),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(http.requests.lock().unwrap().len(), expected);
        assert_eq!(delivery.value["operation"], "fabric.stop");
        if state == "Running" && !yes {
            assert_eq!(delivery.value["error"]["code"], "ApprovalRequired");
        } else {
            assert!(delivery.execution.result.is_ok());
        }
    }
}

#[tokio::test]
async fn native_start_handler_maps_typed_parser_registration_and_executor() {
    let http = ScriptedHttp::new([
        response(200, r#"{"status":"Stopped"}"#, &[]),
        response(200, "", &[]),
    ]);
    let command = start_command::parse_start(
        ["start", TARGET, "--approve", "--timeout", "10"]
            .into_iter()
            .map(OsString::from),
    )
    .unwrap();
    let delivery = v2_start_main::execute(
        &command,
        session(http.clone(), Arc::new(TestCredential::default())),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(delivery.exit, 0);
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].url().path().ends_with("/getMirroringStatus"));
    assert!(requests[1].url().path().ends_with("/startMirroring"));
}

#[cfg(feature = "native-read")]
#[tokio::test]
async fn broker_feature_uses_only_the_injected_synthetic_credential_without_wam() {
    let credential = Arc::new(TestCredential::default());
    let http = ScriptedHttp::new([response(200, r#"{"status":"Stopped"}"#, &[])]);
    let command = stop_command::parse(
        ["stop", TARGET, "--timeout", "10"]
            .into_iter()
            .map(OsString::from),
    )
    .unwrap();
    let delivery = v2_stop_main::execute(
        &command,
        session(http.clone(), credential.clone()),
        pending(),
    )
    .await
    .unwrap();
    assert!(
        delivery
            .execution
            .result
            .as_ref()
            .unwrap()
            .stopped_confirmed()
    );
    assert_eq!(
        credential.calls.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn native_stop_handler_rejects_status_error_and_retains_completed_request_evidence() {
    let command = stop_command::parse(
        ["stop", TARGET, "--yes", "--timeout", "10"]
            .into_iter()
            .map(OsString::from),
    )
    .unwrap();
    for state in ["Running", "Stopped", "Stopping"] {
        let http = ScriptedHttp::new([response(
            200,
            &serde_json::json!({"status":state,"error":{"errorCode":"private-status-error"}})
                .to_string(),
            &[],
        )]);
        let delivery = v2_stop_main::execute(
            &command,
            session(http.clone(), Arc::new(TestCredential::default())),
            pending(),
        )
        .await
        .unwrap();
        assert!(delivery.execution.result.is_err());
        assert_eq!(delivery.value["stoppedConfirmed"], false);
        assert_eq!(delivery.value["mutationMayHaveOccurred"], false);
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        assert!(!delivery.value.to_string().contains("private-"));
    }
    let http = ScriptedHttp::new([
        response(200, r#"{"status":"Running","error":null}"#, &[]),
        response(200, "", &[]),
        response(
            200,
            r#"{"status":"Stopped","error":{"errorCode":"private-status-error"}}"#,
            &[],
        ),
    ]);
    let delivery = v2_stop_main::execute(
        &command,
        session(http.clone(), Arc::new(TestCredential::default())),
        pending(),
    )
    .await
    .unwrap();
    assert!(
        delivery
            .execution
            .result
            .as_ref()
            .unwrap_err()
            .receipt()
            .is_some()
    );
    assert_eq!(delivery.value["stoppedConfirmed"], false);
    assert_eq!(delivery.value["mutationMayHaveOccurred"], true);
    assert_eq!(
        delivery.value["requestEvidence"]["requestOutcome"]["httpStatus"],
        200
    );
    assert_eq!(http.requests.lock().unwrap().len(), 3);
    assert!(!delivery.value.to_string().contains("private-"));
}
