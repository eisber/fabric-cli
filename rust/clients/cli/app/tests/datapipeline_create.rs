// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real parser -> shared host -> SDK -> scripted HttpExecutor, never live Fabric.
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
    datapipeline_create::DataPipelineCreateBinding,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    items::create::{
        CreateError, CreateInputValidator, CreateTarget, NativeId, SchemaRef, TargetScope,
    },
};
use fabric_sdk_http::{ExecutionOptions, ExecutionTrace, HttpExecutor};
use serde_json::{Value, json};
use std::{
    future::{Future, pending},
    io::{self, Read, Write},
    path::Path,
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use support::{ScriptedHttp, TestCredential, TestTimer, response};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OP: &str = "33333333-3333-3333-3333-333333333333";
const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact.DataPipeline";
const LOCATION: &str =
    "https://api.fabric.microsoft.com/v1/operations/33333333-3333-3333-3333-333333333333";
const BINDING: DataPipelineCreateBinding = DataPipelineCreateBinding;

fn resource() -> Value {
    json!({"id":ITEM,"workspaceId":WS,"type":"DataPipeline","displayName":"Exact"})
}

fn listing(items: Value) -> AsyncRawResponse {
    response(200, &json!({"value":items}).to_string(), &[])
}

fn accepted(body: &str) -> AsyncRawResponse {
    response(
        202,
        body,
        &[
            ("Location", LOCATION),
            ("x-ms-operation-id", OP),
            ("Retry-After", "7"),
        ],
    )
}

fn prepare(
    extra: &[&str],
    input: Option<&[u8]>,
    cwd: &Path,
) -> Result<PreparedCreateCommand, CreateCommandError> {
    let mut args = vec!["create", TARGET];
    args.extend_from_slice(extra);
    if input.is_some() {
        args.extend(["--input", "-"]);
    }
    let command = v2_create::parse_args(args).unwrap();
    PreparedCreateCommand::preflight(
        command,
        &BINDING,
        cwd,
        &mut input.unwrap_or_default(),
        false,
    )
}

struct Approval {
    calls: usize,
    denied: bool,
    expected_input: Option<Vec<u8>>,
    approved_body: Option<Vec<u8>>,
    approved_url: Option<String>,
}

impl Approval {
    fn new(input: Option<&[u8]>) -> Self {
        Self {
            calls: 0,
            denied: false,
            expected_input: input.map(<[u8]>::to_vec),
            approved_body: None,
            approved_url: None,
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
        assert_eq!(request.plan().target().item_type(), "DataPipeline");
        assert_eq!(request.raw_input(), self.expected_input.as_deref());
        assert_eq!(
            request.context().selection().account(),
            "offline@example.invalid"
        );
        assert_eq!(request.context().selection().tenant(), "offline-tenant");
        assert_eq!(request.context().selection().audience(), Audience::Fabric);
        assert_eq!(
            request.context().endpoint(),
            &EndpointBinding::public(ServiceOrigin::Fabric)
        );
        assert_eq!(
            request.remote_write(),
            !matches!(
                request.plan().action(),
                fabric_sdk::items::create::CreateAction::Skip { .. }
            )
        );
        assert!(request.anticipated_effects().is_empty());
        if request.remote_write() {
            let operation = request.operation().unwrap();
            let expected = self
                .expected_input
                .clone()
                .unwrap_or_else(|| br#"{"displayName":"Exact"}"#.to_vec());
            assert_eq!(operation.body(), expected);
            assert_eq!(
                operation.method(),
                fabric_sdk_workspaces::mutations::ResourceMethod::Post
            );
            assert_eq!(
                operation.path(),
                format!("/v1/workspaces/{WS}/dataPipelines")
            );
            assert_eq!(
                operation.completion(),
                if request.command().wait {
                    fabric_sdk_workspaces::mutations::CompletionPolicy::WaitForResult
                } else {
                    fabric_sdk_workspaces::mutations::CompletionPolicy::ReturnAccepted
                }
            );
            let url = request.url().unwrap();
            assert_eq!(
                url.as_str(),
                format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/dataPipelines")
            );
            let question = request.approval_question();
            assert!(question.contains("method=POST"));
            assert!(question.contains("audience=Fabric"));
            assert!(question.contains("remoteWrite=true"));
            assert!(
                question.contains(
                    &String::from_utf8(expected.clone())
                        .unwrap()
                        .escape_debug()
                        .to_string()
                )
            );
            self.approved_body = Some(operation.body().to_vec());
            self.approved_url = Some(url.as_str().to_owned());
        } else {
            assert!(request.operation().is_none());
            assert!(request.url().is_none());
            assert!(
                request
                    .approval_question()
                    .contains("no native request (unchanged skip)")
            );
        }
        Box::pin(std::future::ready(if self.denied {
            Err(CreateApprovalError::Denied)
        } else {
            Ok(())
        }))
    }
}

struct Harness {
    credential: Arc<TestCredential>,
    client: Client,
    http: HttpExecutor,
    wire: Arc<ScriptedHttp>,
    timer: Arc<TestTimer>,
}

impl Harness {
    fn new(replies: impl IntoIterator<Item = AsyncRawResponse>) -> Self {
        let credential = Arc::new(TestCredential::default());
        let wire = ScriptedHttp::new(replies);
        let timer = Arc::new(TestTimer::default());
        let context = RequestContext::with_endpoint(
            credential.clone(),
            CredentialSelection::new(
                "offline@example.invalid",
                "offline-tenant",
                Audience::Fabric,
            )
            .unwrap(),
            EndpointBinding::public(ServiceOrigin::Fabric),
        );
        Self {
            credential,
            client: Client::with_context(context),
            http: HttpExecutor::new(wire.clone(), timer.clone(), ExecutionOptions::default())
                .unwrap(),
            wire,
            timer,
        }
    }

    async fn execute(
        &self,
        prepared: PreparedCreateCommand,
        approval: &mut dyn CreateApproval,
        interrupted: impl Future<Output = ()> + Send,
    ) -> Result<CreateDelivery, CreateCommandError> {
        let trace = ExecutionTrace::default();
        let observe = || {
            let snapshot = trace.snapshot();
            CreateTransportEvidence {
                http_status: snapshot.status,
                operation_id: snapshot.operation_id,
            }
        };
        prepared
            .execute(
                &BINDING,
                &self.client,
                &self.http,
                CreateExecution {
                    executor: self.http.clone().with_trace(trace.clone()),
                    observe: &observe,
                },
                approval,
                interrupted,
            )
            .await
    }

    fn assert_methods(&self, expected: &[Method]) {
        let requests = self.wire.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|request| request.method())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(
            requests
                .iter()
                .all(|request| request.url().host_str() == Some("api.fabric.microsoft.com"))
        );
        assert!(
            requests
                .iter()
                .all(|request| !request.url().path().contains("/jobs/"))
        );
    }
}

struct ForbiddenRead;
impl Read for ForbiddenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("this target/policy must reject before input I/O");
    }
}

#[test]
fn unsupported_policies_and_targets_fail_before_input_io() {
    let temp = tempfile::tempdir().unwrap();
    for path in [
        "/Name.Workspace/Exact.DataPipeline",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Notebook",
        "/11111111-1111-1111-1111-111111111111.Workspace/Folder.Folder/Exact.DataPipeline",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.DataPipeline/src/definition",
        "/workspaces/11111111-1111-1111-1111-111111111111/items/22222222-2222-2222-2222-222222222222",
        "/11111111-1111-1111-1111-111111111111.Workspace",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.datapipeline",
    ] {
        let command = v2_create::parse_args(["create", path, "--input", "-"]).unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &BINDING,
                temp.path(),
                &mut ForbiddenRead,
                false
            )
            .is_err()
        );
    }
    for flags in [
        vec!["--force"],
        vec!["--if-exists", "overwrite"],
        vec!["--if-exists", "duplicate"],
    ] {
        for source in ["-", "missing.json"] {
            let args = ["create", TARGET, "--input", source]
                .into_iter()
                .chain(flags.iter().copied());
            let command = v2_create::parse_args(args).unwrap();
            assert!(matches!(
                PreparedCreateCommand::preflight(
                    command,
                    &BINDING,
                    temp.path(),
                    &mut ForbiddenRead,
                    false
                ),
                Err(CreateCommandError::Policy(CreateError::UnsupportedPolicy))
            ));
        }
    }
    for policy in ["error", "skip", "overwrite", "duplicate"] {
        assert!(
            v2_create::parse_args(["create", TARGET, "--force", "--if-exists", policy]).is_err()
        );
    }
}

#[test]
fn strict_metadata_contract_rejects_definition_job_unknown_and_inconsistent_input() {
    let temp = tempfile::tempdir().unwrap();
    let invalid = [
        "",
        "null",
        "[]",
        "{}",
        r#"{"description":"without required displayName"}"#,
        r#"{"displayName":"Other"}"#,
        r#"{"displayName":null}"#,
        r#"{"displayName":"Exact","type":"Notebook"}"#,
        r#"{"displayName":"Exact","type":"DataPipeline"}"#,
        r#"{"displayName":"Exact","definition":{}}"#,
        r#"{"displayName":"Exact","run":true}"#,
        r#"{"displayName":"Exact","jobType":"Pipeline"}"#,
        r#"{"displayName":"Exact","parameters":{}}"#,
        r#"{"displayName":"Exact","creationPayload":{}}"#,
        r#"{"displayName":"Exact","folderId":null}"#,
        r#"{"displayName":"Exact","sensitivityLabelSettings":{}}"#,
        r#"{"displayName":"Exact","description":null}"#,
        r#"{"displayName":"Exact","unknown":false}"#,
        r#"{"displayName":"Exact","displayName":"Exact"}"#,
        r#"{"displayName":"Exact","\u0064isplayName":"Exact"}"#,
        r#"{"displayName":"Exact"} {}"#,
    ];
    for policy in ["error", "skip"] {
        for input in invalid {
            assert!(
                prepare(
                    &["--if-exists", policy],
                    Some(input.as_bytes()),
                    temp.path()
                )
                .is_err(),
                "{input}"
            );
        }
    }
    assert!(prepare(&[], Some(&[0xff]), temp.path()).is_err());
    let mut bounded = br#"{"displayName":"Exact"}"#.to_vec();
    bounded.resize(MAX_BYTES, b' ');
    assert!(prepare(&[], Some(&bounded), temp.path()).is_ok());
    bounded.push(b' ');
    assert!(prepare(&[], Some(&bounded), temp.path()).is_err());
    for length in [256, 257] {
        let input = json!({"displayName":"Exact","description":"é".repeat(length)}).to_string();
        assert_eq!(
            prepare(&[], Some(input.as_bytes()), temp.path()).is_ok(),
            length == 256
        );
    }
    assert!(prepare(&[], Some(&vec![b' '; MAX_BYTES + 1]), temp.path()).is_err());
    let target = CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WS).unwrap(),
        "Exact",
        "DataPipeline",
    )
    .unwrap();
    assert!(
        BINDING
            .validate(&target, &SchemaRef::new("other/v1").unwrap(), None)
            .is_err()
    );
}

#[tokio::test]
async fn omitted_input_creates_only_empty_item_metadata_at_the_typed_route() {
    let temp = tempfile::tempdir().unwrap();
    let harness = Harness::new([
        listing(json!([])),
        response(201, &resource().to_string(), &[]),
    ]);
    let mut approval = Approval::new(None);
    let delivery = harness
        .execute(
            prepare(&[], None, temp.path()).unwrap(),
            &mut approval,
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(approval.calls, 1);
    assert_eq!(delivery.report().exit_code(), 0);
    assert_eq!(delivery.report().value()["meta"]["status"], "completed");
    assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
    assert!(delivery.report().value()["meta"]["operationHandle"].is_null());
    harness.assert_methods(&[Method::Get, Method::Post]);
    let requests = harness.wire.requests.lock().unwrap();
    assert_eq!(
        requests[0].url().path(),
        format!("/v1/workspaces/{WS}/items")
    );
    assert_eq!(requests[0].url().query(), Some("recursive=false"));
    assert_eq!(
        requests[1].url().path(),
        format!("/v1/workspaces/{WS}/dataPipelines")
    );
    let Body::Bytes(body) = requests[1].body() else {
        panic!("native JSON body expected")
    };
    assert_eq!(Some(body.as_ref()), approval.approved_body.as_deref());
    assert_eq!(
        Some(requests[1].url().as_str()),
        approval.approved_url.as_deref()
    );
    assert_eq!(requests[1].url().query(), None);
    assert_eq!(
        serde_json::from_slice::<Value>(body).unwrap(),
        json!({"displayName":"Exact"})
    );
}

#[tokio::test]
async fn explicit_file_bytes_are_bound_to_approval_and_sent_without_reserialization() {
    let temp = tempfile::tempdir().unwrap();
    let raw = b" {\r\n \"description\": \"notes\", \"displayName\": \"Exact\"\r\n}\n";
    std::fs::write(temp.path().join("input.json"), raw).unwrap();
    let command = v2_create::parse_args(["create", TARGET, "--input", "input.json"]).unwrap();
    let prepared =
        PreparedCreateCommand::preflight(command, &BINDING, temp.path(), &mut ForbiddenRead, false)
            .unwrap();
    let harness = Harness::new([
        listing(json!([])),
        response(201, &resource().to_string(), &[]),
    ]);
    let mut approval = Approval::new(Some(raw));
    let delivery = harness
        .execute(prepared, &mut approval, pending())
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    let requests = harness.wire.requests.lock().unwrap();
    let Body::Bytes(body) = requests[1].body() else {
        panic!("native JSON body expected")
    };
    assert_eq!(body.as_ref(), raw);
    assert_eq!(Some(body.as_ref()), approval.approved_body.as_deref());
    assert_eq!(
        Some(requests[1].url().as_str()),
        approval.approved_url.as_deref()
    );
}

#[tokio::test]
async fn collision_error_and_validated_skip_never_post_or_claim_content_equality() {
    let temp = tempfile::tempdir().unwrap();
    let raw = br#"{"displayName":"Exact","description":"not compared"}"#;
    for policy in ["error", "skip"] {
        let harness = Harness::new([listing(json!([resource()]))]);
        let mut approval = Approval::new(Some(raw));
        let result = harness
            .execute(
                prepare(&["--if-exists", policy], Some(raw), temp.path()).unwrap(),
                &mut approval,
                pending(),
            )
            .await;
        if policy == "error" {
            assert!(matches!(
                result,
                Err(CreateCommandError::Policy(CreateError::AlreadyExists))
            ));
            assert_eq!(approval.calls, 0);
        } else {
            let delivery = result.unwrap();
            assert_eq!(approval.calls, 1);
            assert_eq!(delivery.report().value()["meta"]["status"], "unchanged");
            assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
            assert_eq!(
                delivery.report().value()["meta"]["contentEqualityClaimed"],
                false
            );
            assert_eq!(
                delivery.report().value()["meta"]["mutationMayHaveOccurred"],
                false
            );
        }
        harness.assert_methods(&[Method::Get]);
    }
}

#[tokio::test]
async fn skip_missing_creates_and_concurrent_collision_is_not_overwritten_or_retried() {
    let temp = tempfile::tempdir().unwrap();
    for status in [201, 409] {
        let body = if status == 201 {
            resource()
        } else {
            json!({"errorCode":"ItemDisplayNameAlreadyInUse","message":"collision"})
        };
        let harness = Harness::new([listing(json!([])), response(status, &body.to_string(), &[])]);
        let delivery = harness
            .execute(
                prepare(&["--if-exists", "skip"], None, temp.path()).unwrap(),
                &mut Approval::new(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), u8::from(status != 201));
        assert_eq!(delivery.report().value()["meta"]["policy"], "skip");
        assert_eq!(delivery.report().value()["meta"]["action"], "create");
        if status == 409 {
            assert_eq!(
                delivery.report().value()["meta"]["serviceCode"],
                "ItemDisplayNameAlreadyInUse"
            );
        }
        harness.assert_methods(&[Method::Get, Method::Post]);
    }
}

#[tokio::test]
async fn wrong_kind_ambiguous_case_collision_and_wrong_scope_discovery_never_approve() {
    let temp = tempfile::tempdir().unwrap();
    let mut wrong_kind = resource();
    wrong_kind["type"] = json!("Notebook");
    let mut wrong_scope = resource();
    wrong_scope["workspaceId"] = json!(OP);
    let mut wrong_id = resource();
    wrong_id["id"] = json!("not-guid");
    let mut wrong_case = resource();
    wrong_case["displayName"] = json!("exact");
    for items in [
        json!([wrong_kind]),
        json!([wrong_scope]),
        json!([wrong_id]),
        json!([wrong_case]),
        json!([resource(), resource()]),
    ] {
        let harness = Harness::new([listing(items)]);
        let mut approval = Approval::new(None);
        assert!(
            harness
                .execute(
                    prepare(&["--if-exists", "skip"], None, temp.path()).unwrap(),
                    &mut approval,
                    pending(),
                )
                .await
                .is_err()
        );
        assert_eq!(approval.calls, 0);
        harness.assert_methods(&[Method::Get]);
    }
}

#[tokio::test]
async fn incomplete_or_unsafe_discovery_never_authorizes_creation() {
    let temp = tempfile::tempdir().unwrap();
    for reply in [
        response(200, "{", &[]),
        response(403, r#"{"errorCode":"Forbidden"}"#, &[]),
        response(
            200,
            &json!({
                "value":[], "continuationUri":"https://other.invalid/v1/workspaces/x/items",
                "continuationToken":"next",
            })
            .to_string(),
            &[],
        ),
        listing(json!(vec![resource(); 1001])),
    ] {
        let harness = Harness::new([reply]);
        let mut approval = Approval::new(None);
        assert!(
            harness
                .execute(
                    prepare(&[], None, temp.path()).unwrap(),
                    &mut approval,
                    pending(),
                )
                .await
                .is_err()
        );
        assert_eq!(approval.calls, 0);
        harness.assert_methods(&[Method::Get]);
    }
}

#[tokio::test]
async fn typed_root_selection_ignores_valid_nested_and_unrelated_same_name_owners() {
    let temp = tempfile::tempdir().unwrap();
    let mut root = resource();
    root["folderId"] = Value::Null;
    let mut nested = resource();
    nested["id"] = json!(OP);
    nested["folderId"] = json!(OP);
    let mut other = resource();
    other["id"] = json!(OP);
    other["type"] = json!("Notebook");
    for items in [
        json!([root.clone(), other.clone()]),
        json!([other.clone(), root.clone()]),
        json!([nested.clone(), root.clone(), other.clone()]),
    ] {
        let harness = Harness::new([listing(items)]);
        let delivery = harness
            .execute(
                prepare(&["--if-exists", "skip"], None, temp.path()).unwrap(),
                &mut Approval::new(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 0);
        assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
        assert_eq!(delivery.report().value()["meta"]["folderIdObserved"], true);
        assert!(delivery.report().value()["meta"]["folderId"].is_null());
        assert_eq!(delivery.report().value()["meta"]["remoteWrite"], false);
        assert_eq!(
            delivery.report().value()["meta"]["anticipatedEffects"],
            json!([])
        );
        harness.assert_methods(&[Method::Get]);
    }
    for items in [
        json!([nested.clone()]),
        json!([nested, other]),
        json!([root.clone(), root]),
    ] {
        let harness = Harness::new([listing(items)]);
        let mut approval = Approval::new(None);
        assert!(
            harness
                .execute(
                    prepare(&["--if-exists", "skip"], None, temp.path()).unwrap(),
                    &mut approval,
                    pending(),
                )
                .await
                .is_err()
        );
        assert_eq!(approval.calls, 0);
        harness.assert_methods(&[Method::Get]);
    }
}

#[tokio::test]
async fn malformed_folder_identity_is_never_ignored_even_beside_a_valid_root() {
    let temp = tempfile::tempdir().unwrap();
    for folder in [
        json!("not-guid"),
        json!(""),
        json!(42),
        json!({}),
        json!([]),
    ] {
        let mut malformed = resource();
        malformed["folderId"] = folder;
        let harness = Harness::new([listing(json!([resource(), malformed]))]);
        let mut approval = Approval::new(None);
        assert!(
            harness
                .execute(
                    prepare(&["--if-exists", "skip"], None, temp.path()).unwrap(),
                    &mut approval,
                    pending(),
                )
                .await
                .is_err()
        );
        assert_eq!(approval.calls, 0);
        harness.assert_methods(&[Method::Get]);
    }
}

#[tokio::test]
async fn paged_root_discovery_preserves_recursive_false_and_waits_for_complete_selection() {
    let temp = tempfile::tempdir().unwrap();
    let mut other = resource();
    other["type"] = json!("Notebook");
    let harness = Harness::new([
        response(200, &json!({
            "value":[other], "continuationToken":"next token",
            "continuationUri":format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/items?continuationToken=next"),
        }).to_string(), &[]),
        listing(json!([resource()])),
    ]);
    let delivery = harness
        .execute(
            prepare(&["--if-exists", "skip"], None, temp.path()).unwrap(),
            &mut Approval::new(None),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
    harness.assert_methods(&[Method::Get, Method::Get]);
    let requests = harness.wire.requests.lock().unwrap();
    assert_eq!(requests[0].url().query(), Some("recursive=false"));
    assert_eq!(
        requests[1].url().query(),
        Some("recursive=false&continuationToken=next+token")
    );
}

#[tokio::test]
async fn completed_resource_must_be_workspace_root_and_folder_evidence_is_protected() {
    let temp = tempfile::tempdir().unwrap();
    for (wait, folder) in [
        (false, Value::Null),
        (true, Value::Null),
        (false, json!(OP)),
        (true, json!(OP)),
        (false, json!("malformed")),
        (true, json!(42)),
    ] {
        let mut returned = resource();
        returned["folderId"] = folder.clone();
        let mut replies = vec![listing(json!([]))];
        if wait {
            replies.extend([
                accepted(""),
                response(200, r#"{"status":"Succeeded"}"#, &[]),
                response(200, &returned.to_string(), &[]),
            ]);
        } else {
            replies.push(response(201, &returned.to_string(), &[]));
        }
        let harness = Harness::new(replies);
        let options = if wait {
            vec!["--wait", "--project", "missing"]
        } else {
            vec!["--project", "missing"]
        };
        let delivery = harness
            .execute(
                prepare(&options, None, temp.path()).unwrap(),
                &mut Approval::new(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), u8::from(!folder.is_null()));
        assert_eq!(
            delivery.report().value()["meta"]["completed"],
            folder.is_null()
        );
        let mut output = Vec::new();
        delivery.deliver(&mut output).unwrap();
        let receipt: Value = serde_json::from_slice(&output).unwrap();
        assert!(receipt["data"].is_null());
        assert_eq!(receipt["meta"]["folderIdObserved"], true);
        assert_eq!(receipt["meta"]["folderId"], folder);
        assert_eq!(receipt["meta"]["resourceId"], ITEM);
        assert_eq!(receipt["meta"]["anticipatedEffects"], json!([]));
        assert_eq!(receipt["meta"]["remoteWrite"], true);
        if wait {
            assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
            assert_eq!(receipt["meta"]["observation"]["httpStatus"], 200);
            harness.assert_methods(&[Method::Get, Method::Post, Method::Get, Method::Get]);
        } else {
            assert_eq!(receipt["meta"]["submission"]["httpStatus"], 201);
            assert_eq!(receipt["meta"]["submission"]["folderId"], folder);
            assert_eq!(receipt["meta"]["submission"]["folderIdObserved"], true);
            harness.assert_methods(&[Method::Get, Method::Post]);
        }
    }
}

#[tokio::test]
async fn wrong_audience_or_endpoint_origin_rejects_before_token_discovery_or_approval() {
    let temp = tempfile::tempdir().unwrap();
    for (audience, origin) in [
        (Audience::OneLake, ServiceOrigin::Fabric),
        (Audience::Fabric, ServiceOrigin::OneLake),
    ] {
        let mut harness = Harness::new([]);
        harness.client = Client::with_context(RequestContext::with_endpoint(
            harness.credential.clone(),
            CredentialSelection::new("offline@example.invalid", "offline-tenant", audience)
                .unwrap(),
            EndpointBinding::public(origin),
        ));
        let mut approval = Approval::new(None);
        assert!(matches!(
            harness
                .execute(
                    prepare(&[], None, temp.path()).unwrap(),
                    &mut approval,
                    pending()
                )
                .await,
            Err(CreateCommandError::Invalid)
        ));
        assert_eq!(harness.credential.calls.load(Ordering::SeqCst), 0);
        assert_eq!(approval.calls, 0);
        harness.assert_methods(&[]);
    }
    for host in [
        "http://localhost",
        "api.fabric.microsoft.com/path",
        "other.invalid",
        "",
    ] {
        assert!(EndpointBinding::with_host(ServiceOrigin::Fabric, host).is_err());
    }
}

#[tokio::test]
async fn approval_denial_after_exact_discovery_dispatches_zero_posts() {
    let temp = tempfile::tempdir().unwrap();
    let harness = Harness::new([listing(json!([]))]);
    let mut approval = Approval::new(None);
    approval.denied = true;
    assert!(matches!(
        harness
            .execute(
                prepare(&[], None, temp.path()).unwrap(),
                &mut approval,
                pending(),
            )
            .await,
        Err(CreateCommandError::Approval(CreateApprovalError::Denied))
    ));
    assert_eq!(approval.calls, 1);
    harness.assert_methods(&[Method::Get]);
}

#[test]
fn operation_refuses_substituted_raw_input_and_unknown_schema_or_owner() {
    use fabric_sdk::items::create::{ConflictPolicy, LeafState, Preflight, plan_create};
    let target = CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WS).unwrap(),
        "Exact",
        "DataPipeline",
    )
    .unwrap();
    let plan = plan_create(
        ConflictPolicy::Error,
        target,
        Preflight::Leaf(LeafState::Missing),
        &BINDING.capabilities(),
        Some(json!({"displayName":"Exact"})),
        &BINDING,
    )
    .unwrap();
    for raw in [
        None,
        Some(br#"{"displayName":"Other"}"#.as_slice()),
        Some(br#"{"displayName":"Exact","definition":{}}"#),
        Some(br#"{"displayName":"Exact","displayName":"Exact"}"#),
    ] {
        assert!(BINDING.operation(&plan, raw, false).is_err());
    }
    assert!(
        BINDING
            .operation(&plan, Some(br#"{"displayName":"Exact"}"#), false)
            .is_ok()
    );
    for (owner, parent, scope) in [
        ("Notebook", WS, TargetScope::NamedLeaf),
        ("DataPipeline", "workspace-name", TargetScope::NamedLeaf),
        ("DataPipeline", WS, TargetScope::ParentCollection),
    ] {
        let target =
            CreateTarget::new(scope, NativeId::new(parent).unwrap(), "Exact", owner).unwrap();
        assert!(
            BINDING
                .validate(&target, &BINDING.capabilities().schema, None)
                .is_err()
        );
    }
}

#[tokio::test]
async fn accepted_handle_is_not_resource_identity_or_completion() {
    let temp = tempfile::tempdir().unwrap();
    let harness = Harness::new([listing(json!([])), accepted("")]);
    let delivery = harness
        .execute(
            prepare(&[], None, temp.path()).unwrap(),
            &mut Approval::new(None),
            pending(),
        )
        .await
        .unwrap();
    let meta = &delivery.report().value()["meta"];
    assert_eq!(delivery.report().exit_code(), 0);
    assert_eq!(meta["status"], "accepted");
    assert_eq!(meta["completed"], false);
    assert!(meta["resourceId"].is_null());
    assert_eq!(meta["operationHandle"], format!("/operations/{OP}"));
    assert_eq!(meta["mutationMayHaveOccurred"], true);
    harness.assert_methods(&[Method::Get, Method::Post]);
}

#[tokio::test]
async fn wait_obeys_retry_after_and_retrieves_the_native_operation_result_once() {
    let temp = tempfile::tempdir().unwrap();
    let harness = Harness::new([
        listing(json!([])),
        accepted(""),
        response(200, r#"{"status":"Running"}"#, &[("Retry-After", "3")]),
        response(200, r#"{"status":"Succeeded"}"#, &[]),
        response(200, &resource().to_string(), &[]),
    ]);
    let delivery = harness
        .execute(
            prepare(&["--wait"], None, temp.path()).unwrap(),
            &mut Approval::new(None),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    assert_eq!(delivery.report().value()["meta"]["status"], "completed");
    assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
    assert_eq!(
        delivery.report().value()["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(*harness.timer.waits.lock().unwrap(), [7, 3]);
    harness.assert_methods(&[
        Method::Get,
        Method::Post,
        Method::Get,
        Method::Get,
        Method::Get,
    ]);
    let requests = harness.wire.requests.lock().unwrap();
    assert_eq!(requests[2].url().path(), format!("/v1/operations/{OP}"));
    assert_eq!(
        requests[4].url().path(),
        format!("/v1/operations/{OP}/result")
    );
}

#[tokio::test]
async fn malformed_successes_and_native_failures_preserve_uncertainty_without_replay() {
    let temp = tempfile::tempdir().unwrap();
    let mut wrong_resource = resource();
    wrong_resource["type"] = json!("Notebook");
    let mut wrong_scope = resource();
    wrong_scope["workspaceId"] = json!(OP);
    let mut wrong_name = resource();
    wrong_name["displayName"] = json!("Other");
    let mut wrong_id = resource();
    wrong_id["id"] = json!("not-guid");
    for (status, body, headers) in [
        (201, "".into(), vec![]),
        (201, "{".into(), vec![]),
        (201, r#"{"id":"a","id":"b"}"#.into(), vec![]),
        (201, wrong_resource.to_string(), vec![]),
        (201, wrong_scope.to_string(), vec![]),
        (201, wrong_name.to_string(), vec![]),
        (201, wrong_id.to_string(), vec![]),
        (202, "".into(), vec![]),
        (
            202,
            "".into(),
            vec![
                (
                    "Location",
                    "https://other.invalid/v1/operations/33333333-3333-3333-3333-333333333333",
                ),
                ("x-ms-operation-id", OP),
            ],
        ),
        (
            400,
            r#"{"errorCode":"CorruptedPayload","message":"synthetic"}"#.into(),
            vec![],
        ),
        (
            403,
            r#"{"errorCode":"Forbidden","message":"synthetic"}"#.into(),
            vec![],
        ),
        (
            503,
            r#"{"errorCode":"Unavailable","message":"synthetic"}"#.into(),
            vec![],
        ),
    ] {
        let harness = Harness::new([listing(json!([])), response(status, &body, &headers)]);
        let delivery = harness
            .execute(
                prepare(&[], None, temp.path()).unwrap(),
                &mut Approval::new(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1, "{status} {body}");
        assert_eq!(delivery.report().value()["meta"]["completed"], false);
        assert_eq!(delivery.report().value()["meta"]["remoteReplayed"], false);
        assert_eq!(delivery.report().value()["meta"]["remoteCancelled"], false);
        harness.assert_methods(&[Method::Get, Method::Post]);
    }
}

#[tokio::test]
async fn poll_failure_retains_acceptance_resource_and_handle_outside_projection() {
    let temp = tempfile::tempdir().unwrap();
    for state in ["Failed", "Cancelled", "FutureState"] {
        let harness = Harness::new([
            listing(json!([])),
            accepted(&resource().to_string()),
            response(
                200,
                &json!({"status":state,"error":{"errorCode":"Synthetic","message":"failed"}})
                    .to_string(),
                &[],
            ),
        ]);
        let delivery = harness
            .execute(
                prepare(&["--wait", "--project", "missing"], None, temp.path()).unwrap(),
                &mut Approval::new(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        let mut out = Vec::new();
        delivery.deliver(&mut out).unwrap();
        let receipt: Value = serde_json::from_slice(&out).unwrap();
        assert!(receipt["data"].is_null());
        assert_eq!(receipt["meta"]["resourceId"], ITEM);
        assert_eq!(receipt["meta"]["observedResource"]["type"], "DataPipeline");
        assert_eq!(
            receipt["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(receipt["meta"]["remoteReplayed"], false);
        assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
        assert_eq!(receipt["meta"]["submission"]["resource"]["workspaceId"], WS);
        assert_eq!(
            receipt["meta"]["submission"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(receipt["meta"]["observation"]["httpStatus"], 200);
        assert_eq!(receipt["meta"]["anticipatedEffects"], json!([]));
        harness.assert_methods(&[Method::Get, Method::Post, Method::Get]);
    }
}

#[tokio::test]
async fn malformed_final_result_keeps_initial_accepted_identity_separate_from_observation() {
    let temp = tempfile::tempdir().unwrap();
    let mut acceptance = resource();
    acceptance["folderId"] = Value::Null;
    for malformed in ["{", r#"{"id":"a","id":"b"}"#, "\u{fffd}"] {
        let harness = Harness::new([
            listing(json!([])),
            accepted(&acceptance.to_string()),
            response(200, r#"{"status":"Succeeded"}"#, &[]),
            response(200, malformed, &[]),
        ]);
        let delivery = harness
            .execute(
                prepare(&["--wait", "--project", "missing"], None, temp.path()).unwrap(),
                &mut Approval::new(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        let mut output = Vec::new();
        delivery.deliver(&mut output).unwrap();
        let receipt: Value = serde_json::from_slice(&output).unwrap();
        assert!(receipt["data"].is_null());
        assert_eq!(receipt["meta"]["completed"], false);
        assert_eq!(receipt["meta"]["resourceId"], ITEM);
        assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
        assert_eq!(receipt["meta"]["submission"]["folderIdObserved"], true);
        assert_eq!(receipt["meta"]["observation"]["httpStatus"], 200);
        assert!(receipt["meta"]["unparsedResponseBodyBase64"].is_string());
        assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(receipt["meta"]["remoteReplayed"], false);
        harness.assert_methods(&[Method::Get, Method::Post, Method::Get, Method::Get]);
    }
}

#[tokio::test]
async fn malformed_no_wait_acceptance_retains_bytes_and_handle_without_success_or_replay() {
    use base64::Engine;
    let temp = tempfile::tempdir().unwrap();
    for malformed in ["{", r#"{"displayName":"Exact","displayName":"Exact"}"#] {
        let harness = Harness::new([listing(json!([])), accepted(malformed)]);
        let delivery = harness
            .execute(
                prepare(&["--project", "missing"], None, temp.path()).unwrap(),
                &mut Approval::new(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        let mut output = Vec::new();
        delivery.deliver(&mut output).unwrap();
        let receipt: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(receipt["meta"]["completed"], false);
        assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(
            receipt["meta"]["submission"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(
            receipt["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(
            receipt["meta"]["unparsedAcceptanceBodyBase64"],
            base64::engine::general_purpose::STANDARD.encode(malformed)
        );
        assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(receipt["meta"]["remoteReplayed"], false);
        harness.assert_methods(&[Method::Get, Method::Post]);
    }
}

#[tokio::test]
async fn unexpected_success_result_status_is_unknown_and_keeps_original_typed_http_cause() {
    let temp = tempfile::tempdir().unwrap();
    for status in [201, 202, 204] {
        let harness = Harness::new([
            listing(json!([])),
            accepted(&resource().to_string()),
            response(200, r#"{"status":"Succeeded"}"#, &[]),
            response(status, &resource().to_string(), &[]),
        ]);
        let delivery = harness
            .execute(
                prepare(&["--wait", "--project", "missing"], None, temp.path()).unwrap(),
                &mut Approval::new(None),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert!(matches!(
            &delivery.report().error().unwrap().kind,
            fabric_sdk_operations::OperationErrorKind::Http { status: actual, .. } if *actual == status
        ));
        let mut output = Vec::new();
        delivery.deliver(&mut output).unwrap();
        let receipt: Value = serde_json::from_slice(&output).unwrap();
        assert!(receipt["data"].is_null());
        assert_eq!(receipt["meta"]["status"], "unknown");
        assert_eq!(receipt["meta"]["completed"], false);
        assert_eq!(receipt["error"]["code"], "InvalidResponse");
        assert_eq!(receipt["meta"]["httpStatus"], status);
        assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
        assert_eq!(
            receipt["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(receipt["meta"]["resourceId"], ITEM);
        assert_eq!(receipt["meta"]["observation"]["httpStatus"], status);
        assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(receipt["meta"]["remoteReplayed"], false);
        harness.assert_methods(&[Method::Get, Method::Post, Method::Get, Method::Get]);
    }
}

struct PendingApproval;
impl CreateApproval for PendingApproval {
    fn authorize<'a>(
        &'a mut self,
        request: CreateApprovalRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
        assert_eq!(
            request.operation().unwrap().body(),
            br#"{"displayName":"Exact"}"#
        );
        Box::pin(pending())
    }
}

#[tokio::test]
async fn cancelled_immediate_approval_sends_no_create_request() {
    let temp = tempfile::tempdir().unwrap();
    let harness = Harness::new([listing(json!([]))]);
    let interrupted = async {
        while harness.wire.requests.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert!(matches!(
        harness
            .execute(
                prepare(&[], None, temp.path()).unwrap(),
                &mut PendingApproval,
                interrupted,
            )
            .await,
        Err(CreateCommandError::Interrupted)
    ));
    harness.assert_methods(&[Method::Get]);
}

#[tokio::test]
async fn poll_cancellation_and_deadline_keep_receipt_without_remote_cancel_or_replay() {
    let temp = tempfile::tempdir().unwrap();
    for deadline in [false, true] {
        let harness = Harness::new([listing(json!([])), accepted(&resource().to_string())]);
        harness.timer.pending_wait.store(true, Ordering::SeqCst);
        let interrupt = async {
            if deadline {
                tokio::time::sleep(Duration::from_millis(5)).await;
            } else {
                while harness.wire.requests.lock().unwrap().len() < 2 {
                    tokio::task::yield_now().await;
                }
            }
        };
        let delivery = harness
            .execute(
                prepare(&["--wait"], None, temp.path()).unwrap(),
                &mut Approval::new(None),
                interrupt,
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
        assert_eq!(
            delivery.report().value()["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(
            delivery.report().value()["meta"]["mutationMayHaveOccurred"],
            true
        );
        assert_eq!(delivery.report().value()["meta"]["remoteCancelled"], false);
        harness.assert_methods(&[Method::Get, Method::Post]);
    }
}

#[tokio::test]
async fn response_byte_limit_retains_observed_acceptance_header_and_never_replays() {
    let temp = tempfile::tempdir().unwrap();
    let harness = Harness::new([listing(json!([])), accepted(&" ".repeat(MAX_BYTES + 1))]);
    let delivery = harness
        .execute(
            prepare(&[], None, temp.path()).unwrap(),
            &mut Approval::new(None),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(
        delivery.report().value()["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(delivery.report().value()["meta"]["httpStatus"], 202);
    assert_eq!(
        delivery.report().value()["meta"]["mutationMayHaveOccurred"],
        true
    );
    assert!(delivery.report().value()["meta"]["resourceId"].is_null());
    harness.assert_methods(&[Method::Get, Method::Post]);
}

struct BrokenWriter {
    flush_only: bool,
}
impl Write for BrokenWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.flush_only {
            Ok(bytes.len())
        } else {
            Err(io::Error::other("closed reader"))
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("flush failure"))
    }
}

#[tokio::test]
async fn projection_and_stdout_failures_preserve_the_unprojected_receipt() {
    let temp = tempfile::tempdir().unwrap();
    for project in ["missing", "length(missing)"] {
        for flush_only in [false, true] {
            let harness = Harness::new([
                listing(json!([])),
                response(201, &resource().to_string(), &[]),
            ]);
            let delivery = harness
                .execute(
                    prepare(&["--project", project], None, temp.path()).unwrap(),
                    &mut Approval::new(None),
                    pending(),
                )
                .await
                .unwrap();
            let original = delivery.report().value().clone();
            let Err(CreateCommandError::Delivery { report, .. }) =
                delivery.deliver(&mut BrokenWriter { flush_only })
            else {
                panic!("delivery must fail")
            };
            assert_eq!(report.value(), &original);
            assert_eq!(report.value()["meta"]["resourceId"], ITEM);
            harness.assert_methods(&[Method::Get, Method::Post]);
        }
    }
    let harness = Harness::new([
        listing(json!([])),
        response(201, &resource().to_string(), &[]),
    ]);
    let delivery = harness
        .execute(
            prepare(&["--project", "missing"], None, temp.path()).unwrap(),
            &mut Approval::new(None),
            pending(),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    delivery.deliver(&mut out).unwrap();
    let receipt: Value = serde_json::from_slice(&out).unwrap();
    assert!(receipt["data"].is_null());
    assert_eq!(receipt["meta"]["resourceId"], ITEM);
    assert_eq!(receipt["meta"]["type"], "DataPipeline");
}

#[tokio::test]
async fn no_clobber_preflight_and_commit_race_preserve_existing_output_and_remote_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("receipt.json");
    let args = ["--output", path.to_str().unwrap()];
    let prepared = prepare(&args, None, temp.path()).unwrap();
    assert!(!path.exists());
    std::fs::write(&path, b"existing").unwrap();
    assert!(prepare(&args, None, temp.path()).is_err());
    let harness = Harness::new([listing(json!([])), accepted("")]);
    let delivery = harness
        .execute(prepared, &mut Approval::new(None), pending())
        .await
        .unwrap();
    let Err(CreateCommandError::Delivery { report, .. }) = delivery.deliver(&mut Vec::new()) else {
        panic!("must not clobber")
    };
    assert_eq!(std::fs::read(path).unwrap(), b"existing");
    assert_eq!(
        report.value()["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(report.value()["meta"]["mutationMayHaveOccurred"], true);
    harness.assert_methods(&[Method::Get, Method::Post]);
}
