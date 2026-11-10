// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Independent offline acceptance of the MirroredDatabase START frontend.

#![deny(unnameable_test_items)]

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use azure_core::http::{AsyncRawResponse, Body, Method};
use clap::error::ErrorKind;
use fabric_cli::start_command::{self, Delivery, Policy, StartCommand};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    mirroring,
    v2::{
        ApprovalPolicy, Availability, Binding, Effect, Generation, OperationDescriptor,
        PreparedOperation, Registry, Verb,
    },
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, HttpExecutor};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse,
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    future::{Future, pending, poll_fn, ready},
    io::{self, Write},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use support::{ScriptedHttp, TestCredential, TestTimer, response};
use tokio::sync::Notify;

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.MirroredDatabase/runtime/mirroring";
const OTHER_TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/33333333-3333-3333-3333-333333333333.MirroredDatabase/runtime/mirroring";
const HOST: &str = "dailyapi.fabric.microsoft.com";
const ROOT: &str = "https://dailyapi.fabric.microsoft.com";
const OPERATION_ID: &str = "mirroredDatabase.mirroring.start";
const PRIVATE: &str = "private-start-provider-detail";

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn approved() -> StartCommand {
    start_command::parse_start(args(&["start", TARGET, "--approve", "--timeout", "10"])).unwrap()
}

#[test]
fn parses_exactly_one_target_and_an_explicit_bounded_positive_deadline() {
    for (values, seconds, approve) in [
        (
            vec!["start", TARGET, "--approve", "--timeout", "1"],
            1,
            true,
        ),
        (
            vec!["start", "--timeout=86400", "--approve", TARGET],
            86_400,
            true,
        ),
        (vec!["start", TARGET, "--timeout", "10"], 10, false),
    ] {
        let command = start_command::parse_start(args(&values)).unwrap();
        assert_eq!(command.path.as_deref(), Some(TARGET));
        assert_eq!(command.timeout, Some(Duration::from_secs(seconds)));
        assert_eq!(command.approve, approve);
        assert!(!command.schema);
    }
}

#[test]
fn schema_and_help_are_local_and_schema_cannot_authorize_execution() {
    let command = start_command::parse_start(args(&["start", "--schema"])).unwrap();
    assert!(command.schema);
    assert!(command.path.is_none());
    assert!(command.timeout.is_none());
    assert!(!command.approve);
    let schema = mirroring::schemas().unwrap();
    assert_eq!(schema["operations"][0]["id"], OPERATION_ID);
    assert_eq!(schema["operations"][0]["verb"], "start");
    assert_eq!(
        schema["operations"][0]["effects"],
        json!(["remoteRead", "remoteWrite"])
    );
    let help = start_command::start_command()
        .try_get_matches_from(["start", "--help"])
        .unwrap_err();
    assert_eq!(help.kind(), ErrorKind::DisplayHelp);
    let help = help.to_string();
    for option in ["--approve", "--timeout", "--schema"] {
        assert!(help.contains(option), "missing supported option: {option}");
    }
    for option in [
        "--wait",
        "--force",
        "--input",
        "--profile",
        "--output",
        "--query",
    ] {
        assert!(
            !help.contains(option),
            "advertised unsupported option: {option}"
        );
    }
    for values in [
        vec!["start", "--schema", TARGET],
        vec!["start", "--schema", "--timeout", "10"],
        vec!["start", "--schema", "--approve"],
        vec!["start", "--schema", "--schema"],
    ] {
        assert!(
            start_command::parse_start(args(&values)).is_err(),
            "{values:?}"
        );
    }
}

#[test]
fn rejects_alias_names_roots_relative_bulk_and_unadvertised_facets_without_echoing() {
    let invalid = [
        "".to_owned(),
        "/".to_owned(),
        format!("/{WORKSPACE}.Workspace"),
        format!("/{WORKSPACE}.Workspace/{ITEM}.MirroredDatabase"),
        TARGET.trim_start_matches('/').to_owned(),
        TARGET.replace(WORKSPACE, "private-workspace"),
        TARGET.replace(ITEM, "private-item"),
        TARGET.replace(".MirroredDatabase", ".Notebook"),
        TARGET.replace(".MirroredDatabase", ".mirroreddatabase"),
        TARGET.replace(".Workspace", ".workspace"),
        TARGET.replace("/runtime/mirroring", "/mirroring"),
        TARGET.replace("/runtime/mirroring", "/runtime"),
        TARGET.replace("/runtime/mirroring", "/jobs"),
        TARGET.replace("/runtime/mirroring", "/sessions"),
        TARGET.replace("/runtime/mirroring", "/runtime/jobs"),
        TARGET.replace("/runtime/mirroring", "/runtime/sessions"),
        TARGET.replace("/runtime/mirroring", "/src/definition"),
        TARGET.replace(ITEM, "*"),
        TARGET.replace(ITEM, "**"),
        TARGET.replace(ITEM, "22222222-2222-2222-2222-22222222222?"),
        TARGET.replace(ITEM, "{22222222-2222-2222-2222-222222222222}"),
        TARGET.replace("/runtime/", "/./runtime/"),
        TARGET.replace("/runtime/", "/../runtime/"),
        TARGET.replace("/runtime/", "/%72untime/"),
        TARGET.replace("/runtime/", "//runtime/"),
        TARGET.replace("/runtime/", "\\runtime\\"),
        format!("/{TARGET}"),
        format!("{TARGET}/"),
        format!("{TARGET}/private-child"),
        format!("{TARGET}?private-query=value"),
        format!("{TARGET}#private-fragment"),
        format!("https://example.invalid{TARGET}"),
        "C:\\private-start\\target".to_owned(),
        format!(" {TARGET}"),
        format!("{TARGET}\nprivate-control"),
    ];
    for path in invalid {
        let error =
            start_command::parse_start(args(&["start", &path, "--approve", "--timeout", "10"]))
                .unwrap_err();
        let message = error.to_string();
        assert!(!message.contains("private"), "{message}");
        if !path.is_empty() && path != "/" {
            assert!(!message.contains(&path), "echoed rejected path: {path:?}");
        }
    }
}

#[test]
fn rejects_missing_duplicate_unknown_and_abbreviated_execution_arguments() {
    for timeout in [
        "",
        "0",
        "-1",
        "86401",
        "18446744073709551615",
        "18446744073709551616",
        "1.5",
        "1e2",
        "private-timeout",
    ] {
        let error =
            start_command::parse_start(args(&["start", TARGET, "--timeout", timeout, "--approve"]))
                .unwrap_err();
        assert!(!error.to_string().contains("private-timeout"));
    }
    for values in [
        vec![],
        vec!["not-start", TARGET, "--timeout", "10"],
        vec!["start"],
        vec!["start", TARGET],
        vec!["start", "--approve", "--timeout", "10"],
        vec!["start", TARGET, "--timeout"],
        vec!["start", TARGET, OTHER_TARGET, "--timeout", "10"],
        vec!["start", TARGET, TARGET, "--timeout", "10"],
        vec!["start", TARGET, "--timeout", "10", "--timeout", "20"],
        vec!["start", TARGET, "--timeout", "10", "--approve", "--approve"],
        vec!["start", TARGET, "--time", "10", "--approve"],
        vec!["start", TARGET, "--timeout", "10", "--appro"],
        vec!["start", "--schem"],
        vec!["start", TARGET, "--timeout", "10", "--approve=true"],
    ] {
        assert!(
            start_command::parse_start(args(&values)).is_err(),
            "{values:?}"
        );
    }
    for option in [
        "--wait",
        "--force",
        "--input",
        "--profile",
        "--output",
        "--output_format",
        "--query",
        "--preview",
        "--type",
        "--id",
        "--recursive",
        "--bulk",
        "--job",
    ] {
        let values = ["start", TARGET, "--timeout", "10", "--approve", option];
        assert!(
            start_command::parse_start(args(&values)).is_err(),
            "{option}"
        );
    }
}

struct Harness {
    http: Arc<ScriptedHttp>,
    credential: Arc<TestCredential>,
    timer: Arc<TestTimer>,
    client: Client,
    executor: HttpExecutor,
}

impl Harness {
    fn new(replies: impl IntoIterator<Item = AsyncRawResponse>) -> Self {
        let http = ScriptedHttp::new(replies);
        let credential = Arc::new(TestCredential::default());
        let timer = Arc::new(TestTimer::default());
        let endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, HOST).unwrap();
        let client = Client::with_context(RequestContext::with_endpoint(
            credential.clone(),
            CredentialSelection::new("offline", "synthetic-tenant", Audience::Fabric).unwrap(),
            endpoint.clone(),
        ));
        let executor = HttpExecutor::new(http.clone(), timer.clone(), ExecutionOptions::default())
            .unwrap()
            .with_endpoint(endpoint);
        Self {
            http,
            credential,
            timer,
            client,
            executor,
        }
    }

    async fn dispatch(&self, command: &StartCommand) -> Delivery {
        start_command::dispatch(
            command,
            &self.client,
            &self.executor,
            OperationLimits::default(),
            pending(),
        )
        .await
        .unwrap()
    }

    fn assert_actions(&self, actions: &[&str]) {
        let requests = self.http.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            actions.len(),
            "lookup, poll, STOP or replay"
        );
        for (request, action) in requests.iter().zip(actions) {
            assert_eq!(request.method(), Method::Post);
            assert_eq!(
                request.url().as_str(),
                format!("{ROOT}/v1/workspaces/{WORKSPACE}/mirroredDatabases/{ITEM}/{action}")
            );
            let Body::Bytes(body) = request.body() else {
                panic!("mirroring requests must have an empty, non-streaming body");
            };
            assert!(body.is_empty(), "empty is not an empty JSON object");
            assert!(request.headers().get_str(&"authorization".into()).is_ok());
        }
        assert!(
            self.timer.waits.lock().unwrap().is_empty(),
            "implicit polling"
        );
    }

    fn assert_untouched(&self) {
        assert_eq!(self.credential.calls.load(Ordering::SeqCst), 0);
        self.assert_actions(&[]);
    }
}

fn altered_operation(id: &str, effects: Vec<Effect>) -> PreparedOperation {
    let target = mirroring::target(TARGET).unwrap();
    let selection = target.selection();
    let mut registry = Registry::default();
    registry
        .register(OperationDescriptor::new(id, Verb::Start, json!({}), json!({}), effects).unwrap())
        .unwrap();
    registry
        .bind(
            Binding::new(
                &selection.owner_type,
                &selection.facet,
                &selection.representation,
                Generation::Any,
                id,
                Availability::Available,
            )
            .unwrap(),
        )
        .unwrap();
    let resolution = registry.resolve(Verb::Start, selection, None).unwrap();
    PreparedOperation::new(resolution, target, json!({})).unwrap()
}

#[test]
fn immediate_policy_binds_approval_to_exact_path_operation_and_complete_effects() {
    let harness = Harness::new([]);
    let prepared = mirroring::prepare(&harness.client, mirroring::target(TARGET).unwrap()).unwrap();
    assert_eq!(prepared.operation().operation_id(), OPERATION_ID);
    assert_eq!(
        prepared.operation().effects(),
        [Effect::RemoteRead, Effect::RemoteWrite]
    );
    let command = approved();
    assert!(prepared.authorize(&mut Policy(&command)).is_ok());
    let mut refused = command.clone();
    refused.approve = false;
    assert!(prepared.authorize(&mut Policy(&refused)).is_err());
    refused = command.clone();
    refused.path = Some(OTHER_TARGET.into());
    assert!(prepared.authorize(&mut Policy(&refused)).is_err());
    refused = command.clone();
    refused.schema = true;
    assert!(prepared.authorize(&mut Policy(&refused)).is_err());
    for effects in [
        vec![],
        vec![Effect::RemoteRead],
        vec![Effect::RemoteWrite],
        vec![
            Effect::RemoteRead,
            Effect::RemoteWrite,
            Effect::RemoteDelete,
        ],
    ] {
        let operation = altered_operation(OPERATION_ID, effects);
        assert!(Policy(&command).authorize(&operation).is_err());
    }
    let wrong_operation = altered_operation(
        "mirroredDatabase.mirroring.other",
        vec![Effect::RemoteRead, Effect::RemoteWrite],
    );
    assert!(Policy(&command).authorize(&wrong_operation).is_err());
    harness.assert_untouched();
}

fn assert_common_receipt(value: &Value) {
    assert_eq!(value["schemaVersion"], "0.1");
    assert_eq!(value["meta"]["operationId"], OPERATION_ID);
    assert_eq!(value["meta"]["target"]["canonicalPath"], TARGET);
    assert_eq!(
        value["meta"]["target"]["identity"]["workspaceId"],
        WORKSPACE
    );
    assert_eq!(value["meta"]["target"]["identity"]["itemId"], ITEM);
    assert_eq!(
        value["meta"]["declaredEffects"],
        json!(["remoteRead", "remoteWrite"])
    );
    let text = value.to_string();
    assert!(!text.contains("synthetic-offline-token"));
    assert!(!text.contains(PRIVATE));
}

#[tokio::test]
async fn real_dispatch_posts_empty_status_then_one_empty_start_on_the_selected_endpoint() {
    for state in ["Stopped", "Initialized"] {
        let harness = Harness::new([
            response(200, &json!({"status": state}).to_string(), &[]),
            response(200, "", &[]),
        ]);
        let delivery = harness.dispatch(&approved()).await;
        assert_eq!(delivery.exit, 0);
        assert_common_receipt(&delivery.value);
        assert_eq!(delivery.value["status"], "succeeded");
        assert_eq!(
            delivery.value["data"],
            json!({
                "preflightState": state,
                "actionCompleted": true,
                "replicationState": null
            })
        );
        assert_eq!(delivery.value["meta"]["evidence"]["httpStatus"], 200);
        assert_eq!(delivery.value["meta"]["evidence"]["handles"], json!([]));
        assert_eq!(
            delivery.value["meta"]["evidence"]["mutationMayHaveOccurred"],
            true
        );
        assert!(harness.credential.calls.load(Ordering::SeqCst) > 0);
        harness.assert_actions(&["getMirroringStatus", "startMirroring"]);
    }
}

#[tokio::test]
async fn unsuccessful_or_unestablished_preflight_never_dispatches_start() {
    for (status, body) in [
        (
            403,
            json!({"error":{"code":"Forbidden","message":PRIVATE}}).to_string(),
        ),
        (
            200,
            json!({"status":"Stopped","error":{"code":"ProbeFailed","message":PRIVATE}})
                .to_string(),
        ),
        (202, json!({"status":"Stopped"}).to_string()),
        (204, String::new()),
        (200, "{".into()),
        (200, json!({}).to_string()),
        (200, json!({"status":"FutureState"}).to_string()),
        (200, json!({"status":"Running"}).to_string()),
    ] {
        let harness = Harness::new([response(status, &body, &[])]);
        let delivery = harness.dispatch(&approved()).await;
        assert_eq!(delivery.exit, 1, "{status}: {body}");
        assert_common_receipt(&delivery.value);
        assert_eq!(delivery.value["status"], "failed");
        assert_eq!(delivery.value["meta"]["evidence"]["handles"], json!([]));
        assert!(delivery.value.get("data").is_none());
        assert_eq!(
            delivery.value["meta"]["evidence"]["mutationMayHaveOccurred"], false,
            "a status POST is semantically a read"
        );
        harness.assert_actions(&["getMirroringStatus"]);
    }
}

#[tokio::test]
async fn approval_decline_and_schema_dispatch_are_refused_before_token_or_http() {
    let harness = Harness::new([]);
    harness.credential.failed.store(true, Ordering::SeqCst);
    let mut command = approved();
    command.approve = false;
    assert!(
        start_command::dispatch(
            &command,
            &harness.client,
            &harness.executor,
            OperationLimits::default(),
            pending(),
        )
        .await
        .is_err()
    );
    let schema = start_command::parse_start(args(&["start", "--schema"])).unwrap();
    assert!(
        start_command::dispatch(
            &schema,
            &harness.client,
            &harness.executor,
            OperationLimits::default(),
            pending(),
        )
        .await
        .is_err()
    );
    harness.assert_untouched();
}

#[tokio::test]
async fn already_cancelled_dispatch_has_no_token_probe_start_or_remote_effect() {
    let harness = Harness::new([]);
    let delivery = start_command::dispatch(
        &approved(),
        &harness.client,
        &harness.executor,
        OperationLimits::default(),
        ready(()),
    )
    .await
    .unwrap();
    assert_eq!(delivery.exit, 1);
    assert_common_receipt(&delivery.value);
    assert_eq!(delivery.value["status"], "failed");
    assert_eq!(delivery.value["meta"]["evidence"]["handles"], json!([]));
    assert_eq!(
        delivery.value["meta"]["evidence"]["mutationMayHaveOccurred"],
        false
    );
    harness.assert_untouched();
}

struct PendingStart<'a> {
    harness: &'a Harness,
    dispatched: &'a Notify,
}

impl OperationExecutor for PendingStart<'_> {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> ExecutionFuture<'a, Result<OperationResponse, OperationExecutionError>> {
        if !request.url().path().ends_with("/startMirroring") {
            return self.harness.executor.execute(request);
        }
        self.harness.http.pending.store(true, Ordering::SeqCst);
        Box::pin(async move {
            let mut response = self.harness.executor.execute(request);
            poll_fn(|cx| {
                let result = Future::poll(response.as_mut(), cx);
                if self.harness.http.requests.lock().unwrap().len() == 2 {
                    self.dispatched.notify_one();
                }
                result
            })
            .await
        })
    }

    fn wait(&self, delay: Duration) -> ExecutionFuture<'_, Result<(), OperationExecutionError>> {
        self.harness.executor.wait(delay)
    }
}

#[tokio::test]
async fn cancellation_after_start_dispatch_retains_uncertainty_without_stop_or_replay() {
    let harness = Harness::new([response(200, r#"{"status":"Stopped"}"#, &[])]);
    let dispatched = Notify::new();
    let delivery = tokio::time::timeout(
        Duration::from_secs(5),
        start_command::dispatch(
            &approved(),
            &harness.client,
            &PendingStart {
                harness: &harness,
                dispatched: &dispatched,
            },
            OperationLimits::default(),
            dispatched.notified(),
        ),
    )
    .await
    .expect("dispatched START must observe cancellation within the test deadline")
    .unwrap();
    assert_eq!(delivery.exit, 1);
    assert_common_receipt(&delivery.value);
    assert_eq!(delivery.value["status"], "unknown");
    assert_eq!(
        delivery.value["meta"]["evidence"]["mutationMayHaveOccurred"],
        true
    );
    assert_ne!(
        delivery.value["meta"]["evidence"]["coverage"]["kind"],
        "complete"
    );
    assert_eq!(delivery.value["meta"]["evidence"]["handles"], json!([]));
    harness.assert_actions(&["getMirroringStatus", "startMirroring"]);
}

#[tokio::test]
async fn accepted_or_malformed_start_is_not_replication_success_and_is_never_polled() {
    let operation = "33333333-3333-3333-3333-333333333333";
    let location = format!("{ROOT}/v1/operations/{operation}");
    for (status, body) in [(202, ""), (200, "{}"), (200, r#"{"status":"Running"}"#)] {
        let headers = if status == 202 {
            vec![
                ("x-ms-operation-id", operation),
                ("location", location.as_str()),
                ("retry-after", "1"),
            ]
        } else {
            vec![]
        };
        let harness = Harness::new([
            response(200, r#"{"status":"Stopped"}"#, &[]),
            response(status, body, &headers),
        ]);
        let delivery = harness.dispatch(&approved()).await;
        assert_eq!(delivery.exit, 1);
        assert_common_receipt(&delivery.value);
        assert_eq!(delivery.value["status"], "unknown");
        assert_eq!(delivery.value["meta"]["evidence"]["httpStatus"], status);
        assert_eq!(
            delivery.value["meta"]["evidence"]["handles"],
            if status == 202 {
                json!([{"kind": "fabricOperation", "id": operation}])
            } else {
                json!([])
            }
        );
        assert_eq!(
            delivery.value["meta"]["evidence"]["mutationMayHaveOccurred"],
            true
        );
        harness.assert_actions(&["getMirroringStatus", "startMirroring"]);
    }
}

#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
    flushes: usize,
}

impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

fn assert_one_receipt(writer: &Writer, expected: &Value) {
    assert_eq!(writer.flushes, 1);
    assert_eq!(writer.bytes.last(), Some(&b'\n'));
    assert_eq!(
        writer.bytes.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&writer.bytes).unwrap(),
        *expected
    );
}

#[tokio::test]
async fn actual_common_receipts_use_only_the_designated_stream_and_flush_one_line() {
    for success in [true, false] {
        let harness = Harness::new(if success {
            vec![
                response(200, r#"{"status":"Stopped"}"#, &[]),
                response(200, "", &[]),
            ]
        } else {
            vec![response(200, r#"{"status":"Running"}"#, &[])]
        });
        let delivery = harness.dispatch(&approved()).await;
        let mut stdout = Writer::default();
        let mut stderr = Writer::default();
        let expected_exit = u8::from(!success);
        assert_eq!(delivery.write_to(&mut stdout, &mut stderr), expected_exit);
        if success {
            assert_one_receipt(&stdout, &delivery.value);
            assert!(stderr.bytes.is_empty());
            assert_eq!(stderr.flushes, 0);
        } else {
            assert_one_receipt(&stderr, &delivery.value);
            assert!(stdout.bytes.is_empty());
            assert_eq!(stdout.flushes, 0);
        }
    }
}

struct BrokenWriter {
    fail_flush: bool,
}

impl Write for BrokenWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail_flush {
            Ok(bytes.len())
        } else {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
}

#[tokio::test]
async fn write_or_flush_failure_retains_the_original_common_receipt_without_replay() {
    for fail_flush in [false, true] {
        let harness = Harness::new([
            response(200, r#"{"status":"Stopped"}"#, &[]),
            response(200, "", &[]),
        ]);
        let delivery = harness.dispatch(&approved()).await;
        let original = delivery.value.clone();
        let mut stderr = Writer::default();
        assert_eq!(
            delivery.write_to(&mut BrokenWriter { fail_flush }, &mut stderr),
            1
        );
        let fallback: Value = serde_json::from_slice(&stderr.bytes).unwrap();
        assert_eq!(fallback["error"]["code"], "OutputFailed");
        assert_eq!(fallback["evidence"], original);
        assert_eq!(fallback["remoteCancelled"], false);
        assert_eq!(fallback["remoteReplayed"], false);
        assert_eq!(delivery.value, original);
        assert_one_receipt(&stderr, &fallback);
        harness.assert_actions(&["getMirroringStatus", "startMirroring"]);
    }
}
