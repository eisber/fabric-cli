// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual create parser -> shared host -> SDK -> scripted production HTTP.
#![deny(unnameable_test_items)]

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use azure_core::http::{AsyncRawResponse, Body, Method, headers::Headers};
use fabric_cli::{
    copyjob_create::CopyJobCreateBinding,
    create_command::{
        CreateApproval, CreateApprovalError, CreateApprovalRequest, CreateCommandError,
        CreateDelivery, CreateExecution, CreateTransportEvidence, MAX_BYTES, PreparedCreateCommand,
        command_for_binding, v2_create,
    },
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    items::create::CreateError,
};
use fabric_sdk_http::{ExecutionOptions, ExecutionTrace, HttpExecutor};
use serde_json::{Value, json};
use std::{
    future::{Future, pending, ready},
    io::{self, Read, Write},
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use support::{ScriptedHttp, TestCredential, TestTimer, response};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OP: &str = "33333333-3333-3333-3333-333333333333";
const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact.CopyJob";
const POST: &str = "/v1/workspaces/11111111-1111-1111-1111-111111111111/copyJobs";
type ResponseStream =
    Pin<Box<dyn futures_util::Stream<Item = azure_core::Result<azure_core::Bytes>> + Send + Sync>>;

fn metadata() -> Value {
    json!({"id":ITEM,"workspaceId":WS,"displayName":"Exact","type":"CopyJob"})
}

fn listing(rows: Value) -> AsyncRawResponse {
    response(200, &json!({"value":rows}).to_string(), &[])
}

fn created() -> AsyncRawResponse {
    response(201, &metadata().to_string(), &[])
}

fn accepted(body: &str) -> AsyncRawResponse {
    response(
        202,
        body,
        &[("x-ms-operation-id", OP), ("retry-after", "0")],
    )
}

struct ForbiddenRead;
impl Read for ForbiddenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("this invocation must never consume stdin")
    }
}

fn prepare(options: &[&str], raw: Option<&[u8]>) -> PreparedCreateCommand {
    let args = ["create", TARGET]
        .into_iter()
        .chain(options.iter().copied());
    let command = v2_create::parse_args(args).unwrap();
    let cwd = std::env::current_dir().unwrap();
    match raw {
        Some(mut bytes) => PreparedCreateCommand::preflight(
            command,
            &CopyJobCreateBinding,
            &cwd,
            &mut bytes,
            false,
        ),
        None => PreparedCreateCommand::preflight(
            command,
            &CopyJobCreateBinding,
            &cwd,
            &mut ForbiddenRead,
            false,
        ),
    }
    .unwrap()
}

struct Approval {
    deny: bool,
    calls: usize,
    writes: Vec<bool>,
    raw: Option<Vec<u8>>,
}

impl Approval {
    fn allow(raw: Option<&[u8]>) -> Self {
        Self {
            deny: false,
            calls: 0,
            writes: Vec::new(),
            raw: raw.map(<[u8]>::to_vec),
        }
    }
}

impl CreateApproval for Approval {
    fn authorize<'a>(
        &'a mut self,
        request: CreateApprovalRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
        self.calls += 1;
        self.writes.push(request.remote_write());
        assert_eq!(request.plan().target().name(), "Exact");
        assert_eq!(request.plan().target().item_type(), "CopyJob");
        assert_eq!(request.plan().target().parent_id().as_str(), WS);
        assert_eq!(request.context().selection().account(), "offline");
        assert_eq!(request.context().selection().tenant(), "tenant");
        assert_eq!(request.context().selection().audience(), Audience::Fabric);
        assert_eq!(
            request.context().endpoint(),
            &EndpointBinding::public(ServiceOrigin::Fabric)
        );
        assert_eq!(request.raw_input(), self.raw.as_deref());
        Box::pin(ready(if self.deny {
            Err(CreateApprovalError::Denied)
        } else {
            Ok(())
        }))
    }
}

struct Host {
    http: Arc<ScriptedHttp>,
    credential: Arc<TestCredential>,
    executor: HttpExecutor,
    client: Client,
    trace: ExecutionTrace,
}

impl Host {
    fn new(replies: impl IntoIterator<Item = AsyncRawResponse>) -> Self {
        let http = ScriptedHttp::new(replies);
        let credential = Arc::new(TestCredential::default());
        Self {
            executor: HttpExecutor::new(
                http.clone(),
                Arc::new(TestTimer::default()),
                ExecutionOptions::default(),
            )
            .unwrap(),
            client: Client::new(
                credential.clone(),
                CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
            ),
            http,
            credential,
            trace: ExecutionTrace::default(),
        }
    }

    async fn execute(
        &self,
        prepared: PreparedCreateCommand,
        approval: &mut dyn CreateApproval,
        interrupted: impl Future<Output = ()> + Send,
    ) -> Result<CreateDelivery, CreateCommandError> {
        let observe = || {
            let evidence = self.trace.snapshot();
            CreateTransportEvidence {
                http_status: evidence.status,
                operation_id: evidence.operation_id,
            }
        };
        tokio::time::timeout(
            Duration::from_secs(3),
            prepared.execute(
                &CopyJobCreateBinding,
                &self.client,
                &self.executor,
                CreateExecution {
                    executor: self.executor.clone().with_trace(self.trace.clone()),
                    observe: &observe,
                },
                approval,
                interrupted,
            ),
        )
        .await
        .expect("offline create exceeded its finite test deadline")
    }

    fn assert_posts(&self, count: usize, body: Option<&[u8]>) {
        let requests = self.http.requests.lock().unwrap();
        let posts: Vec<_> = requests
            .iter()
            .filter(|request| request.method() == Method::Post)
            .collect();
        assert_eq!(posts.len(), count, "no mutation replay");
        for request in &*requests {
            assert_eq!(request.url().host_str(), Some("api.fabric.microsoft.com"));
            assert!(request.url().query().is_none());
            assert!(
                request.url().path() == format!("/v1/workspaces/{WS}/items")
                    || request.url().path() == POST
                    || request.url().path() == format!("/v1/operations/{OP}")
                    || request.url().path() == format!("/v1/operations/{OP}/result"),
                "no job, schedule, definition, connection or connector dispatch: {}",
                request.url().path()
            );
        }
        for request in posts {
            assert_eq!(request.url().path(), POST);
            if let Some(expected) = body {
                let Body::Bytes(actual) = request.body() else {
                    panic!("expected exact JSON bytes")
                };
                assert_eq!(actual.as_ref(), expected);
            }
        }
    }
}

fn receipt(delivery: CreateDelivery) -> Value {
    let mut bytes = Vec::new();
    delivery.deliver(&mut bytes).unwrap();
    assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn help_is_selected_for_copyjob_only() {
    let help = command_for_binding(&CopyJobCreateBinding)
        .render_long_help()
        .to_string();
    assert!(help.contains("Selected native owner: CopyJob."));
    assert!(help.contains("/<workspaceGUID>.Workspace/<literal-name>.CopyJob"));
    assert!(help.contains("Enabled policies: error and skip."));
    assert!(!help.contains("Notebook"));
}

#[test]
fn invalid_input_fails_before_discovery_or_identity_including_explicit_skip() {
    let host = Host::new([]);
    for raw in [
        b"".as_slice(),
        b"null",
        b"[]",
        b"{}",
        b"{",
        &[0xff],
        br#"{"displayName":null}"#,
        br#"{"displayName":7}"#,
        br#"{"displayName":"Other"}"#,
        br#"{"displayName":"Exact","displayName":"Exact"}"#,
        br#"{"displayName":"Exact","\u0064isplayName":"Exact"}"#,
        br#"{"displayName":"Exact","description":null}"#,
        br#"{"displayName":"Exact","type":"CopyJob"}"#,
        br#"{"displayName":"Exact","definition":{"parts":[]}}"#,
        br#"{"displayName":"Exact","source":{"connectionId":"private"}}"#,
        br#"{"displayName":"Exact","destination":{}}"#,
        br#"{"displayName":"Exact","folderId":null}"#,
        br#"{"displayName":"Exact","schedule":{}}"#,
        br#"{"displayName":"Exact"} {}"#,
    ] {
        let command =
            v2_create::parse_args(["create", TARGET, "--input", "-", "--if-exists", "skip"])
                .unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &CopyJobCreateBinding,
                &std::env::current_dir().unwrap(),
                &mut &raw[..],
                false,
            )
            .is_err()
        );
    }
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
    host.assert_posts(0, None);
    assert!(host.http.requests.lock().unwrap().is_empty());
}

#[test]
fn invalid_owner_guid_path_profile_and_policy_never_consume_input() {
    let cwd = std::env::current_dir().unwrap();
    for path in [
        "/Name.Workspace/Exact.CopyJob",
        "/11111111-1111-1111-1111-11111111111z.Workspace/Exact.CopyJob",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Notebook",
        "/11111111-1111-1111-1111-111111111111.Workspace/folder/Exact.CopyJob",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.CopyJob/src/definition",
    ] {
        let command = v2_create::parse_args(["create", path, "--input", "-"]).unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &CopyJobCreateBinding,
                &cwd,
                &mut ForbiddenRead,
                false,
            )
            .is_err()
        );
    }
    for options in [
        vec!["--force"],
        vec!["--if-exists", "overwrite"],
        vec!["--if-exists", "duplicate"],
        vec!["--profile", "named"],
    ] {
        for input in ["-", "must-not-open-nonexistent.json"] {
            let command = v2_create::parse_args(
                ["create", TARGET, "--input", input]
                    .into_iter()
                    .chain(options.iter().copied()),
            )
            .unwrap();
            assert!(
                PreparedCreateCommand::preflight(
                    command,
                    &CopyJobCreateBinding,
                    &cwd,
                    &mut ForbiddenRead,
                    false,
                )
                .is_err()
            );
        }
    }
}

#[tokio::test]
async fn minimal_metadata_uses_exact_native_route_and_does_not_invent_definition() {
    let host = Host::new([listing(json!([])), created()]);
    let mut approval = Approval::allow(None);
    let delivery = host
        .execute(prepare(&[], None), &mut approval, pending())
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    let result = receipt(delivery);
    assert_eq!(result["meta"]["status"], "completed");
    assert_eq!(result["meta"]["resourceId"], ITEM);
    assert_eq!(result["meta"]["operationHandle"], Value::Null);
    assert_eq!(result["meta"]["type"], "CopyJob");
    assert_eq!(approval.calls, 1);
    assert_eq!(approval.writes, [true]);
    host.assert_posts(1, Some(br#"{"displayName":"Exact"}"#));
}

#[tokio::test]
async fn explicit_json_is_preserved_byte_for_byte_from_stdin_and_regular_file() {
    let raw = b" {\r\n \"description\" : \"metadata only\", \"displayName\":\"Exact\" }\r\n";
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("input.json"), raw).unwrap();
    for file in [false, true] {
        let host = Host::new([listing(json!([])), created()]);
        let prepared = if file {
            let command =
                v2_create::parse_args(["create", TARGET, "--input", "input.json"]).unwrap();
            PreparedCreateCommand::preflight(
                command,
                &CopyJobCreateBinding,
                temp.path(),
                &mut ForbiddenRead,
                false,
            )
            .unwrap()
        } else {
            prepare(&["--input", "-"], Some(raw))
        };
        host.execute(prepared, &mut Approval::allow(Some(raw)), pending())
            .await
            .unwrap();
        host.assert_posts(1, Some(raw));
    }
}

#[tokio::test]
async fn exact_existing_error_skip_wrong_kind_case_and_ambiguity_are_honest() {
    for (rows, policy, expected) in [
        (
            json!([metadata()]),
            "error",
            Some(CreateError::AlreadyExists),
        ),
        (json!([metadata()]), "skip", None),
        (
            json!([metadata(), metadata()]),
            "skip",
            Some(CreateError::AmbiguousLeaf),
        ),
        (
            json!([{"id":ITEM,"displayName":"Exact","type":"DataPipeline"}]),
            "skip",
            Some(CreateError::WrongKind),
        ),
        (
            json!([{"id":ITEM,"displayName":"exact","type":"CopyJob"}]),
            "skip",
            Some(CreateError::NameChanged),
        ),
    ] {
        let host = Host::new([listing(rows)]);
        let mut approval = Approval::allow(None);
        let result = host
            .execute(
                prepare(&["--if-exists", policy], None),
                &mut approval,
                pending(),
            )
            .await;
        if let Some(expected) = expected {
            assert!(
                matches!(result, Err(CreateCommandError::Policy(actual)) if actual == expected)
            );
            assert_eq!(approval.calls, 0);
        } else {
            let result = receipt(result.unwrap());
            assert_eq!(result["meta"]["status"], "unchanged");
            assert_eq!(result["meta"]["resourceId"], ITEM);
            assert_eq!(result["meta"]["mutationMayHaveOccurred"], false);
            assert_eq!(result["meta"]["contentEqualityClaimed"], false);
            assert_eq!(approval.writes, [false]);
        }
        host.assert_posts(0, None);
    }
}

#[tokio::test]
async fn missing_skip_creates_but_service_collision_races_never_become_unchanged() {
    for status in [400, 409] {
        let host = Host::new([
            listing(json!([])),
            response(
                status,
                r#"{"errorCode":"ItemDisplayNameAlreadyInUse","message":"collision"}"#,
                &[],
            ),
        ]);
        let delivery = host
            .execute(
                prepare(&["--if-exists", "skip"], None),
                &mut Approval::allow(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        let value = receipt(delivery);
        assert_eq!(value["meta"]["status"], "failed");
        assert_eq!(value["meta"]["serviceCode"], "ItemDisplayNameAlreadyInUse");
        assert_eq!(value["meta"]["resourceId"], Value::Null);
        assert_eq!(value["meta"]["remoteReplayed"], false);
        host.assert_posts(1, None);
    }
    let host = Host::new([listing(json!([])), created()]);
    let value = receipt(
        host.execute(
            prepare(&["--if-exists", "skip"], None),
            &mut Approval::allow(None),
            pending(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(value["meta"]["policy"], "skip");
    assert_eq!(value["meta"]["action"], "create");
    host.assert_posts(1, None);
}

#[tokio::test]
async fn rejected_create_statuses_never_retry_or_claim_unchanged() {
    for status in [401, 403, 409, 429, 500] {
        let host = Host::new([
            listing(json!([])),
            response(
                status,
                r#"{"errorCode":"NativeRejected"}"#,
                &[("retry-after", "0")],
            ),
        ]);
        let delivery = host
            .execute(prepare(&[], None), &mut Approval::allow(None), pending())
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(delivery.report().value()["meta"]["status"], "failed");
        assert_eq!(delivery.report().value()["meta"]["httpStatus"], status);
        host.assert_posts(1, None);
    }
}

#[tokio::test]
async fn approval_denial_and_wrong_audience_or_endpoint_produce_no_post() {
    let host = Host::new([listing(json!([]))]);
    let mut approval = Approval::allow(None);
    approval.deny = true;
    assert!(matches!(
        host.execute(prepare(&[], None), &mut approval, pending())
            .await,
        Err(CreateCommandError::Approval(CreateApprovalError::Denied))
    ));
    host.assert_posts(0, None);
    for (audience, origin) in [
        (Audience::OneLake, ServiceOrigin::Fabric),
        (Audience::Fabric, ServiceOrigin::Arm),
    ] {
        let mut host = Host::new([]);
        host.client = Client::with_context(RequestContext::with_endpoint(
            host.credential.clone(),
            CredentialSelection::new("offline", "tenant", audience).unwrap(),
            EndpointBinding::public(origin),
        ));
        assert!(matches!(
            host.execute(prepare(&[], None), &mut Approval::allow(None), pending())
                .await,
            Err(CreateCommandError::Invalid)
        ));
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
        assert!(host.http.requests.lock().unwrap().is_empty());
    }
    for invalid in [
        "http://api.fabric.microsoft.com",
        "https://evil.invalid/path",
        "not-a-host",
    ] {
        assert!(EndpointBinding::with_host(ServiceOrigin::Fabric, invalid).is_err());
    }
}

#[tokio::test]
async fn accepted_and_completed_wait_keep_resource_and_operation_ids_separate() {
    for wait in [false, true] {
        let mut responses = vec![listing(json!([])), accepted(&metadata().to_string())];
        if wait {
            responses.push(response(200, r#"{"status":"Succeeded"}"#, &[]));
            responses.push(response(200, &metadata().to_string(), &[]));
        }
        let host = Host::new(responses);
        let delivery = host
            .execute(
                prepare(if wait { &["--wait"] } else { &[] }, None),
                &mut Approval::allow(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 0);
        let result = receipt(delivery);
        assert_eq!(
            result["meta"]["status"],
            if wait { "completed" } else { "accepted" }
        );
        assert_eq!(result["meta"]["completed"], wait);
        assert_eq!(result["meta"]["resourceId"], ITEM);
        assert_eq!(
            result["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        host.assert_posts(1, None);
    }
}

#[tokio::test]
async fn lro_failures_preserve_acceptance_identity_and_never_resubmit() {
    for body in [
        r#"{"status":"Failed","error":{"errorCode":"ProvisioningFailed","message":"synthetic"}}"#,
        r#"{"status":"FutureState"}"#,
        r#"{"status":null}"#,
    ] {
        let host = Host::new([
            listing(json!([])),
            accepted(&metadata().to_string()),
            response(200, body, &[]),
        ]);
        let delivery = host
            .execute(
                prepare(&["--wait"], None),
                &mut Approval::allow(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        let result = receipt(delivery);
        assert_eq!(
            result["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(result["meta"]["resourceId"], ITEM);
        assert_eq!(result["meta"]["remoteCancelled"], false);
        assert_eq!(result["meta"]["remoteReplayed"], false);
        assert_ne!(result["meta"]["status"], "completed");
        host.assert_posts(1, None);
    }
}

#[tokio::test]
async fn malformed_lro_result_keeps_previously_observed_acceptance_identity() {
    for body in ["{", "{}", r#"{"type":"CopyJob"}"#] {
        let host = Host::new([
            listing(json!([])),
            accepted(&metadata().to_string()),
            response(200, r#"{"status":"Succeeded"}"#, &[]),
            response(200, body, &[]),
        ]);
        let delivery = host
            .execute(
                prepare(&["--wait"], None),
                &mut Approval::allow(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        let value = receipt(delivery);
        assert_eq!(value["meta"]["status"], "unknown");
        assert_eq!(value["meta"]["resourceId"], ITEM);
        assert_eq!(
            value["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(value["meta"]["remoteReplayed"], false);
        host.assert_posts(1, None);
    }
}

#[tokio::test]
async fn malformed_success_is_unknown_with_observed_identity_not_success() {
    for (status, body) in [
        (200, metadata().to_string()),
        (201, "{}".into()),
        (201, r#"{"id":"not-a-guid","type":"CopyJob"}"#.into()),
        (
            201,
            metadata().to_string().replace("CopyJob", "DataPipeline"),
        ),
        (201, metadata().to_string().replace(WS, OP)),
        (201, metadata().to_string().replace("Exact", "Other")),
        (201, r#"{"id":"one","id":"two"}"#.into()),
    ] {
        let host = Host::new([listing(json!([])), response(status, &body, &[])]);
        let delivery = host
            .execute(prepare(&[], None), &mut Approval::allow(None), pending())
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(delivery.report().value()["meta"]["status"], "unknown");
        host.assert_posts(1, None);
    }
}

#[tokio::test]
async fn body_loss_overflow_and_cancellation_keep_received_202_header_handle() {
    for mode in ["lost", "overflow", "cancel"] {
        let mut headers = Headers::new();
        headers.insert("x-ms-operation-id", OP);
        let stream: ResponseStream = match mode {
            "lost" => Box::pin(futures_util::stream::once(ready(Err(
                azure_core::Error::with_message(
                    azure_core::error::ErrorKind::Io,
                    "synthetic body loss",
                ),
            )))),
            "overflow" => Box::pin(futures_util::stream::once(ready(Ok(vec![
                b'x';
                MAX_BYTES + 1
            ]
            .into())))),
            _ => Box::pin(futures_util::stream::pending()),
        };
        let host = Host::new([
            listing(json!([])),
            AsyncRawResponse::new(202.into(), headers, stream),
        ]);
        let interrupted = async {
            if mode != "cancel" {
                pending::<()>().await;
            }
            while host.http.requests.lock().unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
        };
        let delivery = host
            .execute(prepare(&[], None), &mut Approval::allow(None), interrupted)
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        let result = receipt(delivery);
        assert_eq!(
            result["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(result["meta"]["httpStatus"], 202);
        assert_eq!(result["meta"]["resourceId"], Value::Null);
        assert_eq!(result["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(result["meta"]["remoteReplayed"], false);
        host.assert_posts(1, None);
    }
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed pipe"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("closed pipe"))
    }
}

struct FlushFailure;
impl Write for FlushFailure {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("flush failed"))
    }
}

#[tokio::test]
async fn receipt_flush_failure_retains_acceptance_and_never_resubmits() {
    let host = Host::new([listing(json!([])), accepted(&metadata().to_string())]);
    let delivery = host
        .execute(prepare(&[], None), &mut Approval::allow(None), pending())
        .await
        .unwrap();
    let Err(CreateCommandError::Delivery { report, .. }) = delivery.deliver(&mut FlushFailure)
    else {
        panic!("flush failure must retain original report");
    };
    assert_eq!(
        report.value()["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(report.value()["meta"]["resourceId"], ITEM);
    host.assert_posts(1, None);
}

#[tokio::test]
async fn projection_and_failed_delivery_preserve_unprojected_acceptance() {
    for broken in [false, true] {
        let host = Host::new([listing(json!([])), accepted(&metadata().to_string())]);
        let delivery = host
            .execute(
                prepare(&["--project", "missing"], None),
                &mut Approval::allow(None),
                pending(),
            )
            .await
            .unwrap();
        let value = if broken {
            let Err(CreateCommandError::Delivery { report, .. }) =
                delivery.deliver(&mut BrokenWriter)
            else {
                panic!("failed writer must retain receipt");
            };
            assert_eq!(report.value()["data"]["id"], ITEM);
            report.value().clone()
        } else {
            let value = receipt(delivery);
            assert_eq!(value["data"], Value::Null);
            value
        };
        assert_eq!(value["meta"]["resourceId"], ITEM);
        assert_eq!(
            value["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(value["meta"]["status"], "accepted");
        host.assert_posts(1, None);
    }
}

#[tokio::test]
async fn output_race_is_no_clobber_and_keeps_remote_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("receipt.json");
    let host = Host::new([listing(json!([])), created()]);
    let prepared = prepare(&["--output", path.to_str().unwrap()], None);
    assert!(!path.exists());
    let delivery = host
        .execute(prepared, &mut Approval::allow(None), pending())
        .await
        .unwrap();
    std::fs::write(&path, b"preserve").unwrap();
    let Err(CreateCommandError::Delivery { report, .. }) = delivery.deliver(&mut Vec::new()) else {
        panic!("output race must not overwrite");
    };
    assert_eq!(report.value()["meta"]["resourceId"], ITEM);
    assert_eq!(std::fs::read(path).unwrap(), b"preserve");
    host.assert_posts(1, None);
}

#[test]
fn input_byte_limit_is_enforced_without_remote_effects() {
    let raw = vec![b' '; MAX_BYTES + 1];
    let command = v2_create::parse_args(["create", TARGET, "--input", "-"]).unwrap();
    assert!(matches!(
        PreparedCreateCommand::preflight(
            command,
            &CopyJobCreateBinding,
            &std::env::current_dir().unwrap(),
            &mut raw.as_slice(),
            false,
        ),
        Err(CreateCommandError::Invalid)
    ));
}

#[tokio::test]
async fn denied_incomplete_and_wrong_workspace_discovery_never_authorize_create() {
    for reply in [
        response(403, r#"{"errorCode":"Forbidden"}"#, &[]),
        response(
            200,
            &json!({
                "value":[], "continuationUri":"https://evil.invalid/next"
            })
            .to_string(),
            &[],
        ),
        listing(json!([{"id":ITEM,"workspaceId":OP,"displayName":"Exact","type":"CopyJob"}])),
        listing(json!([{"id":"malformed","displayName":"Exact","type":"CopyJob"}])),
    ] {
        let host = Host::new([reply]);
        let mut approval = Approval::allow(None);
        assert!(
            host.execute(prepare(&[], None), &mut approval, pending())
                .await
                .is_err()
        );
        assert_eq!(approval.calls, 0);
        host.assert_posts(0, None);
    }
}

struct WaitingApproval;
impl CreateApproval for WaitingApproval {
    fn authorize<'a>(
        &'a mut self,
        _: CreateApprovalRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
        Box::pin(pending())
    }
}

#[tokio::test]
async fn original_deadline_includes_approval_and_never_starts_a_fresh_budget() {
    let host = Host::new([listing(json!([]))]);
    let started = std::time::Instant::now();
    let interrupted = tokio::time::sleep(Duration::from_millis(10));
    assert!(matches!(
        host.execute(prepare(&[], None), &mut WaitingApproval, interrupted)
            .await,
        Err(CreateCommandError::Interrupted)
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    host.assert_posts(0, None);
}

#[tokio::test]
async fn interrupted_poll_retains_accepted_body_and_handle_without_remote_cancel() {
    let mut headers = Headers::new();
    headers.insert("content-type", "application/json");
    let host = Host::new([
        listing(json!([])),
        accepted(&metadata().to_string()),
        AsyncRawResponse::new(
            200.into(),
            headers,
            Box::pin(futures_util::stream::pending()),
        ),
    ]);
    let interrupted = async {
        while host.http.requests.lock().unwrap().len() < 3 {
            tokio::task::yield_now().await;
        }
    };
    let delivery = host
        .execute(
            prepare(&["--wait"], None),
            &mut Approval::allow(None),
            interrupted,
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    let value = receipt(delivery);
    assert_eq!(
        value["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(value["meta"]["resourceId"], ITEM);
    assert_eq!(value["meta"]["status"], "unknown");
    assert_eq!(value["meta"]["remoteCancelled"], false);
    host.assert_posts(1, None);
}

#[cfg(feature = "headless")]
mod binary {
    use super::*;
    use std::{
        process::{Child, Command, Output, Stdio},
        thread,
        time::Instant,
    };

    struct Process(Child);
    impl Drop for Process {
        fn drop(&mut self) {
            if self.0.try_wait().unwrap().is_none() {
                self.0.kill().unwrap();
                self.0.wait().unwrap();
            }
        }
    }

    fn command(args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
        command
            .args(args)
            .env_remove("FAB_RUST_AUTH_MODE")
            .env_remove("FAB_RUST_CLIENT_SECRET")
            .env_remove("FAB_RUST_ASSERTION_FILE")
            .env_remove("FAB_RUST_FABRIC_HOST")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(mut command: Command) -> Output {
        let mut child = Process(command.spawn().unwrap());
        drop(command);
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.0.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "offline binary exceeded its bounded deadline"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(mut out) = child.0.stdout.take() {
            out.read_to_end(&mut stdout).unwrap();
        }
        if let Some(mut err) = child.0.stderr.take() {
            err.read_to_end(&mut stderr).unwrap();
        }
        Output {
            status: child.0.wait().unwrap(),
            stdout,
            stderr,
        }
    }

    #[test]
    fn real_binary_exposes_only_opt_in_copyjob_create_help_and_no_ambient_identity() {
        for args in [vec!["--help"], vec!["create", "--help"]] {
            let output = run(command(&args));
            assert!(output.status.success());
            assert!(String::from_utf8_lossy(&output.stdout).contains(".CopyJob"));
            assert!(output.stderr.is_empty());
        }
        let output = run(command(&["create", TARGET]));
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let value: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(value["error"]["code"], "ApprovalRequired");
    }

    #[test]
    fn real_binary_rejects_bad_schema_and_unsupported_policy_before_identity_or_input() {
        for options in [
            vec!["--force"],
            vec!["--if-exists", "overwrite"],
            vec!["--if-exists", "duplicate"],
        ] {
            let mut args = vec!["create", TARGET, "--input", "-"];
            args.extend(options);
            let mut invocation = command(&args);
            // Keep the write end open while the child runs: an input read would hang.
            invocation.stdin(Stdio::piped());
            let output = run(invocation);
            assert_eq!(output.status.code(), Some(2));
            let value: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(value["error"]["code"], "CreatePolicyRejected");
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("input.json");
        for raw in [
            br#"{"displayName":"Exact","definition":{}}"#.as_slice(),
            br#"{"displayName":"Exact","displayName":"Exact"}"#,
            b"null",
        ] {
            std::fs::write(&path, raw).unwrap();
            let output = run(command(&[
                "create",
                TARGET,
                "--input",
                path.to_str().unwrap(),
            ]));
            assert_eq!(output.status.code(), Some(2));
            assert!(output.stdout.is_empty());
            let value: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert!(matches!(
                value["error"]["code"].as_str(),
                Some("InvalidInput" | "CreatePolicyRejected")
            ));
        }
    }

    #[test]
    fn real_binary_original_deadline_bounds_blocked_stdin_without_identity() {
        let mut invocation = command(&["create", TARGET, "--input", "-", "--timeout", "20ms"]);
        invocation.stdin(Stdio::piped());
        let started = Instant::now();
        let output = run(invocation);
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let value: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(value["error"]["code"], "TimedOut");
    }

    #[test]
    fn real_binary_closed_reader_stdout_and_stderr_pipes_fail_portably() {
        for stdout in [true, false] {
            let (reader, writer) = io::pipe().unwrap();
            drop(reader);
            let mut invocation = if stdout {
                command(&["create", "--help"])
            } else {
                command(&["create", TARGET])
            };
            if stdout {
                invocation.stdout(writer);
            } else {
                invocation.stderr(writer);
            }
            let output = run(invocation);
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            if stdout {
                let value: Value = serde_json::from_slice(&output.stderr).unwrap();
                assert_eq!(value["error"]["code"], "OutputFailed");
            }
        }
    }
}
