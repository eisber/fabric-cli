// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline parser -> shared host -> SDK -> HTTP contracts for ReadWrite creation.
#![deny(unnameable_test_items)]

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use azure_core::http::{AsyncRawResponse, Body, Method, headers::Headers};
use fabric_cli::{
    create_command::{
        CreateApproval, CreateApprovalError, CreateApprovalRequest, CreateBinding,
        CreateCommandError, CreateDelivery, CreateExecution, CreateTransportEvidence, MAX_BYTES,
        PreparedCreateCommand, v2_create,
    },
    kql_database_create::KqlReadWriteCreate,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    items::create::{self, ConflictPolicy, CreateAction, CreateError, LeafState, Preflight},
};
use fabric_sdk_http::{ExecutionOptions, ExecutionTrace, HttpExecutor};
use futures_util::stream;
use serde_json::{Value, json};
use std::{
    future::{Future, pending, poll_fn, ready},
    io::{self, Read, Write},
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    task::Poll,
};
use support::{ScriptedHttp, TestCredential, TestTimer, response};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const PARENT: &str = "22222222-2222-2222-2222-222222222222";
const ITEM: &str = "33333333-3333-3333-3333-333333333333";
const OP: &str = "44444444-4444-4444-4444-444444444444";
const HOST: &str = "dailyapi.fabric.microsoft.com";
const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Database_1.KQLDatabase";

fn body() -> Value {
    json!({
        "displayName":"Database_1",
        "creationPayload":{"databaseType":"ReadWrite","parentEventhouseItemId":PARENT}
    })
}

fn resource() -> Value {
    json!({"id":ITEM,"workspaceId":WS,"type":"KQLDatabase","displayName":"Database_1"})
}

fn json_response(status: u16, body: Value) -> AsyncRawResponse {
    response(status, &body.to_string(), &[])
}

fn missing() -> AsyncRawResponse {
    json_response(200, json!({"value":[]}))
}

fn accepted(body: &str, delay: &str) -> AsyncRawResponse {
    response(
        202,
        body,
        &[
            ("x-ms-operation-id", OP),
            ("Location", &format!("https://{HOST}/v1/operations/{OP}")),
            ("Retry-After", delay),
        ],
    )
}

fn prepare(bytes: &[u8], options: &[&str]) -> Result<PreparedCreateCommand, CreateCommandError> {
    let command = v2_create::parse_args(
        ["create", TARGET, "--input", "-"]
            .into_iter()
            .chain(options.iter().copied()),
    )
    .unwrap();
    PreparedCreateCommand::preflight(
        command,
        &KqlReadWriteCreate,
        &std::env::current_dir().unwrap(),
        &mut &bytes[..],
        false,
    )
}

#[derive(Default)]
struct Approval {
    denied: bool,
    calls: Vec<Value>,
}

impl CreateApproval for Approval {
    fn authorize<'a>(
        &'a mut self,
        request: CreateApprovalRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
        self.calls.push(json!({
            "workspace":request.plan().target().parent_id().as_str(),
            "name":request.plan().target().name(),
            "type":request.plan().target().item_type(),
            "body":request.raw_input().map(|bytes| serde_json::from_slice::<Value>(bytes).unwrap()),
            "method":request.operation().map(|operation|format!("{:?}",operation.method())),
            "url":request.url().map(|url|url.as_str()),
            "wireBody":request.operation().map(|operation|String::from_utf8(operation.body().to_vec()).unwrap()),
            "anticipatedEffects":request.anticipated_effects(),
            "question":request.approval_question(),
            "account":request.context().selection().account(),
            "tenant":request.context().selection().tenant(),
            "audience":format!("{:?}",request.context().selection().audience()),
            "endpoint":format!("{:?}",request.context().endpoint()),
            "remoteWrite":request.remote_write(),
            "wait":request.command().wait,
            "timeout":request.command().timeout.as_millis()
        }));
        Box::pin(ready(if self.denied {
            Err(CreateApprovalError::Denied)
        } else {
            Ok(())
        }))
    }
}

struct Harness {
    http: Arc<ScriptedHttp>,
    timer: Arc<TestTimer>,
    credential: Arc<TestCredential>,
    client: Client,
    executor: HttpExecutor,
}

impl Harness {
    fn new(replies: impl IntoIterator<Item = AsyncRawResponse>) -> Self {
        Self::with_context(replies, Audience::Fabric, ServiceOrigin::Fabric)
    }

    fn with_context(
        replies: impl IntoIterator<Item = AsyncRawResponse>,
        audience: Audience,
        origin: ServiceOrigin,
    ) -> Self {
        let http = ScriptedHttp::new(replies);
        let timer = Arc::new(TestTimer::default());
        let credential = Arc::new(TestCredential::default());
        let endpoint = if origin == ServiceOrigin::Fabric {
            EndpointBinding::with_host(origin, HOST).unwrap()
        } else {
            EndpointBinding::public(origin)
        };
        let client = Client::with_context(RequestContext::with_endpoint(
            credential.clone(),
            CredentialSelection::new("offline@example.invalid", "synthetic-tenant", audience)
                .unwrap(),
            endpoint.clone(),
        ));
        let executor = HttpExecutor::new(http.clone(), timer.clone(), ExecutionOptions::default())
            .unwrap()
            .with_endpoint(endpoint);
        Self {
            http,
            timer,
            credential,
            client,
            executor,
        }
    }

    async fn execute(
        &self,
        prepared: PreparedCreateCommand,
        approval: &mut Approval,
        interrupted: impl Future<Output = ()> + Send,
    ) -> Result<CreateDelivery, CreateCommandError> {
        let trace = ExecutionTrace::default();
        let observe = || {
            let evidence = trace.snapshot();
            CreateTransportEvidence {
                http_status: evidence.status,
                operation_id: evidence.operation_id,
            }
        };
        prepared
            .execute(
                &KqlReadWriteCreate,
                &self.client,
                &self.executor,
                CreateExecution {
                    executor: self.executor.clone().with_trace(trace.clone()),
                    observe: &observe,
                },
                approval,
                interrupted,
            )
            .await
    }

    fn assert_wire(&self, raw: &[u8], polls: &[&str]) {
        let requests = self.http.requests.lock().unwrap();
        assert_eq!(requests.len(), 2 + polls.len());
        assert_eq!(requests[0].method(), Method::Get);
        assert_eq!(
            requests[0].url().path(),
            format!("/v1/workspaces/{WS}/items")
        );
        assert_eq!(requests[0].url().query(), Some("recursive=false"));
        assert_eq!(requests[1].method(), Method::Post);
        assert_eq!(
            requests[1].url().path(),
            format!("/v1/workspaces/{WS}/kqlDatabases")
        );
        let Body::Bytes(actual) = requests[1].body() else {
            panic!("native JSON must not become a streaming or converted payload")
        };
        assert_eq!(actual.as_ref(), raw);
        for (request, path) in requests[2..].iter().zip(polls) {
            assert_eq!(request.method(), Method::Get);
            assert_eq!(request.url().path(), *path);
        }
        for (index, request) in requests.iter().enumerate() {
            assert_eq!(request.url().scheme(), "https");
            assert_eq!(request.url().host_str(), Some(HOST));
            if index != 0 {
                assert!(request.url().query().is_none());
            }
            assert!(request.url().fragment().is_none());
        }
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.method() == Method::Post)
                .count(),
            1
        );
        assert_eq!(self.credential.calls.load(Ordering::SeqCst), requests.len());
    }

    fn assert_no_access(&self) {
        assert!(self.http.requests.lock().unwrap().is_empty());
        assert_eq!(self.credential.calls.load(Ordering::SeqCst), 0);
    }
}

struct ForbiddenRead;
impl Read for ForbiddenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("unsupported target/policy must reject before input I/O")
    }
}

#[test]
fn strict_required_readwrite_parent_and_exact_native_fields() {
    let mut invalid = vec![Value::Null, json!({}), json!([])];
    for field in ["displayName", "creationPayload"] {
        let mut value = body();
        value.as_object_mut().unwrap().remove(field);
        invalid.push(value);
        let mut value = body();
        value[field] = Value::Null;
        invalid.push(value);
    }
    for value in [
        Value::Null,
        json!(true),
        json!({}),
        json!(""),
        json!("eventhouse"),
        json!("00000000-0000-0000-0000-000000000000"),
        json!("https://other.invalid/eventhouses/22222222-2222-2222-2222-222222222222"),
        json!("/other.Workspace/Existing.Eventhouse"),
        json!({"workspaceId":WS,"itemId":PARENT}),
    ] {
        let mut input = body();
        input["creationPayload"]["parentEventhouseItemId"] = value;
        invalid.push(input);
    }
    for value in [
        Value::Null,
        json!(1),
        json!("Shortcut"),
        json!("readwrite"),
        json!("future"),
    ] {
        let mut input = body();
        input["creationPayload"]["databaseType"] = value;
        invalid.push(input);
    }
    for field in ["databaseType", "parentEventhouseItemId"] {
        let mut input = body();
        input["creationPayload"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        invalid.push(input);
    }
    for field in [
        "sourceClusterUri",
        "sourceDatabaseName",
        "invitationToken",
        "clusterURI",
        "query",
        "definition",
        "connection",
        "workspaceId",
        "unknown",
    ] {
        let mut input = body();
        input["creationPayload"][field] = json!("forbidden");
        invalid.push(input);
    }
    for field in [
        "type",
        "definition",
        "properties",
        "query",
        "connections",
        "unknown",
    ] {
        let mut input = body();
        input[field] = json!({});
        invalid.push(input);
    }
    for value in [
        Value::Null,
        json!(42),
        json!("other"),
        json!("database_1"),
        json!(""),
    ] {
        let mut input = body();
        input["displayName"] = value;
        invalid.push(input);
    }
    for input in invalid {
        for options in [vec![], vec!["--if-exists", "skip"]] {
            assert!(
                prepare(&serde_json::to_vec(&input).unwrap(), &options).is_err(),
                "{input}"
            );
        }
    }
    assert!(prepare(&serde_json::to_vec(&body()).unwrap(), &[]).is_ok());
}

#[test]
fn unique_utf8_json_bounds_and_missing_input_are_local() {
    for bytes in [
        b"".as_slice(), b"null", b"{", b"\xff",
        br#"{"displayName":"Database_1","displayName":"Database_1"}"#,
        br#"{"displayName":"Database_1","\u0064isplayName":"Database_1"}"#,
        br#"{"displayName":"Database_1","creationPayload":{"databaseType":"ReadWrite","parentEventhouseItemId":"22222222-2222-2222-2222-222222222222","parentEventhouseItemId":"22222222-2222-2222-2222-222222222222"}}"#,
        br#"{"displayName":"Database_1","creationPayload":{"databaseType":"ReadWrite","parentEventhouseItemId":"22222222-2222-2222-2222-222222222222"}} {}"#,
    ] {
        assert!(prepare(bytes, &["--if-exists", "skip"]).is_err());
    }
    let mut exact = serde_json::to_vec(&body()).unwrap();
    exact.resize(MAX_BYTES, b' ');
    assert!(prepare(&exact, &[]).is_ok());
    exact.push(b' ');
    assert!(prepare(&exact, &[]).is_err());
    let command = v2_create::parse_args(["create", TARGET]).unwrap();
    assert!(
        PreparedCreateCommand::preflight(
            command,
            &KqlReadWriteCreate,
            &std::env::current_dir().unwrap(),
            &mut ForbiddenRead,
            false,
        )
        .is_err()
    );
}

#[test]
fn policy_owner_scope_name_and_profile_reject_before_input() {
    for options in [
        vec!["--force"],
        vec!["--if-exists", "overwrite"],
        vec!["--if-exists", "duplicate"],
    ] {
        for input in ["-", "nonexistent.json"] {
            let command = v2_create::parse_args(
                ["create", TARGET, "--input", input]
                    .into_iter()
                    .chain(options.iter().copied()),
            )
            .unwrap();
            assert!(matches!(
                PreparedCreateCommand::preflight(
                    command,
                    &KqlReadWriteCreate,
                    &std::env::current_dir().unwrap(),
                    &mut ForbiddenRead,
                    false,
                ),
                Err(CreateCommandError::Policy(CreateError::UnsupportedPolicy))
            ));
        }
    }
    for path in [
        "/Name.Workspace/Database_1.KQLDatabase",
        "/11111111-1111-1111-1111-111111111111.Workspace/Database_1.Eventhouse",
        "/11111111-1111-1111-1111-111111111111.Workspace/Database_1.Notebook",
        "/11111111-1111-1111-1111-111111111111.Workspace/Database_1.kqldatabase",
        "/11111111-1111-1111-1111-111111111111.Workspace",
        "/11111111-1111-1111-1111-111111111111.Workspace/Database_1.KQLDatabase/src/definition",
    ] {
        let command = v2_create::parse_args(["create", path, "--input", "-"]).unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &KqlReadWriteCreate,
                &std::env::current_dir().unwrap(),
                &mut ForbiddenRead,
                false,
            )
            .is_err()
        );
    }
    for name in ["with space", "bad+name", "emoji\u{1f600}", "bad@name"] {
        let path = TARGET.replace("Database_1", name);
        let mut input = body();
        input["displayName"] = json!(name);
        let command = v2_create::parse_args(["create", &path, "--input", "-"]).unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &KqlReadWriteCreate,
                &std::env::current_dir().unwrap(),
                &mut &serde_json::to_vec(&input).unwrap()[..],
                false,
            )
            .is_err()
        );
    }
    assert!(
        prepare(
            &serde_json::to_vec(&body()).unwrap(),
            &["--profile", "named"]
        )
        .is_err()
    );
    for policy in ["error", "overwrite", "skip", "duplicate"] {
        assert!(
            v2_create::parse_args(["create", TARGET, "--force", "--if-exists", policy]).is_err()
        );
    }
}

#[test]
fn documented_optional_fields_remain_strict_and_native() {
    let mut input = body();
    input["description"] = json!("x".repeat(256));
    input["folderId"] = Value::Null;
    input["sensitivityLabelSettings"] = json!({
        "labelId":ITEM,"sensitivityLabelApplyStrategy":"ApplyOrFail"
    });
    assert!(prepare(&serde_json::to_vec(&input).unwrap(), &[]).is_ok());
    for (field, invalid) in [
        ("description", Value::Null),
        ("description", json!("x".repeat(257))),
        ("folderId", json!("folder")),
        ("folderId", json!(ITEM)),
        ("sensitivityLabelSettings", Value::Null),
        ("sensitivityLabelSettings", json!({})),
        ("sensitivityLabelSettings", json!({"labelId":"invalid"})),
        (
            "sensitivityLabelSettings",
            json!({"labelId":ITEM,"sensitivityLabelApplyStrategy":null}),
        ),
        (
            "sensitivityLabelSettings",
            json!({"labelId":ITEM,"sensitivityLabelApplyStrategy":"future"}),
        ),
        (
            "sensitivityLabelSettings",
            json!({"labelId":ITEM,"extra":false}),
        ),
    ] {
        let mut input = body();
        input[field] = invalid;
        for options in [vec![], vec!["--if-exists", "skip"]] {
            assert!(prepare(&serde_json::to_vec(&input).unwrap(), &options).is_err());
        }
    }
}

#[tokio::test]
async fn native_post_preserves_full_bytes_and_immediate_approval_context() {
    let raw = format!(
        "{{\r\n \"displayName\":\"Database_1\", \"description\":\"native\", \
         \"creationPayload\":{{\"parentEventhouseItemId\":\"{PARENT}\",\"databaseType\":\"ReadWrite\"}} }}"
    ).into_bytes();
    let harness = Harness::new([missing(), json_response(201, resource())]);
    let mut approval = Approval::default();
    let delivery = harness
        .execute(prepare(&raw, &[]).unwrap(), &mut approval, pending())
        .await
        .unwrap();
    assert_eq!(approval.calls.len(), 1);
    let approved = &approval.calls[0];
    assert_eq!(approved["workspace"], WS);
    assert_eq!(approved["name"], "Database_1");
    assert_eq!(approved["type"], "KQLDatabase");
    assert_eq!(
        approved["body"],
        serde_json::from_slice::<Value>(&raw).unwrap()
    );
    assert_eq!(approved["account"], "offline@example.invalid");
    assert_eq!(approved["tenant"], "synthetic-tenant");
    assert_eq!(approved["audience"], "Fabric");
    assert!(approved["endpoint"].as_str().unwrap().contains(HOST));
    assert_eq!(approved["remoteWrite"], true);
    assert_eq!(approved["method"], "Post");
    assert_eq!(
        approved["url"],
        format!("https://{HOST}/v1/workspaces/{WS}/kqlDatabases")
    );
    assert_eq!(approved["wireBody"], std::str::from_utf8(&raw).unwrap());
    assert_eq!(approved["anticipatedEffects"], json!([]));
    let question = approved["question"].as_str().unwrap();
    assert!(question.contains("method=POST"));
    assert!(question.contains(&format!(
        "url=\"https://{HOST}/v1/workspaces/{WS}/kqlDatabases\""
    )));
    assert!(
        question.contains(
            &std::str::from_utf8(&raw)
                .unwrap()
                .escape_debug()
                .to_string()
        )
    );
    assert!(question.contains("audience=Fabric"));
    assert_eq!(approved["timeout"], 30_000);
    assert_eq!(delivery.report().value()["meta"]["status"], "completed");
    assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
    assert_eq!(
        delivery.report().value()["meta"]["operationHandle"],
        Value::Null
    );
    assert_eq!(delivery.report().exit_code(), 0);
    harness.assert_wire(&raw, &[]);
}

#[tokio::test]
async fn denied_approval_and_wrong_context_do_not_post() {
    let raw = serde_json::to_vec(&body()).unwrap();
    let harness = Harness::new([missing()]);
    let mut approval = Approval {
        denied: true,
        ..Approval::default()
    };
    assert!(matches!(
        harness
            .execute(prepare(&raw, &[]).unwrap(), &mut approval, pending())
            .await,
        Err(CreateCommandError::Approval(CreateApprovalError::Denied))
    ));
    assert_eq!(harness.http.requests.lock().unwrap().len(), 1);
    for (audience, origin) in [
        (Audience::Arm, ServiceOrigin::Fabric),
        (Audience::Fabric, ServiceOrigin::Arm),
    ] {
        let harness = Harness::with_context([], audience, origin);
        assert!(matches!(
            harness
                .execute(
                    prepare(&raw, &[]).unwrap(),
                    &mut Approval::default(),
                    pending()
                )
                .await,
            Err(CreateCommandError::Invalid)
        ));
        harness.assert_no_access();
    }
}

#[tokio::test]
async fn exact_skip_collision_ambiguity_wrong_kind_scope_and_race_are_distinct() {
    let raw = serde_json::to_vec(&body()).unwrap();
    for options in [vec![], vec!["--if-exists", "skip"]] {
        let harness = Harness::new([json_response(200, json!({"value":[resource()]}))]);
        let mut approval = Approval::default();
        let result = harness
            .execute(prepare(&raw, &options).unwrap(), &mut approval, pending())
            .await;
        if options.is_empty() {
            assert!(matches!(
                result,
                Err(CreateCommandError::Policy(CreateError::AlreadyExists))
            ));
            assert!(approval.calls.is_empty());
        } else {
            let delivery = result.unwrap();
            assert_eq!(delivery.report().value()["meta"]["status"], "unchanged");
            assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
            assert_eq!(
                delivery.report().value()["meta"]["contentEqualityClaimed"],
                false
            );
            assert_eq!(approval.calls[0]["remoteWrite"], false);
            assert_eq!(approval.calls[0]["wireBody"], Value::Null);
            assert_eq!(approval.calls[0]["url"], Value::Null);
            assert_eq!(approval.calls[0]["anticipatedEffects"], json!([]));
            assert!(
                approval.calls[0]["question"]
                    .as_str()
                    .unwrap()
                    .contains("no native request")
            );
        }
        assert_eq!(harness.http.requests.lock().unwrap().len(), 1);
    }
    for rows in [
        json!([resource(), resource()]),
        json!([{"id":ITEM,"type":"Eventhouse","displayName":"Database_1"}]),
        json!([{"id":ITEM,"type":"KQLDatabase","displayName":"database_1"}]),
        json!([{"id":ITEM,"type":"KQLDatabase","displayName":"Database_1","workspaceId":PARENT}]),
    ] {
        let harness = Harness::new([json_response(200, json!({"value":rows}))]);
        let mut approval = Approval::default();
        assert!(
            harness
                .execute(
                    prepare(&raw, &["--if-exists", "skip"]).unwrap(),
                    &mut approval,
                    pending()
                )
                .await
                .is_err()
        );
        assert!(approval.calls.is_empty());
        assert_eq!(harness.http.requests.lock().unwrap().len(), 1);
    }
    let harness = Harness::new([
        missing(),
        json_response(409, json!({"errorCode":"ItemDisplayNameAlreadyInUse"})),
    ]);
    let delivery = harness
        .execute(
            prepare(&raw, &["--if-exists", "skip"]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(delivery.report().value()["meta"]["status"], "failed");
    assert_ne!(delivery.report().value()["meta"]["status"], "unchanged");
    harness.assert_wire(&raw, &[]);
}

#[tokio::test]
async fn skip_missing_target_posts_once_and_parent_authority_is_service_owned() {
    let raw = serde_json::to_vec(&body()).unwrap();
    for (status, data) in [
        (201, resource()),
        (
            403,
            json!({"errorCode":"InsufficientPrivileges","message":"synthetic parent access denied"}),
        ),
        (
            404,
            json!({"errorCode":"ItemNotFound","message":"synthetic parent does not exist here"}),
        ),
        (429, json!({"errorCode":"TooManyRequests"})),
        (500, json!({"errorCode":"InternalError"})),
    ] {
        let harness = Harness::new([missing(), json_response(status, data)]);
        let delivery = harness
            .execute(
                prepare(&raw, &["--if-exists", "skip"]).unwrap(),
                &mut Approval::default(),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), u8::from(status != 201));
        if status != 201 {
            assert_eq!(delivery.report().value()["meta"]["httpStatus"], status);
            assert!(!delivery.report().value()["meta"]["serviceError"].is_null());
        }
        harness.assert_wire(&raw, &[]);
    }
}

#[tokio::test]
async fn incomplete_or_foreign_discovery_never_authorizes_a_post() {
    let raw = serde_json::to_vec(&body()).unwrap();
    for reply in [
        json_response(
            200,
            json!({"value":[],"continuationToken":"next","continuationUri":"https://foreign.invalid/v1/workspaces/11111111-1111-1111-1111-111111111111/items"}),
        ),
        json_response(
            200,
            json!({"value":[{"id":null,"displayName":"Database_1"}]}),
        ),
        response(200, "{", &[]),
        json_response(403, json!({"errorCode":"InsufficientPrivileges"})),
    ] {
        let harness = Harness::new([reply]);
        let mut approval = Approval::default();
        assert!(
            harness
                .execute(prepare(&raw, &[]).unwrap(), &mut approval, pending(),)
                .await
                .is_err()
        );
        assert!(approval.calls.is_empty());
        assert_eq!(harness.http.requests.lock().unwrap().len(), 1);
    }
    let harness = Harness::new([]);
    assert!(
        harness
            .execute(
                prepare(&raw, &[]).unwrap(),
                &mut Approval::default(),
                ready(()),
            )
            .await
            .is_err()
    );
    harness.assert_no_access();
}

#[tokio::test]
async fn malformed_acceptance_body_survives_poll_failure_as_explicit_base64() {
    use base64::Engine;
    let raw = serde_json::to_vec(&body()).unwrap();
    let harness = Harness::new([
        missing(),
        accepted("not-json", "0"),
        json_response(
            200,
            json!({"status":"Failed","error":{"errorCode":"ParentRejected"}}),
        ),
    ]);
    let delivery = harness
        .execute(
            prepare(&raw, &["--wait"]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    let value = delivery.report().value();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(
        value["meta"]["unparsedAcceptanceBodyBase64"],
        base64::engine::general_purpose::STANDARD.encode(b"not-json")
    );
    assert_eq!(value["meta"]["resourceId"], Value::Null);
    assert_eq!(
        value["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    harness.assert_wire(&raw, &[&format!("/v1/operations/{OP}")]);
}

#[tokio::test]
async fn malformed_acceptance_without_wait_keeps_both_body_and_initial_status() {
    use base64::Engine;
    let raw = serde_json::to_vec(&body()).unwrap();
    let harness = Harness::new([missing(), accepted("not-json", "0")]);
    let delivery = harness
        .execute(
            prepare(&raw, &[]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    let value = delivery.report().value();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(value["meta"]["status"], "unknown");
    assert_eq!(value["meta"]["completed"], false);
    assert_eq!(value["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(value["meta"]["submission"]["resource"], Value::Null);
    assert_eq!(
        value["meta"]["submission"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(value["meta"]["observation"]["httpStatus"], 202);
    assert_eq!(value["meta"]["resourceId"], Value::Null);
    assert_eq!(
        value["meta"]["unparsedAcceptanceBodyBase64"],
        base64::engine::general_purpose::STANDARD.encode(b"not-json")
    );
    assert_eq!(
        value["meta"]["unparsedResponseBodyBase64"],
        base64::engine::general_purpose::STANDARD.encode(b"not-json")
    );
    assert_eq!(value["meta"]["remoteReplayed"], false);
    assert_eq!(value["meta"]["remoteCancelled"], false);
    harness.assert_wire(&raw, &[]);
}

#[tokio::test]
async fn malformed_final_result_keeps_submission_resource_independently_of_last_response() {
    use base64::Engine;
    let raw = serde_json::to_vec(&body()).unwrap();
    let harness = Harness::new([
        missing(),
        accepted(&resource().to_string(), "0"),
        json_response(200, json!({"status":"Succeeded"})),
        response(200, "not-json", &[]),
    ]);
    let delivery = harness
        .execute(
            prepare(&raw, &["--wait", "--project", "missing"]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    let mut output = Vec::new();
    delivery.deliver(&mut output).unwrap();
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["data"], Value::Null);
    assert_eq!(value["meta"]["status"], "unknown");
    assert_eq!(value["meta"]["completed"], false);
    assert_eq!(value["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(value["meta"]["submission"]["resource"], resource());
    assert_eq!(
        value["meta"]["submission"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(value["meta"]["observation"]["httpStatus"], 200);
    assert_eq!(value["meta"]["httpStatus"], 200);
    assert_eq!(value["meta"]["resourceId"], ITEM);
    assert_eq!(
        value["meta"]["unparsedResponseBodyBase64"],
        base64::engine::general_purpose::STANDARD.encode(b"not-json")
    );
    assert_eq!(value["meta"]["remoteReplayed"], false);
    assert_eq!(value["meta"]["remoteCancelled"], false);
    harness.assert_wire(
        &raw,
        &[
            &format!("/v1/operations/{OP}"),
            &format!("/v1/operations/{OP}/result"),
        ],
    );
}

#[tokio::test]
async fn unexpected_success_result_retains_typed_http_cause_but_not_definitive_failure() {
    use fabric_sdk_operations::OperationErrorKind;
    let raw = serde_json::to_vec(&body()).unwrap();
    for status in [201, 202, 204, 400, 403, 409] {
        let harness = Harness::new([
            missing(),
            accepted(&resource().to_string(), "0"),
            json_response(200, json!({"status":"Succeeded"})),
            if status == 204 {
                response(status, "", &[])
            } else if status < 300 {
                json_response(status, resource())
            } else {
                json_response(status, json!({"errorCode":"NativeResultRejected"}))
            },
        ]);
        let delivery = harness
            .execute(
                prepare(&raw, &["--wait", "--project", "missing"]).unwrap(),
                &mut Approval::default(),
                pending(),
            )
            .await
            .unwrap();
        let report = delivery.report();
        assert_eq!(report.exit_code(), 1);
        assert!(matches!(
            &report.error().unwrap().kind,
            OperationErrorKind::Http {status:actual,..} if *actual == status
        ));
        let mut output = Vec::new();
        delivery.deliver(&mut output).unwrap();
        let value: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["data"], Value::Null);
        assert_eq!(
            value["error"]["code"],
            if status < 300 {
                "InvalidResponse"
            } else {
                "ServiceRejected"
            }
        );
        assert_eq!(
            value["meta"]["status"],
            if status < 300 { "unknown" } else { "failed" }
        );
        assert_eq!(value["meta"]["completed"], false);
        assert_eq!(value["meta"]["httpStatus"], status);
        assert_eq!(value["meta"]["observation"]["httpStatus"], status);
        assert_eq!(value["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(value["meta"]["submission"]["resource"], resource());
        assert_eq!(
            value["meta"]["submission"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(value["meta"]["resourceId"], ITEM);
        assert_eq!(
            value["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(value["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(value["meta"]["remoteReplayed"], false);
        assert_eq!(value["meta"]["remoteCancelled"], false);
        harness.assert_wire(
            &raw,
            &[
                &format!("/v1/operations/{OP}"),
                &format!("/v1/operations/{OP}/result"),
            ],
        );
    }
}

#[tokio::test]
async fn same_name_foreign_owner_is_not_a_second_kql_match() {
    let raw = serde_json::to_vec(&body()).unwrap();
    let harness = Harness::new([json_response(
        200,
        json!({"value":[
            {"id":PARENT,"workspaceId":WS,"displayName":"Database_1","type":"Eventhouse"},
            resource()
        ]}),
    )]);
    let mut approval = Approval::default();
    let delivery = harness
        .execute(
            prepare(&raw, &["--if-exists", "skip"]).unwrap(),
            &mut approval,
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    assert_eq!(delivery.report().value()["meta"]["status"], "unchanged");
    assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
    assert_eq!(approval.calls.len(), 1);
    assert_eq!(approval.calls[0]["remoteWrite"], false);
    assert_eq!(harness.http.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn root_identity_and_protected_folder_observation_do_not_guess_nested_targets() {
    let raw = serde_json::to_vec(&body()).unwrap();
    for folder in [None, Some(Value::Null)] {
        let mut root = resource();
        if let Some(folder) = &folder {
            root["folderId"] = folder.clone();
        }
        let mut nested = resource();
        nested["id"] = json!(OP);
        nested["folderId"] = json!(PARENT);
        let harness = Harness::new([json_response(200, json!({"value":[nested,root]}))]);
        let delivery = harness
            .execute(
                prepare(&raw, &["--if-exists", "skip", "--project", "missing"]).unwrap(),
                &mut Approval::default(),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 0);
        let mut output = Vec::new();
        delivery.deliver(&mut output).unwrap();
        let value: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["meta"]["status"], "unchanged");
        assert_eq!(value["meta"]["resourceId"], ITEM);
        assert_eq!(value["meta"]["folderId"], Value::Null);
        assert_eq!(value["meta"]["folderIdObserved"], folder.is_some());
        assert_eq!(harness.http.requests.lock().unwrap().len(), 1);
    }
    for folder in [
        json!(PARENT),
        json!("invalid-folder"),
        json!(false),
        json!({}),
        json!(0),
    ] {
        let mut nested = resource();
        nested["folderId"] = folder;
        let harness = Harness::new([json_response(200, json!({"value":[nested]}))]);
        let mut approval = Approval::default();
        assert!(
            harness
                .execute(
                    prepare(&raw, &["--if-exists", "skip"]).unwrap(),
                    &mut approval,
                    pending(),
                )
                .await
                .is_err()
        );
        assert!(approval.calls.is_empty());
        assert_eq!(harness.http.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn root_pagination_preserves_recursive_filter_and_complete_typed_cardinality() {
    let raw = serde_json::to_vec(&body()).unwrap();
    for ambiguous in [false, true] {
        let first = if ambiguous {
            resource()
        } else {
            json!({"id":PARENT,"workspaceId":WS,"displayName":"Database_1","type":"Eventhouse"})
        };
        let harness = Harness::new([
            json_response(200, json!({"value":[first],"continuationToken":"next/+"})),
            json_response(200, json!({"value":[resource()]})),
        ]);
        let mut approval = Approval::default();
        let result = harness
            .execute(
                prepare(&raw, &["--if-exists", "skip"]).unwrap(),
                &mut approval,
                pending(),
            )
            .await;
        if ambiguous {
            assert!(matches!(
                result,
                Err(CreateCommandError::Policy(CreateError::AmbiguousLeaf))
            ));
            assert!(approval.calls.is_empty());
        } else {
            let delivery = result.unwrap();
            assert_eq!(delivery.report().value()["meta"]["status"], "unchanged");
            assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
            assert_eq!(approval.calls.len(), 1);
        }
        let requests = harness.http.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].url().query(), Some("recursive=false"));
        let pairs = requests[1]
            .url()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        assert_eq!(
            pairs,
            vec![
                ("recursive".to_owned(), "false".to_owned()),
                ("continuationToken".to_owned(), "next/+".to_owned())
            ]
        );
        for request in requests.iter() {
            assert_eq!(request.method(), Method::Get);
            assert_eq!(request.url().path(), format!("/v1/workspaces/{WS}/items"));
            assert_eq!(request.url().host_str(), Some(HOST));
        }
    }
}

#[tokio::test]
async fn completed_nested_or_malformed_folder_never_claims_root_creation() {
    let raw = serde_json::to_vec(&body()).unwrap();
    for folder in [
        json!(PARENT),
        json!("invalid-folder"),
        json!(false),
        json!({}),
    ] {
        for wait in [false, true] {
            let mut observed = resource();
            observed["folderId"] = folder.clone();
            let mut replies = vec![missing()];
            if wait {
                replies.push(accepted(&resource().to_string(), "0"));
                replies.push(json_response(200, json!({"status":"Succeeded"})));
            }
            replies.push(json_response(if wait { 200 } else { 201 }, observed));
            let harness = Harness::new(replies);
            let options = if wait {
                vec!["--wait", "--project", "missing"]
            } else {
                vec!["--project", "missing"]
            };
            let delivery = harness
                .execute(
                    prepare(&raw, &options).unwrap(),
                    &mut Approval::default(),
                    pending(),
                )
                .await
                .unwrap();
            assert_eq!(delivery.report().exit_code(), 1);
            let mut output = Vec::new();
            delivery.deliver(&mut output).unwrap();
            let value: Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(value["data"], Value::Null);
            assert_eq!(value["meta"]["status"], "unknown");
            assert_eq!(value["meta"]["completed"], false);
            assert_eq!(value["meta"]["folderId"], folder);
            assert_eq!(value["meta"]["folderIdObserved"], true);
            assert_eq!(value["meta"]["resourceId"], ITEM);
            assert_eq!(value["meta"]["remoteReplayed"], false);
            assert_eq!(value["meta"]["remoteCancelled"], false);
            if wait {
                assert_eq!(value["meta"]["submission"]["httpStatus"], 202);
                assert_eq!(value["meta"]["submission"]["resource"], resource());
                assert_eq!(value["meta"]["submission"]["folderIdObserved"], false);
                harness.assert_wire(
                    &raw,
                    &[
                        &format!("/v1/operations/{OP}"),
                        &format!("/v1/operations/{OP}/result"),
                    ],
                );
            } else {
                assert_eq!(value["meta"]["submission"]["httpStatus"], 201);
                assert_eq!(value["meta"]["submission"]["folderId"], folder);
                assert_eq!(value["meta"]["submission"]["folderIdObserved"], true);
                harness.assert_wire(&raw, &[]);
            }
        }
    }
}

#[tokio::test]
async fn explicit_root_folder_observation_survives_accepted_failure_and_projection() {
    let raw = serde_json::to_vec(&body()).unwrap();
    let mut accepted_resource = resource();
    accepted_resource["folderId"] = Value::Null;
    let harness = Harness::new([
        missing(),
        accepted(&accepted_resource.to_string(), "0"),
        json_response(
            200,
            json!({"status":"Failed","error":{"errorCode":"NativeFailure"}}),
        ),
    ]);
    let delivery = harness
        .execute(
            prepare(&raw, &["--wait", "--project", "missing"]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 1);
    let mut output = Vec::new();
    delivery.deliver(&mut output).unwrap();
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["data"], Value::Null);
    assert_eq!(value["meta"]["folderId"], Value::Null);
    assert_eq!(value["meta"]["folderIdObserved"], true);
    assert_eq!(value["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(value["meta"]["submission"]["folderId"], Value::Null);
    assert_eq!(value["meta"]["submission"]["folderIdObserved"], true);
    assert_eq!(value["meta"]["submission"]["resource"], resource());
    assert_eq!(
        value["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    harness.assert_wire(&raw, &[&format!("/v1/operations/{OP}")]);
}

#[tokio::test]
async fn accepted_handle_is_not_resource_identity_and_wait_fetches_actual_result() {
    let raw = serde_json::to_vec(&body()).unwrap();
    let harness = Harness::new([missing(), accepted("", "7")]);
    let delivery = harness
        .execute(
            prepare(&raw, &[]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    let meta = &delivery.report().value()["meta"];
    assert_eq!(meta["status"], "accepted");
    assert_eq!(meta["completed"], false);
    assert_eq!(meta["resourceId"], Value::Null);
    assert_eq!(meta["operationHandle"], format!("/operations/{OP}"));
    harness.assert_wire(&raw, &[]);
    let harness = Harness::new([
        missing(),
        accepted("", "7"),
        response(200, r#"{"status":"Running"}"#, &[("Retry-After", "3")]),
        json_response(200, json!({"status":"Succeeded"})),
        json_response(200, resource()),
    ]);
    let mut approval = Approval::default();
    let delivery = harness
        .execute(
            prepare(&raw, &["--wait"]).unwrap(),
            &mut approval,
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(approval.calls[0]["timeout"], 300_000);
    assert_eq!(delivery.report().value()["meta"]["status"], "completed");
    assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
    assert_eq!(
        delivery.report().value()["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(*harness.timer.waits.lock().unwrap(), [7, 3]);
    harness.assert_wire(
        &raw,
        &[
            &format!("/v1/operations/{OP}"),
            &format!("/v1/operations/{OP}"),
            &format!("/v1/operations/{OP}/result"),
        ],
    );
}

#[tokio::test]
async fn failed_poll_keeps_accepted_body_handle_status_and_service_error() {
    let raw = serde_json::to_vec(&body()).unwrap();
    for state in ["Failed", "Cancelled", "FutureState"] {
        let harness = Harness::new([
            missing(),
            accepted(&resource().to_string(), "0"),
            json_response(
                200,
                json!({"status":state,"error":{"errorCode":"ParentRejected"}}),
            ),
        ]);
        let delivery = harness
            .execute(
                prepare(&raw, &["--wait"]).unwrap(),
                &mut Approval::default(),
                pending(),
            )
            .await
            .unwrap();
        let value = delivery.report().value();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(value["data"], resource());
        assert_eq!(value["meta"]["resourceId"], ITEM);
        assert_eq!(
            value["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(value["meta"]["remoteReplayed"], false);
        assert_eq!(value["meta"]["remoteCancelled"], false);
        assert_eq!(value["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(value["meta"]["submission"]["resource"], resource());
        assert_eq!(
            value["meta"]["submission"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(value["meta"]["observation"]["httpStatus"], 200);
        assert_eq!(value["meta"]["httpStatus"], 200);
        if state != "FutureState" {
            assert_eq!(value["meta"]["operationStatus"], state);
            assert_eq!(value["meta"]["serviceError"]["errorCode"], "ParentRejected");
        }
        harness.assert_wire(&raw, &[&format!("/v1/operations/{OP}")]);
    }
}

#[tokio::test]
async fn invalid_acceptance_result_and_foreign_lro_never_become_success_or_replay() {
    let raw = serde_json::to_vec(&body()).unwrap();
    for reply in [
        response(202, "", &[]),
        response(202, "", &[("x-ms-operation-id", "not-a-guid")]),
        response(
            202,
            "",
            &[
                ("x-ms-operation-id", OP),
                (
                    "Location",
                    "https://foreign.invalid/v1/operations/44444444-4444-4444-4444-444444444444",
                ),
            ],
        ),
        json_response(201, json!({"id":OP})),
        json_response(
            201,
            json!({"id":ITEM,"workspaceId":PARENT,"type":"KQLDatabase","displayName":"Database_1"}),
        ),
        json_response(
            201,
            json!({"id":ITEM,"workspaceId":WS,"type":"Eventhouse","displayName":"Database_1"}),
        ),
        json_response(
            201,
            json!({"id":ITEM,"workspaceId":WS,"type":"KQLDatabase","displayName":"changed"}),
        ),
        json_response(200, resource()),
    ] {
        let harness = Harness::new([missing(), reply]);
        let delivery = harness
            .execute(
                prepare(&raw, &[]).unwrap(),
                &mut Approval::default(),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(delivery.report().value()["meta"]["completed"], false);
        harness.assert_wire(&raw, &[]);
    }
    for delay in ["bad", "999999999"] {
        let harness = Harness::new([missing(), accepted(&resource().to_string(), delay)]);
        let delivery = harness
            .execute(
                prepare(&raw, &["--wait"]).unwrap(),
                &mut Approval::default(),
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
        harness.assert_wire(&raw, &[]);
    }
}

#[tokio::test]
async fn cancellation_and_deadline_keep_accepted_evidence_without_remote_cancel() {
    let raw = serde_json::to_vec(&body()).unwrap();
    for deadline in [false, true] {
        let harness = Harness::new([missing(), accepted(&resource().to_string(), "1")]);
        harness.timer.pending_wait.store(true, Ordering::SeqCst);
        let interrupted = async {
            poll_fn(|cx| {
                if harness.http.requests.lock().unwrap().len() >= 2 {
                    Poll::Ready(())
                } else {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            })
            .await;
            if deadline {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        };
        let delivery = harness
            .execute(
                prepare(&raw, &["--wait"]).unwrap(),
                &mut Approval::default(),
                interrupted,
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        let value = delivery.report().value();
        assert_eq!(value["meta"]["resourceId"], ITEM);
        assert_eq!(
            value["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(value["meta"]["remoteCancelled"], false);
        assert_eq!(value["meta"]["remoteReplayed"], false);
        assert_eq!(value["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(value["meta"]["submission"]["resource"], resource());
        assert_eq!(
            value["meta"]["submission"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(value["meta"]["observation"]["httpStatus"], 202);
        assert_eq!(
            harness
                .http
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.method() == Method::Post)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn partial_acceptance_body_retains_observed_header_without_inventing_resource() {
    let raw = serde_json::to_vec(&body()).unwrap();
    let mut headers = Headers::new();
    headers.insert("x-ms-operation-id", OP);
    headers.insert("location", format!("https://{HOST}/v1/operations/{OP}"));
    let broken = stream::once(async {
        Err(azure_core::Error::new(
            azure_core::error::ErrorKind::Other,
            "synthetic body failure",
        ))
    });
    let harness = Harness::new([
        missing(),
        AsyncRawResponse::new(202.into(), headers, Box::pin(broken)),
    ]);
    let delivery = harness
        .execute(
            prepare(&raw, &[]).unwrap(),
            &mut Approval::default(),
            pending(),
        )
        .await
        .unwrap();
    let value = delivery.report().value();
    assert_eq!(delivery.report().exit_code(), 1);
    assert_eq!(value["meta"]["httpStatus"], 202);
    assert_eq!(
        value["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(value["meta"]["resourceId"], Value::Null);
    assert_eq!(value["meta"]["status"], "unknown");
    assert_eq!(value["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(
        value["meta"]["submission"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(value["meta"]["submission"]["resource"], Value::Null);
    harness.assert_wire(&raw, &[]);
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn projection_and_failed_delivery_keep_unprojected_evidence_and_no_clobber() {
    let raw = serde_json::to_vec(&body()).unwrap();
    for broken in [false, true] {
        let harness = Harness::new([missing(), accepted(&resource().to_string(), "0")]);
        let delivery = harness
            .execute(
                prepare(&raw, &["--project", "missing"]).unwrap(),
                &mut Approval::default(),
                pending(),
            )
            .await
            .unwrap();
        if broken {
            let Err(CreateCommandError::Delivery { report, .. }) =
                delivery.deliver(&mut BrokenWriter)
            else {
                panic!("broken pipe must retain the original report")
            };
            assert_eq!(report.value()["data"], resource());
            assert_eq!(report.value()["meta"]["resourceId"], ITEM);
            assert_eq!(report.value()["meta"]["submission"]["httpStatus"], 202);
            assert_eq!(report.value()["meta"]["submission"]["resource"], resource());
            assert_eq!(report.value()["meta"]["anticipatedEffects"], json!([]));
            assert_eq!(
                report.value()["meta"]["operationHandle"],
                format!("/operations/{OP}")
            );
        } else {
            let mut output = Vec::new();
            delivery.deliver(&mut output).unwrap();
            let value: Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(value["data"], Value::Null);
            assert_eq!(value["meta"]["resourceId"], ITEM);
            assert_eq!(value["meta"]["submission"]["httpStatus"], 202);
            assert_eq!(value["meta"]["submission"]["resource"], resource());
            assert_eq!(value["meta"]["anticipatedEffects"], json!([]));
            assert_eq!(
                value["meta"]["operationHandle"],
                format!("/operations/{OP}")
            );
        }
        harness.assert_wire(&raw, &[]);
    }
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("receipt.json");
    let options = ["--output", path.to_str().unwrap()];
    let prepared = prepare(&raw, &options).unwrap();
    assert!(!path.exists());
    std::fs::write(&path, b"raced-existing").unwrap();
    let harness = Harness::new([missing(), accepted("", "0")]);
    let delivery = harness
        .execute(prepared, &mut Approval::default(), pending())
        .await
        .unwrap();
    assert!(matches!(
        delivery.deliver(&mut Vec::new()),
        Err(CreateCommandError::Delivery { .. })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), b"raced-existing");
    assert!(prepare(&raw, &options).is_err());
    harness.assert_wire(&raw, &[]);
}

#[test]
fn native_operation_rejects_mismatched_raw_bytes_or_another_action() {
    let raw = serde_json::to_vec(&body()).unwrap();
    let target = create::CreateTarget::new(
        create::TargetScope::NamedLeaf,
        create::NativeId::new(WS).unwrap(),
        "Database_1",
        "KQLDatabase",
    )
    .unwrap();
    let capabilities = KqlReadWriteCreate.capabilities();
    let plan = create::plan_create(
        ConflictPolicy::Error,
        target,
        Preflight::Leaf(LeafState::Missing),
        &capabilities,
        Some(body()),
        &KqlReadWriteCreate,
    )
    .unwrap();
    assert!(matches!(plan.action(), CreateAction::Create { .. }));
    assert!(KqlReadWriteCreate.operation(&plan, None, false).is_err());
    assert!(
        KqlReadWriteCreate
            .operation(&plan, Some(b"{}"), false)
            .is_err()
    );
    assert!(
        KqlReadWriteCreate
            .operation(&plan, Some(&raw), false)
            .is_ok()
    );
    let plan = create::plan_create(
        ConflictPolicy::Skip,
        plan.target().clone(),
        Preflight::Leaf(LeafState::Existing(
            create::ResourceIdentity::new(
                create::NativeId::new(ITEM).unwrap(),
                create::NativeId::new(WS).unwrap(),
                "KQLDatabase",
            )
            .unwrap(),
        )),
        &capabilities,
        Some(body()),
        &KqlReadWriteCreate,
    )
    .unwrap();
    assert!(
        KqlReadWriteCreate
            .operation(&plan, Some(&raw), false)
            .is_err()
    );
}
