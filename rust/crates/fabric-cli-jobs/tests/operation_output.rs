// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real job handler -> shared operation session -> shared borrowed CLI formatter.

#[path = "../../../tests/compatibility/jobs/support.rs"]
mod support;

use fabric_cli_context::{Catalog, Element, HomeContext, ItemType, NodeKind, WorkspaceType};
use fabric_cli_jobs::{
    Confirmation, HttpJobExecutor, JobCommand, JobCommandError, JobConfig, JobEvent, JobHost,
    JobUi, NoFiles, command_from_matches, job_subcommand,
};
use fabric_cli_output::{CommandError, Indent, Timestamp};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_jobs::{Audience, CredentialSelection, JobClient, JobLimits, SharedJobOperations};
use fabric_sdk_operations::{OperationErrorKind, OperationLimits};
use serde_json::{Value, json};
use std::{
    future::{pending, poll_fn},
    sync::{Arc, Mutex},
    task::Poll,
};
use support::*;

const OPERATION: &str = "11111111-1111-1111-1111-111111111111";

#[derive(Default)]
struct Ui(Mutex<Vec<JobEvent>>);

impl JobUi for Ui {
    fn emit(&self, event: JobEvent) -> Result<(), CommandError> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
    fn confirm(&self) -> Confirmation<'_> {
        Box::pin(async { Ok(false) })
    }
}

struct Harness {
    sdk: JobClient,
    operations: SharedJobOperations,
    http: Arc<ScriptedHttp>,
    catalog: Catalog,
    current: Arc<Element>,
    ui: Ui,
}

impl Harness {
    fn new(reply: azure_core::http::AsyncRawResponse, run_timeout: bool) -> Self {
        let mut replies = Vec::new();
        if run_timeout {
            replies.push(accepted());
        }
        replies.push(response(202, "", &[("x-ms-operation-id", OPERATION)]));
        replies.push(reply);
        let http = ScriptedHttp::new(replies);
        let timer = Arc::new(TestTimer::default());
        let executor = Arc::new(HttpJobExecutor::new(
            HttpExecutor::new(http.clone(), timer, ExecutionOptions::default()).unwrap(),
        ));
        let credential = Arc::new(TestCredential::default());
        let selection = CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap();
        let sdk = JobClient::new(
            credential.clone(),
            selection.clone(),
            executor.clone(),
            JobLimits::default(),
        );
        let operations = SharedJobOperations::from_job_executor(
            credential,
            selection,
            executor,
            OperationLimits::default(),
        );
        let workspace = Element::child(
            "demo",
            Some("ws".into()),
            NodeKind::Workspace(WorkspaceType::Workspace),
            Element::tenant("tenant", "tenant"),
        )
        .unwrap();
        let item = Element::child(
            "item",
            Some("item".into()),
            NodeKind::Item(ItemType::parse("DataPipeline").unwrap()),
            workspace.clone(),
        )
        .unwrap();
        Self {
            sdk,
            operations,
            http,
            catalog: Catalog::new(vec![workspace.clone(), item]),
            current: workspace,
            ui: Ui::default(),
        }
    }
    fn host(&self) -> JobHost<'_> {
        JobHost {
            sdk: &self.sdk,
            operations: Some(&self.operations),
            lookup: &self.catalog,
            current: self.current.clone(),
            home: HomeContext::Unspecified,
            files: &NoFiles,
            ui: &self.ui,
            config: JobConfig::default(),
            timestamp: timestamp(),
        }
    }
}

fn timestamp() -> Timestamp {
    Timestamp::parse("2000-01-01T00:00:00Z").unwrap()
}
fn command(timeout: bool) -> JobCommand {
    let args = if timeout {
        vec!["job", "run", "item.DataPipeline", "--timeout", "3"]
    } else {
        vec![
            "job",
            "run-cancel",
            "item.DataPipeline",
            "--id",
            "run",
            "--wait",
        ]
    };
    command_from_matches(&job_subcommand().try_get_matches_from(args).unwrap()).unwrap()
}
fn cases() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/compatibility/jobs/operation_output.json"
    ))
    .unwrap()
}

fn assert_request_path(h: &Harness, timeout: bool) {
    let requests = h.http.requests.lock().unwrap();
    assert_eq!(requests.len(), if timeout { 3 } else { 2 });
    let cancel = usize::from(timeout);
    assert_eq!(requests[cancel].method(), azure_core::http::Method::Post);
    assert!(
        requests[cancel]
            .url()
            .path()
            .ends_with("/jobs/instances/run/cancel")
    );
    assert_eq!(requests[cancel + 1].method(), azure_core::http::Method::Get);
    assert_eq!(
        requests[cancel + 1].url().path(),
        format!("/v1/operations/{OPERATION}")
    );
}

#[tokio::test]
async fn failed_cancelled_raw_values_project_through_shared_formatter_without_losing_evidence() {
    for state in ["Failed", "Cancelled"] {
        for case in cases()["terminal"].as_array().unwrap() {
            for timeout in [false, true] {
                let body = json!({"status":state,"error":case["detail"]}).to_string();
                let h = Harness::new(response(200, &body, &[]), timeout);
                let cmd = command(timeout);
                let failure = h.host().execute(&cmd, pending()).await.unwrap_err();
                let JobCommandError::Operation(ref error) = failure else {
                    panic!("operation evidence was erased");
                };
                let original = error.as_ref().clone();
                assert_eq!(error.raw_service_error.as_deref(), Some(&case["detail"]));
                assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
                assert!(error.mutation_may_have_occurred);
                assert!(
                    matches!(&error.kind,OperationErrorKind::OperationFailed{status,..} if status==state)
                );
                let envelope = failure.envelope(timestamp(), Some(cmd.command_path()));
                let plan = envelope.render(Indent::Four).unwrap();
                assert_eq!(plan.exit_code, 1);
                assert!(plan.payload.is_ascii());
                let output: Value = serde_json::from_slice(&plan.payload).unwrap();
                let prefix = if state == "Cancelled" {
                    "The operation was cancelled: "
                } else {
                    "The operation failed: "
                };
                assert_eq!(
                    output["result"]["message"],
                    format!("{prefix}{}", case["repr"].as_str().unwrap()),
                    "{}",
                    case["name"]
                );
                assert_eq!(
                    output["result"]["error_code"],
                    if state == "Cancelled" {
                        "LongRunningOperationCancelled"
                    } else {
                        "LongRunningOperationFailed"
                    }
                );
                assert_eq!(output["command"], cmd.command_path());
                assert_eq!(error.as_ref(), &original);
                assert_request_path(&h, timeout);
            }
        }
    }
}

#[tokio::test]
async fn actual_json_dispatch_preserves_http_messages_codes_exits_and_empty_period_only_result() {
    for case in cases()["http"].as_array().unwrap() {
        let headers: Vec<_> = case
            .get("headers")
            .and_then(Value::as_object)
            .map(|v| {
                v.iter()
                    .map(|(k, v)| (k.as_str(), v.as_str().unwrap()))
                    .collect()
            })
            .unwrap_or_default();
        let h = Harness::new(
            response(
                case["status"].as_u64().unwrap() as u16,
                &case["body"].to_string(),
                &headers,
            ),
            false,
        );
        let plan = h
            .host()
            .dispatch_json(&command(false), pending())
            .await
            .unwrap();
        let output: Value = serde_json::from_slice(&plan.payload).unwrap();
        assert_eq!(json!(plan.exit_code), case["exit"], "{}", case["name"]);
        assert_eq!(
            output["result"]["message"], case["message"],
            "{}",
            case["name"]
        );
        assert_eq!(
            output["result"]["error_code"], case["code"],
            "{}",
            case["name"]
        );
        assert_eq!(output["command"], "job run-cancel");
        assert!(output["result"].get("data").is_none());
        assert_request_path(&h, false);
    }
}

#[tokio::test]
async fn json_writer_and_repeated_borrowed_projection_do_not_apply_command_error_defaults_twice() {
    let h = Harness::new(
        response(400, r#"{"message":"...","errorCode":""}"#, &[]),
        false,
    );
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = h
        .host()
        .run_json(&command(false), pending(), &mut stdout, &mut stderr)
        .await
        .unwrap();
    assert_eq!(exit, 1);
    assert!(stderr.is_empty());
    let output: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(output["result"]["message"], "");
    assert_eq!(output["result"]["error_code"], "UnknownError");
    assert_request_path(&h, false);

    let h = Harness::new(
        response(200, r#"{"status":"Failed","error":["日",null,true]}"#, &[]),
        false,
    );
    let failure = h
        .host()
        .execute(&command(false), pending())
        .await
        .unwrap_err();
    let first = failure
        .envelope(timestamp(), Some("job run-cancel".into()))
        .to_value()
        .unwrap();
    let second = failure
        .envelope(timestamp(), Some("job run-cancel".into()))
        .to_value()
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(
        first["result"]["message"],
        "The operation failed: ['日', None, True]"
    );
    let JobCommandError::Operation(error) = failure else {
        panic!("evidence consumed");
    };
    assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
    assert!(error.mutation_may_have_occurred);
}

#[tokio::test]
async fn handler_cancellation_after_post_retains_shared_classification_and_no_mutation_replay() {
    let h = Harness::new(response(200, r#"{"status":"Succeeded"}"#, &[]), false);
    let cancelled = poll_fn(|cx| {
        if h.http.requests.lock().unwrap().is_empty() {
            cx.waker().wake_by_ref();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    });
    let failure = h
        .host()
        .execute(&command(false), cancelled)
        .await
        .unwrap_err();
    let JobCommandError::Operation(ref error) = failure else {
        panic!("operation evidence dropped");
    };
    assert_eq!(error.kind, OperationErrorKind::Cancelled);
    assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
    assert!(error.mutation_may_have_occurred);
    let output = failure
        .envelope(timestamp(), Some("job run-cancel".into()))
        .to_value()
        .unwrap();
    assert_eq!(
        output["result"]["error_code"],
        "LongRunningOperationCancelled"
    );
    assert_ne!(output["result"]["error_code"], "JobFailed");
    let requests = h.http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method(), azure_core::http::Method::Post);
}

#[tokio::test]
async fn direct_sdk_service_failure_retains_its_distinct_job_http_classification() {
    let raw =
        json!({"errorCode":"Denied","message":"source job status error","requestId":"synthetic"});
    let http = ScriptedHttp::new([response(403, &raw.to_string(), &[])]);
    let service = sdk(http.clone(), Arc::new(TestTimer::default()));
    let target =
        fabric_sdk_jobs::JobTarget::new("ws", "item", fabric_sdk_jobs::JobType::Pipeline).unwrap();
    let error = service.status(&target, "run", pending()).await.unwrap_err();
    let fabric_sdk_jobs::JobError::Http {
        status,
        raw_service_error,
        ..
    } = error
    else {
        panic!("ordinary job status errors must not be fabricated as generic operation failures");
    };
    assert_eq!(status, 403);
    assert_eq!(raw_service_error.as_deref(), Some(&raw));
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method(), azure_core::http::Method::Get);
}

#[tokio::test]
async fn skill_reaches_generic_operation_polling_without_erasing_errors_or_mutation_evidence() {
    for timeout in [false, true] {
        let h = Harness::new(
            response(
                200,
                r#"{"status":"Failed","error":{"message":"source failure"}}"#,
                &[],
            ),
            timeout,
        );
        let mut cmd = command(timeout);
        cmd.skill = Some("job-lro".into());
        let failure = h.host().execute(&cmd, pending()).await.unwrap_err();
        let JobCommandError::Operation(ref error) = failure else {
            panic!("lost operation evidence");
        };
        assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
        assert!(error.mutation_may_have_occurred);
        assert_eq!(
            error.raw_service_error.as_deref(),
            Some(&json!({"message":"source failure"}))
        );
        let output = failure
            .envelope(timestamp(), Some(cmd.command_path()))
            .to_value()
            .unwrap();
        assert_eq!(output["result"]["error_code"], "LongRunningOperationFailed");
        assert_eq!(
            output["result"]["message"],
            "The operation failed: {'message': 'source failure'}"
        );
        assert_request_path(&h, timeout);
        for request in h.http.requests.lock().unwrap().iter() {
            assert_eq!(
                request
                    .headers()
                    .get_optional_str(&"x-ms-fabric-skill".into()),
                Some("job-lro")
            );
        }
        assert!(h.sdk.request_headers().is_empty());
        assert!(h.operations.request_headers().is_empty());
    }
}
