// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline standalone stop parsing, shared SDK execution and output contracts.

#![deny(unnameable_test_items)]

use fabric_cli_mutations::v2_stop::{self, ArgumentError, StopCommand, StopRun};
use fabric_sdk_items::stop::{MirroringState, StopOutcome};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::Future,
    io::{self, Write},
    pin::Pin,
};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aAaAaAaA-bBbB-cCcC-dDdD-eEeEeEeEeEeE";
const TARGET: &str = "/11111111-2222-3333-4444-555555555555.Workspace/aAaAaAaA-bBbB-cCcC-dDdD-eEeEeEeEeEeE.MirroredDatabase/runtime/mirroring";
const OPERATION: &str = "33333333-4444-5555-6666-777777777777";
const PRIVATE: &str = "PRIVATE_PROVIDER_TEXT_NOT_FOR_OUTPUT";

struct Operations {
    replies: VecDeque<Result<ResourceOutcome, ResourceError>>,
    requests: Vec<ResourceOperation>,
}

impl Operations {
    fn new(replies: impl IntoIterator<Item = Result<ResourceOutcome, ResourceError>>) -> Self {
        Self {
            replies: replies.into_iter().collect(),
            requests: Vec::new(),
        }
    }

    fn assert_actions(&self, actions: &[&str]) {
        assert_eq!(self.requests.len(), actions.len());
        assert!(
            self.replies.is_empty(),
            "expected replies were not consumed"
        );
        for (request, action) in self.requests.iter().zip(actions) {
            assert_eq!(request.method(), ResourceMethod::Post);
            assert_eq!(
                request.path(),
                format!("/v1/workspaces/{WORKSPACE}/mirroredDatabases/{ITEM}/{action}")
            );
            assert!(request.body().is_none());
            assert_eq!(request.completion(), CompletionPolicy::ReturnAccepted);
        }
    }
}

impl ResourceOperations for Operations {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.requests.push(operation);
        assert!(
            self.requests
                .iter()
                .filter(|request| request.path().ends_with("/stopMirroring"))
                .count()
                <= 1,
            "a stop must never be replayed"
        );
        Box::pin(std::future::ready(
            self.replies
                .pop_front()
                .expect("no extra reads, polling, writes or compensation"),
        ))
    }
}

fn command(approved: bool) -> StopCommand {
    let mut args = vec!["stop", TARGET];
    if approved {
        args.push("--yes");
    }
    v2_stop::parse_args(args).unwrap()
}

fn completed(data: Option<Value>) -> Result<ResourceOutcome, ResourceError> {
    Ok(ResourceOutcome::Completed { status: 200, data })
}

fn state(status: &str) -> Result<ResourceOutcome, ResourceError> {
    completed(Some(json!({"status": status})))
}

fn accepted(operation_id: Option<&str>) -> Result<ResourceOutcome, ResourceError> {
    Ok(ResourceOutcome::Accepted {
        operation_id: operation_id.map(str::to_owned),
    })
}

fn failure(code: &str, uncertain: bool) -> ResourceError {
    ResourceError {
        code: code.into(),
        message: PRIVATE.into(),
        status: Some(503),
        service_error: None,
        raw_service_error: Some(Box::new(json!({PRIVATE: {"message": PRIVATE}}))),
        operation_id: Some(OPERATION.into()),
        mutation_may_have_occurred: uncertain,
    }
}

fn document(bytes: &[u8]) -> Value {
    assert_eq!(bytes.last(), Some(&b'\n'));
    let text = std::str::from_utf8(bytes).unwrap();
    assert_eq!(text.lines().count(), 1);
    serde_json::from_slice(bytes).unwrap()
}

fn assert_safe(value: &Value) {
    let text = value.to_string();
    assert!(!text.contains(PRIVATE), "{text}");
}

async fn invoke(
    approved: bool,
    replies: impl IntoIterator<Item = Result<ResourceOutcome, ResourceError>>,
) -> (StopRun, Operations, Value) {
    let mut operations = Operations::new(replies);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let report = v2_stop::run(
        &command(approved),
        &mut operations,
        &mut stdout,
        &mut stderr,
    )
    .await;
    assert!(!report.output_failed);
    let value = if report.result.is_ok() {
        assert!(stderr.is_empty());
        assert_eq!(report.exit_code(), 0);
        document(&stdout)
    } else {
        assert!(stdout.is_empty());
        assert_eq!(report.exit_code(), 1);
        document(&stderr)
    };
    assert_safe(&value);
    (report, operations, value)
}

#[test]
fn schema_is_standalone_honest_and_bounded() {
    v2_stop::command().debug_assert();
    let help = v2_stop::command().render_long_help().to_string();
    for expected in ["--yes", "--format", "not an installed executable"] {
        assert!(help.contains(expected), "{expected}");
    }
    assert!(!help.contains("--wait"));
    assert!(!help.contains("--timeout"));
    assert!(!help.contains("--force"));
    assert!(matches!(
        v2_stop::parse_args(["stop", "--help"]),
        Err(ArgumentError::Syntax(clap::error::ErrorKind::DisplayHelp))
    ));
}

#[test]
fn default_json_and_explicit_approval_preserve_exact_id_spelling() {
    for args in [
        vec!["stop", TARGET],
        vec!["stop", TARGET, "--format", "json"],
        vec!["stop", "--format=json", "--", TARGET],
    ] {
        let parsed = v2_stop::parse_args(args).unwrap();
        assert_eq!(parsed.target().workspace_id(), WORKSPACE);
        assert_eq!(parsed.target().item_id(), ITEM);
        assert!(!parsed.approved());
    }
    for args in [
        vec!["stop", TARGET, "--yes"],
        vec!["stop", "--yes", "--format", "json", TARGET],
    ] {
        assert!(v2_stop::parse_args(args).unwrap().approved());
    }
}

#[test]
fn wrong_scope_or_non_id_targets_fail_without_an_executor() {
    let mut invalid = vec![
        String::new(),
        TARGET.trim_start_matches('/').into(),
        format!("https://example.invalid{TARGET}"),
        format!("/{TARGET}"),
        format!("{TARGET}/"),
        format!("{TARGET}/child"),
        format!("{TARGET}?private=secret"),
        format!("{TARGET}#fragment"),
        format!("{TARGET},{TARGET}"),
        format!(" {TARGET}"),
        TARGET.replace(".Workspace", ".workspace"),
        TARGET.replace(".Workspace", ".Workspace/.."),
        TARGET.replace(".MirroredDatabase", ".mirroreddatabase"),
        TARGET.replace(".MirroredDatabase", ".Notebook"),
        TARGET.replace(".MirroredDatabase", ".Lakehouse"),
        TARGET.replace(".MirroredDatabase", ".SQLDatabase"),
        TARGET.replace("/runtime/mirroring", ""),
        TARGET.replace("/runtime/mirroring", "/runtime"),
        TARGET.replace("/runtime/mirroring", "/runtime/Mirroring"),
        TARGET.replace("/runtime/mirroring", "/src/definition"),
        TARGET.replace("/runtime/mirroring", "/docs/schema"),
        TARGET.replace("/runtime/mirroring", "/runtime/runs/run-id"),
        TARGET.replace("/runtime/mirroring", "/runtime/sql/sessions/session-id"),
        TARGET.replace("/runtime/mirroring", "/runtime//mirroring"),
        TARGET.replace("/runtime/mirroring", "/runtime/../mirroring"),
        TARGET.replace("/runtime/mirroring", "/runtime\\mirroring"),
        TARGET.replace(WORKSPACE, "workspace-name"),
        TARGET.replace(ITEM, "item-name"),
        TARGET.replace(ITEM, "*"),
        TARGET.replace(ITEM, "[item]"),
        TARGET.replace(ITEM, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeeg"),
        TARGET.replace(ITEM, "aaaaaaaabbbbccccddddeeeeeeeeeeee"),
        TARGET.replace(ITEM, "{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}"),
        TARGET.replace(ITEM, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeee"),
        TARGET.replace(ITEM, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeeee"),
        TARGET.replace(ITEM, "aaaaaaaa%2fbbbb-cccc-dddd-eeeeeeeeeeee"),
        TARGET.replace(ITEM, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeee\n"),
        format!("/workspaces/{WORKSPACE}/mirroredDatabases/{ITEM}/runtime/mirroring"),
    ];
    invalid.extend(
        [
            "aaaaaaaa_bbbb-cccc-dddd-eeeeeeeeeeee",
            "aaaaaaaa-bbbb-cccc-dddd_eeeeeeeeeeee",
            "ａａａａａａａａ-bbbb-cccc-dddd-eeeeeeeeeeee",
        ]
        .into_iter()
        .map(|item| TARGET.replace(ITEM, item)),
    );
    for path in invalid {
        let error = v2_stop::parse_target(&path).unwrap_err();
        assert_eq!(error, ArgumentError::InvalidTarget, "{path:?}");
        assert!(
            v2_stop::parse_args(["stop", path.as_str(), "--yes"]).is_err(),
            "{path:?}"
        );
        let mut stderr = Vec::new();
        v2_stop::write_argument_error(error, &mut stderr).unwrap();
        let output = document(&stderr);
        assert_eq!(output["error"]["code"], "InvalidTarget");
        assert!(!output.to_string().contains(&path) || path.is_empty());
    }
}

#[test]
fn unknown_duplicate_or_unsupported_options_are_not_ignored() {
    for args in [
        vec!["stop"],
        vec!["stop", TARGET, TARGET],
        vec!["stop", TARGET, "--yes", "--yes"],
        vec!["stop", TARGET, "--yes=false"],
        vec!["stop", TARGET, "--yes", "false"],
        vec!["stop", TARGET, "--y"],
        vec!["stop", TARGET, "--format"],
        vec!["stop", TARGET, "--format", "text"],
        vec!["stop", TARGET, "--format", "JSON"],
        vec!["stop", TARGET, "--format", ""],
        vec!["stop", TARGET, "--format", PRIVATE],
        vec!["stop", TARGET, "--format", "json", "--format", "json"],
        vec!["stop", TARGET, "--wait"],
        vec!["stop", TARGET, "--timeout", "30s"],
        vec!["stop", TARGET, "--force"],
        vec!["stop", TARGET, "-y"],
        vec!["stop", TARGET, "--input", "private.json"],
        vec!["stop", TARGET, "--output", "private.json"],
        vec!["stop", TARGET, "--project", "status"],
        vec!["stop", TARGET, "--profile", "private"],
        vec!["stop", TARGET, "--token", PRIVATE],
    ] {
        let executor = Operations::new([]);
        let error = match v2_stop::parse_args(args) {
            Err(error) => error,
            Ok(_) => panic!("invalid invocation passed local preflight"),
        };
        let mut stderr = Vec::new();
        v2_stop::write_argument_error(error, &mut stderr).unwrap();
        let output = document(&stderr);
        assert_eq!(output["error"]["code"], "InvalidArguments");
        assert_safe(&output);
        assert!(!output.to_string().contains(TARGET));
        executor.assert_actions(&[]);
    }
}

#[cfg(windows)]
#[test]
fn native_non_unicode_target_is_rejected_without_lossy_conversion() {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};

    let invalid = OsString::from_wide(&[0xd800]);
    let error = v2_stop::parse_args([OsString::from("stop"), invalid]).unwrap_err();
    assert!(matches!(error, ArgumentError::Syntax(_)));
}

#[tokio::test]
async fn stopped_and_stopping_are_no_write_receipts_without_approval() {
    for approved in [false, true] {
        for (status, outcome, confirmed) in [
            ("Stopped", "noOp", true),
            ("Stopping", "alreadyStopping", false),
        ] {
            let (report, operations, output) = invoke(approved, [state(status)]).await;
            operations.assert_actions(&["getMirroringStatus"]);
            assert_eq!(output["outcome"], outcome);
            assert_eq!(output["observedState"], status);
            assert_eq!(output["stoppedConfirmed"], confirmed);
            assert_eq!(output["mutationMayHaveOccurred"], false);
            assert!(output["requestOutcome"].is_null());
            assert_eq!(output["target"]["path"], TARGET);
            assert_eq!(output["target"]["workspaceId"], WORKSPACE);
            assert_eq!(output["target"]["itemId"], ITEM);
            assert_eq!(output["target"]["type"], "MirroredDatabase");
            assert_eq!(output["target"]["facet"], "runtime/mirroring");
            assert_eq!(report.result.unwrap().stopped_confirmed(), confirmed);
        }
    }
}

#[tokio::test]
async fn running_needs_explicit_approval_before_any_stop_submission() {
    let (report, operations, output) = invoke(false, [state("Running")]).await;
    operations.assert_actions(&["getMirroringStatus"]);
    assert_eq!(report.result.unwrap_err().code(), "ApprovalRequired");
    assert_eq!(output["error"]["code"], "ApprovalRequired");
    assert_eq!(output["mutationMayHaveOccurred"], false);
    assert_eq!(output["stoppedConfirmed"], false);
    assert!(output["requestEvidence"].is_null());
}

#[tokio::test]
async fn other_known_precondition_states_never_authorize_a_stop() {
    for status in ["Initializing", "Initialized", "Starting", "Paused"] {
        for approved in [false, true] {
            let (_, operations, output) = invoke(approved, [state(status)]).await;
            operations.assert_actions(&["getMirroringStatus"]);
            assert!(output["error"].is_object());
            assert_eq!(output["mutationMayHaveOccurred"], false);
            assert_eq!(output["stoppedConfirmed"], false);
        }
    }
}

#[tokio::test]
async fn malformed_and_substring_states_fail_before_submission() {
    for data in [
        None,
        Some(Value::Null),
        Some(json!({})),
        Some(json!({"status": null})),
        Some(json!({"status": 1})),
        Some(json!({"status": true})),
        Some(json!({"status": []})),
        Some(json!({"status": {}})),
        Some(json!({"state": "Running"})),
        Some(json!("Running")),
        Some(json!({"status": ""})),
        Some(json!({"status": "Run"})),
        Some(json!({"status": "running"})),
        Some(json!({"status": "STOPPED"})),
        Some(json!({"status": " Running"})),
        Some(json!({"status": "Running "})),
        Some(json!({"status": PRIVATE})),
    ] {
        let (_, operations, output) = invoke(true, [completed(data)]).await;
        operations.assert_actions(&["getMirroringStatus"]);
        assert!(output["error"].is_object());
        assert_eq!(output["mutationMayHaveOccurred"], false);
        assert_eq!(output["stoppedConfirmed"], false);
    }
}

#[tokio::test]
async fn accepted_or_wrong_http_precondition_is_not_followed_or_treated_as_running() {
    for reply in [
        accepted(Some(OPERATION)),
        accepted(None),
        Ok(ResourceOutcome::Completed {
            status: 201,
            data: Some(json!({"status": "Running"})),
        }),
        Ok(ResourceOutcome::Completed {
            status: 202,
            data: Some(json!({"status": "Running"})),
        }),
        Ok(ResourceOutcome::Completed {
            status: 204,
            data: None,
        }),
    ] {
        let (_, operations, output) = invoke(true, [reply]).await;
        operations.assert_actions(&["getMirroringStatus"]);
        assert!(output["error"].is_object());
        assert_eq!(output["mutationMayHaveOccurred"], false);
    }
}

#[tokio::test]
async fn accepted_stop_preserves_only_actual_operation_identity_and_does_not_poll() {
    for operation_id in [Some(OPERATION), None] {
        let (report, operations, output) =
            invoke(true, [state("Running"), accepted(operation_id)]).await;
        operations.assert_actions(&["getMirroringStatus", "stopMirroring"]);
        assert_eq!(output["outcome"], "accepted");
        assert_eq!(output["observedState"], "Running");
        assert_eq!(output["mutationMayHaveOccurred"], true);
        assert_eq!(output["stoppedConfirmed"], false);
        assert_eq!(output["requestOutcome"]["status"], "accepted");
        assert_eq!(
            output["requestOutcome"]["operationId"],
            operation_id.map_or(Value::Null, |value| json!(value))
        );
        let receipt = report.result.unwrap();
        assert_eq!(receipt.outcome, StopOutcome::Accepted);
        assert_eq!(receipt.observed_state, Some(MirroringState::Running));
        assert_eq!(
            receipt.request_outcome,
            Some(ResourceOutcome::Accepted {
                operation_id: operation_id.map(str::to_owned)
            })
        );
    }
}

#[tokio::test]
async fn completed_request_requires_one_exact_stopped_readback_for_confirmation() {
    for status in [
        "Initializing",
        "Initialized",
        "Starting",
        "Running",
        "Paused",
        "Stopping",
        "Stopped",
    ] {
        let (report, operations, output) =
            invoke(true, [state("Running"), completed(None), state(status)]).await;
        operations.assert_actions(&["getMirroringStatus", "stopMirroring", "getMirroringStatus"]);
        let stopped = status == "Stopped";
        assert_eq!(
            output["outcome"],
            if stopped {
                "stopped"
            } else {
                "requestCompleted"
            }
        );
        assert_eq!(output["observedState"], status);
        assert_eq!(output["stoppedConfirmed"], stopped);
        assert_eq!(output["mutationMayHaveOccurred"], true);
        assert_eq!(
            output["requestOutcome"],
            json!({"status": "completed", "httpStatus": 200})
        );
        assert_eq!(report.result.unwrap().stopped_confirmed(), stopped);
    }
}

#[tokio::test]
async fn service_payloads_remain_native_evidence_but_are_not_rendered() {
    let stop_body = json!({PRIVATE: {"message": PRIVATE}});
    let replies = [
        completed(Some(json!({"status": "Running", PRIVATE: PRIVATE}))),
        completed(Some(stop_body.clone())),
        completed(Some(json!({"status": "Stopped", PRIVATE: PRIVATE}))),
    ];
    let (report, operations, output) = invoke(true, replies).await;
    operations.assert_actions(&["getMirroringStatus", "stopMirroring", "getMirroringStatus"]);
    assert_safe(&output);
    assert_eq!(
        report.result.unwrap().request_outcome,
        Some(ResourceOutcome::Completed {
            status: 200,
            data: Some(stop_body)
        })
    );
}

#[tokio::test]
async fn precondition_and_submission_errors_preserve_sanitized_stage_uncertainty() {
    for code in [PRIVATE, "Cancelled", "Timeout", "ServiceError"] {
        let native = failure(code, false);
        let (report, operations, output) = invoke(true, [Err(native.clone())]).await;
        operations.assert_actions(&["getMirroringStatus"]);
        assert_eq!(output["mutationMayHaveOccurred"], false);
        assert_eq!(output["executionEvidence"]["httpStatus"], 503);
        assert_eq!(output["executionEvidence"]["operationId"], OPERATION);
        let error = report.result.unwrap_err();
        assert_eq!(error.resource_error(), Some(&native));
        assert!(!error.to_string().contains(PRIVATE));
        for uncertain in [false, true] {
            let native = failure(code, uncertain);
            let (report, operations, output) =
                invoke(true, [state("Running"), Err(native.clone())]).await;
            operations.assert_actions(&["getMirroringStatus", "stopMirroring"]);
            assert_eq!(output["mutationMayHaveOccurred"], uncertain);
            assert_eq!(
                output["executionEvidence"]["mutationMayHaveOccurred"],
                uncertain
            );
            assert_eq!(output["stoppedConfirmed"], false);
            assert_eq!(report.result.unwrap_err().resource_error(), Some(&native));
        }
    }
}

#[tokio::test]
async fn readback_failures_retain_completed_request_and_never_resubmit() {
    for readback in [
        Err(failure(PRIVATE, false)),
        Err(failure("Cancelled", false)),
        Err(failure("Timeout", false)),
        completed(None),
        completed(Some(json!({"status": PRIVATE}))),
        completed(Some(json!({"status": "Run"}))),
        accepted(Some(OPERATION)),
        Ok(ResourceOutcome::Completed {
            status: 204,
            data: None,
        }),
    ] {
        let (report, operations, output) =
            invoke(true, [state("Running"), completed(None), readback]).await;
        operations.assert_actions(&["getMirroringStatus", "stopMirroring", "getMirroringStatus"]);
        assert_eq!(output["mutationMayHaveOccurred"], true);
        assert_eq!(output["stoppedConfirmed"], false);
        assert_eq!(output["requestEvidence"]["outcome"], "requestCompleted");
        assert_eq!(
            output["requestEvidence"]["requestOutcome"],
            json!({"status": "completed", "httpStatus": 200})
        );
        let error = report.result.unwrap_err();
        assert!(error.mutation_may_have_occurred);
        assert_eq!(
            error.receipt().unwrap().request_outcome,
            Some(ResourceOutcome::Completed {
                status: 200,
                data: None
            })
        );
    }
}

#[tokio::test]
async fn unexpected_submission_status_preserves_evidence_without_readback() {
    for status in [201, 202, 204, 400, 503] {
        let submission = ResourceOutcome::Completed {
            status,
            data: Some(json!({PRIVATE: PRIVATE})),
        };
        let (report, operations, output) =
            invoke(true, [state("Running"), Ok(submission.clone())]).await;
        operations.assert_actions(&["getMirroringStatus", "stopMirroring"]);
        assert_eq!(output["error"]["code"], "InvalidResponse");
        assert_eq!(output["mutationMayHaveOccurred"], true);
        assert_eq!(output["stoppedConfirmed"], false);
        assert_eq!(
            output["requestEvidence"]["requestOutcome"],
            json!({"status": "completed", "httpStatus": status})
        );
        assert_eq!(
            output["responseEvidence"],
            json!({"status": "completed", "httpStatus": status})
        );
        let error = report.result.unwrap_err();
        assert_eq!(error.response_outcome(), Some(&submission));
        assert_eq!(
            error.receipt().unwrap().request_outcome.as_ref(),
            Some(&submission)
        );
    }
}

#[tokio::test]
async fn effects_and_azure_sql_warning_do_not_overstate_completion_or_scope() {
    let (_, operations, output) = invoke(true, [state("Running"), accepted(None)]).await;
    operations.assert_actions(&["getMirroringStatus", "stopMirroring"]);
    assert_eq!(output["effects"]["intent"], "stopReplication");
    assert_eq!(
        output["effects"]["itemAndReplicatedData"],
        "preservedAccordingToProviderContract"
    );
    assert_eq!(output["effects"]["sourceDatabaseShutdown"], false);
    assert_eq!(output["effects"]["zeroCostGuaranteed"], false);
    assert_eq!(output["effects"]["drainGuaranteed"], false);
    assert_eq!(output["warnings"][0]["appliesTo"], "AzureSql");
    assert!(
        output["warnings"][0]["message"]
            .as_str()
            .unwrap()
            .contains("fully reinitializes replication")
    );
    assert!(
        output["warnings"][0]["message"]
            .as_str()
            .unwrap()
            .contains("provider-specific")
    );
    assert_eq!(output["warnings"][1]["code"], "NoRemoteRollback");
}

struct BrokenWriter {
    accepted_bytes: usize,
    fail_after: usize,
    fail_flush: bool,
}

impl Write for BrokenWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.accepted_bytes >= self.fail_after {
            return Err(io::Error::other(PRIVATE));
        }
        let length = bytes.len().min(self.fail_after - self.accepted_bytes);
        self.accepted_bytes += length;
        Ok(length)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            Err(io::Error::other(PRIVATE))
        } else {
            Ok(())
        }
    }
}

struct RecoveringWriter {
    fail_once: bool,
    bytes: Vec<u8>,
}

impl Write for RecoveringWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail_once {
            self.fail_once = false;
            return Err(io::Error::other(PRIVATE));
        }
        self.bytes.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.bytes.flush()
    }
}

#[tokio::test]
async fn output_write_partial_and_flush_failures_are_nonzero_with_original_evidence() {
    for (fail_after, fail_flush) in [(0, false), (17, false), (usize::MAX, true)] {
        for replies in [
            vec![state("Stopped")],
            vec![state("Stopping")],
            vec![state("Running"), accepted(Some(OPERATION))],
            vec![state("Running"), completed(None), state("Stopping")],
            vec![state("Running"), completed(None), state("Stopped")],
        ] {
            let parsed = command(true);
            let mut operations = Operations::new(replies);
            let mut stdout = BrokenWriter {
                accepted_bytes: 0,
                fail_after,
                fail_flush,
            };
            let mut stderr = Vec::new();
            let report = v2_stop::run(&parsed, &mut operations, &mut stdout, &mut stderr).await;
            assert!(report.output_failed);
            assert_eq!(report.exit_code(), 1);
            assert!(operations.replies.is_empty());
            let fallback = document(&stderr);
            assert_safe(&fallback);
            assert_eq!(fallback["error"]["code"], "OutputFailed");
            assert_eq!(
                fallback["originalResult"],
                v2_stop::result_metadata(&parsed, &report.result)
            );
            assert!(report.result.is_ok());
        }
    }
}

#[tokio::test]
async fn output_failure_after_failed_readback_preserves_the_original_sdk_error() {
    let parsed = command(true);
    let native = failure(PRIVATE, false);
    let mut operations = Operations::new([state("Running"), completed(None), Err(native.clone())]);
    let mut stdout = Vec::new();
    let mut stderr = BrokenWriter {
        accepted_bytes: 0,
        fail_after: 0,
        fail_flush: false,
    };
    let report = v2_stop::run(&parsed, &mut operations, &mut stdout, &mut stderr).await;
    assert!(stdout.is_empty());
    assert!(report.output_failed);
    assert_eq!(report.exit_code(), 1);
    operations.assert_actions(&["getMirroringStatus", "stopMirroring", "getMirroringStatus"]);
    let error = report.result.unwrap_err();
    assert_eq!(error.resource_error(), Some(&native));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(
        error.receipt().unwrap().request_outcome,
        Some(ResourceOutcome::Completed {
            status: 200,
            data: None
        })
    );
}

#[tokio::test]
async fn recoverable_stderr_failure_delivers_original_error_evidence_best_effort() {
    let parsed = command(true);
    let mut operations = Operations::new([
        state("Running"),
        completed(None),
        Err(failure(PRIVATE, false)),
    ]);
    let mut stdout = Vec::new();
    let mut stderr = RecoveringWriter {
        fail_once: true,
        bytes: Vec::new(),
    };
    let report = v2_stop::run(&parsed, &mut operations, &mut stdout, &mut stderr).await;
    operations.assert_actions(&["getMirroringStatus", "stopMirroring", "getMirroringStatus"]);
    assert!(stdout.is_empty());
    assert!(report.output_failed);
    assert_eq!(report.exit_code(), 1);
    let fallback = document(&stderr.bytes);
    assert_safe(&fallback);
    assert_eq!(fallback["error"]["code"], "OutputFailed");
    assert_eq!(
        fallback["originalResult"],
        v2_stop::result_metadata(&parsed, &report.result)
    );
    assert_eq!(
        fallback["originalResult"]["requestEvidence"]["requestOutcome"]["httpStatus"],
        200
    );
    assert!(report.result.is_err());
}

#[tokio::test]
async fn both_broken_streams_do_not_erase_accepted_native_evidence() {
    let mut operations = Operations::new([state("Running"), accepted(Some(OPERATION))]);
    let mut stdout = BrokenWriter {
        accepted_bytes: 0,
        fail_after: 0,
        fail_flush: false,
    };
    let mut stderr = BrokenWriter {
        accepted_bytes: 0,
        fail_after: 0,
        fail_flush: false,
    };
    let report = v2_stop::run(&command(true), &mut operations, &mut stdout, &mut stderr).await;
    operations.assert_actions(&["getMirroringStatus", "stopMirroring"]);
    assert_eq!(report.exit_code(), 1);
    assert!(report.output_failed);
    let receipt = report.result.unwrap();
    assert!(!receipt.stopped_confirmed());
    assert_eq!(
        receipt.request_outcome,
        Some(ResourceOutcome::Accepted {
            operation_id: Some(OPERATION.into())
        })
    );
}
