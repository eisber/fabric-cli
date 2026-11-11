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
use fabric_cli::parser::{Invocation, parse_args};
use fabric_sdk::{Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin};
use fabric_sdk_http::ExecutionOptions;
use fabric_sdk_transport::TokioTimer;
use serde_json::Value;
use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const MODEL: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";

#[test]
fn actual_native_score_transport_budget_is_not_the_ordinary_thirty_second_read_budget() {
    for (tail, expected) in [
        (
            vec![
                "score",
                "--version",
                "01",
                "--force",
                "--body",
                r#"{"inputs":[]}"#,
            ],
            240,
        ),
        (
            vec![
                "score",
                "--version",
                "01",
                "--force",
                "--timeout",
                "75",
                "--",
                "x=0",
            ],
            75,
        ),
        (vec!["settings", "get"], 30),
    ] {
        let command = parse(&tail);
        let options = transport_options(&command);
        assert_eq!(options.read_timeout, Duration::from_secs(expected));
        assert_eq!(options.connect_timeout, Duration::from_secs(10));
        assert_eq!(options.write_timeout, Duration::from_secs(30));
        assert_eq!(
            execution_options(&command).timeout,
            if command.is_score() {
                command.timeout
            } else {
                Duration::from_secs(240)
            }
        );
    }
}

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|s| (*s).into()).collect());
        Ok(AccessToken::new(
            "synthetic-ml-endpoint-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug, Default)]
struct Wire {
    replies: Mutex<VecDeque<(StatusCode, Headers, Vec<u8>)>>,
    requests: Mutex<Vec<Request>>,
    pending_after_replies: bool,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(
            request.url().host_str(),
            Some("msitapi.fabric.microsoft.com")
        );
        self.requests.lock().unwrap().push(request.clone());
        let reply = self.replies.lock().unwrap().pop_front();
        if self.pending_after_replies && reply.is_none() {
            return std::future::pending().await;
        }
        let (status, headers, body) = reply.expect("unexpected dispatch or mutation replay");
        Ok(AsyncRawResponse::from_bytes(status, headers, body))
    }
}

fn reply(status: StatusCode, body: Value) -> (StatusCode, Headers, Vec<u8>) {
    (status, Headers::new(), serde_json::to_vec(&body).unwrap())
}

fn accepted(location: &str) -> (StatusCode, Headers, Vec<u8>) {
    let mut headers = Headers::new();
    headers.insert("x-ms-operation-id", OPERATION);
    headers.insert("location", location.to_owned());
    headers.insert("retry-after", "0");
    (StatusCode::Accepted, headers, b"null".to_vec())
}

fn parse(leaf: &[&str]) -> EndpointCommand {
    let (options, record) = leaf.split_at(
        leaf.iter()
            .position(|value| *value == "--")
            .unwrap_or(leaf.len()),
    );
    let Invocation::MlEndpoint(command) = parse_args(
        ["fab-rs", "artifact", "ml-endpoint"]
            .into_iter()
            .chain(options.iter().copied())
            .chain(["--workspace-id", WORKSPACE, "--model-id", MODEL])
            .chain(record.iter().copied()),
    )
    .unwrap() else {
        panic!("root ML endpoint dispatch required")
    };
    command
}

fn configured(wire: Arc<dyn HttpClient>) -> (Client, HttpExecutor, Arc<Credential>) {
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "msitapi.fabric.microsoft.com").unwrap();
    let credential = Arc::new(Credential::default());
    let sdk = Client::with_context(RequestContext::with_endpoint(
        credential.clone(),
        CredentialSelection::new("synthetic-account", "synthetic-tenant", Audience::Fabric)
            .unwrap(),
        endpoint.clone(),
    ));
    let executor = HttpExecutor::new(wire, Arc::new(TokioTimer), ExecutionOptions::default())
        .unwrap()
        .with_endpoint(endpoint);
    (sdk, executor, credential)
}

async fn invoke(
    wire: Arc<Wire>,
    leaf: &[&str],
    timeout: Duration,
) -> (CommandOutcome, Value, Arc<Credential>) {
    invoke_client(wire, leaf, timeout).await
}

async fn invoke_client(
    wire: Arc<dyn HttpClient>,
    leaf: &[&str],
    timeout: Duration,
) -> (CommandOutcome, Value, Arc<Credential>) {
    let command = parse(leaf);
    let (sdk, executor, credential) = configured(wire);
    let mut state = crate::batch_state::BatchState::capture();
    let delivery = super::super::execute_authenticated_injected(
        super::super::NativeCommand::MlEndpoint(&command, Instant::now() + timeout),
        sdk,
        executor,
        "/",
        fabric_cli_context::HomeContext::Unspecified,
        super::super::settings::NativeSettings::select(None).unwrap(),
        &mut state,
        false,
        None,
    )
    .await
    .unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let outcome = delivery.write_to(&mut out, &mut err).unwrap();
    let bytes = if matches!(outcome, CommandOutcome::Returned(0)) {
        assert!(err.is_empty());
        out
    } else {
        assert!(out.is_empty());
        err
    };
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains("synthetic-ml-endpoint-token"));
    (outcome, serde_json::from_slice(&bytes).unwrap(), credential)
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    Dispatch,
    Stream,
    AfterAccepted,
    Delayed,
}

#[derive(Debug)]
struct FaultWire {
    fault: Fault,
    requests: Mutex<Vec<Request>>,
}

#[async_trait]
impl HttpClient for FaultWire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        match self.fault {
            Fault::Dispatch => Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Connection,
                "private-input-or-token",
            )),
            Fault::Stream => {
                let mut headers = Headers::new();
                headers.insert("request-id", OPERATION);
                headers.insert("x-ms-operation-id", OPERATION);
                headers.insert("authorization", "private-input-or-token");
                headers.insert("set-cookie", "private-input-or-token");
                Ok(AsyncRawResponse::new(
                    StatusCode::Accepted,
                    headers,
                    Box::pin(futures_util::stream::iter([Err(
                        azure_core::Error::with_message(
                            azure_core::error::ErrorKind::Io,
                            "private-input-or-token",
                        ),
                    )])),
                ))
            }
            Fault::AfterAccepted if request.method() == Method::Post => {
                let (status, headers, body) = accepted(&format!(
                    "https://msitapi.fabric.microsoft.com/v1/operations/{OPERATION}"
                ));
                Ok(AsyncRawResponse::from_bytes(status, headers, body))
            }
            Fault::AfterAccepted => Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Io,
                "private-input-or-token",
            )),
            Fault::Delayed => {
                tokio::time::sleep(Duration::from_millis(40)).await;
                Ok(AsyncRawResponse::from_bytes(
                    StatusCode::Ok,
                    Headers::new(),
                    br#"{"predictions":[0]}"#.to_vec(),
                ))
            }
        }
    }
}

#[tokio::test]
async fn score_transport_stream_and_post_accept_failures_have_safe_actions_and_never_rescore() {
    for fault in [Fault::Dispatch, Fault::Stream, Fault::AfterAccepted] {
        let wire = Arc::new(FaultWire {
            fault,
            requests: Mutex::new(Vec::new()),
        });
        let (_, output, _) = invoke_client(
            wire.clone(),
            &[
                "score",
                "--version",
                "01",
                "--force",
                "--wait",
                "--body",
                r#"{"inputs":[0]}"#,
            ],
            Duration::from_secs(2),
        )
        .await;
        assert_eq!(output["error"]["code"], "ExecutionFailed");
        assert_eq!(output["error"]["mutationMayHaveOccurred"], true);
        assert_eq!(
            output["error"]["diagnostics"]["httpExecutionFailure"],
            "Transport"
        );
        assert_eq!(
            wire.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.method() == Method::Post)
                .count(),
            1
        );
        assert!(!output.to_string().contains("private-input-or-token"));
        assert!(output["error"]["nextActions"].is_array());
        println!("OFFLINE_ML_FAILURE {output}");
        match fault {
            Fault::Dispatch => {
                assert!(output["error"]["httpStatus"].is_null());
                assert!(output["error"]["operationId"].is_null());
                assert_eq!(
                    output["error"]["diagnostics"]["transportKind"],
                    "connection"
                );
            }
            Fault::Stream => {
                assert_eq!(output["error"]["httpStatus"], 202);
                assert_eq!(output["error"]["operationId"], OPERATION);
                assert_eq!(output["error"]["diagnostics"]["phase"], "response_body");
                assert_eq!(
                    output["error"]["diagnostics"]["requestIds"]["request-id"],
                    OPERATION
                );
            }
            Fault::AfterAccepted => {
                assert_eq!(output["error"]["operationId"], OPERATION);
                assert!(output["error"]["diagnostics"]["lastHttpStatus"].is_null());
                let argv: Vec<String> = output["error"]["nextActions"][1]["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap().into())
                    .collect();
                assert!(parse_args(argv).is_ok());
            }
            Fault::Delayed => unreachable!(),
        }
    }
}

#[tokio::test]
async fn delayed_initial_score_is_bounded_and_observed_once() {
    let wire = Arc::new(FaultWire {
        fault: Fault::Delayed,
        requests: Mutex::new(Vec::new()),
    });
    let (outcome, output, _) = invoke_client(
        wire.clone(),
        &[
            "score",
            "--version",
            "01",
            "--force",
            "--wait",
            "--body",
            r#"{"inputs":[0]}"#,
        ],
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(outcome, CommandOutcome::Returned(0));
    assert_eq!(output["data"]["predictions"], json!([0]));
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn scoring_service_text_and_parameters_are_not_logged_as_model_inputs() {
    let raw = json!({"errorCode":"Denied","message":"private-model-input","requestId":OPERATION,
        "parameters":[{"value":"private-token"}],"future":"private-model-input"});
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([reply(StatusCode::Forbidden, raw)])),
        ..Wire::default()
    });
    let (_, output, _) = invoke(
        wire,
        &[
            "score",
            "--version",
            "01",
            "--force",
            "--body",
            r#"{"inputs":[0]}"#,
        ],
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(output["error"]["serviceError"]["errorCode"], "Denied");
    assert_eq!(output["error"]["serviceError"]["requestId"], OPERATION);
    assert!(!output.to_string().contains("private-model-input"));
    assert!(!output.to_string().contains("private-token"));
}

#[tokio::test]
async fn actual_root_native_dispatch_uses_msit_fabric_audience_without_item_discovery() {
    for (leaf, suffix, data) in [
        (
            vec!["settings", "get"],
            "",
            json!({"defaultVersionName":"1","extra":false}),
        ),
        (
            vec!["serving-version", "list"],
            "/versions",
            json!({"value":[],"continuationToken":"next","extra":null}),
        ),
        (
            vec!["serving-version", "get", "--version", "01"],
            "/versions/01",
            json!({"versionName":"01","status":"FutureStatus","extra":[]}),
        ),
    ] {
        let wire = Arc::new(Wire {
            replies: Mutex::new(VecDeque::from([reply(StatusCode::Ok, data.clone())])),
            ..Wire::default()
        });
        let (outcome, output, credential) =
            invoke(wire.clone(), &leaf, Duration::from_secs(10)).await;
        assert_eq!(outcome, CommandOutcome::Returned(0));
        assert_eq!(output["data"], data);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), Method::Get);
        assert_eq!(
            requests[0].url().as_str(),
            format!(
                "https://msitapi.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/mlmodels/{MODEL}/endpoint{suffix}"
            )
        );
        assert_eq!(
            *credential.scopes.lock().unwrap(),
            vec![vec!["https://api.fabric.microsoft.com/.default".to_owned()]]
        );
    }
}

#[tokio::test]
async fn actual_root_default_reads_settings_then_exact_configured_version() {
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([
            reply(StatusCode::Ok, json!({"defaultVersionName":"01"})),
            reply(
                StatusCode::Ok,
                json!({"versionName":"01","status":"Active"}),
            ),
        ])),
        ..Wire::default()
    });
    let (outcome, output, _) = invoke(
        wire.clone(),
        &["serving-version", "get", "--default"],
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(outcome, CommandOutcome::Returned(0));
    assert_eq!(output["selection"]["versionName"], "01");
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].url().path().ends_with("/endpoint"));
    assert!(requests[1].url().path().ends_with("/versions/01"));
}

#[tokio::test]
async fn accepted_activation_and_score_are_not_completed_or_predictions() {
    for leaf in [
        vec!["serving-version", "activate", "--version", "1", "--force"],
        vec![
            "score",
            "--default",
            "--force",
            "--body",
            r#"{"inputs":[]}"#,
        ],
    ] {
        let wire = Arc::new(Wire {
            replies: Mutex::new(VecDeque::from([accepted(&format!(
                "https://msitapi.fabric.microsoft.com/v1/operations/{OPERATION}"
            ))])),
            ..Wire::default()
        });
        let (outcome, output, _) = invoke(wire.clone(), &leaf, Duration::from_secs(10)).await;
        assert_eq!(outcome, CommandOutcome::Returned(0));
        assert_eq!(output["status"], "accepted");
        assert_eq!(output["httpStatus"], 202);
        assert_eq!(output["operationId"], OPERATION);
        assert_eq!(output["mutationMayHaveOccurred"], true);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn waiting_score_returns_native_predictions_from_result_not_state() {
    let native = json!({"predictions":[false,0,null,[],{}],"future":""});
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([
            accepted(&format!(
                "https://msitapi.fabric.microsoft.com/v1/operations/{OPERATION}"
            )),
            reply(StatusCode::Ok, json!({"status":"Succeeded"})),
            reply(StatusCode::Ok, native.clone()),
        ])),
        ..Wire::default()
    });
    let (outcome, output, _) = invoke(
        wire.clone(),
        &[
            "score",
            "--version",
            "01",
            "--force",
            "--wait",
            "--body",
            r#"{"inputs":[false,0,null]}"#,
        ],
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(outcome, CommandOutcome::Returned(0));
    assert_eq!(output["data"], native);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method(), Method::Post);
    assert!(requests[0].url().path().ends_with("/versions/01/score"));
    assert!(
        requests[2]
            .url()
            .path()
            .ends_with(&format!("/operations/{OPERATION}/result"))
    );
}

#[tokio::test]
async fn foreign_lro_link_is_rejected_without_following_or_replaying() {
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([accepted(&format!(
            "https://api.fabric.microsoft.com/v1/operations/{OPERATION}"
        ))])),
        ..Wire::default()
    });
    let (outcome, output, _) = invoke(
        wire.clone(),
        &[
            "serving-version",
            "activate",
            "--version",
            "1",
            "--force",
            "--wait",
        ],
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(outcome, CommandOutcome::Returned(1));
    assert_eq!(output["error"]["code"], "InvalidLink");
    assert_eq!(output["error"]["mutationMayHaveOccurred"], true);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn timeout_preserves_operation_and_uncertain_effect_after_one_mutation() {
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([accepted(&format!(
            "https://msitapi.fabric.microsoft.com/v1/operations/{OPERATION}"
        ))])),
        pending_after_replies: true,
        ..Wire::default()
    });
    let (outcome, output, _) = invoke(
        wire.clone(),
        &[
            "serving-version",
            "activate",
            "--version",
            "1",
            "--force",
            "--wait",
        ],
        Duration::from_millis(30),
    )
    .await;
    assert_eq!(outcome, CommandOutcome::Stop(1));
    assert_eq!(output["error"]["code"], "timed_out");
    assert_eq!(output["error"]["operationId"], OPERATION);
    assert_eq!(output["error"]["mutationMayHaveOccurred"], true);
    assert_eq!(
        wire.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method() == Method::Post)
            .count(),
        1
    );
}

#[tokio::test]
async fn provider_failure_retains_raw_fields_and_actual_status_on_stderr() {
    let raw = json!({"errorCode":"Denied","message":"synthetic-provider-error","future":{"present":false}});
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([reply(StatusCode::Forbidden, raw.clone())])),
        ..Wire::default()
    });
    let (outcome, output, _) =
        invoke(wire.clone(), &["settings", "get"], Duration::from_secs(10)).await;
    assert_eq!(outcome, CommandOutcome::Returned(1));
    assert_eq!(output["error"]["httpStatus"], 403);
    assert_eq!(output["error"]["serviceError"], raw);
    assert_eq!(output["error"]["mutationMayHaveOccurred"], false);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn expired_preconnect_deadline_does_not_poll_credential_work() {
    let result: Result<(), Failure> = before_connect(
        async { panic!("expired authentication must not start") },
        Instant::now() - Duration::from_secs(1),
    )
    .await;
    assert_eq!(result.unwrap_err().0, "timed_out");
}

#[tokio::test]
async fn interrupted_input_preparation_never_resumes_a_shell_with_a_pending_reader() {
    for (deadline, signal) in [
        (Instant::now() + Duration::from_secs(1), true),
        (Instant::now() + Duration::from_millis(20), false),
    ] {
        let started = std::sync::atomic::AtomicBool::new(false);
        let error = before_connect_with_signal(
            std::future::poll_fn(|_| {
                started.store(true, Ordering::SeqCst);
                std::task::Poll::<Result<(), Failure>>::Pending
            }),
            deadline,
            async move {
                if signal {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Ok(())
                } else {
                    std::future::pending().await
                }
            },
        )
        .await
        .unwrap_err();
        assert!(started.load(Ordering::SeqCst));
        assert!(matches!(error.0, "cancelled" | "timed_out"));
        assert!(matches!(
            input_failure_outcome(&error),
            CommandOutcome::Stop(_)
        ));
    }
}

#[tokio::test]
async fn malformed_successful_score_result_keeps_accepted_operation_for_recovery() {
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([
            accepted(&format!(
                "https://msitapi.fabric.microsoft.com/v1/operations/{OPERATION}"
            )),
            reply(StatusCode::Ok, json!({"status":"Succeeded"})),
            reply(StatusCode::Ok, json!({"future":"missing predictions"})),
        ])),
        ..Wire::default()
    });
    let (outcome, output, _) = invoke(
        wire.clone(),
        &[
            "score",
            "--default",
            "--force",
            "--wait",
            "--body",
            r#"{"inputs":[]}"#,
        ],
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(outcome, CommandOutcome::Returned(1));
    assert_eq!(output["error"]["operationId"], OPERATION);
    assert_eq!(output["error"]["mutationMayHaveOccurred"], true);
    assert_eq!(output["error"]["httpStatus"], 200);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn cancellation_between_default_requests_or_before_score_result_keeps_evidence() {
    for (leaf, replies, cancel_after, mutating) in [
        (
            vec!["serving-version", "get", "--default"],
            vec![reply(StatusCode::Ok, json!({"defaultVersionName":"1"}))],
            1,
            false,
        ),
        (
            vec![
                "score",
                "--default",
                "--force",
                "--wait",
                "--body",
                r#"{"inputs":[]}"#,
            ],
            vec![
                accepted(&format!(
                    "https://msitapi.fabric.microsoft.com/v1/operations/{OPERATION}"
                )),
                reply(StatusCode::Ok, json!({"status":"Succeeded"})),
            ],
            2,
            true,
        ),
    ] {
        let wire = Arc::new(Wire {
            replies: Mutex::new(replies.into()),
            ..Wire::default()
        });
        let (sdk, executor, _) = configured(wire.clone());
        let interrupted = AtomicU8::new(0);
        let cancelled = std::future::poll_fn(|_| {
            if wire.requests.lock().unwrap().len() >= cancel_after {
                interrupted.store(1, Ordering::SeqCst);
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        });
        let mut operations = OperationSession::with_context(
            sdk.context(),
            executor,
            OperationLimits::default(),
            cancelled,
        );
        let result = parse(&leaf).execute(&mut operations).await;
        let evidence = operations.evidence(OperationErrorKind::InvalidResponse);
        let delivery = deliver(result, evidence, &interrupted).unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            delivery.write_to(&mut out, &mut err).unwrap(),
            CommandOutcome::Cancelled(1)
        );
        assert!(out.is_empty());
        let output: Value = serde_json::from_slice(&err).unwrap();
        assert_eq!(output["error"]["code"], "cancelled");
        assert_eq!(output["error"]["mutationMayHaveOccurred"], mutating);
        if mutating {
            assert_eq!(output["error"]["operationId"], OPERATION);
        }
        assert_eq!(wire.requests.lock().unwrap().len(), cancel_after);
    }
}

struct BlockedWriter {
    release: std::sync::mpsc::Receiver<()>,
}

impl Write for BlockedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.release.recv().map_err(io::Error::other)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn blocked_output_has_bounded_abandonment_and_retains_operation_evidence() {
    let (release, receiver) = std::sync::mpsc::channel();
    let started = Instant::now();
    let error = publish_to(
        BlockedWriter { release: receiver },
        b"native output".to_vec(),
        CommandOutcome::Returned(0),
        Instant::now() + Duration::from_millis(30),
        std::future::pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, "timed_out");
    assert!(started.elapsed() < Duration::from_secs(2));
    let response =
        json!({"status":"completed","operationId":OPERATION,"mutationMayHaveOccurred":true});
    let diagnostic = output_failure(&response, error);
    assert_eq!(diagnostic["error"]["operationId"], OPERATION);
    assert_eq!(diagnostic["error"]["mutationMayHaveOccurred"], true);
    release.send(()).unwrap();
}

struct FlushFailure;

impl Write for FlushFailure {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
}

#[tokio::test]
async fn output_flush_failure_and_cancel_are_not_successful_delivery() {
    let error = publish_to(
        FlushFailure,
        b"native output".to_vec(),
        CommandOutcome::Returned(0),
        Instant::now() + Duration::from_secs(1),
        std::future::pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, "output_failed");
    let (release, receiver) = std::sync::mpsc::channel();
    let error = publish_to(
        BlockedWriter { release: receiver },
        b"native output".to_vec(),
        CommandOutcome::Returned(0),
        Instant::now() + Duration::from_secs(1),
        async { super::super::failure("cancelled", "synthetic output cancel", 2) },
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, "cancelled");
    release.send(()).unwrap();
}

#[test]
fn shared_json_encoder_makes_non_ascii_provider_fields_console_safe_without_loss() {
    let value = json!({"native":"\u{1f642}\u{4e2d}","unknown":false});
    let encoded = fabric_cli_output::json_dumps(&value).unwrap();
    assert!(encoded.is_ascii());
    assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), value);
}
