// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline bounded mirroring-stop contracts through the public SDK.

#![deny(unnameable_test_items)]

use fabric_sdk_items::stop::{
    MirroringState, MirroringTarget, StopError, StopOptions, StopOutcome, stop,
};
use fabric_sdk_workspaces::{
    FabricServiceError,
    mutations::{
        CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
        ResourceOutcome,
    },
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    error::Error,
    future::Future,
    pin::Pin,
    task::{Context, Waker},
};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "aAbBcCdD-eEfF-0123-4567-89aBcDeF0123";
const STATES: [MirroringState; 7] = [
    MirroringState::Initializing,
    MirroringState::Initialized,
    MirroringState::Starting,
    MirroringState::Running,
    MirroringState::Paused,
    MirroringState::Stopping,
    MirroringState::Stopped,
];

fn target() -> MirroringTarget {
    MirroringTarget::new(WORKSPACE, ITEM).unwrap()
}

fn approved() -> StopOptions {
    StopOptions { approved: true }
}

fn completed(data: Option<Value>) -> ResourceOutcome {
    ResourceOutcome::Completed { status: 200, data }
}

fn status(state: &str) -> ResourceOutcome {
    completed(Some(json!({"status": state})))
}

fn native_error(code: &str, http_status: Option<u16>, mutation: bool) -> ResourceError {
    let service_error: FabricServiceError = serde_json::from_value(json!({
        "errorCode": "private-provider-code",
        "message": "private-provider-message",
        "requestId": "private-request-id",
        "private-key": {"details": "private-service-details"}
    }))
    .unwrap();
    ResourceError {
        code: code.into(),
        message: "private-executor-message".into(),
        status: http_status,
        service_error: Some(Box::new(service_error)),
        raw_service_error: Some(Box::new(json!({
            "error": {"private-key": ["private-raw-details"]}
        }))),
        operation_id: Some("private-operation-id".into()),
        mutation_may_have_occurred: mutation,
    }
}

enum Reply {
    Ready(Result<ResourceOutcome, ResourceError>),
    Pending,
}

struct Capture {
    calls: Vec<ResourceOperation>,
    replies: VecDeque<Reply>,
}

impl Capture {
    fn new(replies: impl IntoIterator<Item = Result<ResourceOutcome, ResourceError>>) -> Self {
        Self {
            calls: Vec::new(),
            replies: replies.into_iter().map(Reply::Ready).collect(),
        }
    }

    fn responses(replies: impl IntoIterator<Item = ResourceOutcome>) -> Self {
        Self::new(replies.into_iter().map(Ok))
    }

    fn pending_after(replies: impl IntoIterator<Item = ResourceOutcome>) -> Self {
        let mut capture = Self::responses(replies);
        capture.replies.push_back(Reply::Pending);
        capture
    }
}

impl ResourceOperations for Capture {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.calls.push(operation);
        match self.replies.pop_front().expect("unexpected executor call") {
            Reply::Ready(reply) => Box::pin(std::future::ready(reply)),
            Reply::Pending => Box::pin(std::future::pending()),
        }
    }
}

fn assert_calls(capture: &Capture, expected_actions: &[&str]) {
    assert_eq!(capture.calls.len(), expected_actions.len());
    assert!(
        capture.replies.is_empty(),
        "expected responses not consumed"
    );
    for (operation, action) in capture.calls.iter().zip(expected_actions) {
        assert_eq!(operation.method(), ResourceMethod::Post);
        assert_eq!(
            operation.path(),
            format!("/v1/workspaces/{WORKSPACE}/mirroredDatabases/{ITEM}/{action}")
        );
        assert_eq!(operation.body(), None);
        assert_eq!(operation.completion(), CompletionPolicy::ReturnAccepted);
    }
    assert!(
        capture
            .calls
            .iter()
            .filter(|operation| operation.path().ends_with("/stopMirroring"))
            .count()
            <= 1
    );
}

fn assert_sanitized(error: &StopError) {
    assert_eq!(error.to_string(), error.message());
    let diagnostic = format!("{error}\n{error:?}");
    assert!(!diagnostic.contains("private-"));
    assert!(!diagnostic.contains("bearer"));
}

fn assert_no_submission(error: &StopError) {
    assert!(!error.mutation_may_have_occurred);
    assert!(!error.mutation_may_have_occurred());
    assert!(error.receipt().is_none());
    assert_sanitized(error);
}

fn assert_retained_submission(error: &StopError, submitted: &ResourceOutcome) {
    assert!(error.mutation_may_have_occurred);
    assert!(error.mutation_may_have_occurred());
    let receipt = error.receipt().unwrap();
    assert_eq!(receipt.target, target());
    assert_eq!(receipt.outcome, StopOutcome::RequestCompleted);
    assert_eq!(receipt.observed_state, Some(MirroringState::Running));
    assert_eq!(receipt.request_outcome.as_ref(), Some(submitted));
    assert!(receipt.mutation_may_have_occurred);
    assert!(!receipt.stopped_confirmed());
    assert!(!format!("{receipt:?}").contains("private-"));
    assert_sanitized(error);
}

fn malformed_statuses() -> Vec<ResourceOutcome> {
    let mut responses = vec![
        completed(None),
        completed(Some(Value::Null)),
        completed(Some(json!({}))),
        completed(Some(json!({"status": null}))),
        completed(Some(json!({"status": 1}))),
        completed(Some(json!({"status": true}))),
        completed(Some(json!({"status": []}))),
        completed(Some(json!({"status": {"status": "Stopped"}}))),
        completed(Some(json!({"Status": "Stopped"}))),
        completed(Some(json!({"error": {"status": "Stopped"}}))),
        completed(Some(json!(["Stopped"]))),
        completed(Some(json!("Stopped"))),
        completed(Some(json!(false))),
    ];
    for invalid in [
        "",
        "Run",
        "Stop",
        "running",
        "stopped",
        "Stopping ",
        " Stopped",
        "Running\n",
        "RunningStopped",
        "Failed",
        "Error",
        "Cancelled",
        "private-future-state",
    ] {
        responses.push(status(invalid));
    }
    responses
}

fn unexpected_status_responses() -> Vec<ResourceOutcome> {
    let mut responses: Vec<_> = [0, 201, 202, 204, 400, 503]
        .into_iter()
        .map(|http_status| ResourceOutcome::Completed {
            status: http_status,
            data: Some(json!({"status": "Stopped", "private-key": "private-details"})),
        })
        .collect();
    responses.extend([
        ResourceOutcome::Accepted { operation_id: None },
        ResourceOutcome::Accepted {
            operation_id: Some("private-unexpected-read-operation".into()),
        },
    ]);
    responses
}

#[test]
fn target_validates_locally_and_preserves_exact_guid_spelling() {
    let original = target();
    assert_eq!(original.workspace_id(), WORKSPACE);
    assert_eq!(original.item_id(), ITEM);
    let workspace = "AaBbCcDd-EeFf-0011-2233-445566778899";
    let upper_item = ITEM.to_ascii_uppercase();
    let mixed = MirroringTarget::new(workspace, &upper_item).unwrap();
    assert_eq!(mixed.workspace_id(), workspace);
    assert_eq!(mixed.item_id(), upper_item);
    let nil = "00000000-0000-0000-0000-000000000000";
    assert_eq!(MirroringTarget::new(nil, nil).unwrap().item_id(), nil);
    assert!(!StopOptions::default().approved);
}

#[test]
fn invalid_targets_fail_before_any_executor_work_without_echoing_input() {
    let capture = Capture::responses([]);
    for invalid in [
        "",
        "private-name",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeee",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeeee",
        "aaaaaaaa_bbbb-cccc-dddd-eeeeeeeeeeee",
        "gaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeeé",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee/",
        "{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}",
        " aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee ",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n",
        "https://private-host/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        "../aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee?x=1",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\\runtime",
        "*",
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeee%2e",
        "[aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee]",
    ] {
        for result in [
            MirroringTarget::new(invalid, ITEM),
            MirroringTarget::new(WORKSPACE, invalid),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.code(), "InvalidInput");
            assert_eq!(
                error.message(),
                "Mirroring stop requires exact workspace and item GUIDs"
            );
            assert_no_submission(&error);
            assert!(error.resource_error().is_none());
            assert!(error.source().is_none());
            assert!(error.response_outcome().is_none());
        }
    }
    assert_calls(&capture, &[]);
}

#[test]
fn state_and_outcome_spellings_are_explicit_and_stable() {
    for (state, spelling) in STATES.into_iter().zip([
        "Initializing",
        "Initialized",
        "Starting",
        "Running",
        "Paused",
        "Stopping",
        "Stopped",
    ]) {
        assert_eq!(state.as_str(), spelling);
        assert_eq!(serde_json::to_value(state).unwrap(), json!(spelling));
    }
    for (outcome, spelling) in [
        (StopOutcome::AlreadyStopped, "AlreadyStopped"),
        (StopOutcome::AlreadyStopping, "AlreadyStopping"),
        (StopOutcome::Accepted, "Accepted"),
        (StopOutcome::RequestCompleted, "RequestCompleted"),
        (StopOutcome::Stopped, "Stopped"),
    ] {
        assert_eq!(outcome.as_str(), spelling);
        assert_eq!(serde_json::to_value(outcome).unwrap(), json!(spelling));
    }
}

#[tokio::test]
async fn already_stopped_is_a_confirmed_no_write_even_without_approval() {
    for approved in [false, true] {
        let mut capture = Capture::responses([status("Stopped")]);
        let receipt = stop(&mut capture, &target(), StopOptions { approved })
            .await
            .unwrap();
        assert_eq!(receipt.target, target());
        assert_eq!(receipt.outcome, StopOutcome::AlreadyStopped);
        assert_eq!(receipt.observed_state, Some(MirroringState::Stopped));
        assert!(receipt.stopped_confirmed());
        assert!(!receipt.mutation_may_have_occurred);
        assert!(receipt.request_outcome.is_none());
        assert_calls(&capture, &["getMirroringStatus"]);
    }
}

#[tokio::test]
async fn already_stopping_is_pending_without_a_write_or_new_operation_identity() {
    for approved in [false, true] {
        let mut capture = Capture::responses([status("Stopping")]);
        let receipt = stop(&mut capture, &target(), StopOptions { approved })
            .await
            .unwrap();
        assert_eq!(receipt.target, target());
        assert_eq!(receipt.outcome, StopOutcome::AlreadyStopping);
        assert_eq!(receipt.observed_state, Some(MirroringState::Stopping));
        assert!(!receipt.stopped_confirmed());
        assert!(!receipt.mutation_may_have_occurred);
        assert!(receipt.request_outcome.is_none());
        assert_calls(&capture, &["getMirroringStatus"]);
    }
}

#[tokio::test]
async fn running_without_explicit_approval_cannot_submit() {
    let preflight = status("Running");
    let mut capture = Capture::responses([preflight.clone()]);
    let error = stop(&mut capture, &target(), StopOptions::default())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "ApprovalRequired");
    assert_no_submission(&error);
    assert!(error.resource_error().is_none());
    assert_eq!(error.response_outcome(), Some(&preflight));
    assert_calls(&capture, &["getMirroringStatus"]);
}

#[tokio::test]
async fn other_known_preflight_states_cannot_authorize_a_stop_even_when_approved() {
    for state in [
        MirroringState::Initializing,
        MirroringState::Initialized,
        MirroringState::Starting,
        MirroringState::Paused,
    ] {
        for approved in [false, true] {
            let preflight = status(state.as_str());
            let mut capture = Capture::responses([preflight.clone()]);
            let error = stop(&mut capture, &target(), StopOptions { approved })
                .await
                .unwrap_err();
            assert_eq!(error.code(), "InvalidState");
            assert_no_submission(&error);
            assert!(error.resource_error().is_none());
            assert_eq!(error.response_outcome(), Some(&preflight));
            assert_calls(&capture, &["getMirroringStatus"]);
        }
    }
}

#[tokio::test]
async fn malformed_and_substring_preflight_states_never_authorize_a_write() {
    for preflight in malformed_statuses() {
        let mut capture = Capture::responses([preflight.clone()]);
        let error = stop(&mut capture, &target(), approved()).await.unwrap_err();
        assert_eq!(error.code(), "InvalidResponse");
        assert_no_submission(&error);
        assert!(error.resource_error().is_none());
        assert_eq!(error.response_outcome(), Some(&preflight));
        assert_calls(&capture, &["getMirroringStatus"]);
    }
}

#[tokio::test]
async fn preflight_requires_completed_200_and_never_follows_accepted_read_operations() {
    for preflight in unexpected_status_responses() {
        let mut capture = Capture::responses([preflight.clone()]);
        let error = stop(&mut capture, &target(), approved()).await.unwrap_err();
        assert_eq!(error.code(), "InvalidResponse");
        assert_no_submission(&error);
        assert!(error.resource_error().is_none());
        assert_eq!(error.response_outcome(), Some(&preflight));
        assert_calls(&capture, &["getMirroringStatus"]);
    }
}

#[tokio::test]
async fn preflight_native_failures_preserve_source_but_do_not_claim_stop_dispatch() {
    for (code, http_status) in [
        ("private-service-failure", Some(403)),
        ("private-execution-failure", None),
        ("Cancelled", None),
        ("Timeout", None),
    ] {
        for mutation in [false, true] {
            let source = native_error(code, http_status, mutation);
            let mut capture = Capture::new([Err(source.clone())]);
            let error = stop(&mut capture, &target(), approved()).await.unwrap_err();
            assert_eq!(error.code(), "PreconditionFailed");
            assert_no_submission(&error);
            assert_eq!(error.resource_error(), Some(&source));
            assert_eq!(
                error.source().unwrap().downcast_ref::<ResourceError>(),
                Some(&source)
            );
            assert!(error.response_outcome().is_none());
            assert_calls(&capture, &["getMirroringStatus"]);
        }
    }
}

#[tokio::test]
async fn accepted_stop_retains_actual_optional_id_without_a_readback_or_monitor() {
    for operation_id in [None, Some("private-actual-operation-id".to_owned())] {
        let accepted = ResourceOutcome::Accepted { operation_id };
        let mut capture = Capture::responses([status("Running"), accepted.clone()]);
        let receipt = stop(&mut capture, &target(), approved()).await.unwrap();
        assert_eq!(receipt.target, target());
        assert_eq!(receipt.outcome, StopOutcome::Accepted);
        assert_eq!(receipt.observed_state, Some(MirroringState::Running));
        assert_eq!(receipt.request_outcome, Some(accepted));
        assert!(receipt.mutation_may_have_occurred);
        assert!(!receipt.stopped_confirmed());
        assert!(!format!("{receipt:?}").contains("private-"));
        assert_calls(&capture, &["getMirroringStatus", "stopMirroring"]);
    }
}

#[tokio::test]
async fn empty_completed_200_stop_is_confirmed_only_by_one_exact_stopped_readback() {
    let submitted = completed(None);
    let mut capture = Capture::responses([status("Running"), submitted.clone(), status("Stopped")]);
    let receipt = stop(&mut capture, &target(), approved()).await.unwrap();
    assert_eq!(receipt.target, target());
    assert_eq!(receipt.outcome, StopOutcome::Stopped);
    assert_eq!(receipt.observed_state, Some(MirroringState::Stopped));
    assert_eq!(receipt.request_outcome, Some(submitted));
    assert!(receipt.stopped_confirmed());
    assert!(receipt.mutation_may_have_occurred);
    assert_calls(
        &capture,
        &["getMirroringStatus", "stopMirroring", "getMirroringStatus"],
    );
}

#[tokio::test]
async fn completed_request_body_is_native_evidence_not_a_terminal_state_observation() {
    for data in [
        None,
        Some(Value::Null),
        Some(json!({"status": "Stopped", "private-key": "private-stop-data"})),
    ] {
        let submitted = completed(data);
        let mut capture =
            Capture::responses([status("Running"), submitted.clone(), status("Running")]);
        let receipt = stop(&mut capture, &target(), approved()).await.unwrap();
        assert_eq!(receipt.outcome, StopOutcome::RequestCompleted);
        assert_eq!(receipt.observed_state, Some(MirroringState::Running));
        assert_eq!(receipt.request_outcome, Some(submitted));
        assert!(receipt.mutation_may_have_occurred);
        assert!(!receipt.stopped_confirmed());
        assert!(!format!("{receipt:?}").contains("private-"));
        assert_calls(
            &capture,
            &["getMirroringStatus", "stopMirroring", "getMirroringStatus"],
        );
    }
}

#[tokio::test]
async fn every_known_readback_state_is_an_observed_fact_but_only_stopped_confirms() {
    for observed in STATES {
        let mut capture = Capture::responses([
            status("Running"),
            completed(None),
            status(observed.as_str()),
        ]);
        let receipt = stop(&mut capture, &target(), approved()).await.unwrap();
        let stopped = observed == MirroringState::Stopped;
        assert_eq!(
            receipt.outcome,
            if stopped {
                StopOutcome::Stopped
            } else {
                StopOutcome::RequestCompleted
            }
        );
        assert_eq!(receipt.observed_state, Some(observed));
        assert_eq!(receipt.stopped_confirmed(), stopped);
        assert_eq!(receipt.request_outcome, Some(completed(None)));
        assert!(receipt.mutation_may_have_occurred);
        assert_calls(
            &capture,
            &["getMirroringStatus", "stopMirroring", "getMirroringStatus"],
        );
    }
}

#[tokio::test]
async fn malformed_readback_is_an_error_retaining_the_completed_stop_and_uncertainty() {
    let submitted = completed(Some(json!({"private-key": "private-stop-result"})));
    for readback in malformed_statuses() {
        let mut capture =
            Capture::responses([status("Running"), submitted.clone(), readback.clone()]);
        let error = stop(&mut capture, &target(), approved()).await.unwrap_err();
        assert_eq!(error.code(), "InvalidResponse");
        assert_retained_submission(&error, &submitted);
        assert!(error.resource_error().is_none());
        assert!(error.source().is_none());
        assert_eq!(error.response_outcome(), Some(&readback));
        assert_calls(
            &capture,
            &["getMirroringStatus", "stopMirroring", "getMirroringStatus"],
        );
    }
}

#[tokio::test]
async fn unexpected_readback_status_or_acceptance_retains_stop_without_following_or_retrying() {
    let submitted = completed(None);
    for readback in unexpected_status_responses() {
        let mut capture =
            Capture::responses([status("Running"), submitted.clone(), readback.clone()]);
        let error = stop(&mut capture, &target(), approved()).await.unwrap_err();
        assert_eq!(error.code(), "InvalidResponse");
        assert_retained_submission(&error, &submitted);
        assert!(error.resource_error().is_none());
        assert_eq!(error.response_outcome(), Some(&readback));
        assert_calls(
            &capture,
            &["getMirroringStatus", "stopMirroring", "getMirroringStatus"],
        );
    }
}

#[tokio::test]
async fn readback_native_failures_retain_successful_submission_and_unchanged_source() {
    let submitted = completed(Some(json!({"private-key": "private-submission-evidence"})));
    for (code, http_status) in [
        ("private-service-failure", Some(503)),
        ("private-execution-failure", None),
        ("Cancelled", None),
        ("Timeout", None),
    ] {
        for mutation in [false, true] {
            let source = native_error(code, http_status, mutation);
            let mut capture = Capture::new([
                Ok(status("Running")),
                Ok(submitted.clone()),
                Err(source.clone()),
            ]);
            let error = stop(&mut capture, &target(), approved()).await.unwrap_err();
            assert_eq!(error.code(), "ReadbackFailed");
            assert_retained_submission(&error, &submitted);
            assert_eq!(error.resource_error(), Some(&source));
            assert_eq!(
                error.source().unwrap().downcast_ref::<ResourceError>(),
                Some(&source)
            );
            assert!(error.response_outcome().is_none());
            assert_calls(
                &capture,
                &["getMirroringStatus", "stopMirroring", "getMirroringStatus"],
            );
        }
    }
}

#[tokio::test]
async fn failed_submission_preserves_native_dispatch_evidence_and_never_reads_back_or_replays() {
    for (code, http_status) in [
        ("private-service-failure", Some(400)),
        ("private-execution-failure", None),
        ("Cancelled", None),
        ("Timeout", None),
    ] {
        for mutation in [false, true] {
            let source = native_error(code, http_status, mutation);
            let mut capture = Capture::new([Ok(status("Running")), Err(source.clone())]);
            let error = stop(&mut capture, &target(), approved()).await.unwrap_err();
            assert_eq!(error.code(), "SubmissionFailed");
            assert_eq!(error.mutation_may_have_occurred, mutation);
            assert_eq!(error.mutation_may_have_occurred(), mutation);
            assert_eq!(error.resource_error(), Some(&source));
            assert_eq!(
                error.source().unwrap().downcast_ref::<ResourceError>(),
                Some(&source)
            );
            assert!(error.receipt().is_none());
            assert!(error.response_outcome().is_none());
            assert_sanitized(&error);
            assert_calls(&capture, &["getMirroringStatus", "stopMirroring"]);
        }
    }
}

#[tokio::test]
async fn unexpected_completed_stop_status_preserves_actual_response_without_readback() {
    for http_status in [0, 201, 202, 204, 400, 503] {
        let submitted = ResourceOutcome::Completed {
            status: http_status,
            data: Some(json!({"status": "Stopped", "private-key": "private-stop-data"})),
        };
        let mut capture = Capture::responses([status("Running"), submitted.clone()]);
        let error = stop(&mut capture, &target(), approved()).await.unwrap_err();
        assert_eq!(error.code(), "InvalidResponse");
        assert_retained_submission(&error, &submitted);
        assert_eq!(error.response_outcome(), Some(&submitted));
        assert!(error.resource_error().is_none());
        assert_calls(&capture, &["getMirroringStatus", "stopMirroring"]);
    }
}

#[tokio::test]
async fn valid_status_with_future_fields_remains_exact_and_diagnostics_do_not_echo_them() {
    let preflight = completed(Some(json!({
        "status": "Running",
        "private-key": {"details": "private-future-data"}
    })));
    let submitted = completed(None);
    let readback = completed(Some(json!({
        "status": "Stopped",
        "private-key": "private-future-data"
    })));
    let mut capture = Capture::responses([preflight, submitted, readback]);
    let receipt = stop(&mut capture, &target(), approved()).await.unwrap();
    assert_eq!(receipt.outcome, StopOutcome::Stopped);
    assert!(receipt.stopped_confirmed());
    assert!(!format!("{receipt:?}").contains("private-"));
    assert_calls(
        &capture,
        &["getMirroringStatus", "stopMirroring", "getMirroringStatus"],
    );
}

#[tokio::test]
async fn executor_receives_case_preserved_ids_without_lookup_or_normalization() {
    let workspace = "AaBbCcDd-EeFf-0011-2233-445566778899";
    let item = ITEM.to_ascii_uppercase();
    let target = MirroringTarget::new(workspace, &item).unwrap();
    let mut capture = Capture::responses([
        status("Running"),
        ResourceOutcome::Accepted { operation_id: None },
    ]);
    let receipt = stop(&mut capture, &target, approved()).await.unwrap();
    assert_eq!(receipt.target, target);
    assert_eq!(capture.calls.len(), 2);
    assert!(capture.replies.is_empty());
    for (operation, action) in capture
        .calls
        .iter()
        .zip(["getMirroringStatus", "stopMirroring"])
    {
        assert_eq!(
            operation.path(),
            format!("/v1/workspaces/{workspace}/mirroredDatabases/{item}/{action}")
        );
        assert_eq!(operation.method(), ResourceMethod::Post);
        assert_eq!(operation.body(), None);
        assert_eq!(operation.completion(), CompletionPolicy::ReturnAccepted);
    }
}

#[test]
fn dropping_at_each_await_boundary_cannot_start_cleanup_compensation_or_another_write() {
    for (ready, actions) in [
        (vec![], vec!["getMirroringStatus"]),
        (
            vec![status("Running")],
            vec!["getMirroringStatus", "stopMirroring"],
        ),
        (
            vec![status("Running"), completed(None)],
            vec!["getMirroringStatus", "stopMirroring", "getMirroringStatus"],
        ),
    ] {
        let target = target();
        let mut capture = Capture::pending_after(ready);
        {
            let mut pending = Box::pin(stop(&mut capture, &target, approved()));
            let mut context = Context::from_waker(Waker::noop());
            assert!(pending.as_mut().poll(&mut context).is_pending());
        }
        assert_calls(&capture, &actions);
    }
}

#[tokio::test]
async fn non_null_status_errors_never_authorize_a_write_or_successful_observation() {
    for state in ["Running", "Stopped", "Stopping"] {
        for error in [
            json!({"errorCode":"private-status-code","message":"private-status-message"}),
            json!({}),
            json!([]),
            json!(false),
            json!(0),
            json!("private-status-error"),
        ] {
            let response = completed(Some(json!({"status":state,"error":error})));
            let mut capture = Capture::responses([response.clone()]);
            let error = stop(&mut capture, &target(), approved()).await.unwrap_err();
            assert_eq!(error.code(), "InvalidResponse");
            assert_no_submission(&error);
            assert_eq!(error.response_outcome(), Some(&response));
            assert_calls(&capture, &["getMirroringStatus"]);
        }
    }
}

#[tokio::test]
async fn status_error_on_readback_preserves_completed_stop_without_confirmation() {
    for error in [
        json!({"errorCode":"private-status-code","message":"private-status-message"}),
        json!({}),
        json!([]),
        json!(false),
        json!(0),
        json!("private-status-error"),
    ] {
        let submitted = completed(None);
        let readback = completed(Some(json!({"status":"Stopped","error":error})));
        let mut capture =
            Capture::responses([status("Running"), submitted.clone(), readback.clone()]);
        let error = stop(&mut capture, &target(), approved()).await.unwrap_err();
        assert_eq!(error.code(), "InvalidResponse");
        assert_retained_submission(&error, &submitted);
        assert_eq!(error.response_outcome(), Some(&readback));
        assert_calls(
            &capture,
            &["getMirroringStatus", "stopMirroring", "getMirroringStatus"],
        );
    }
}

#[tokio::test]
async fn null_status_error_and_unrelated_future_fields_remain_valid() {
    for (state, expected) in [
        ("Stopped", StopOutcome::AlreadyStopped),
        ("Stopping", StopOutcome::AlreadyStopping),
    ] {
        let mut capture = Capture::responses([completed(Some(json!({
            "status":state,"error":null,"private-future":{"field":true}
        })))]);
        let receipt = stop(&mut capture, &target(), StopOptions::default())
            .await
            .unwrap();
        assert_eq!(receipt.outcome, expected);
        assert_calls(&capture, &["getMirroringStatus"]);
    }
    let mut capture = Capture::responses([
        completed(Some(json!({"status":"Running","error":null}))),
        completed(None),
        completed(Some(
            json!({"status":"Stopped","error":null,"future":[1,2]}),
        )),
    ]);
    assert!(
        stop(&mut capture, &target(), approved())
            .await
            .unwrap()
            .stopped_confirmed()
    );
    assert_calls(
        &capture,
        &["getMirroringStatus", "stopMirroring", "getMirroringStatus"],
    );
}
