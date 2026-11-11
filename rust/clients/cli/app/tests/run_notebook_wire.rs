// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline root/facade to native HTTP, lifecycle and evidence contracts.
#![cfg(all(feature = "v2-run", feature = "native-host"))]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::{
    parser::{Invocation, parse_args},
    run_notebook::{PreparedNotebook, preflight},
};
use fabric_sdk::{Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_jobs::{
    JobClient, JobFuture, JobLimits,
    run::{RunClock, RunError},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::pending,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const RUN: &str = "33333333-3333-3333-3333-333333333333";
const TARGET: &str =
    "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook";

#[derive(Debug, Default)]
struct Credential(AtomicUsize);
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "synthetic-notebook-test",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Wire {
    replies: Mutex<VecDeque<(StatusCode, Headers, Vec<u8>)>>,
    requests: Mutex<Vec<Request>>,
}
#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        let (status, headers, body) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected request: submission must never replay");
        Ok(AsyncRawResponse::from_bytes(status, headers, body))
    }
}

#[derive(Debug)]
struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(tokio::time::sleep(delay))
    }
}
struct Clock(Instant);
impl RunClock for Clock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
    fn sleep(&self, delay: Duration) -> JobFuture<'_, ()> {
        Box::pin(tokio::time::sleep(delay))
    }
}

fn command(extra: &[&str]) -> fabric_cli::native_tabular::NativeCommand {
    let args = ["fab-rs", "run", TARGET]
        .into_iter()
        .chain(extra.iter().copied());
    let Invocation::NativeTabular(command) = parse_args(args).unwrap() else {
        panic!("root Run not mounted")
    };
    command
}

fn accepted(location: String) -> (StatusCode, Headers, Vec<u8>) {
    let mut headers = Headers::new();
    headers.insert("location", location);
    headers.insert("retry-after", "0");
    (StatusCode::Accepted, headers, Vec::new())
}
fn location() -> String {
    format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/items/{ITEM}/jobs/instances/{RUN}")
}
fn status(state: &str) -> (StatusCode, Headers, Vec<u8>) {
    let mut headers = Headers::new();
    headers.insert("retry-after", "0");
    (
        StatusCode::Ok,
        headers,
        serde_json::to_vec(&json!({"id":RUN,"itemId":ITEM,"status":state})).unwrap(),
    )
}
fn client(replies: Vec<(StatusCode, Headers, Vec<u8>)>) -> (JobClient, Arc<Wire>, Arc<Credential>) {
    let wire = Arc::new(Wire {
        replies: Mutex::new(replies.into()),
        requests: Mutex::new(Vec::new()),
    });
    let (client, credential) = bound_client(wire.clone(), JobLimits::default());
    (client, wire, credential)
}

fn bound_client(wire: Arc<dyn HttpClient>, limits: JobLimits) -> (JobClient, Arc<Credential>) {
    let credential = Arc::new(Credential::default());
    let endpoint = EndpointBinding::public(ServiceOrigin::Fabric);
    let context = RequestContext::with_endpoint(
        credential.clone(),
        CredentialSelection::new("synthetic", WS, Audience::Fabric).unwrap(),
        endpoint.clone(),
    );
    // Deliberately tiny legacy cap: Run must replace it with its remaining budget.
    let http = HttpExecutor::new(
        wire,
        Arc::new(Timer),
        ExecutionOptions {
            timeout: Duration::from_nanos(1),
            ..ExecutionOptions::default()
        },
    )
    .unwrap()
    .with_endpoint(endpoint);
    (
        JobClient::with_context(
            context,
            Arc::new(fabric_cli_jobs::HttpJobExecutor::new(http)),
            limits,
        ),
        credential,
    )
}

#[test]
fn root_and_notebook_facade_share_typed_normalization() {
    let root = command(&["--wait", "--timeout", "1m"]);
    let Invocation::NativeTabular(facade) = parse_args([
        "fab-rs",
        "artifact",
        "notebook",
        "run",
        TARGET,
        "--wait",
        "--timeout",
        "1m",
    ])
    .unwrap() else {
        panic!("facade missing")
    };
    assert_eq!(preflight(&root).unwrap(), preflight(&facade).unwrap());
    for path in [
        "N.Notebook",
        "/x.Workspace/y.Notebook",
        "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook/cells/0",
    ] {
        let mut invalid = root.clone();
        invalid.path = path.into();
        assert!(preflight(&invalid).is_err());
    }
    for timeout in ["0s", "25h", "-1s", "1.5m", "1", "18446744073709551615h"] {
        let mut invalid = root.clone();
        if let fabric_cli::native_tabular::Operation::Run(options) = &mut invalid.operation {
            options.timeout = Some(timeout.into());
        }
        assert!(preflight(&invalid).is_err());
    }
    assert!(preflight(&command(&["--wait", "--check-exit"])).is_err());
    let output = command(&["--format", "json", "--output", "new-receipt.json"]);
    assert!(preflight(&output).is_ok());
}

#[test]
fn strict_native_input_rejects_duplicates_null_unknown_and_wrong_parameter_types() {
    let command = command(&["--input", "-"]);
    for input in [
        "",
        "null",
        "[]",
        "{\"parameters\":[],\"parameters\":[]}",
        "{\"parameters\":[{\"name\":\"x\",\"type\":\"Integer\",\"value\":\"7\"}]}",
        "{\"parameters\":[{\"name\":\"x\",\"type\":\"Text\",\"value\":\"a\",\"value\":\"b\"}]}",
        "{\"executionData\":{\"compute\":\"Spark\",\"unknown\":true}}",
        "{\"parameters\":{\"x\":7}}",
        "{\"unknown\":true}",
        "{} {}",
        "{\"executionData\":{\"compute\":\"Spark\",\"computeConfiguration\":{\"highConcurrencyModeOptions\":{\"enabled\":true}}}}",
        "{\"parameters\":[{\"name\":\"x\",\"type\":\"Number\",\"value\":18446744073709551616}]}",
    ] {
        assert!(
            PreparedNotebook::new(&command, Some(input.as_bytes())).is_err(),
            "{input}"
        );
    }
    assert!(PreparedNotebook::new(&command, None).is_err());
    assert!(PreparedNotebook::new(&command, Some(&vec![b' '; 1_048_577])).is_err());
}

#[tokio::test]
async fn root_submits_exact_native_body_once_and_retains_scoped_acceptance() {
    let input = br#"{"parameters":[{"name":"Text","type":"Text","value":"hello"},{"name":"Integer","type":"Integer","value":9007199254740993},{"name":"Boolean","type":"Boolean","value":false}],"executionData":{"compute":"Jupyter","computeConfiguration":{"numCores":4}}}"#;
    let prepared = PreparedNotebook::new(&command(&["--input", "-"]), Some(input)).unwrap();
    let (client, wire, credential) = client(vec![accepted(location())]);
    let result = prepared
        .dispatch(&client, Arc::new(Clock(Instant::now())), pending())
        .await;
    assert_eq!(result.exit_code(), 0, "{}", result.value());
    assert_eq!(result.value()["status"], "accepted");
    assert_eq!(result.value()["meta"]["handle"]["kind"], "itemJobRun");
    assert_eq!(result.value()["meta"]["handle"]["id"], RUN);
    assert_eq!(result.value()["meta"]["applicationSuccessVerified"], false);
    assert_eq!(result.value()["meta"]["remoteCancellationRequested"], false);
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method(), Method::Post);
    assert_eq!(
        requests[0].url().path(),
        format!("/v1/workspaces/{WS}/notebooks/{ITEM}/jobs/execute/instances")
    );
    assert_eq!(requests[0].url().query(), Some("beta=false"));
    let azure_core::http::Body::Bytes(body) = requests[0].body() else {
        panic!("native JSON bytes expected")
    };
    assert_eq!(
        serde_json::from_slice::<Value>(body).unwrap(),
        serde_json::from_slice::<Value>(input).unwrap()
    );
}

#[tokio::test]
async fn omitted_and_empty_object_remain_distinct_on_the_wire() {
    for (args, input, expected) in [
        (vec![], None, b"".as_slice()),
        (
            vec!["--input", "-"],
            Some(b"{}".as_slice()),
            b"{}".as_slice(),
        ),
    ] {
        let prepared = PreparedNotebook::new(&command(&args), input).unwrap();
        let (client, wire, _) = client(vec![accepted(location())]);
        assert_eq!(
            prepared
                .dispatch(&client, Arc::new(Clock(Instant::now())), pending())
                .await
                .exit_code(),
            0
        );
        let requests = wire.requests.lock().unwrap();
        let azure_core::http::Body::Bytes(body) = requests[0].body() else {
            panic!("body")
        };
        assert_eq!(body.as_ref(), expected);
    }
}

#[tokio::test]
async fn bounded_observation_reports_scheduler_completion_without_application_verdict() {
    let prepared = PreparedNotebook::new(&command(&["--wait"]), None).unwrap();
    let (client, wire, _) = client(vec![
        accepted(location()),
        status("InProgress"),
        status("Completed"),
    ]);
    let result = prepared
        .dispatch(&client, Arc::new(Clock(Instant::now())), pending())
        .await;
    assert_eq!(result.exit_code(), 0, "{}", result.value());
    assert_eq!(result.value()["status"], "completed");
    assert_eq!(result.value()["meta"]["completionDomain"], "scheduler");
    assert_eq!(result.value()["meta"]["applicationSuccessVerified"], false);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == Method::Post)
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == Method::Get)
            .count(),
        2
    );
}

#[tokio::test]
async fn failed_and_unknown_native_states_never_exit_successfully() {
    for state in [
        "Failed",
        "Cancelled",
        "Deduped",
        "Running",
        "FutureNativeState",
    ] {
        let prepared = PreparedNotebook::new(&command(&["--wait"]), None).unwrap();
        let (client, wire, _) = client(vec![accepted(location()), status(state)]);
        let result = prepared
            .dispatch(&client, Arc::new(Clock(Instant::now())), pending())
            .await;
        assert_eq!(result.exit_code(), 1, "{state}: {}", result.value());
        assert_eq!(result.value()["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(wire.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn generic_operation_is_not_invented_as_a_run_or_polled_as_a_scheduler() {
    for wait in [false, true] {
        let prepared =
            PreparedNotebook::new(&command(if wait { &["--wait"] } else { &[] }), None).unwrap();
        let (client, wire, _) = client(vec![accepted(format!(
            "https://api.fabric.microsoft.com/v1/operations/{RUN}"
        ))]);
        let result = prepared
            .dispatch(&client, Arc::new(Clock(Instant::now())), pending())
            .await;
        assert_eq!(result.exit_code(), u8::from(wait));
        assert_eq!(result.value()["status"], "accepted");
        assert_eq!(result.value()["meta"]["handle"]["kind"], "operation");
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn rejection_and_malicious_location_do_not_replay_submission() {
    for reply in [
        (
            StatusCode::TooManyRequests,
            Headers::new(),
            br#"{"secret":"not-in-diagnostic"}"#.to_vec(),
        ),
        accepted("https://unrelated.example/v1/operations/opaque".into()),
        accepted(format!(
            "https://api.fabric.microsoft.com/v1/workspaces/{WS}/items/other/jobs/instances/{RUN}"
        )),
    ] {
        let prepared = PreparedNotebook::new(&command(&[]), None).unwrap();
        let (client, wire, _) = client(vec![reply]);
        let result = prepared
            .dispatch(&client, Arc::new(Clock(Instant::now())), pending())
            .await;
        assert_eq!(result.exit_code(), 1);
        assert!(!result.value().to_string().contains("not-in-diagnostic"));
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn interruption_before_submission_never_acquires_a_token() {
    let prepared = PreparedNotebook::new(&command(&[]), None).unwrap();
    let (client, wire, credential) = client(vec![]);
    let result = prepared
        .dispatch(&client, Arc::new(Clock(Instant::now())), async {})
        .await;
    assert_eq!(result.exit_code(), 1);
    assert_eq!(result.value()["error"]["code"], "Cancelled");
    assert_eq!(result.value()["meta"]["mutationMayHaveOccurred"], false);
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[test]
fn timeout_error_projection_is_explicit() {
    let receipt = fabric_cli::run_notebook::Dispatch::unavailable(RunError::Deadline);
    assert_eq!(receipt.value()["error"]["code"], "TimedOut");
    assert_eq!(receipt.value()["meta"]["mutationMayHaveOccurred"], false);
}

#[derive(Clone, Copy, Debug)]
enum StreamMode {
    Pending,
    Failure,
    Oversized,
    NoHeaders,
}

#[derive(Debug)]
struct StreamWire {
    mode: StreamMode,
    requests: Mutex<Vec<Request>>,
    suspended: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl HttpClient for StreamWire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        use futures_util::stream;
        self.requests.lock().unwrap().push(request.clone());
        if matches!(self.mode, StreamMode::NoHeaders) {
            return pending().await;
        }
        let (_, headers, _) = accepted(location());
        let suspended = self.suspended.clone();
        let mode = self.mode;
        let mut first = true;
        let body = stream::poll_fn(move |_| {
            if first {
                first = false;
                return std::task::Poll::Ready(Some(Ok(azure_core::Bytes::from_static(
                    b"{\"prefix\":",
                ))));
            }
            match mode {
                StreamMode::Failure => {
                    std::task::Poll::Ready(Some(Err(azure_core::Error::with_message(
                        azure_core::error::ErrorKind::Io,
                        "synthetic-private-body-error",
                    ))))
                }
                StreamMode::Oversized => std::task::Poll::Ready(Some(Ok(
                    azure_core::Bytes::from_static(b"01234567890123456789"),
                ))),
                _ => {
                    suspended.notify_one();
                    std::task::Poll::Pending
                }
            }
        });
        Ok(AsyncRawResponse::new(
            StatusCode::Accepted,
            headers,
            Box::pin(body),
        ))
    }
}

fn stream_client(mode: StreamMode, bytes: usize) -> (JobClient, Arc<StreamWire>) {
    let wire = Arc::new(StreamWire {
        mode,
        requests: Mutex::new(Vec::new()),
        suspended: Arc::new(tokio::sync::Notify::new()),
    });
    let (client, _) = bound_client(
        wire.clone(),
        JobLimits {
            max_response_bytes: std::num::NonZeroUsize::new(bytes).unwrap(),
            ..JobLimits::default()
        },
    );
    (client, wire)
}

#[tokio::test]
async fn pending_body_timeout_preserves_actual_accepted_headers_and_run_id() {
    let prepared = PreparedNotebook::new(&command(&["--timeout", "50ms"]), None).unwrap();
    let (client, wire) = stream_client(StreamMode::Pending, 1024);
    let result = prepared
        .dispatch(&client, Arc::new(Clock(Instant::now())), pending())
        .await;
    assert_eq!(result.exit_code(), 1);
    assert_eq!(result.value()["error"]["code"], "TimedOut");
    assert_eq!(result.value()["status"], "accepted");
    assert_eq!(result.value()["meta"]["handle"]["id"], RUN);
    assert_eq!(result.value()["meta"]["initialResponseComplete"], false);
    assert_eq!(result.value()["meta"]["mutationMayHaveOccurred"], true);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn local_interruption_after_headers_never_sends_remote_cancellation() {
    let prepared = PreparedNotebook::new(&command(&[]), None).unwrap();
    let (client, wire) = stream_client(StreamMode::Pending, 1024);
    let result = prepared
        .dispatch(
            &client,
            Arc::new(Clock(Instant::now())),
            wire.suspended.notified(),
        )
        .await;
    assert_eq!(result.value()["error"]["code"], "Cancelled");
    assert_eq!(result.value()["meta"]["handle"]["id"], RUN);
    assert_eq!(result.value()["meta"]["remoteCancellationRequested"], false);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn stream_failure_and_oversize_keep_acceptance_without_fabricated_complete_empty_body() {
    for mode in [StreamMode::Failure, StreamMode::Oversized] {
        let prepared = PreparedNotebook::new(&command(&[]), None).unwrap();
        let (client, wire) = stream_client(mode, 16);
        let result = prepared
            .dispatch(&client, Arc::new(Clock(Instant::now())), pending())
            .await;
        assert_eq!(result.exit_code(), 1);
        assert_eq!(result.value()["status"], "accepted");
        assert_eq!(result.value()["meta"]["initialResponseComplete"], false);
        assert_eq!(
            result.value()["meta"]["responseBodyOmitted"],
            matches!(mode, StreamMode::Oversized)
        );
        assert_eq!(result.value()["meta"]["handle"]["id"], RUN);
        assert!(
            !result
                .value()
                .to_string()
                .contains("synthetic-private-body-error")
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn lost_submission_response_is_unknown_and_never_automatically_replayed() {
    let prepared = PreparedNotebook::new(&command(&["--timeout", "50ms"]), None).unwrap();
    let (client, wire) = stream_client(StreamMode::NoHeaders, 1024);
    let result = prepared
        .dispatch(&client, Arc::new(Clock(Instant::now())), pending())
        .await;
    assert_eq!(result.exit_code(), 1);
    assert_eq!(result.value()["status"], "unknown");
    assert_eq!(result.value()["meta"]["mutationMayHaveOccurred"], true);
    assert!(result.value()["meta"]["handle"].is_null());
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn expired_outer_budget_cannot_be_restarted_after_identity_preparation() {
    let prepared = PreparedNotebook::new(&command(&[]), None).unwrap();
    let (client, wire, credential) = client(vec![]);
    let result = prepared
        .dispatch_with_budget(
            &client,
            Arc::new(Clock(Instant::now())),
            pending(),
            Duration::ZERO,
        )
        .await;
    assert_eq!(result.value()["error"]["code"], "TimedOut");
    assert_eq!(result.value()["meta"]["mutationMayHaveOccurred"], false);
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());
}

struct SlowPreflight;
impl fabric_sdk_jobs::run::RunOutput for SlowPreflight {
    fn preflight(&self, _: fabric_sdk_jobs::run::RunFormat) -> Result<(), RunError> {
        std::thread::sleep(Duration::from_millis(25));
        Ok(())
    }
    fn commit<'a>(&'a self, _: &'a [u8]) -> JobFuture<'a, Result<(), RunError>> {
        panic!("expired original deadline must prevent output and submission")
    }
}

#[tokio::test]
async fn local_preflight_cannot_restart_a_supplied_absolute_deadline() {
    use fabric_sdk_jobs::run::{RunDeadline, RunTimeout};
    let prepared = PreparedNotebook::new(&command(&[]), None).unwrap();
    let (client, wire, credential) = client(vec![]);
    let clock = Arc::new(Clock(Instant::now()));
    let deadline =
        RunDeadline::from_start(Duration::ZERO, RunTimeout::parse("10ms").unwrap()).unwrap();
    let result = prepared
        .dispatch_before(&client, clock, pending(), deadline, &SlowPreflight)
        .await;
    assert_eq!(result.value()["error"]["code"], "TimedOut");
    assert_eq!(result.value()["meta"]["mutationMayHaveOccurred"], false);
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(wire.requests.lock().unwrap().is_empty());
}
