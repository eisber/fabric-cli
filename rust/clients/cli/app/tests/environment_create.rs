// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic parser -> shared host -> SDK -> real HTTP adapter contracts.
#![deny(unnameable_test_items)]

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use azure_core::http::{AsyncRawResponse, Body, Method};
use fabric_cli::{
    create_command::{
        CreateApproval, CreateApprovalError, CreateApprovalRequest, CreateBinding,
        CreateCommandError, CreateDelivery, CreateExecution, CreateTransportEvidence, MAX_BYTES,
        PreparedCreateCommand, v2_create,
    },
    environment_create::EnvironmentCreate,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    items::create::{
        self, ConflictPolicy, CreateAction, CreateError, CreateTarget, LeafState, NativeId,
        Preflight, SchemaRef, TargetScope,
    },
};
use fabric_sdk_http::{ExecutionOptions, ExecutionTrace, HttpExecutor};
use serde_json::{Value, json};
use std::{
    future::{Future, pending},
    io::{self, Read, Write},
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use support::{ScriptedHttp, TestCredential, TestTimer, response};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OP: &str = "33333333-3333-3333-3333-333333333333";
const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Environment";

fn resource() -> Value {
    json!({"id":ITEM,"workspaceId":WS,"displayName":"Exact","type":"Environment"})
}

fn listed(values: Vec<Value>) -> AsyncRawResponse {
    response(200, &json!({"value":values}).to_string(), &[])
}

fn accepted(body: &str) -> AsyncRawResponse {
    response(
        202,
        body,
        &[
            ("x-ms-operation-id", OP),
            (
                "location",
                "https://api.fabric.microsoft.com/v1/operations/33333333-3333-3333-3333-333333333333",
            ),
            ("retry-after", "1"),
        ],
    )
}

struct ForbiddenRead;
impl Read for ForbiddenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("this invocation must not consume input")
    }
}

fn prepare(options: &[&str], input: Option<&[u8]>, cwd: &std::path::Path) -> PreparedCreateCommand {
    let command = v2_create::parse_args(
        ["create", TARGET]
            .into_iter()
            .chain(options.iter().copied()),
    )
    .unwrap();
    let mut bytes = input.unwrap_or_default();
    PreparedCreateCommand::preflight(command, &EnvironmentCreate, cwd, &mut bytes, false).unwrap()
}

struct Approval {
    allow: bool,
    calls: usize,
    input: Option<Value>,
}

impl Approval {
    fn allowing(input: Option<Value>) -> Self {
        Self {
            allow: true,
            calls: 0,
            input,
        }
    }
}

impl CreateApproval for Approval {
    fn authorize<'a>(
        &'a mut self,
        request: CreateApprovalRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
        self.calls += 1;
        assert_eq!(request.plan().target().parent_id().as_str(), WS);
        assert_eq!(request.plan().target().name(), "Exact");
        assert_eq!(request.plan().target().item_type(), "Environment");
        assert_eq!(request.context().selection().account(), "offline");
        assert_eq!(request.context().selection().tenant(), "synthetic");
        assert_eq!(request.context().selection().audience(), Audience::Fabric);
        assert_eq!(
            request.context().endpoint().host(),
            "api.fabric.microsoft.com"
        );
        assert_eq!(
            request
                .raw_input()
                .map(|bytes| serde_json::from_slice::<Value>(bytes).unwrap()),
            self.input,
        );
        assert_eq!(
            request.remote_write(),
            matches!(request.plan().action(), CreateAction::Create { .. })
        );
        let allow = self.allow;
        Box::pin(async move {
            if allow {
                Ok(())
            } else {
                Err(CreateApprovalError::Denied)
            }
        })
    }
}

struct Rig {
    client: Client,
    http: Arc<ScriptedHttp>,
    credential: Arc<TestCredential>,
    timer: Arc<TestTimer>,
    executor: HttpExecutor,
    trace: ExecutionTrace,
}

impl Rig {
    fn new(replies: impl IntoIterator<Item = AsyncRawResponse>) -> Self {
        let http = ScriptedHttp::new(replies);
        let timer = Arc::new(TestTimer::default());
        let credential = Arc::new(TestCredential::default());
        let client = Client::with_context(RequestContext::with_endpoint(
            credential.clone(),
            CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
            EndpointBinding::public(ServiceOrigin::Fabric),
        ));
        let trace = ExecutionTrace::default();
        let executor =
            HttpExecutor::new(http.clone(), timer.clone(), ExecutionOptions::default()).unwrap();
        Self {
            client,
            http,
            credential,
            timer,
            executor,
            trace,
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
        prepared
            .execute(
                &EnvironmentCreate,
                &self.client,
                &self.executor,
                CreateExecution {
                    executor: self.executor.clone().with_trace(self.trace.clone()),
                    observe: &observe,
                },
                approval,
                interrupted,
            )
            .await
    }

    fn assert_requests(&self, posts: usize) {
        let requests = self.http.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method() == Method::Post)
                .count(),
            posts
        );
        for request in requests.iter() {
            assert_eq!(request.url().host_str(), Some("api.fabric.microsoft.com"));
            match request.method() {
                Method::Post => assert_eq!(
                    request.url().path(),
                    format!("/v1/workspaces/{WS}/environments")
                ),
                Method::Get => assert!(
                    request.url().path() == format!("/v1/workspaces/{WS}/items")
                        || request.url().path() == format!("/v1/operations/{OP}")
                        || request.url().path() == format!("/v1/operations/{OP}/result")
                ),
                method => panic!("unexpected effect: {method:?}"),
            }
        }
    }
}

#[test]
fn strict_metadata_schema_and_target_validation_precede_effects() {
    let temp = tempfile::tempdir().unwrap();
    for bytes in [
        b"".as_slice(),
        b"null",
        b"[]",
        b"{}",
        b"{",
        b"{}{}",
        br#"{"displayName":null}"#,
        br#"{"displayName":"Other"}"#,
        br#"{"displayName":"Exact","displayName":"Exact"}"#,
        br#"{"displayName":"Exact","\u0064isplayName":"Exact"}"#,
        br#"{"displayName":"Exact","description":null}"#,
        br#"{"displayName":"Exact","type":null}"#,
        br#"{"displayName":"Exact","type":"Notebook"}"#,
        br#"{"displayName":"Exact","type":"environment"}"#,
        br#"{"displayName":"Exact","folderId":"22222222-2222-2222-2222-222222222222"}"#,
        br#"{"displayName":"Exact","definition":{}}"#,
        br#"{"displayName":"Exact","libraries":[]}"#,
        br#"{"displayName":"Exact","settings":{}}"#,
        br#"{"displayName":"Exact","creationPayload":{}}"#,
        br#"{"displayName":"Exact","sensitivityLabelSettings":{}}"#,
        br#"{"displayName":"Exact","unknown":true}"#,
    ] {
        let command =
            v2_create::parse_args(["create", TARGET, "--input", "-", "--if-exists", "skip"])
                .unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &EnvironmentCreate,
                temp.path(),
                &mut &bytes[..],
                false
            )
            .is_err()
        );
    }
    for bytes in [
        vec![b' '; MAX_BYTES + 1],
        serde_json::to_vec(&json!({"displayName":"Exact","description":"x".repeat(257)})).unwrap(),
    ] {
        let command = v2_create::parse_args(["create", TARGET, "--input", "-"]).unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &EnvironmentCreate,
                temp.path(),
                &mut &bytes[..],
                false
            )
            .is_err()
        );
    }
    for target in [
        "/Name.Workspace/Exact.Environment",
        "/11111111-1111-1111-1111-11111111111Z.Workspace/Exact.Environment",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Notebook",
        "/11111111-1111-1111-1111-111111111111.Workspace/Folder.Folder/Exact.Environment",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Environment/src/definition",
        "/11111111-1111-1111-1111-111111111111.Workspace",
    ] {
        let command = v2_create::parse_args(["create", target, "--input", "-"]).unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &EnvironmentCreate,
                temp.path(),
                &mut ForbiddenRead,
                false
            )
            .is_err()
        );
    }
}

#[test]
fn unsupported_policies_fail_before_file_or_stdin_access() {
    let temp = tempfile::tempdir().unwrap();
    for policy in [
        vec!["--force"],
        vec!["--if-exists", "overwrite"],
        vec!["--if-exists", "duplicate"],
    ] {
        for input in ["-", "missing.json"] {
            let command = v2_create::parse_args(
                ["create", TARGET, "--input", input]
                    .into_iter()
                    .chain(policy.iter().copied()),
            )
            .unwrap();
            assert!(matches!(
                PreparedCreateCommand::preflight(
                    command,
                    &EnvironmentCreate,
                    temp.path(),
                    &mut ForbiddenRead,
                    false
                ),
                Err(CreateCommandError::Policy(CreateError::UnsupportedPolicy))
            ));
        }
    }
}

#[test]
fn metadata_limits_are_explicit_and_guid_looking_leaf_names_remain_literal() {
    let temp = tempfile::tempdir().unwrap();
    let bytes =
        serde_json::to_vec(&json!({"displayName":"Exact","description":"\u{e9}".repeat(256)}))
            .unwrap();
    let _ = prepare(&["--input", "-"], Some(&bytes), temp.path());
    for (name, valid) in [
        (ITEM.to_owned(), true),
        ("x".repeat(256), true),
        ("x".repeat(257), false),
    ] {
        let target = format!("/{WS}.Workspace/{name}.Environment");
        let command = v2_create::parse_args(["create", target.as_str()]).unwrap();
        assert_eq!(
            PreparedCreateCommand::preflight(
                command,
                &EnvironmentCreate,
                temp.path(),
                &mut ForbiddenRead,
                false
            )
            .is_ok(),
            valid
        );
    }
}

#[test]
fn binding_revalidates_schema_and_raw_input_instead_of_trusting_arbitrary_json() {
    let target = CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WS).unwrap(),
        "Exact",
        "Environment",
    )
    .unwrap();
    let plan = create::plan_create(
        ConflictPolicy::Error,
        target,
        Preflight::Leaf(LeafState::Missing),
        &EnvironmentCreate.capabilities(),
        Some(json!({"displayName":"Exact"})),
        &EnvironmentCreate,
    )
    .unwrap();
    assert!(
        EnvironmentCreate
            .operation(&plan, Some(br#"{"displayName":"Other"}"#), false)
            .is_err()
    );
    assert!(EnvironmentCreate.operation(&plan, None, false).is_err());
    assert!(
        fabric_sdk::items::create::CreateInputValidator::validate(
            &EnvironmentCreate,
            plan.target(),
            &SchemaRef::new("wrong/v1").unwrap(),
            None,
        )
        .is_err()
    );
}

#[tokio::test]
async fn real_http_post_is_definition_free_and_receipt_is_the_provider_result_not_the_request() {
    let temp = tempfile::tempdir().unwrap();
    for input in [
        None,
        Some(br#"{"displayName":"Exact","type":"Environment","folderId":null,"description":"Synthetic metadata"}"#.as_slice()),
    ] {
        let options: &[&str] = if input.is_some() { &["--input", "-"] } else { &[] };
        let rig = Rig::new([listed(vec![]), response(201, &resource().to_string(), &[])]);
        let mut approval = Approval::allowing(input.map(|bytes| serde_json::from_slice(bytes).unwrap()));
        let delivery = rig.execute(prepare(options, input, temp.path()), &mut approval, pending()).await.unwrap();
        assert_eq!(approval.calls, 1);
        assert_eq!(delivery.report().exit_code(), 0);
        assert_eq!(delivery.report().value()["data"], resource());
        assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
        assert_eq!(delivery.report().value()["meta"]["completed"], true);
        rig.assert_requests(1);
        let requests = rig.http.requests.lock().unwrap();
        let Body::Bytes(bytes) = requests[1].body() else { panic!("native JSON bytes") };
        let mut expected = json!({"displayName":"Exact","folderId":null});
        if input.is_some() { expected["description"] = json!("Synthetic metadata"); }
        assert_eq!(serde_json::from_slice::<Value>(bytes).unwrap(), expected);
    }
}

#[tokio::test]
async fn exact_skip_collision_case_ambiguity_and_wrong_kind_never_post() {
    let temp = tempfile::tempdir().unwrap();
    for (values, policy, skip) in [
        (vec![resource()], "skip", true),
        (vec![resource()], "error", false),
        (vec![resource(), resource()], "skip", false),
        (
            vec![json!({"id":ITEM,"displayName":"exact","type":"Environment"})],
            "skip",
            false,
        ),
        (
            vec![json!({"id":ITEM,"displayName":"Exact","type":"Notebook"})],
            "skip",
            false,
        ),
        (
            vec![json!({"id":"not-guid","displayName":"Exact","type":"Environment"})],
            "skip",
            false,
        ),
    ] {
        let rig = Rig::new([listed(values)]);
        let mut approval = Approval::allowing(None);
        let result = rig
            .execute(
                prepare(&["--if-exists", policy], None, temp.path()),
                &mut approval,
                pending(),
            )
            .await;
        if skip {
            let delivery = result.unwrap();
            assert_eq!(delivery.report().value()["meta"]["action"], "skip");
            assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
            assert_eq!(
                delivery.report().value()["meta"]["contentEqualityClaimed"],
                false
            );
            assert_eq!(approval.calls, 1);
        } else {
            assert!(result.is_err());
            assert_eq!(approval.calls, 0);
        }
        rig.assert_requests(0);
    }
}

#[tokio::test]
async fn denial_and_context_mismatch_have_zero_mutations_or_identity_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let rig = Rig::new([listed(vec![])]);
    let mut approval = Approval {
        allow: false,
        calls: 0,
        input: None,
    };
    assert!(matches!(
        rig.execute(prepare(&[], None, temp.path()), &mut approval, pending())
            .await,
        Err(CreateCommandError::Approval(CreateApprovalError::Denied))
    ));
    rig.assert_requests(0);
    for (audience, origin) in [
        (Audience::Arm, ServiceOrigin::Arm),
        (Audience::OneLake, ServiceOrigin::OneLake),
    ] {
        let mut rig = Rig::new([]);
        rig.client = Client::with_context(RequestContext::with_endpoint(
            rig.credential.clone(),
            CredentialSelection::new("offline", "synthetic", audience).unwrap(),
            EndpointBinding::public(origin),
        ));
        assert!(matches!(
            rig.execute(
                prepare(&[], None, temp.path()),
                &mut Approval::allowing(None),
                pending()
            )
            .await,
            Err(CreateCommandError::Invalid)
        ));
        assert!(rig.http.requests.lock().unwrap().is_empty());
        assert_eq!(rig.credential.calls.load(Ordering::SeqCst), 0);
    }
    assert!(EndpointBinding::with_host(ServiceOrigin::Fabric, "https://foreign.invalid").is_err());
}

#[tokio::test]
async fn accepted_is_not_completed_and_wait_uses_the_same_operation_without_replay() {
    let temp = tempfile::tempdir().unwrap();
    for wait in [false, true] {
        let mut replies = vec![listed(vec![]), accepted("")];
        if wait {
            replies.push(response(200, r#"{"status":"Succeeded"}"#, &[]));
            replies.push(response(200, &resource().to_string(), &[]));
        }
        let rig = Rig::new(replies);
        let options: &[&str] = if wait { &["--wait"] } else { &[] };
        let delivery = rig
            .execute(
                prepare(options, None, temp.path()),
                &mut Approval::allowing(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 0);
        assert_eq!(delivery.report().value()["meta"]["completed"], wait);
        assert_eq!(
            delivery.report().value()["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(
            delivery.report().value()["meta"]["resourceId"],
            if wait { json!(ITEM) } else { Value::Null }
        );
        assert_eq!(delivery.report().value()["meta"]["remoteReplayed"], false);
        rig.assert_requests(1);
        assert_eq!(
            rig.http.requests.lock().unwrap().len(),
            if wait { 4 } else { 2 }
        );
    }
}

#[tokio::test]
async fn provider_collision_and_native_errors_are_not_raced_skip_or_success() {
    let temp = tempfile::tempdir().unwrap();
    for status in [400, 409, 429, 500] {
        let rig = Rig::new([
            listed(vec![]),
            response(
                status,
                r#"{"errorCode":"ItemDisplayNameAlreadyInUse","message":"Synthetic collision"}"#,
                &[],
            ),
        ]);
        let delivery = rig
            .execute(
                prepare(&["--if-exists", "skip"], None, temp.path()),
                &mut Approval::allowing(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(delivery.report().value()["meta"]["httpStatus"], status);
        assert_eq!(
            delivery.report().value()["meta"]["serviceCode"],
            "ItemDisplayNameAlreadyInUse"
        );
        assert_ne!(delivery.report().value()["meta"]["action"], "skip");
        rig.assert_requests(1);
        assert_eq!(rig.http.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn failed_poll_retains_accepted_resource_and_operation_as_distinct_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let rig = Rig::new([
        listed(vec![]),
        accepted(&resource().to_string()),
        response(
            200,
            r#"{"status":"Failed","error":{"errorCode":"SyntheticProvisioningFailure","message":"synthetic"}}"#,
            &[],
        ),
    ]);
    let delivery = rig
        .execute(
            prepare(&["--wait"], None, temp.path()),
            &mut Approval::allowing(None),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
    assert_eq!(
        delivery.report().value()["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(delivery.report().value()["meta"]["completed"], false);
    assert_eq!(
        delivery.report().value()["meta"]["mutationMayHaveOccurred"],
        true
    );
    rig.assert_requests(1);
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("synthetic blocked pipe"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn projection_no_clobber_and_broken_output_retain_identity_and_effects() {
    let temp = tempfile::tempdir().unwrap();
    let rig = Rig::new([listed(vec![]), response(201, &resource().to_string(), &[])]);
    let delivery = rig
        .execute(
            prepare(&["--project", "displayName"], None, temp.path()),
            &mut Approval::allowing(None),
            pending(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    delivery.deliver(&mut bytes).unwrap();
    let receipt: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(receipt["data"], "Exact");
    assert_eq!(receipt["meta"]["resourceId"], ITEM);
    rig.assert_requests(1);
    for file in [false, true] {
        let rig = Rig::new([listed(vec![]), accepted(&resource().to_string())]);
        let options: &[&str] = if file {
            &["--output", "receipt.json"]
        } else {
            &[]
        };
        let prepared = prepare(options, None, temp.path());
        if file {
            std::fs::write(temp.path().join("receipt.json"), b"keep").unwrap();
        }
        let delivery = rig
            .execute(prepared, &mut Approval::allowing(None), pending())
            .await
            .unwrap();
        let Err(CreateCommandError::Delivery { report, .. }) = delivery.deliver(&mut BrokenWriter)
        else {
            panic!("delivery must fail with retained report")
        };
        assert_eq!(
            report.value()["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(report.value()["meta"]["resourceId"], ITEM);
        assert_eq!(report.value()["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(report.value()["meta"]["completed"], false);
        assert_eq!(report.value()["meta"]["remoteCancelled"], false);
        rig.assert_requests(1);
        if file {
            assert_eq!(
                std::fs::read(temp.path().join("receipt.json")).unwrap(),
                b"keep"
            );
        }
    }
}

#[tokio::test]
async fn cancellation_before_credentials_and_during_wait_keeps_actual_acceptance() {
    let temp = tempfile::tempdir().unwrap();
    let rig = Rig::new([]);
    assert!(
        rig.execute(
            prepare(&[], None, temp.path()),
            &mut Approval::allowing(None),
            async {}
        )
        .await
        .is_err()
    );
    assert_eq!(rig.credential.calls.load(Ordering::SeqCst), 0);
    assert!(rig.http.requests.lock().unwrap().is_empty());
    let rig = Rig::new([listed(vec![]), accepted(&resource().to_string())]);
    rig.timer.pending_wait.store(true, Ordering::SeqCst);
    let cancelled = async {
        loop {
            if rig.http.requests.lock().unwrap().len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    };
    let delivery = tokio::time::timeout(
        Duration::from_secs(2),
        rig.execute(
            prepare(&["--wait"], None, temp.path()),
            &mut Approval::allowing(None),
            cancelled,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(
        delivery.report().value()["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
    assert_eq!(delivery.report().value()["meta"]["remoteCancelled"], false);
    assert_eq!(delivery.report().value()["meta"]["remoteReplayed"], false);
    rig.assert_requests(1);
}

#[tokio::test]
async fn invalid_completion_and_bounded_discovery_fail_without_false_success() {
    let temp = tempfile::tempdir().unwrap();
    for result in [
        response(
            201,
            r#"{"id":"invalid","displayName":"Exact","type":"Environment","workspaceId":"11111111-1111-1111-1111-111111111111"}"#,
            &[],
        ),
        response(
            201,
            &resource().to_string().replace("Environment", "Notebook"),
            &[],
        ),
        response(200, &resource().to_string(), &[]),
        response(201, "{", &[]),
    ] {
        let rig = Rig::new([listed(vec![]), result]);
        let delivery = rig
            .execute(
                prepare(&[], None, temp.path()),
                &mut Approval::allowing(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(
            delivery.report().value()["meta"]["mutationMayHaveOccurred"],
            true
        );
        rig.assert_requests(1);
    }
    let rig = Rig::new([listed((0..1001).map(|index| json!({"id":ITEM,"displayName":format!("other-{index}"),"type":"Environment"})).collect())]);
    assert!(
        rig.execute(
            prepare(&[], None, temp.path()),
            &mut Approval::allowing(None),
            pending()
        )
        .await
        .is_err()
    );
    rig.assert_requests(0);
}

#[cfg(feature = "headless")]
mod process {
    use super::*;
    use std::{
        process::{Command, Stdio},
        time::Instant,
    };

    fn command() -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
        command
            .env_remove("FAB_RUST_AUTH_MODE")
            .env_remove("FAB_RUST_FABRIC_HOST");
        command
    }

    #[test]
    fn mounted_help_and_preflight_never_require_live_identity() {
        for args in [vec!["--help"], vec!["create", "--help"]] {
            let output = command().args(args).output().unwrap();
            assert!(output.status.success());
            let help = String::from_utf8(output.stdout).unwrap();
            assert!(help.contains("Environment"));
            assert!(output.stderr.is_empty());
        }
        let output = command().args(["create", TARGET]).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"]["code"],
            "ApprovalRequired"
        );
        let output = command()
            .args(["create", TARGET, "--force", "--input", "missing.json"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"]["code"],
            "CreatePolicyRejected"
        );
    }

    #[test]
    fn broken_help_pipe_uses_a_portable_closed_reader_and_is_nonzero() {
        let (reader, writer) = io::pipe().unwrap();
        drop(reader);
        let output = command()
            .args(["create", "--help"])
            .stdout(writer)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"]["code"],
            "OutputFailed"
        );
    }

    #[test]
    fn original_deadline_bounds_blocked_stdin_without_identity_or_post() {
        let mut child = command()
            .args(["create", TARGET, "--input", "-", "--timeout", "20ms"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        let input = child.stdin.take().unwrap();
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if started.elapsed() > Duration::from_secs(5) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("blocked input escaped the original deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(input);
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"]["code"],
            "TimedOut"
        );
    }
}
