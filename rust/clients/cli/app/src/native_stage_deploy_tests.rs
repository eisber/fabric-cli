// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
#![deny(unnameable_test_items)]

use super::*;
use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_deploy::stage::DeploymentOutcome;
use fabric_sdk::{Audience, CredentialSelection, EndpointBinding, ServiceOrigin};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use std::{
    collections::VecDeque,
    future::{pending, ready},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const PIPELINE: &str = "11111111-1111-1111-1111-111111111111";
const SOURCE: &str = "22222222-2222-2222-2222-222222222222";
const TARGET: &str = "33333333-3333-3333-3333-333333333333";
const SOURCE_WS: &str = "44444444-4444-4444-4444-444444444444";
const TARGET_WS: &str = "55555555-5555-5555-5555-555555555555";
const ITEM: &str = "66666666-6666-6666-6666-666666666666";
const OPERATION: &str = "77777777-7777-7777-7777-777777777777";
const DEPLOYMENT: &str = "88888888-8888-8888-8888-888888888888";

fn body() -> Value {
    json!({"sourceStageId":SOURCE,"targetStageId":TARGET,
        "items":[{"sourceItemId":ITEM,"itemType":"Notebook"}],"note":"exact note\n\u{1b}[2J\u{202e}"})
}

fn command(path: &std::path::Path, all: bool, wait: bool, max: usize) -> StageDeployCommand {
    let mut args: Vec<OsString> = [
        "fab-rs",
        "deploy",
        &format!("/.deployment-pipelines/{PIPELINE}/stages/{TARGET}"),
        "--input",
        path.to_str().unwrap(),
        "--max-input-bytes",
        &max.to_string(),
        if all {
            "--all-items"
        } else {
            "--selected-items"
        },
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    if wait {
        args.push("--wait".into());
    }
    let StageDeployInvocation::Deploy(command) =
        fabric_cli::stage_deploy::parse_args(args).unwrap()
    else {
        panic!("native deployment command")
    };
    command
}

fn input(
    value: &Value,
    all: bool,
    wait: bool,
) -> (tempfile::TempDir, StageDeployCommand, DeploymentRequest) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native.json");
    std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    let command = command(&path, all, wait, 65536);
    let request = read_request(&command).unwrap();
    (directory, command, request)
}

#[derive(Debug)]
struct Credential(Arc<AtomicUsize>);
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "synthetic-stage-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

type Reply = (StatusCode, Headers, Value);

struct CaptureWriter(Arc<Mutex<Vec<u8>>>);
impl Write for CaptureWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn reply(status: StatusCode, body: Value) -> Reply {
    (status, Headers::new(), body)
}
fn stages(order: u64, workspace: &str) -> Vec<Reply> {
    vec![
        reply(
            StatusCode::Ok,
            json!({"id":SOURCE,"workspaceId":SOURCE_WS,"order":0}),
        ),
        reply(
            StatusCode::Ok,
            json!({"id":TARGET,"workspaceId":workspace,"order":order}),
        ),
    ]
}
fn accepted() -> Reply {
    let mut headers = Headers::new();
    headers.insert("x-ms-operation-id", OPERATION);
    headers.insert("deployment-id", DEPLOYMENT);
    headers.insert("retry-after", "0");
    (StatusCode::Accepted, headers, Value::Null)
}

#[derive(Debug)]
struct Wire {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Request>>,
    posts: AtomicUsize,
}
impl Wire {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::new(vec![]),
            posts: AtomicUsize::new(0),
        })
    }
}
#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        if request.method() == Method::Post && request.url().path().ends_with("/deploy") {
            self.posts.fetch_add(1, Ordering::SeqCst);
        }
        self.requests.lock().unwrap().push(request.clone());
        let (status, headers, body) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP or replay");
        Ok(AsyncRawResponse::from_bytes(
            status,
            headers,
            if body.is_null() {
                vec![]
            } else {
                serde_json::to_vec(&body).unwrap()
            },
        ))
    }
}

fn context(wire: Arc<Wire>) -> (RequestContext, HttpExecutor, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "msitapi.fabric.microsoft.com").unwrap();
    let context = RequestContext::with_endpoint(
        Arc::new(Credential(calls.clone())),
        CredentialSelection::new("selected-account", "selected-tenant", Audience::Fabric).unwrap(),
        endpoint.clone(),
    );
    let executor = HttpExecutor::new(
        wire,
        Arc::new(fabric_sdk_transport::TokioTimer),
        ExecutionOptions::default(),
    )
    .unwrap()
    .with_endpoint(endpoint);
    (context, executor, calls)
}

fn stop() -> Stopping<'static> {
    stopping(Instant::now() + Duration::from_secs(5), pending())
}

#[tokio::test]
async fn parser_file_native_host_sdk_and_http_bind_exact_body_routes_and_approval() {
    for (all, status) in [(false, StatusCode::Ok), (true, StatusCode::Created)] {
        let mut value = body();
        if all {
            value.as_object_mut().unwrap().remove("items");
        }
        let (_directory, command, request) = input(&value, all, false);
        let wire = Wire::new(stages(1, TARGET_WS).into_iter().chain([
            reply(status, json!({"id":DEPLOYMENT,"type":"Deploy","sourceStageId":SOURCE,"targetStageId":TARGET,"status":"Succeeded"}))
        ]));
        let (context, executor, calls) = context(wire.clone());
        let receipt = execute(
            &command,
            request,
            &context,
            &executor,
            "provider/client/tenant/account",
            |question| {
                for expected in [
                    PIPELINE,
                    SOURCE,
                    TARGET,
                    SOURCE_WS,
                    TARGET_WS,
                    "provider/client/tenant/account",
                    "msitapi.fabric.microsoft.com",
                    "exact note",
                    "\\u001b",
                    "Pipeline.Deploy",
                    "unguarded",
                ] {
                    assert!(question.contains(expected), "{expected}: {question}");
                }
                assert!(!question.contains('\u{1b}'));
                assert!(question.is_ascii());
                ready(Ok(true))
            },
            stop(),
        )
        .await
        .unwrap();
        assert_eq!(receipt.outcome, DeploymentOutcome::Succeeded);
        assert_eq!(receipt.http_status, Some(u16::from(status)));
        assert_eq!(wire.posts.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        for (request, method, path) in [
            (
                &requests[0],
                Method::Get,
                format!("/v1/deploymentPipelines/{PIPELINE}/stages/{SOURCE}"),
            ),
            (
                &requests[1],
                Method::Get,
                format!("/v1/deploymentPipelines/{PIPELINE}/stages/{TARGET}"),
            ),
            (
                &requests[2],
                Method::Post,
                format!("/v1/deploymentPipelines/{PIPELINE}/deploy"),
            ),
        ] {
            assert_eq!(request.method(), method);
            assert_eq!(
                request.url().as_str(),
                format!("https://msitapi.fabric.microsoft.com{path}")
            );
        }
        let azure_core::http::Body::Bytes(bytes) = requests[2].body() else {
            panic!("native JSON bytes")
        };
        assert_eq!(serde_json::from_slice::<Value>(bytes).unwrap(), value);
    }
}

#[test]
fn malformed_duplicate_unknown_typed_scope_and_bounded_input_fail_before_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native.json");
    let valid = serde_json::to_string(&body()).unwrap();
    let mut invalid = vec![
        valid.replacen(
            "\"sourceStageId\":",
            &format!("\"sourceStageId\":\"{SOURCE}\",\"sourceStageId\":"),
            1,
        ),
        valid.replacen("\"itemType\":", "\"itemType\":\"Report\",\"itemType\":", 1),
        valid.replacen("\"note\":", "\"n\\u006fte\":\"hidden\",\"note\":", 1),
    ];
    for value in [
        json!({"sourceStageId":SOURCE,"targetStageId":TARGET,"items":null}),
        json!({"sourceStageId":SOURCE,"targetStageId":TARGET,"items":[]}),
        json!({"sourceStageId":SOURCE,"targetStageId":TARGET,"items":[{"sourceItemId":ITEM,"itemType":"Lakehouse"}]}),
        json!({"sourceStageId":SOURCE,"targetStageId":TARGET,"items":[{"sourceItemId":ITEM,"itemType":"Report","extra":false}]}),
        json!({"sourceStageId":SOURCE,"targetStageId":TARGET,"options":{}}),
    ] {
        invalid.push(value.to_string());
    }
    for bytes in invalid {
        std::fs::write(&path, bytes).unwrap();
        for all in [false, true] {
            assert!(read_request(&command(&path, all, false, 65536)).is_err());
        }
    }
    std::fs::write(&path, valid.as_bytes()).unwrap();
    assert!(read_request(&command(&path, false, false, valid.len())).is_ok());
    assert!(read_request(&command(&path, false, false, valid.len() - 1)).is_err());
    assert!(read_request(&command(directory.path(), false, false, 65536)).is_err());
}

#[tokio::test]
async fn denied_unavailable_and_unsafe_stage_metadata_make_zero_posts() {
    for order in [0, 2, 1] {
        let (_directory, command, request) = input(&body(), false, false);
        let wire = Wire::new(stages(
            order,
            if order == 1 { SOURCE_WS } else { TARGET_WS },
        ));
        let (context, executor, _) = context(wire.clone());
        let error = execute(
            &command,
            request,
            &context,
            &executor,
            "identity",
            |_| async { panic!("unsafe metadata must not ask approval") },
            stop(),
        )
        .await
        .err()
        .unwrap();
        assert!(matches!(
            error.kind,
            DeploymentErrorKind::TargetMismatch | DeploymentErrorKind::UnsupportedCapability
        ));
        assert_eq!(wire.posts.load(Ordering::SeqCst), 0);
    }
    for unavailable in [false, true] {
        let (_directory, command, request) = input(&body(), false, false);
        let wire = Wire::new(stages(1, TARGET_WS));
        let (context, executor, _) = context(wire.clone());
        let error = execute(
            &command,
            request,
            &context,
            &executor,
            "identity",
            |_| {
                ready(if unavailable {
                    Err(io::Error::other("terminal unavailable"))
                } else {
                    Ok(false)
                })
            },
            stop(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            error.kind,
            if unavailable {
                DeploymentErrorKind::ApprovalUnavailable
            } else {
                DeploymentErrorKind::ApprovalDenied
            }
        );
        assert_eq!(wire.posts.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn accepted_wait_native_failure_and_unknown_keep_original_handles_without_result_get() {
    for wait in [false, true] {
        let (_directory, command, request) = input(&body(), false, wait);
        let wire = Wire::new(stages(1, TARGET_WS).into_iter().chain([
            accepted(),
            reply(StatusCode::Ok, json!({"status":"Succeeded"})),
        ]));
        let (context, executor, _) = context(wire.clone());
        let receipt = execute(
            &command,
            request,
            &context,
            &executor,
            "identity",
            |_| ready(Ok(true)),
            stop(),
        )
        .await
        .unwrap();
        assert_eq!(
            receipt.outcome,
            if wait {
                DeploymentOutcome::Succeeded
            } else {
                DeploymentOutcome::Accepted
            }
        );
        assert_eq!(receipt.operation_id.as_deref(), Some(OPERATION));
        assert_eq!(receipt.deployment_id.as_deref(), Some(DEPLOYMENT));
        assert_eq!(receipt.http_status, Some(202));
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), if wait { 4 } else { 3 });
        if wait {
            assert_eq!(
                requests[3].url().path(),
                format!("/v1/operations/{OPERATION}")
            );
        }
        assert_eq!(wire.posts.load(Ordering::SeqCst), 1);
    }
    for native_status in ["Failed", "Mystery"] {
        let (_directory, command, request) = input(&body(), false, false);
        let wire = Wire::new(stages(1, TARGET_WS).into_iter().chain([
            reply(StatusCode::Ok, json!({"id":DEPLOYMENT,"type":"Deploy","sourceStageId":SOURCE,"targetStageId":TARGET,"status":native_status,
                "executionPlan":{"steps":[{"index":0,"status":"Succeeded"},{"index":1,"status":"Failed"}]}})),
        ]));
        let (context, executor, _) = context(wire.clone());
        let error = execute(
            &command,
            request,
            &context,
            &executor,
            "identity",
            |_| ready(Ok(true)),
            stop(),
        )
        .await
        .err()
        .unwrap();
        let receipt = error.receipt.unwrap();
        assert_eq!(receipt.deployment_id.as_deref(), Some(DEPLOYMENT));
        assert!(receipt.execution_plan.is_some());
        assert!(receipt.mutation_may_have_occurred);
        if native_status == "Failed" {
            assert_eq!(receipt.outcome, DeploymentOutcome::Partial);
        }
        assert_eq!(wire.posts.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn cancellation_deadline_and_retry_after_do_not_reset_or_replay() {
    for deadline in [false, true] {
        let (_directory, command, request) = input(&body(), false, true);
        let mut accepted = accepted();
        accepted.1.insert("retry-after", "1");
        let wire = Wire::new(stages(1, TARGET_WS).into_iter().chain([accepted]));
        let (context, executor, _) = context(wire.clone());
        let stop = if deadline {
            stopping(Instant::now() + Duration::from_millis(50), pending())
        } else {
            let wire = wire.clone();
            stopping(Instant::now() + Duration::from_secs(5), async move {
                while wire.posts.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
                Stop::Cancelled
            })
        };
        let error = execute(
            &command,
            request,
            &context,
            &executor,
            "identity",
            |_| ready(Ok(true)),
            stop,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(
            error.kind,
            if deadline {
                DeploymentErrorKind::TimedOut
            } else {
                DeploymentErrorKind::Cancelled
            }
        );
        let receipt = error.receipt.unwrap();
        assert_eq!(receipt.operation_id.as_deref(), Some(OPERATION));
        assert_eq!(receipt.deployment_id.as_deref(), Some(DEPLOYMENT));
        assert_eq!(receipt.http_status, Some(202));
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
        assert_eq!(wire.posts.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn whole_deadline_includes_approval_and_pre_dispatch_cancellation_sends_nothing() {
    let (_directory, command, request) = input(&body(), false, false);
    let wire = Wire::new(stages(1, TARGET_WS));
    let (context, executor, calls) = context(wire.clone());
    let error = execute(
        &command,
        request,
        &context,
        &executor,
        "identity",
        |_| pending::<io::Result<bool>>(),
        stopping(Instant::now() + Duration::from_millis(30), pending()),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, DeploymentErrorKind::TimedOut);
    assert_eq!(wire.posts.load(Ordering::SeqCst), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let (_directory, command, request) = input(&body(), false, false);
    let error = execute(
        &command,
        request,
        &context,
        &executor,
        "identity",
        |_| ready(Ok(true)),
        stopping(Instant::now() - Duration::from_secs(1), pending()),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, DeploymentErrorKind::TimedOut);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[cfg(feature = "headless")]
#[tokio::test]
async fn production_connection_uses_selected_official_identity_and_existing_http_executor() {
    let (_directory, command, request) = input(&body(), false, false);
    let wire = Wire::new([
        reply(StatusCode::Ok, json!({"access_token":"synthetic-official-stage-token","token_type":"Bearer","expires_in":3600})),
    ].into_iter().chain(stages(1, TARGET_WS)).chain([accepted()]));
    let selection =
        crate::headless::Selection::from_inputs("client-secret", |name| -> Option<OsString> {
            match name {
                "FAB_RUST_TENANT" => Some(PIPELINE.into()),
                "FAB_RUST_CLIENT_ID" => Some(SOURCE.into()),
                "FAB_RUST_CLIENT_SECRET" => Some("synthetic-only-secret".into()),
                _ => None,
            }
        })
        .unwrap();
    let (sdk, executor) = super::super::connect_native(
        super::super::NativeCredential::Headless(selection),
        wire.clone(),
        wire.clone(),
        EndpointBinding::public(ServiceOrigin::Fabric),
        ExecutionOptions::default(),
        Some(Instant::now() + Duration::from_secs(5)),
    )
    .await
    .unwrap();
    let receipt = execute(
        &command,
        request,
        sdk.context(),
        &executor,
        "selected official provider",
        |_| ready(Ok(true)),
        stop(),
    )
    .await
    .unwrap();
    assert_eq!(receipt.outcome, DeploymentOutcome::Accepted);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[0].url().host_str(),
        Some("login.microsoftonline.com")
    );
    assert!(
        requests[0]
            .url()
            .path()
            .starts_with(&format!("/{PIPELINE}/"))
    );
    assert!(
        requests[1..]
            .iter()
            .all(|request| request.url().host_str() == Some("api.fabric.microsoft.com"))
    );
}

#[test]
fn blocked_stdout_and_stderr_process_exits_under_original_output_deadline() {
    for mode in ["help", "error"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "native_read::stage_deploy::tests::blocked_output_child",
                "--nocapture",
            ])
            .env("FAB_STAGE_BLOCKED_OUTPUT_TEST", mode)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = child.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("native stage output held the process past its bounded deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn blocked_output_child() {
    let Ok(mode) = std::env::var("FAB_STAGE_BLOCKED_OUTPUT_TEST") else {
        return;
    };
    for stream in [OutputStream::Stdout, OutputStream::Stderr] {
        let mut writer = output::owned_stream(stream, true).unwrap();
        std::thread::spawn(move || {
            let _ = writer.write_all(&vec![b'x'; 1024 * 1024]);
        });
    }
    std::thread::sleep(Duration::from_millis(100));
    let began = Instant::now();
    let result = run_before(
        ["fab-rs", if mode == "help" { "--help" } else { "invalid" }]
            .into_iter()
            .map(Into::into),
        began + Duration::from_millis(100),
    );
    assert!(result.is_err());
    assert!(began.elapsed() < Duration::from_secs(3));
    // Do not let the test harness flush the deliberately blocked streams.
    std::process::exit(0);
}

#[tokio::test]
async fn partial_receipt_output_keeps_actual_execution_and_both_handles() {
    struct PartialWriter {
        wrote: bool,
    }
    impl Write for PartialWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.wrote {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected failed receipt pipe",
                ));
            }
            self.wrote = true;
            Ok(bytes.len().min(8))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let (_directory, command, request) = input(&body(), false, false);
    let wire = Wire::new(stages(1, TARGET_WS).into_iter().chain([accepted()]));
    let (context, executor, _) = context(wire.clone());
    let execution = execute(
        &command,
        request,
        &context,
        &executor,
        "identity",
        |_| ready(Ok(true)),
        stop(),
    )
    .await;
    let (value, exit) = execution_value(&execution);
    let error = publish_to(
        PartialWriter { wrote: false },
        value,
        exit,
        Instant::now() + Duration::from_secs(5),
        stop(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    let retained = delivery_failure(Ok(execution), error);
    let StageDeployError::Delivery { execution, .. } = retained
        .get_ref()
        .unwrap()
        .downcast_ref::<StageDeployError>()
        .unwrap()
    else {
        panic!("original execution missing")
    };
    let receipt = execution.as_ref().as_ref().unwrap();
    assert_eq!(receipt.outcome, DeploymentOutcome::Accepted);
    assert_eq!(receipt.operation_id.as_deref(), Some(OPERATION));
    assert_eq!(receipt.deployment_id.as_deref(), Some(DEPLOYMENT));
    assert_eq!(wire.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelled_accepted_operation_can_deliver_its_receipt_before_original_deadline() {
    let (_directory, command, request) = input(&body(), false, true);
    let mut acceptance = accepted();
    acceptance.1.insert("retry-after", "1");
    let wire = Wire::new(stages(1, TARGET_WS).into_iter().chain([acceptance]));
    let (context, executor, _) = context(wire.clone());
    let deadline = Instant::now() + Duration::from_secs(5);
    let signal_wire = wire.clone();
    let stop = stopping(deadline, async move {
        while signal_wire.posts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        Stop::Cancelled
    });
    let execution = execute(
        &command,
        request,
        &context,
        &executor,
        "identity",
        |_| ready(Ok(true)),
        stop.clone(),
    )
    .await;
    let (value, exit) = execution_value(&execution);
    assert_eq!(exit, 2);
    let capture = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(
        publish_to(
            CaptureWriter(capture.clone()),
            value,
            exit,
            deadline,
            publication_stop(stop, deadline)
        )
        .await
        .unwrap(),
        2
    );
    let value: Value = serde_json::from_slice(&capture.lock().unwrap()).unwrap();
    assert_eq!(value["error"]["code"], "Cancelled");
    assert_eq!(value["evidence"]["operationId"], OPERATION);
    assert_eq!(value["evidence"]["deploymentId"], DEPLOYMENT);
    assert_eq!(wire.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn blocked_credential_selection_worker_cannot_extend_total_deadline() {
    let began = Instant::now();
    let result = blocking_before(
        || {
            std::thread::sleep(Duration::from_millis(200));
            Ok("late selection")
        },
        stopping(began + Duration::from_millis(20), pending()),
    )
    .await;
    assert!(matches!(result, Err(("TimedOut", _, 1))));
    assert!(began.elapsed() < Duration::from_millis(150));
}
