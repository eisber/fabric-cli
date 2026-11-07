// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline delete approval, evidence, projection and output contracts.

#![deny(unnameable_test_items)]

use fabric_cli::{
    delete::{
        DeleteOutputFiles, NativeDeleteFiles, dispatch, host_failure, preflight, prepare_output,
        render, write_dispatch,
    },
    parser::{DeleteCommand, Invocation, parse_args},
};
use fabric_sdk::items::delete::DeleteApproval;
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};
use std::{
    future::Future,
    io::{self, Write},
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

const TARGET: &str =
    "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook";

fn command(options: &[&str]) -> DeleteCommand {
    let args = ["fab-rs", "delete", TARGET, "--permanent=true"]
        .into_iter()
        .chain(options.iter().copied());
    let Invocation::Delete(command) = parse_args(args).unwrap() else {
        panic!("expected delete")
    };
    command
}

#[test]
fn selected_context_is_bound_to_assessment_and_host_failure_metadata() {
    let prepared = preflight(&command(&[]), None).unwrap();
    assert!(prepared.metadata().unwrap()["context"].is_null());
    let selection = fabric_sdk::CredentialSelection::new(
        "caller@example.invalid",
        "11111111-1111-1111-1111-111111111111",
        fabric_sdk::Audience::Fabric,
    )
    .unwrap();
    let endpoint = fabric_sdk::EndpointBinding::public(fabric_sdk::ServiceOrigin::Fabric);
    let prepared = prepared.with_selection(&selection, &endpoint).unwrap();
    let envelope =
        host_failure(&prepared, "NotConfigured", "Delete host is not configured").unwrap();
    assert_eq!(envelope.status, "failed");
    assert!(envelope.data.is_null());
    assert_eq!(
        envelope.meta["context"],
        serde_json::to_value(&prepared.assessment().metadata().context).unwrap()
    );
    assert!(
        envelope.meta["context"]
            .to_string()
            .contains("caller@example.invalid")
    );
    assert!(
        envelope.meta["context"]
            .to_string()
            .contains("api.fabric.microsoft.com")
    );
    assert_eq!(envelope.meta["hostFailure"]["dispatch"], "not_dispatched");
    assert!(envelope.meta["assessment"].is_object());
    assert!(envelope.meta["effects"]["declared"].is_array());
    assert!(envelope.meta["effects"]["observed"].is_null());
}

#[test]
fn non_fabric_context_is_rejected_without_credentials() {
    let selection = fabric_sdk::CredentialSelection::new(
        "caller@example.invalid",
        "11111111-1111-1111-1111-111111111111",
        fabric_sdk::Audience::OneLake,
    )
    .unwrap();
    let endpoint = fabric_sdk::EndpointBinding::public(fabric_sdk::ServiceOrigin::Fabric);
    assert!(
        preflight(&command(&[]), None)
            .unwrap()
            .with_selection(&selection, &endpoint)
            .is_err()
    );
}

struct Operations {
    requests: Vec<ResourceOperation>,
    outcome: Result<ResourceOutcome, ResourceError>,
}

impl Operations {
    fn accepted() -> Self {
        Self {
            requests: Vec::new(),
            outcome: Ok(ResourceOutcome::Accepted {
                operation_id: Some("operation-123".into()),
            }),
        }
    }

    fn completed() -> Self {
        Self {
            requests: Vec::new(),
            outcome: Ok(ResourceOutcome::Completed {
                status: 200,
                data: None,
            }),
        }
    }
}

impl ResourceOperations for Operations {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.requests.push(operation);
        Box::pin(std::future::ready(self.outcome.clone()))
    }
}

#[derive(Default)]
struct Files {
    reservations: usize,
    deny: bool,
    writer_fails: bool,
    bytes: Arc<Mutex<Vec<u8>>>,
}

struct Writer {
    bytes: Arc<Mutex<Vec<u8>>>,
    fails: bool,
}

impl Write for Writer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.fails {
            return Err(io::Error::other("synthetic write failure"));
        }
        self.bytes.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl DeleteOutputFiles for Files {
    fn reserve_new(&mut self, _: &Path) -> io::Result<Box<dyn Write + Send>> {
        self.reservations += 1;
        if self.deny {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        Ok(Box::new(Writer {
            bytes: self.bytes.clone(),
            fails: self.writer_fails,
        }))
    }
}

#[tokio::test]
async fn every_unsupported_requirement_stops_before_file_and_provider_writes() {
    let valid = command(&["-o", "negative.json"]);
    let mut cases = Vec::new();
    let mut candidate = valid.clone();
    candidate.permanent = false;
    cases.push(candidate);
    let mut candidate = valid.clone();
    candidate.recursive = true;
    cases.push(candidate);
    let mut candidate = valid.clone();
    candidate.if_match = Some("etag".into());
    cases.push(candidate);
    let mut candidate = valid.clone();
    candidate.output_format = Some("text".into());
    cases.push(candidate);
    let mut candidate = valid.clone();
    candidate.project = Some("[?".into());
    cases.push(candidate);
    let mut candidate = valid.clone();
    candidate.profile = Some("unselected".into());
    cases.push(candidate);
    let mut candidate = valid;
    candidate.path = "/".into();
    cases.push(candidate);
    for candidate in cases {
        let mut operations = Operations::completed();
        let mut files = Files::default();
        let preparation = preflight(&candidate, None);
        assert!(preparation.is_err());
        if let Ok(prepared) = preparation {
            let approval = DeleteApproval::approve(prepared.assessment());
            let output = prepare_output(&prepared, &mut files).unwrap();
            let _ = dispatch(&mut operations, &prepared, Some(&approval), output).await;
        }
        assert_eq!(files.reservations, 0);
        assert!(operations.requests.is_empty());
    }
}

#[tokio::test]
async fn irreversible_opt_in_does_not_replace_exact_injected_approval() {
    let prepared = preflight(&command(&[]), None).unwrap();
    let mut operations = Operations::completed();
    let mut files = Files::default();
    let output = prepare_output(&prepared, &mut files).unwrap();
    let result = dispatch(&mut operations, &prepared, None, output)
        .await
        .unwrap();
    assert!(result.result.is_err());
    assert_ne!(result.exit_code(), 0);
    assert!(operations.requests.is_empty());
    assert_eq!(
        files.reservations, 0,
        "stdout performs no local reservation"
    );
}

#[tokio::test]
async fn approval_before_context_binding_cannot_authorize_bound_execution() {
    let prepared = preflight(&command(&[]), None).unwrap();
    let approval = DeleteApproval::approve(prepared.assessment());
    let selection = fabric_sdk::CredentialSelection::new(
        "caller@example.invalid",
        "11111111-1111-1111-1111-111111111111",
        fabric_sdk::Audience::Fabric,
    )
    .unwrap();
    let endpoint = fabric_sdk::EndpointBinding::public(fabric_sdk::ServiceOrigin::Fabric);
    let prepared = prepared.with_selection(&selection, &endpoint).unwrap();
    let output = prepare_output(&prepared, &mut Files::default()).unwrap();
    let mut operations = Operations::completed();
    let result = dispatch(&mut operations, &prepared, Some(&approval), output)
        .await
        .unwrap();
    assert_eq!(
        result.result.as_ref().unwrap_err().code(),
        "ApprovalMismatch"
    );
    assert!(operations.requests.is_empty());
    let envelope: Value = serde_json::from_slice(&render(&result).unwrap()).unwrap();
    assert_eq!(envelope["status"], "failed");
    assert!(envelope["data"].is_null());
    assert!(envelope["meta"]["context"].is_object());
}

#[tokio::test]
async fn approval_for_another_exact_assessment_cannot_authorize_this_delete() {
    let first = preflight(&command(&[]), None).unwrap();
    let approval = DeleteApproval::approve(first.assessment());
    let mut changed = command(&[]);
    changed.path = TARGET.replace(
        "22222222-2222-2222-2222-222222222222",
        "33333333-3333-3333-3333-333333333333",
    );
    let second = preflight(&changed, None).unwrap();
    let mut operations = Operations::completed();
    let output = prepare_output(&second, &mut Files::default()).unwrap();
    let result = dispatch(&mut operations, &second, Some(&approval), output)
        .await
        .unwrap();
    assert!(result.result.is_err());
    assert!(operations.requests.is_empty());
}

#[tokio::test]
async fn accepted_receipt_is_not_claimed_as_completion_and_has_a_handle() {
    let prepared = preflight(&command(&[]), None).unwrap();
    let approval = DeleteApproval::approve(prepared.assessment());
    let mut operations = Operations::accepted();
    let output = prepare_output(&prepared, &mut Files::default()).unwrap();
    let result = dispatch(&mut operations, &prepared, Some(&approval), output)
        .await
        .unwrap();
    assert_eq!(result.exit_code(), 0);
    let snapshot: Value = serde_json::from_slice(&render(&result).unwrap()).unwrap();
    assert_eq!(snapshot["schemaVersion"], "0.1");
    assert_eq!(snapshot["status"], "accepted");
    assert!(snapshot["data"]["state"].is_null());
    assert!(snapshot["data"]["changes"].is_null());
    assert!(snapshot["meta"]["coverage"]["complete"].is_null());
    assert!(snapshot["meta"].to_string().contains("operation-123"));
    assert!(snapshot["meta"]["assessment"].is_object());
    assert!(snapshot["meta"]["receipt"].is_object());
    assert_eq!(
        snapshot["meta"]["effects"]["declared"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(snapshot["meta"]["effects"]["observed"].is_null());
    assert!(snapshot["error"].is_null());
    assert_eq!(operations.requests.len(), 1);
    assert_eq!(operations.requests[0].method(), ResourceMethod::Delete);
    assert_eq!(
        operations.requests[0].path(),
        "/v1/workspaces/11111111-1111-1111-1111-111111111111/notebooks/22222222-2222-2222-2222-222222222222?hardDelete=true"
    );
    assert_eq!(
        operations.requests[0].completion(),
        CompletionPolicy::ReturnAccepted
    );
    assert!(operations.requests[0].body().is_none());
}

#[tokio::test]
async fn completed_delete_retains_provider_evidence_without_inventing_resource_state() {
    let prepared = preflight(&command(&["--wait=true"]), None).unwrap();
    let approval = DeleteApproval::approve(prepared.assessment());
    let mut operations = Operations::completed();
    let output = prepare_output(&prepared, &mut Files::default()).unwrap();
    let result = dispatch(&mut operations, &prepared, Some(&approval), output)
        .await
        .unwrap();
    assert_eq!(result.exit_code(), 0);
    let snapshot: Value = serde_json::from_slice(&render(&result).unwrap()).unwrap();
    assert_eq!(snapshot["status"], "succeeded");
    assert!(snapshot["data"]["state"].is_null());
    assert_eq!(snapshot["meta"]["evidence"]["http_status"], 200);
    assert_eq!(snapshot["meta"]["evidence"]["status"], "succeeded");
    assert!(snapshot["meta"]["operation"].is_object());
    assert_eq!(operations.requests.len(), 1);
    assert_eq!(
        operations.requests[0].completion(),
        CompletionPolicy::WaitForCompletion
    );
}

#[tokio::test]
async fn scalar_projection_cannot_remove_status_metadata_or_errors() {
    let prepared = preflight(&command(&["-q", "resource.id"]), None).unwrap();
    let approval = DeleteApproval::approve(prepared.assessment());
    let mut operations = Operations::accepted();
    let output = prepare_output(&prepared, &mut Files::default()).unwrap();
    let result = dispatch(&mut operations, &prepared, Some(&approval), output)
        .await
        .unwrap();
    let snapshot: Value = serde_json::from_slice(&render(&result).unwrap()).unwrap();
    assert_eq!(snapshot["data"], "22222222-2222-2222-2222-222222222222");
    assert_eq!(snapshot["status"], "accepted");
    assert!(snapshot["meta"]["assessment"].is_object());
    assert!(snapshot["meta"].to_string().contains("operation-123"));
    assert!(snapshot.as_object().unwrap().contains_key("error"));
}

#[tokio::test]
async fn uncertain_failure_keeps_typed_error_but_only_safe_json_evidence() {
    let prepared = preflight(&command(&[]), None).unwrap();
    let approval = DeleteApproval::approve(prepared.assessment());
    let original = ResourceError {
        code: "TimedOut".into(),
        message: "Delete reconciliation timed out".into(),
        status: Some(202),
        service_error: Some(Box::new(
            serde_json::from_value(json!({
                "errorCode": "ProviderEvidence", "message": "synthetic", "requestId": "request-1"
            }))
            .unwrap(),
        )),
        raw_service_error: Some(Box::new(json!({"future": {"field": 7}}))),
        operation_id: Some("operation-kept".into()),
        mutation_may_have_occurred: true,
    };
    let mut operations = Operations {
        requests: Vec::new(),
        outcome: Err(original.clone()),
    };
    let output = prepare_output(&prepared, &mut Files::default()).unwrap();
    let result = dispatch(&mut operations, &prepared, Some(&approval), output)
        .await
        .unwrap();
    assert!(result.result.is_err());
    assert_eq!(
        result.result.as_ref().unwrap_err().resource_error(),
        Some(&original)
    );
    assert_eq!(result.exit_code(), 1);
    let snapshot: Value = serde_json::from_slice(&render(&result).unwrap()).unwrap();
    assert_eq!(snapshot["status"], "unknown");
    assert_eq!(snapshot["meta"]["resourceError"]["status"], 202);
    assert_eq!(
        snapshot["meta"]["resourceError"]["operationId"],
        "operation-kept"
    );
    assert!(
        snapshot["meta"]["resourceError"]
            .get("rawServiceError")
            .is_none()
    );
    assert!(
        snapshot["meta"]["resourceError"]
            .get("serviceError")
            .is_none()
    );
    let rendered = snapshot.to_string();
    for private in ["ProviderEvidence", "synthetic", "request-1", "future"] {
        assert!(!rendered.contains(private));
    }
    assert_eq!(
        snapshot["meta"]["resourceError"]["mutationMayHaveOccurred"],
        true
    );
    assert!(snapshot["meta"]["operation"].is_object());
    assert!(snapshot["data"].is_null());
    assert_eq!(operations.requests.len(), 1);
}

#[tokio::test]
async fn known_failure_has_null_data_and_finite_diagnostics_even_with_projection() {
    let prepared = preflight(&command(&["--project", "resource.id"]), None).unwrap();
    let approval = DeleteApproval::approve(prepared.assessment());
    let mut error = ResourceError::input();
    error.status = Some(403);
    error.code = "private-provider-code".into();
    error.message = "private-provider-message".into();
    let mut operations = Operations {
        requests: Vec::new(),
        outcome: Err(error),
    };
    let output = prepare_output(&prepared, &mut Files::default()).unwrap();
    let result = dispatch(&mut operations, &prepared, Some(&approval), output)
        .await
        .unwrap();
    let snapshot: Value = serde_json::from_slice(&render(&result).unwrap()).unwrap();
    assert_eq!(snapshot["status"], "failed");
    assert!(snapshot["data"].is_null());
    assert_eq!(snapshot["error"]["code"], "AccessDenied");
    assert!(!snapshot.to_string().contains("private-provider"));
}

#[test]
fn preexisting_output_blocks_reservation_before_any_provider_write() {
    let prepared = preflight(&command(&["-o", "existing.json"]), None).unwrap();
    let operations = Operations::accepted();
    let mut files = Files {
        deny: true,
        ..Files::default()
    };
    assert!(prepare_output(&prepared, &mut files).is_err());
    assert!(operations.requests.is_empty());
    assert_eq!(files.reservations, 1);
}

#[tokio::test]
async fn output_failure_after_acceptance_retains_dispatch_without_replay() {
    let prepared = preflight(&command(&["-o", "receipt.json"]), None).unwrap();
    let approval = DeleteApproval::approve(prepared.assessment());
    let mut files = Files {
        writer_fails: true,
        ..Files::default()
    };
    let output = prepare_output(&prepared, &mut files).unwrap();
    assert_eq!(files.reservations, 1);
    let mut operations = Operations::accepted();
    let result = dispatch(&mut operations, &prepared, Some(&approval), output)
        .await
        .unwrap();
    let failure = write_dispatch(result, &mut Vec::new()).unwrap_err();
    assert_eq!(failure.exit_code(), 1);
    assert!(failure.dispatch.result.is_ok());
    assert_eq!(failure.dispatch.envelope().status, "accepted");
    let snapshot = serde_json::to_value(failure.envelope()).unwrap();
    assert_eq!(snapshot["error"]["code"], "OutputFailed");
    assert_eq!(snapshot["status"], "failed");
    assert!(snapshot["data"].is_null());
    assert_eq!(snapshot["meta"]["dispatch"]["status"], "accepted");
    assert!(
        snapshot["meta"]["dispatch"]["meta"]
            .to_string()
            .contains("operation-123")
    );
    assert_eq!(operations.requests.len(), 1);
}

#[tokio::test]
async fn successful_local_output_uses_only_the_reserved_handle() {
    let prepared = preflight(&command(&["-o", "receipt.json"]), None).unwrap();
    let approval = DeleteApproval::approve(prepared.assessment());
    let mut files = Files::default();
    let output = prepare_output(&prepared, &mut files).unwrap();
    let mut operations = Operations::accepted();
    let result = dispatch(&mut operations, &prepared, Some(&approval), output)
        .await
        .unwrap();
    let mut stdout = Vec::new();
    let result = write_dispatch(result, &mut stdout).unwrap();
    assert!(stdout.is_empty());
    let bytes = files.bytes.lock().unwrap();
    assert_eq!(bytes.last(), Some(&b'\n'));
    let snapshot: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(snapshot["status"], "accepted");
    assert_eq!(files.reservations, 1);
    drop(bytes);
    let failure = write_dispatch(result, &mut stdout).unwrap_err();
    assert_eq!(failure.error.code(), "OutputFailed");
    assert_eq!(operations.requests.len(), 1);
}

#[tokio::test]
async fn mismatched_output_reservation_cannot_dispatch() {
    let first = preflight(&command(&[]), None).unwrap();
    let output = prepare_output(&first, &mut Files::default()).unwrap();
    let second = preflight(&command(&["--wait"]), None).unwrap();
    let approval = DeleteApproval::approve(second.assessment());
    let mut operations = Operations::accepted();
    let error = dispatch(&mut operations, &second, Some(&approval), output)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "InvalidInput");
    assert!(operations.requests.is_empty());
}

#[tokio::test]
async fn stdout_flush_failure_retains_the_original_receipt() {
    struct FlushFails;
    impl Write for FlushFails {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("synthetic flush failure"))
        }
    }
    let prepared = preflight(&command(&[]), None).unwrap();
    let approval = DeleteApproval::approve(prepared.assessment());
    let output = prepare_output(&prepared, &mut Files::default()).unwrap();
    let mut operations = Operations::accepted();
    let result = dispatch(&mut operations, &prepared, Some(&approval), output)
        .await
        .unwrap();
    let failure = write_dispatch(result, &mut FlushFails).unwrap_err();
    assert!(failure.dispatch.result.is_ok());
    assert_eq!(operations.requests.len(), 1);
}

#[test]
fn malformed_projection_and_unsafe_outputs_fail_before_reservation() {
    let mut command = command(&[]);
    command.project = Some("[?".into());
    assert!(preflight(&command, None).is_err());
    command.project = None;
    for path in [
        "",
        "*.json",
        "../result.json",
        "https://example.test/receipt",
        "NUL",
        "file:stream",
        "folder/",
    ] {
        command.output = path.into();
        assert!(preflight(&command, None).is_err(), "{path}");
    }
}

#[test]
fn native_reservation_is_create_new_and_keeps_original_existing_bytes() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let base = std::env::current_dir().unwrap();
    let name = format!(
        "delete-contract-{}-{}.json",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let selected = base.join(&name);
    let mut native = NativeDeleteFiles::new(base).unwrap();
    let mut first = native.reserve_new(Path::new(&name)).unwrap();
    first.write_all(b"original").unwrap();
    first.flush().unwrap();
    assert!(native.reserve_new(Path::new(&name)).is_err());
    drop(first);
    assert_eq!(std::fs::read(&selected).unwrap(), b"original");
    std::fs::remove_file(selected).unwrap();
}
