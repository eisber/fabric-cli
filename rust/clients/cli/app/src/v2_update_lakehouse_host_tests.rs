// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use super::*;
use async_trait::async_trait;
use azure_core::credentials::{AccessToken, TokenRequestOptions};
use azure_core::time::OffsetDateTime;
use fabric_cli::update::{self, UpdateInput};
use fabric_sdk::{Audience, CredentialSelection, RequestContext, TokenCredential};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationRequest, OperationResponse,
};
use std::{
    collections::BTreeMap,
    future::pending,
    io::{Cursor, Write},
    pin::Pin,
    sync::{Arc, Mutex},
};

const WS: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const TARGET: &str = "/11111111-2222-3333-4444-555555555555.Workspace/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.Lakehouse";

#[derive(Debug)]
struct Credential;

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, &[Audience::Fabric.scope()]);
        Ok(AccessToken::new(
            "synthetic-lakehouse-host",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

fn context(account: &str) -> RequestContext {
    RequestContext::new(
        Arc::new(Credential),
        CredentialSelection::new(account, "offline-tenant", Audience::Fabric).unwrap(),
    )
}

fn command(facade: bool, extra: &[&str]) -> UpdateCommand {
    let mut args = vec!["fab-cli-v2"];
    if facade {
        args.extend(["artifact", "lakehouse"]);
    }
    args.extend(["update", TARGET, "--input", "-", "--mode", "patch"]);
    args.extend(extra);
    update::parse(args).unwrap()
}

fn bound(command: UpdateCommand, bytes: &[u8], cwd: &std::path::Path) -> PreparedUpdateCommand {
    require_lakehouse(&command).unwrap();
    PreparedUpdateCommand::preflight(command, cwd, &mut Cursor::new(bytes))
        .unwrap()
        .with_context(context("offline-account"), OperationLimits::default())
        .unwrap()
}

fn policy(prepared: &PreparedUpdateCommand, approved: bool) -> ExactApproval {
    ExactApproval {
        captured: prepared.update().prepared().clone(),
        approved,
    }
}

#[derive(Clone)]
struct Wire {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    response: OperationResponse,
    pending: bool,
}

impl Wire {
    fn response(status: u16, body: Value) -> Self {
        Self {
            calls: Arc::default(),
            response: OperationResponse {
                status,
                headers: BTreeMap::new(),
                body: serde_json::to_vec(&body).unwrap(),
            },
            pending: false,
        }
    }

    fn success() -> Self {
        Self::response(
            200,
            json!({
                "id":ITEM, "workspaceId":WS, "type":"Lakehouse",
                "displayName":"renamed", "description":""
            }),
        )
    }
}

impl OperationExecutor for Wire {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        assert_eq!(
            request.method(),
            fabric_sdk_workspaces::mutations::ResourceMethod::Patch
        );
        assert_eq!(
            request.url().as_str(),
            format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/lakehouses/{ITEM}")
        );
        self.calls.lock().unwrap().push((
            request.url().to_string(),
            serde_json::from_slice(request.body()).unwrap(),
        ));
        if self.pending {
            Box::pin(pending())
        } else {
            Box::pin(std::future::ready(Ok(self.response.clone())))
        }
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("metadata update must not poll or retry")
    }
}

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn root_and_facade_send_one_exact_patch_and_project_only_data() {
    let cwd = tempfile::tempdir().unwrap();
    for facade in [false, true] {
        for bytes in [
            br#"{"displayName":"renamed"}"#.as_slice(),
            br#"{"description":""}"#.as_slice(),
            br#"{"displayName":"renamed","description":""}"#.as_slice(),
        ] {
            let prepared = bound(
                command(facade, &["--project", "displayName"]),
                bytes,
                cwd.path(),
            );
            let mut approval = policy(&prepared, true);
            let question = approval_question(&approval.captured).unwrap();
            assert!(question.contains("offline-account"));
            assert!(question.contains("unguardedConcurrency"));
            assert!(question.contains("existingOnly"));
            assert!(question.contains("body"));
            let wire = Wire::success();
            let delivery = prepared
                .execute_bound(wire.clone(), &mut approval, pending())
                .await
                .unwrap();
            let output = Output::default();
            publish(
                delivery,
                Some(output.clone()),
                &NativeDeadline::new(Duration::from_secs(5)).unwrap(),
            )
            .await
            .unwrap();
            let receipt: Value = serde_json::from_slice(&output.0.lock().unwrap()).unwrap();
            assert_eq!(receipt["status"], "succeeded");
            assert_eq!(receipt["data"], "renamed");
            assert_eq!(receipt["meta"]["target"]["identity"]["itemId"], ITEM);
            assert_eq!(
                receipt["meta"]["preconditions"][1]["kind"],
                "unguardedConcurrency"
            );
            assert_eq!(wire.calls.lock().unwrap().len(), 1);
            assert_eq!(
                wire.calls.lock().unwrap()[0].1,
                serde_json::from_slice::<Value>(bytes).unwrap()
            );
        }
    }
}

#[tokio::test]
async fn declined_or_different_body_context_target_and_type_never_send() {
    let cwd = tempfile::tempdir().unwrap();
    let wire = Wire::success();
    let body = br#"{"displayName":"renamed"}"#;
    for change in 0..7 {
        let prepared = bound(command(false, &[]), body, cwd.path());
        let mut approval = policy(&prepared, change != 0);
        if change != 0 {
            let mut changed = command(false, &[]);
            let context = context(if change == 1 {
                "different"
            } else {
                "offline-account"
            });
            let bytes = if change == 2 {
                br#"{"description":"different"}"#.as_slice()
            } else {
                body.as_slice()
            };
            if change == 3 {
                changed.target = update::parse_target(
                    &TARGET.replace(ITEM, "bbbbbbbb-cccc-dddd-eeee-ffffffffffff"),
                )
                .unwrap();
            }
            if change == 4 {
                changed.target =
                    update::parse_target(&TARGET.replace("Lakehouse", "Notebook")).unwrap();
            }
            if change == 5 {
                changed.options.timeout_millis += 1;
            }
            let mut limits = OperationLimits::default();
            if change == 6 {
                limits.max_requests = std::num::NonZeroUsize::new(3).unwrap();
            }
            let different =
                PreparedUpdateCommand::preflight(changed, cwd.path(), &mut Cursor::new(bytes))
                    .unwrap()
                    .with_context(context, limits)
                    .unwrap();
            approval.captured = different.update().prepared().clone();
        }
        assert!(matches!(
            prepared
                .execute_bound(wire.clone(), &mut approval, pending())
                .await,
            Err(UpdateCommandError::Approval)
        ));
    }
    assert!(wire.calls.lock().unwrap().is_empty());
    let prepared = bound(command(false, &[]), body, cwd.path());
    let mut approval = policy(&prepared, true);
    assert!(approval.authorize(prepared.update().prepared()).is_ok());
    assert!(approval.authorize(prepared.update().prepared()).is_err());
}

#[test]
fn strict_input_type_and_output_preflight_are_local() {
    let cwd = tempfile::tempdir().unwrap();
    for body in [
        r#"{}"#,
        r#"{"displayName":null}"#,
        r#"{"description":null}"#,
        r#"{"displayName":"a","displayName":"b"}"#,
        r#"{"description":1}"#,
        r#"{"tables":[]}"#,
        r#"{"settings":{}}"#,
        r#"{"definition":{}}"#,
        r#"{"displayName":"x"} trailing"#,
    ] {
        assert!(
            PreparedUpdateCommand::preflight(
                command(true, &[]),
                cwd.path(),
                &mut Cursor::new(body)
            )
            .is_err(),
            "{body}"
        );
    }
    let oversize = vec![b' '; update::update::MAX_INPUT_BYTES + 1];
    assert!(
        PreparedUpdateCommand::preflight(
            command(false, &[]),
            cwd.path(),
            &mut Cursor::new(oversize)
        )
        .is_err()
    );
    let output = cwd.path().join("keep.json");
    std::fs::write(&output, b"keep").unwrap();
    assert!(
        PreparedUpdateCommand::preflight(
            command(false, &["--output", output.to_str().unwrap()]),
            cwd.path(),
            &mut Cursor::new(br#"{"description":""}"#)
        )
        .is_err()
    );
    assert_eq!(std::fs::read(output).unwrap(), b"keep");
    let mut notebook = command(false, &[]);
    notebook.target = update::parse_target(&TARGET.replace("Lakehouse", "Notebook")).unwrap();
    assert!(require_lakehouse(&notebook).is_err());
    assert!(require_lakehouse(&command(false, &["--plan"])).is_err());
    for path in [
        format!("{TARGET}/Tables"),
        format!("{TARGET}/Files"),
        format!("{TARGET}/settings"),
        TARGET.replace("Lakehouse", "Warehouse"),
    ] {
        assert!(update::parse_target(&path).is_err());
    }
}

#[tokio::test]
async fn native_partial_unknown_denial_and_handles_remain_non_success() {
    let cwd = tempfile::tempdir().unwrap();
    for (status, data, expected, handle) in [
        (
            200,
            json!({"id":ITEM,"type":"Lakehouse","displayName":"old"}),
            "partial",
            false,
        ),
        (
            200,
            json!({"id":ITEM,"type":"Notebook","displayName":"renamed"}),
            "unknown",
            false,
        ),
        (
            403,
            json!({"errorCode":"Denied","message":"PRIVATE_CAUSE"}),
            "failed",
            false,
        ),
        (404, json!({"errorCode":"ItemNotFound"}), "failed", false),
        (
            412,
            json!({"errorCode":"PreconditionFailed"}),
            "failed",
            false,
        ),
        (503, json!({"errorCode":"Unavailable"}), "unknown", false),
        (202, Value::Null, "unknown", false),
        (202, Value::Null, "accepted", true),
    ] {
        let prepared = bound(
            command(true, &[]),
            br#"{"displayName":"renamed"}"#,
            cwd.path(),
        );
        let mut approval = policy(&prepared, true);
        let mut wire = Wire::response(status, data);
        if handle {
            wire.response.headers.insert(
                "x-ms-operation-id".into(),
                "bbbbbbbb-cccc-dddd-eeee-ffffffffffff".into(),
            );
        }
        let delivery = prepared
            .execute_bound(wire.clone(), &mut approval, pending())
            .await
            .unwrap();
        let receipt = serde_json::to_value(&delivery.report().receipt).unwrap();
        assert_eq!(receipt["status"], expected);
        if status == 403 {
            assert!(delivery.report().cause.is_some());
        }
        let output = Output::default();
        let failure = publish(
            delivery,
            Some(output.clone()),
            &NativeDeadline::new(Duration::from_secs(5)).unwrap(),
        )
        .await
        .unwrap_err();
        let written: Value = serde_json::from_slice(&output.0.lock().unwrap()).unwrap();
        assert_eq!(written, receipt);
        assert_eq!(failure.2["receipt"], receipt);
        assert!(
            !serde_json::to_string(&failure.2)
                .unwrap()
                .contains("PRIVATE_CAUSE")
        );
        assert_eq!(wire.calls.lock().unwrap().len(), 1);
        assert_eq!(receipt["meta"]["evidence"]["httpStatus"], status);
        if handle {
            assert!(!delivery_handles(&receipt).is_empty());
        }
    }
}

fn delivery_handles(receipt: &Value) -> &Vec<Value> {
    receipt["meta"]["evidence"]["handles"].as_array().unwrap()
}

struct Broken;
impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("PRIVATE_WRITER"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn writer_failure_and_file_race_retain_original_remote_receipt() {
    let cwd = tempfile::tempdir().unwrap();
    for file in [false, true] {
        let path = cwd.path().join("race.json");
        let mut command = command(false, &[]);
        if file {
            command.output = Some(path.clone());
        }
        let prepared = bound(command, br#"{"displayName":"renamed"}"#, cwd.path());
        let mut approval = policy(&prepared, true);
        let wire = Wire::success();
        let delivery = prepared
            .execute_bound(wire.clone(), &mut approval, pending())
            .await
            .unwrap();
        if file {
            std::fs::write(&path, b"racing owner").unwrap();
        }
        let error = publish(
            delivery,
            Some(Broken),
            &NativeDeadline::new(Duration::from_secs(5)).unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, "OutputFailed");
        assert_eq!(error.2["receipt"]["status"], "succeeded");
        assert_eq!(wire.calls.lock().unwrap().len(), 1);
        assert!(
            !serde_json::to_string(&error.2)
                .unwrap()
                .contains("PRIVATE_WRITER")
        );
        if file {
            assert_eq!(std::fs::read(path).unwrap(), b"racing owner");
            assert_eq!(error.2["localOutput"]["pathCreated"], false);
            assert_eq!(error.2["localOutput"]["bytesWritten"], 0);
        }
    }
}

#[tokio::test]
async fn cancellation_before_dispatch_never_sends_and_after_dispatch_is_unknown() {
    let cwd = tempfile::tempdir().unwrap();
    for dispatched in [false, true] {
        let prepared = bound(command(false, &[]), br#"{"description":""}"#, cwd.path());
        let mut approval = policy(&prepared, true);
        let mut wire = Wire::success();
        wire.pending = dispatched;
        let calls = wire.calls.clone();
        let interruption = async move {
            if dispatched {
                while calls.lock().unwrap().is_empty() {
                    tokio::task::yield_now().await;
                }
            }
        };
        let delivery = prepared
            .execute_bound(wire.clone(), &mut approval, interruption)
            .await
            .unwrap();
        assert_eq!(wire.calls.lock().unwrap().len(), usize::from(dispatched));
        if dispatched {
            assert!(matches!(
                delivery.report().receipt.outcome(),
                Outcome::Unknown { .. }
            ));
        } else {
            assert!(matches!(
                delivery.report().receipt.outcome(),
                Outcome::Failed { .. }
            ));
        }
    }
}

#[tokio::test]
async fn whole_deadline_bounds_preparation_and_preserves_receipt_before_delivery() {
    let deadline = NativeDeadline::new(Duration::from_millis(5)).unwrap();
    let error = before_submission::<()>(&deadline, pending())
        .await
        .unwrap_err();
    assert_eq!(error.0, "TimedOut");
    let cwd = tempfile::tempdir().unwrap();
    let prepared = bound(command(false, &[]), br#"{"description":""}"#, cwd.path());
    let mut approval = policy(&prepared, true);
    let wire = Wire::success();
    let delivery = prepared
        .execute_bound(wire.clone(), &mut approval, pending())
        .await
        .unwrap();
    let error = publish(delivery, Some(Output::default()), &deadline)
        .await
        .unwrap_err();
    assert_eq!(error.0, "TimedOut");
    assert_eq!(error.2["receipt"]["status"], "succeeded");
    assert_eq!(wire.calls.lock().unwrap().len(), 1);
}

#[test]
fn approval_is_default_deny_and_never_uses_json_input_as_consent() {
    for (answer, approved) in [
        (b"\n".as_slice(), false),
        (b"no\n".as_slice(), false),
        (b"yes\n".as_slice(), true),
    ] {
        assert_eq!(
            fabric_cli::console::read_explicit_confirmation(
                &mut Cursor::new(answer),
                &mut Vec::new(),
                "exact patch"
            )
            .unwrap(),
            approved
        );
    }
    for answer in [
        b"".as_slice(),
        b"yes".as_slice(),
        br#"{"displayName":"yes"}"#.as_slice(),
    ] {
        assert!(
            fabric_cli::console::read_explicit_confirmation(
                &mut Cursor::new(answer),
                &mut Vec::new(),
                "exact patch"
            )
            .is_err()
        );
    }
    assert_eq!(command(false, &[]).input, UpdateInput::Stdin);
}

#[test]
fn terminal_json_is_ascii_without_changing_unicode_approval_values() {
    let cwd = tempfile::tempdir().unwrap();
    let prepared = bound(
        command(false, &[]),
        br#"{"description":"\u00e9\uD83E\uDD80"}"#,
        cwd.path(),
    );
    let captured = prepared.update().prepared();
    let ascii = ascii_json(captured).unwrap();
    assert!(ascii.is_ascii());
    assert!(ascii.contains("\\u00e9\\ud83e\\udd80"));
    assert_eq!(
        serde_json::from_str::<Value>(&ascii).unwrap(),
        serde_json::to_value(captured).unwrap()
    );
    assert!(approval_question(captured).unwrap().is_ascii());
}

#[tokio::test]
async fn stalled_native_setup_worker_returns_without_dispatch() {
    let cwd = tempfile::tempdir().unwrap();
    let prepared = bound(command(false, &[]), br#"{"description":""}"#, cwd.path());
    let mut approval = policy(&prepared, true);
    let wire = Wire::success();
    let deadline = NativeDeadline::new(Duration::from_millis(10)).unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let result = before_submission(&deadline, async {
        tokio::task::spawn_blocking(move || {
            wait.recv().unwrap();
            Ok::<_, Failure>(())
        })
        .await
        .unwrap()?;
        prepared
            .execute_bound(wire.clone(), &mut approval, pending())
            .await
            .map_err(host_failure)
    })
    .await;
    release.send(()).unwrap();
    assert_eq!(result.unwrap_err().0, "TimedOut");
    assert!(wire.calls.lock().unwrap().is_empty());
}

struct StalledOutput(std::sync::mpsc::Receiver<()>);
impl Write for StalledOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.recv().map_err(io::Error::other)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn stalled_diagnostic_writer_is_bounded_and_failure_is_not_success() {
    let (release, wait) = std::sync::mpsc::channel();
    let started = std::time::Instant::now();
    let exit = finish_to(
        Err(failed("TimedOut", "No PATCH was sent.", 1)),
        StalledOutput(wait),
        Duration::from_millis(10),
    );
    assert_eq!(exit, 1);
    assert!(started.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    drop(release);
}

#[tokio::test]
async fn stalled_receipt_writer_times_out_with_remote_evidence() {
    let cwd = tempfile::tempdir().unwrap();
    let prepared = bound(command(false, &[]), br#"{"description":""}"#, cwd.path());
    let mut approval = policy(&prepared, true);
    let wire = Wire::success();
    let delivery = prepared
        .execute_bound(wire.clone(), &mut approval, pending())
        .await
        .unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let error = publish(
        delivery,
        Some(StalledOutput(wait)),
        &NativeDeadline::new(Duration::from_millis(10)).unwrap(),
    )
    .await
    .unwrap_err();
    release.send(()).unwrap();
    drop(release);
    assert_eq!(error.0, "TimedOut");
    assert_eq!(error.2["receipt"]["status"], "succeeded");
    assert_eq!(wire.calls.lock().unwrap().len(), 1);
}

#[test]
fn rejected_receipt_build_retains_original_outcome_status_and_handle_evidence() {
    use fabric_sdk::v2::{
        Coverage, ExecutionEvidence, OperationHandle, ReceiptBuildError, ReceiptError,
    };
    let evidence = ExecutionEvidence {
        http_status: Some(202),
        handles: vec![OperationHandle::FabricOperation {
            id: "invalid handle".into(),
        }],
        coverage: Coverage::Unknown,
        mutation_may_have_occurred: true,
    };
    let failure = host_failure(UpdateCommandError::Sdk(
        fabric_sdk::items::update::UpdateError::Receipt(Box::new(ReceiptBuildError {
            kind: ReceiptError::InvalidEvidence,
            outcome: Outcome::Accepted,
            evidence: evidence.clone(),
        })),
    ));
    assert_eq!(failure.0, "ReceiptFailed");
    assert_eq!(failure.2["outcome"]["status"], "accepted");
    assert_eq!(
        failure.2["evidence"],
        serde_json::to_value(evidence).unwrap()
    );
    assert_eq!(failure.2["remoteReplayed"], false);
}
