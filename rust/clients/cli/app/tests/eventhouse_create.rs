// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline parser -> shared host -> SDK planner -> shared HTTP executor.
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    error::ErrorKind,
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::{
    create_command::{
        CreateApproval, CreateApprovalError, CreateApprovalRequest, CreateBinding,
        CreateCommandError, CreateDelivery, CreateEffectKind, CreateExecution,
        CreateTransportEvidence, MAX_BYTES, PreparedCreateCommand, v2_create,
    },
    eventhouse_create::EventhouseCreate,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    items::create::{
        ConflictPolicy, CreateAction, CreateError, CreateInputValidator, CreateTarget, LeafState,
        NativeId, Preflight, SchemaRef, TargetScope, plan_create,
    },
};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, ExecutionTrace, HttpExecutor,
};
use fabric_sdk_operations::OperationErrorKind;
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use futures_util::stream;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending},
    io::{self, Read, Write},
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OP: &str = "33333333-3333-3333-3333-333333333333";
const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Eventhouse";

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
            "synthetic-eventhouse",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Wire {
    replies: Mutex<VecDeque<AsyncRawResponse>>,
    requests: Mutex<Vec<Request>>,
}
#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request/replay"))
    }
}

#[derive(Debug)]
struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(tokio::time::sleep(delay))
    }
}

struct Harness {
    client: Client,
    http: HttpExecutor,
    wire: Arc<Wire>,
    credential: Arc<Credential>,
}
impl Harness {
    fn new(replies: Vec<AsyncRawResponse>) -> Self {
        let wire = Arc::new(Wire {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
        });
        let credential = Arc::new(Credential::default());
        let endpoint =
            EndpointBinding::with_host(ServiceOrigin::Fabric, "api.fabric.microsoft.com").unwrap();
        let client = Client::with_context(RequestContext::with_endpoint(
            credential.clone(),
            CredentialSelection::new("synthetic-owner", WS, Audience::Fabric).unwrap(),
            endpoint.clone(),
        ));
        let http = HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default())
            .unwrap()
            .with_endpoint(endpoint);
        Self {
            client,
            http,
            wire,
            credential,
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
            let evidence = trace.snapshot();
            CreateTransportEvidence {
                http_status: evidence.status,
                operation_id: evidence.operation_id,
            }
        };
        prepared
            .execute(
                &EventhouseCreate,
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

    fn assert_posts(&self, expected: usize) {
        let requests = self.wire.requests.lock().unwrap();
        let posts: Vec<_> = requests
            .iter()
            .filter(|request| request.method() == Method::Post)
            .collect();
        assert_eq!(posts.len(), expected);
        for request in posts {
            assert_eq!(
                request.url().path(),
                format!("/v1/workspaces/{WS}/eventhouses")
            );
            assert!(request.url().query().is_none());
        }
        for request in requests
            .iter()
            .filter(|request| request.url().path() == format!("/v1/workspaces/{WS}/items"))
        {
            assert_eq!(request.method(), Method::Get);
            let query: Vec<_> = request.url().query_pairs().collect();
            assert_eq!(
                query.iter().filter(|(key, _)| key == "recursive").count(),
                1
            );
            assert!(
                query
                    .iter()
                    .any(|(key, value)| key == "recursive" && value == "false")
            );
            assert!(
                query
                    .iter()
                    .all(|(key, _)| key == "recursive" || key == "continuationToken")
            );
            assert!(Bytes::from(request.body()).is_empty());
        }
        assert!(
            requests
                .iter()
                .all(|request| !request.url().path().contains("kqlDatabases"))
        );
    }
}

struct Approval {
    allow: bool,
    calls: usize,
    name: &'static str,
    approved: Vec<ApprovedWire>,
}

struct ApprovedWire {
    body: Vec<u8>,
    url: String,
}
impl Approval {
    fn yes() -> Self {
        Self {
            allow: true,
            calls: 0,
            name: "Exact",
            approved: Vec::new(),
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
        assert_eq!(request.plan().target().item_type(), "Eventhouse");
        assert_eq!(request.plan().target().name(), self.name);
        assert_eq!(request.context().selection().account(), "synthetic-owner");
        assert_eq!(request.context().selection().tenant(), WS);
        assert_eq!(request.context().selection().audience(), Audience::Fabric);
        assert_eq!(
            request.context().endpoint(),
            &EndpointBinding::public(ServiceOrigin::Fabric)
        );
        if let CreateAction::Create { input } = request.plan().action() {
            assert!(request.remote_write());
            let operation = request
                .operation()
                .expect("approval must inspect the prepared request");
            let url = request.url().expect("approval must inspect the bound URL");
            assert_eq!(operation.method(), ResourceMethod::Post);
            assert_eq!(operation.path(), format!("/v1/workspaces/{WS}/eventhouses"));
            assert_eq!(
                url.as_str(),
                format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/eventhouses")
            );
            assert_eq!(
                operation.completion(),
                if request.command().wait {
                    CompletionPolicy::WaitForResult
                } else {
                    CompletionPolicy::ReturnAccepted
                }
            );
            let question = request.approval_question();
            assert!(!question.contains(['\r', '\n', '\u{1b}']));
            assert!(question.contains("method=POST"));
            assert!(question.contains("Eventhouse create policy"));
            assert!(question.contains("synthetic-owner") && question.contains(WS));
            assert!(question.contains("audience=Fabric"));
            assert!(question.contains(url.as_str()));
            let effects = request.anticipated_effects();
            assert_eq!(effects.len(), 2);
            assert_eq!(effects[0].kind(), CreateEffectKind::ChildResourceCreation);
            assert_eq!(effects[0].resource_type(), Some("KQLDatabase"));
            assert!(
                effects[0]
                    .description()
                    .contains("may create a same-name default KQL database")
            );
            assert!(
                effects[0]
                    .contract()
                    .contains("165f66efc3c8da81a156627a63eb0b8e0cb0c7ef")
            );
            assert_eq!(effects[1].kind(), CreateEffectKind::MeteredConsumption);
            assert_eq!(effects[1].resource_type(), Some("Eventhouse"));
            assert!(
                effects[1]
                    .description()
                    .contains("no query, run, start or minimum-consumption")
            );
            assert!(
                effects[1]
                    .contract()
                    .contains("b659751abeffc3a236c6a5be0c9a1fc1233a80aa")
            );
            assert!(question.contains("Anticipated effects are not observed outcomes."));
            assert!(question.contains("KQLDatabase") && question.contains("metered UpTime"));
            if let Some(raw) = request.raw_input() {
                assert_eq!(
                    input.supplied(),
                    Some(&serde_json::from_slice::<Value>(raw).unwrap())
                );
                assert_eq!(operation.body(), raw);
            } else {
                assert_eq!(
                    operation.body(),
                    serde_json::to_vec(&json!({"displayName":self.name})).unwrap()
                );
            }
            assert!(
                question.contains(
                    &std::str::from_utf8(operation.body())
                        .unwrap()
                        .escape_debug()
                        .to_string()
                )
            );
            self.approved.push(ApprovedWire {
                body: operation.body().to_vec(),
                url: url.to_string(),
            });
        } else {
            assert!(matches!(request.plan().action(), CreateAction::Skip { .. }));
            assert!(!request.remote_write());
            assert!(request.operation().is_none());
            assert!(request.url().is_none());
            assert!(request.anticipated_effects().is_empty());
            assert!(
                request
                    .approval_question()
                    .contains("no native request (unchanged skip)")
            );
        }
        Box::pin(std::future::ready(if self.allow {
            Ok(())
        } else {
            Err(CreateApprovalError::Denied)
        }))
    }
}

fn assert_approved_wire(harness: &Harness, approval: &Approval) {
    let requests = harness.wire.requests.lock().unwrap();
    let posts: Vec<_> = requests
        .iter()
        .filter(|request| request.method() == Method::Post)
        .collect();
    assert_eq!(posts.len(), approval.approved.len());
    for (post, approved) in posts.into_iter().zip(&approval.approved) {
        assert_eq!(post.url().as_str(), approved.url.as_str());
        assert_eq!(Bytes::from(post.body()).as_ref(), approved.body.as_slice());
    }
}

fn assert_anticipated_effects(receipt: &Value) {
    let effects = receipt["meta"]["anticipatedEffects"].as_array().unwrap();
    assert_eq!(effects.len(), 2);
    assert_eq!(effects[0]["kind"], "childResourceCreation");
    assert_eq!(effects[0]["resourceType"], "KQLDatabase");
    assert!(
        effects[0]["contract"]
            .as_str()
            .unwrap()
            .contains("165f66ef")
    );
    assert_eq!(effects[1]["kind"], "meteredConsumption");
    assert!(
        effects[1]["contract"]
            .as_str()
            .unwrap()
            .contains("b659751a")
    );
    for effect in effects {
        assert!(effect.get("id").is_none());
        assert!(effect.get("completed").is_none());
        assert!(effect.get("observed").is_none());
    }
    assert_eq!(receipt["meta"]["remoteWrite"], true);
}

fn response(status: u16, body: Value) -> AsyncRawResponse {
    AsyncRawResponse::from_bytes(
        StatusCode::from(status),
        Headers::new(),
        serde_json::to_vec(&body).unwrap(),
    )
}
fn listing(items: Vec<Value>) -> AsyncRawResponse {
    response(200, json!({"value":items}))
}
fn resource() -> Value {
    json!({"id":ITEM,"workspaceId":WS,"displayName":"Exact","type":"Eventhouse","properties":{"databaseIds":[OP]}})
}
fn accepted(body: Option<Value>) -> AsyncRawResponse {
    AsyncRawResponse::from_bytes(
        StatusCode::Accepted,
        operation_headers(),
        body.map(|body| serde_json::to_vec(&body).unwrap())
            .unwrap_or_default(),
    )
}
fn operation_headers() -> Headers {
    let mut headers = Headers::new();
    headers.insert("x-ms-operation-id", OP);
    headers.insert(
        "location",
        format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
    );
    headers.insert("retry-after", "0");
    headers
}
fn prepared(
    cwd: &Path,
    extra: &[&str],
    input: Option<&[u8]>,
) -> Result<PreparedCreateCommand, CreateCommandError> {
    let command = v2_create::parse_args(
        ["create", TARGET]
            .into_iter()
            .chain(input.map(|_| ["--input", "-"]).into_iter().flatten())
            .chain(extra.iter().copied()),
    )
    .unwrap();
    PreparedCreateCommand::preflight(
        command,
        &EventhouseCreate,
        cwd,
        &mut input.unwrap_or_default(),
        false,
    )
}
fn delivered(delivery: CreateDelivery) -> Value {
    let mut output = Vec::new();
    delivery.deliver(&mut output).unwrap();
    serde_json::from_slice(&output).unwrap()
}
struct ForbiddenRead;
impl Read for ForbiddenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("unsupported request read input")
    }
}

#[test]
fn metadata_schema_and_explicit_parent_are_strict_before_any_effect() {
    let dir = tempfile::tempdir().unwrap();
    for input in [
        b"".as_slice(),
        b"{}",
        b"null",
        b"[]",
        b"{",
        br#"{"displayName":"other"}"#,
        br#"{"displayName":"exact"}"#,
        br#"{"displayName":"Exact","displayName":"Exact"}"#,
        br#"{"displayName":"Exact","\u0064isplayName":"Exact"}"#,
        br#"{"displayName":"Exact","description":null}"#,
        br#"{"displayName":"Exact","description":1}"#,
        br#"{"displayName":"Exact","type":"KQLDatabase"}"#,
        br#"{"displayName":"Exact","type":"Eventhouse"}"#,
        br#"{"displayName":"Exact"} {}"#,
        b"\xff",
    ] {
        assert!(
            prepared(dir.path(), &["--if-exists", "skip"], Some(input)).is_err(),
            "{input:?}"
        );
    }
    for field in [
        "definition",
        "creationPayload",
        "folderId",
        "sensitivityLabelSettings",
        "dependency",
        "parentEventhouseItemId",
        "connection",
        "connections",
        "run",
        "compute",
        "minimumConsumptionUnits",
        "defaultDatabase",
    ] {
        let input = serde_json::to_vec(&json!({"displayName":"Exact",(field):{}})).unwrap();
        assert!(prepared(dir.path(), &[], Some(&input)).is_err(), "{field}");
    }
    let boundary =
        serde_json::to_vec(&json!({"displayName":"Exact","description":"x".repeat(256)})).unwrap();
    assert!(prepared(dir.path(), &[], Some(&boundary)).is_ok());
    let too_long =
        serde_json::to_vec(&json!({"displayName":"Exact","description":"x".repeat(257)})).unwrap();
    assert!(prepared(dir.path(), &[], Some(&too_long)).is_err());
    assert!(prepared(dir.path(), &[], Some(&vec![b' '; MAX_BYTES + 1])).is_err());
    for path in [
        "/Named.Workspace/Exact.Eventhouse",
        "/11111111-1111-1111-1111-111111111111.Workspace",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.KQLDatabase",
        "/11111111-1111-1111-1111-111111111111.Workspace/Folder.Folder/Exact.Eventhouse",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Eventhouse/src/definition",
    ] {
        let command = v2_create::parse_args(["create", path, "--input", "-"]).unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &EventhouseCreate,
                dir.path(),
                &mut ForbiddenRead,
                false
            )
            .is_err()
        );
    }
    for name in ["has space", "bad@name", "caf\u{e9}"] {
        let command =
            v2_create::parse_args(["create", &format!("/{WS}.Workspace/{name}.Eventhouse")])
                .unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &EventhouseCreate,
                dir.path(),
                &mut ForbiddenRead,
                false
            )
            .is_err()
        );
    }
}

#[test]
fn unsupported_policies_read_neither_stdin_nor_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    for policy in [
        vec!["--force"],
        vec!["--if-exists", "overwrite"],
        vec!["--if-exists", "duplicate"],
    ] {
        for input in ["-", "nonexistent.json"] {
            let command = v2_create::parse_args(
                ["create", TARGET, "--input", input]
                    .into_iter()
                    .chain(policy.iter().copied()),
            )
            .unwrap();
            assert!(matches!(
                PreparedCreateCommand::preflight(
                    command,
                    &EventhouseCreate,
                    dir.path(),
                    &mut ForbiddenRead,
                    false
                ),
                Err(CreateCommandError::Policy(CreateError::UnsupportedPolicy))
            ));
        }
    }
}

#[test]
fn native_binding_rejects_substituted_raw_bytes_and_unrelated_schema() {
    let target = CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WS).unwrap(),
        "Exact",
        "Eventhouse",
    )
    .unwrap();
    let input = json!({"displayName":"Exact"});
    let plan = plan_create(
        ConflictPolicy::Error,
        target.clone(),
        Preflight::Leaf(LeafState::Missing),
        &EventhouseCreate.capabilities(),
        Some(input),
        &EventhouseCreate,
    )
    .unwrap();
    for raw in [
        None,
        Some(br#"{"displayName":"Other"}"#.as_slice()),
        Some(br#"{"displayName":"Exact","displayName":"Exact"}"#.as_slice()),
    ] {
        assert!(EventhouseCreate.operation(&plan, raw, false).is_err());
    }
    assert!(
        EventhouseCreate
            .validate(&target, &SchemaRef::new("wrong/v1").unwrap(), None)
            .is_err()
    );
}

#[tokio::test]
async fn standalone_create_sends_exact_metadata_once_and_projects_only_data() {
    let dir = tempfile::tempdir().unwrap();
    let raw = b" {\r\n \"displayName\": \"Exact\", \"description\":\"native metadata\" }\n";
    let harness = Harness::new(vec![listing(vec![]), response(201, resource())]);
    let mut approval = Approval::yes();
    let result = harness
        .execute(
            prepared(dir.path(), &["--project", "displayName"], Some(raw)).unwrap(),
            &mut approval,
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.report().exit_code(),
        0,
        "{}",
        result.report().value()
    );
    let receipt = delivered(result);
    assert_eq!(receipt["data"], "Exact");
    assert_eq!(receipt["meta"]["status"], "completed");
    assert_eq!(receipt["meta"]["resourceId"], ITEM);
    assert_eq!(receipt["meta"]["type"], "Eventhouse");
    assert_eq!(receipt["meta"]["operationHandle"], Value::Null);
    assert_eq!(receipt["meta"]["remoteReplayed"], false);
    assert_eq!(receipt["meta"]["observedResource"]["id"], ITEM);
    assert_anticipated_effects(&receipt);
    assert_approved_wire(&harness, &approval);
    assert_eq!(receipt["meta"]["submission"]["httpStatus"], 201);
    assert!(receipt["meta"]["submission"]["resource"].is_null());
    harness.assert_posts(1);
    let requests = harness.wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].url().path(),
        format!("/v1/workspaces/{WS}/items")
    );
    assert_eq!(requests[0].url().query(), Some("recursive=false"));
    assert!(receipt["meta"]["folderId"].is_null());
    assert_eq!(receipt["meta"]["folderIdObserved"], false);
    assert_eq!(receipt["meta"]["submission"]["folderIdObserved"], false);
    assert_eq!(Bytes::from(requests[1].body()).as_ref(), raw);
    assert_eq!(approval.calls, 1);
    assert_eq!(harness.credential.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn omitted_input_uses_literal_name_and_never_resolves_it_as_an_item_id() {
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(vec![listing(vec![]), response(201, resource())]);
    let mut approval = Approval::yes();
    let result = harness
        .execute(
            prepared(dir.path(), &[], None).unwrap(),
            &mut approval,
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.report().exit_code(), 0);
    assert_anticipated_effects(result.report().value());
    assert_approved_wire(&harness, &approval);
    {
        let requests = harness.wire.requests.lock().unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&Bytes::from(requests[1].body())).unwrap(),
            json!({"displayName":"Exact"})
        );
    }
    harness.assert_posts(1);
    let target = CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WS).unwrap(),
        ITEM,
        "Eventhouse",
    )
    .unwrap();
    assert!(
        EventhouseCreate
            .validate(&target, &EventhouseCreate.capabilities().schema, None)
            .is_ok()
    );
    let command =
        v2_create::parse_args(["create", &format!("/{WS}.Workspace/{ITEM}.Eventhouse")]).unwrap();
    let prepared = PreparedCreateCommand::preflight(
        command,
        &EventhouseCreate,
        dir.path(),
        &mut ForbiddenRead,
        false,
    )
    .unwrap();
    let harness = Harness::new(vec![
        listing(vec![]),
        response(
            201,
            json!({"id":OP,"workspaceId":WS,"displayName":ITEM,"type":"Eventhouse"}),
        ),
    ]);
    let mut approval = Approval {
        allow: true,
        calls: 0,
        name: ITEM,
        approved: Vec::new(),
    };
    let result = harness
        .execute(prepared, &mut approval, pending())
        .await
        .unwrap();
    assert_eq!(result.report().exit_code(), 0);
    let requests = harness.wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        serde_json::from_slice::<Value>(&Bytes::from(requests[1].body())).unwrap(),
        json!({"displayName":ITEM})
    );
    drop(requests);
    harness.assert_posts(1);
}

#[tokio::test]
async fn exact_skip_validates_explicit_input_and_never_posts() {
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(vec![listing(vec![resource()])]);
    assert!(
        prepared(
            dir.path(),
            &["--if-exists", "skip"],
            Some(br#"{"displayName":"Exact","definition":{}}"#)
        )
        .is_err()
    );
    assert_eq!(harness.credential.0.load(Ordering::SeqCst), 0);
    let result = harness
        .execute(
            prepared(
                dir.path(),
                &["--if-exists", "skip"],
                Some(br#"{"displayName":"Exact"}"#),
            )
            .unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    let receipt = delivered(result);
    assert_eq!(receipt["meta"]["status"], "unchanged");
    assert_eq!(receipt["meta"]["resourceId"], ITEM);
    assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], false);
    assert_eq!(receipt["meta"]["contentEqualityClaimed"], false);
    assert_eq!(receipt["meta"]["remoteWrite"], false);
    assert_eq!(receipt["meta"]["anticipatedEffects"], json!([]));
    assert!(receipt["meta"]["submission"].is_null());
    harness.assert_posts(0);
}

#[tokio::test]
async fn exact_eventhouse_skip_is_not_ambiguous_with_its_same_name_default_database() {
    let dir = tempfile::tempdir().unwrap();
    let database = json!({"id":OP,"workspaceId":WS,"displayName":"Exact","type":"KQLDatabase"});
    let harness = Harness::new(vec![listing(vec![resource(), database])]);
    let result = harness
        .execute(
            prepared(
                dir.path(),
                &["--if-exists", "skip"],
                Some(br#"{"displayName":"Exact"}"#),
            )
            .unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    let receipt = delivered(result);
    assert_eq!(receipt["meta"]["status"], "unchanged");
    assert_eq!(receipt["meta"]["resourceId"], ITEM);
    assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], false);
    assert_eq!(receipt["meta"]["remoteWrite"], false);
    assert_eq!(receipt["meta"]["anticipatedEffects"], json!([]));
    harness.assert_posts(0);
    assert_eq!(harness.wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn workspace_root_skip_never_substitutes_a_matching_nested_eventhouse() {
    let dir = tempfile::tempdir().unwrap();
    let nested = json!({
        "id":ITEM,"workspaceId":WS,"displayName":"Exact","type":"Eventhouse","folderId":OP
    });
    let harness = Harness::new(vec![listing(vec![nested])]);
    let mut approval = Approval::yes();
    assert!(
        harness
            .execute(
                prepared(dir.path(), &["--if-exists", "skip"], None).unwrap(),
                &mut approval,
                pending(),
            )
            .await
            .is_err()
    );
    assert_eq!(approval.calls, 0);
    harness.assert_posts(0);
}

#[tokio::test]
async fn root_and_nested_name_matches_select_only_the_exact_root_and_preserve_folder_observation() {
    let dir = tempfile::tempdir().unwrap();
    for explicit_null in [false, true] {
        let mut root = resource();
        if explicit_null {
            root["folderId"] = Value::Null;
        }
        let nested = json!({"id":OP,"workspaceId":WS,"displayName":"Exact","type":"Eventhouse","folderId":WS});
        let database = json!({"id":WS,"workspaceId":WS,"displayName":"Exact","type":"KQLDatabase"});
        let harness = Harness::new(vec![listing(vec![nested, database, root])]);
        let mut approval = Approval::yes();
        let result = harness
            .execute(
                prepared(
                    dir.path(),
                    &["--if-exists", "skip", "--project", "displayName"],
                    None,
                )
                .unwrap(),
                &mut approval,
                pending(),
            )
            .await
            .unwrap();
        let receipt = delivered(result);
        assert_eq!(receipt["meta"]["status"], "unchanged");
        assert_eq!(receipt["meta"]["resourceId"], ITEM);
        assert_eq!(receipt["meta"]["folderIdObserved"], explicit_null);
        assert!(receipt["meta"]["folderId"].is_null());
        assert_eq!(receipt["meta"]["anticipatedEffects"], json!([]));
        assert_eq!(receipt["meta"]["remoteWrite"], false);
        assert_eq!(approval.calls, 1);
        assert!(approval.approved.is_empty());
        harness.assert_posts(0);
    }
}

#[tokio::test]
async fn malformed_folder_evidence_cannot_authorize_skip_even_alongside_a_valid_root() {
    let dir = tempfile::tempdir().unwrap();
    for folder in [
        json!("not-a-guid"),
        json!(""),
        json!(7),
        json!({}),
        json!([]),
        json!(false),
    ] {
        let malformed = json!({"id":OP,"workspaceId":WS,"displayName":"Exact","type":"Eventhouse","folderId":folder});
        for items in [vec![malformed.clone()], vec![resource(), malformed]] {
            let harness = Harness::new(vec![listing(items)]);
            let mut approval = Approval::yes();
            assert!(
                harness
                    .execute(
                        prepared(dir.path(), &["--if-exists", "skip"], None).unwrap(),
                        &mut approval,
                        pending(),
                    )
                    .await
                    .is_err()
            );
            assert_eq!(approval.calls, 0);
            harness.assert_posts(0);
        }
    }
}

#[tokio::test]
async fn completed_creation_rejects_nonroot_or_malformed_folder_and_retains_observed_fields() {
    let dir = tempfile::tempdir().unwrap();
    for wait in [false, true] {
        for folder in [json!(OP), json!("invalid-folder"), json!(17), json!({})] {
            let mut item = resource();
            item["folderId"] = folder.clone();
            let replies = if wait {
                vec![
                    listing(vec![]),
                    accepted(Some(resource())),
                    response(200, json!({"status":"Succeeded"})),
                    response(200, item),
                ]
            } else {
                vec![listing(vec![]), response(201, item)]
            };
            let harness = Harness::new(replies);
            let options = if wait {
                vec!["--wait", "--project", "displayName"]
            } else {
                vec!["--project", "displayName"]
            };
            let result = harness
                .execute(
                    prepared(dir.path(), &options, None).unwrap(),
                    &mut Approval::yes(),
                    pending(),
                )
                .await
                .unwrap();
            assert_eq!(result.report().exit_code(), 1);
            let receipt = delivered(result);
            assert_eq!(receipt["meta"]["status"], "unknown");
            assert_eq!(receipt["meta"]["completed"], false);
            assert_eq!(receipt["meta"]["folderId"], folder);
            assert_eq!(receipt["meta"]["folderIdObserved"], true);
            assert_eq!(receipt["meta"]["resourceId"], ITEM);
            assert_eq!(receipt["meta"]["submission"]["folderIdObserved"], !wait);
            if wait {
                assert!(receipt["meta"]["submission"]["folderId"].is_null());
                assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
                assert_eq!(
                    receipt["meta"]["operationHandle"],
                    format!("/operations/{OP}")
                );
            } else {
                assert_eq!(receipt["meta"]["submission"]["folderId"], folder);
            }
            assert_anticipated_effects(&receipt);
            harness.assert_posts(1);
        }
    }
}

#[tokio::test]
async fn root_null_completion_and_initial_accepted_folder_remain_separate_from_final_projection() {
    let dir = tempfile::tempdir().unwrap();
    let mut root = resource();
    root["folderId"] = Value::Null;
    let harness = Harness::new(vec![listing(vec![]), response(201, root.clone())]);
    let result = harness
        .execute(
            prepared(dir.path(), &["--project", "displayName"], None).unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.report().exit_code(), 0);
    let receipt = delivered(result);
    assert_eq!(receipt["meta"]["folderIdObserved"], true);
    assert!(receipt["meta"]["folderId"].is_null());
    assert_eq!(receipt["meta"]["submission"]["folderIdObserved"], true);
    assert!(receipt["meta"]["submission"]["folderId"].is_null());
    assert_anticipated_effects(&receipt);
    harness.assert_posts(1);
    let mut initial = resource();
    initial["folderId"] = json!(OP);
    let harness = Harness::new(vec![
        listing(vec![]),
        accepted(Some(initial)),
        response(200, json!({"status":"Succeeded"})),
        response(200, root),
    ]);
    let result = harness
        .execute(
            prepared(dir.path(), &["--wait", "--project", "displayName"], None).unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.report().exit_code(), 0);
    let receipt = delivered(result);
    assert_eq!(receipt["data"], "Exact");
    assert_eq!(receipt["meta"]["status"], "completed");
    assert_eq!(receipt["meta"]["folderIdObserved"], true);
    assert!(receipt["meta"]["folderId"].is_null());
    assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(receipt["meta"]["submission"]["folderId"], OP);
    assert_eq!(receipt["meta"]["submission"]["folderIdObserved"], true);
    assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
    assert_anticipated_effects(&receipt);
    harness.assert_posts(1);
}

#[tokio::test]
async fn missing_skip_creates_but_collision_ambiguity_wrong_type_and_case_never_write() {
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(vec![listing(vec![]), response(201, resource())]);
    let result = harness
        .execute(
            prepared(dir.path(), &["--if-exists", "skip"], None).unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.report().value()["meta"]["policy"], "skip");
    assert_eq!(result.report().value()["meta"]["action"], "create");
    harness.assert_posts(1);
    for (items, policy) in [
        (vec![resource()], "error"),
        (vec![resource(), resource()], "skip"),
        (
            vec![json!({"id":ITEM,"displayName":"Exact","type":"KQLDatabase"})],
            "skip",
        ),
        (
            vec![json!({"id":ITEM,"displayName":"exact","type":"Eventhouse"})],
            "skip",
        ),
        (
            vec![json!({"id":ITEM,"workspaceId":ITEM,"displayName":"Exact","type":"Eventhouse"})],
            "skip",
        ),
    ] {
        let harness = Harness::new(vec![listing(items)]);
        let mut approval = Approval::yes();
        assert!(
            harness
                .execute(
                    prepared(dir.path(), &["--if-exists", policy], None).unwrap(),
                    &mut approval,
                    pending()
                )
                .await
                .is_err()
        );
        assert_eq!(approval.calls, 0);
        harness.assert_posts(0);
    }
}

#[tokio::test]
async fn immediate_denial_and_discovery_failure_cannot_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(vec![listing(vec![])]);
    let mut denied = Approval {
        allow: false,
        calls: 0,
        name: "Exact",
        approved: Vec::new(),
    };
    assert!(matches!(
        harness
            .execute(
                prepared(dir.path(), &[], None).unwrap(),
                &mut denied,
                pending()
            )
            .await,
        Err(CreateCommandError::Approval(CreateApprovalError::Denied))
    ));
    assert_eq!(denied.calls, 1);
    assert_eq!(denied.approved.len(), 1);
    harness.assert_posts(0);
    let harness = Harness::new(vec![response(403, json!({"errorCode":"Forbidden"}))]);
    assert!(
        harness
            .execute(
                prepared(dir.path(), &[], None).unwrap(),
                &mut Approval::yes(),
                pending()
            )
            .await
            .is_err()
    );
    harness.assert_posts(0);
}

#[tokio::test]
async fn accepted_is_not_completed_and_wait_reuses_shared_operation_result() {
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(vec![listing(vec![]), accepted(None)]);
    let receipt = delivered(
        harness
            .execute(
                prepared(dir.path(), &[], None).unwrap(),
                &mut Approval::yes(),
                pending(),
            )
            .await
            .unwrap(),
    );
    assert_eq!(receipt["meta"]["status"], "accepted");
    assert_eq!(receipt["meta"]["completed"], false);
    assert_eq!(receipt["meta"]["resourceId"], Value::Null);
    assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
    assert!(receipt["meta"]["submission"]["resource"].is_null());
    assert_anticipated_effects(&receipt);
    assert_eq!(
        receipt["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(harness.wire.requests.lock().unwrap().len(), 2);
    harness.assert_posts(1);
    let harness = Harness::new(vec![
        listing(vec![]),
        accepted(None),
        response(200, json!({"status":"Succeeded"})),
        response(200, resource()),
    ]);
    let result = harness
        .execute(
            prepared(dir.path(), &["--wait"], None).unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.report().exit_code(),
        0,
        "{}",
        result.report().value()
    );
    let receipt = delivered(result);
    assert_eq!(receipt["meta"]["status"], "completed");
    assert_eq!(
        receipt["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(receipt["meta"]["resourceId"], ITEM);
    assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
    assert!(receipt["meta"]["submission"]["resource"].is_null());
    assert_eq!(receipt["meta"]["observation"]["httpStatus"], 200);
    assert_anticipated_effects(&receipt);
    let requests = harness.wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2].url().path(), format!("/v1/operations/{OP}"));
    assert_eq!(
        requests[3].url().path(),
        format!("/v1/operations/{OP}/result")
    );
    drop(requests);
    harness.assert_posts(1);
}

#[tokio::test]
async fn service_conflict_and_failed_poll_preserve_evidence_without_replay() {
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(vec![
        listing(vec![]),
        response(409, json!({"errorCode":"ItemDisplayNameAlreadyInUse"})),
    ]);
    let result = harness
        .execute(
            prepared(dir.path(), &[], None).unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.report().exit_code(), 1);
    assert_eq!(
        result.report().value()["meta"]["serviceCode"],
        "ItemDisplayNameAlreadyInUse"
    );
    assert!(matches!(
        &result.report().error().unwrap().kind,
        OperationErrorKind::Http { status: 409, .. }
    ));
    assert_eq!(result.report().value()["meta"]["status"], "failed");
    assert_eq!(result.report().value()["error"]["code"], "ServiceRejected");
    harness.assert_posts(1);
    let harness = Harness::new(vec![
        listing(vec![]),
        accepted(Some(resource())),
        response(
            200,
            json!({"status":"Failed","error":{"errorCode":"ProvisioningFailed","message":"synthetic"}}),
        ),
    ]);
    let result = harness
        .execute(
            prepared(dir.path(), &["--wait", "--project", "displayName"], None).unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.report().exit_code(), 1);
    assert!(matches!(
        &result.report().error().unwrap().kind,
        OperationErrorKind::OperationFailed { status, .. } if status == "Failed"
    ));
    let receipt = delivered(result);
    assert_eq!(receipt["meta"]["status"], "failed");
    assert_eq!(
        receipt["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(receipt["meta"]["resourceId"], ITEM);
    assert_eq!(receipt["meta"]["remoteCancelled"], false);
    assert_eq!(receipt["data"], "Exact");
    assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
    assert_eq!(
        receipt["meta"]["submission"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(receipt["meta"]["observation"]["httpStatus"], 200);
    assert_anticipated_effects(&receipt);
    harness.assert_posts(1);
}

#[tokio::test]
async fn unexpected_success_result_status_preserves_original_http_cause_but_stays_unknown() {
    let dir = tempfile::tempdir().unwrap();
    for status in [201, 202, 204] {
        let reply = if status == 204 {
            AsyncRawResponse::from_bytes(StatusCode::NoContent, Headers::new(), Vec::new())
        } else {
            response(status, resource())
        };
        let harness = Harness::new(vec![
            listing(vec![]),
            accepted(Some(resource())),
            response(200, json!({"status":"Succeeded"})),
            reply,
        ]);
        let mut approval = Approval::yes();
        let result = harness
            .execute(
                prepared(dir.path(), &["--wait", "--project", "displayName"], None).unwrap(),
                &mut approval,
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(result.report().exit_code(), 1);
        let original = result
            .report()
            .error()
            .expect("retain the shared SDK error");
        assert!(
            matches!(&original.kind,OperationErrorKind::Http { status:actual,.. } if *actual==status)
        );
        assert_eq!(original.operation_id.as_deref(), Some(OP));
        assert!(original.mutation_may_have_occurred);
        let receipt = delivered(result);
        assert_eq!(receipt["error"]["code"], "InvalidResponse");
        assert_eq!(receipt["meta"]["status"], "unknown");
        assert_eq!(receipt["meta"]["completed"], false);
        assert_eq!(receipt["meta"]["httpStatus"], status);
        assert_eq!(receipt["meta"]["observation"]["httpStatus"], status);
        assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
        assert_eq!(
            receipt["meta"]["submission"]["resource"]["type"],
            "Eventhouse"
        );
        assert_eq!(
            receipt["meta"]["submission"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(receipt["meta"]["resourceId"], ITEM);
        assert_eq!(
            receipt["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(receipt["meta"]["remoteReplayed"], false);
        assert_eq!(receipt["meta"]["remoteCancelled"], false);
        assert_eq!(receipt["data"], "Exact");
        assert_anticipated_effects(&receipt);
        assert_approved_wire(&harness, &approval);
        harness.assert_posts(1);
        let requests = harness.wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[2].url().path(), format!("/v1/operations/{OP}"));
        assert_eq!(
            requests[3].url().path(),
            format!("/v1/operations/{OP}/result")
        );
    }
}

#[tokio::test]
async fn genuine_result_http_rejection_stays_failed_and_retains_initial_acceptance() {
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(vec![
        listing(vec![]),
        accepted(Some(resource())),
        response(200, json!({"status":"Succeeded"})),
        response(
            400,
            json!({"errorCode":"InvalidRequest","message":"synthetic"}),
        ),
    ]);
    let result = harness
        .execute(
            prepared(dir.path(), &["--wait", "--project", "displayName"], None).unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.report().exit_code(), 1);
    assert!(matches!(
        &result.report().error().unwrap().kind,
        OperationErrorKind::Http { status: 400, .. }
    ));
    let receipt = delivered(result);
    assert_eq!(receipt["error"]["code"], "ServiceRejected");
    assert_eq!(receipt["meta"]["status"], "failed");
    assert_eq!(receipt["meta"]["completed"], false);
    assert_eq!(receipt["meta"]["httpStatus"], 400);
    assert_eq!(receipt["meta"]["serviceCode"], "InvalidRequest");
    assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
    assert_eq!(receipt["meta"]["resourceId"], ITEM);
    assert_eq!(
        receipt["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(receipt["meta"]["remoteReplayed"], false);
    assert_anticipated_effects(&receipt);
    harness.assert_posts(1);
    assert_eq!(harness.wire.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn malformed_final_lro_result_retains_previously_observed_acceptance_identity() {
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(vec![
        listing(vec![]),
        accepted(Some(resource())),
        response(200, json!({"status":"Succeeded"})),
        AsyncRawResponse::from_bytes(StatusCode::Ok, Headers::new(), b"{".to_vec()),
    ]);
    let result = harness
        .execute(
            prepared(dir.path(), &["--wait", "--project", "displayName"], None).unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.report().exit_code(), 1);
    let receipt = delivered(result);
    assert_eq!(receipt["meta"]["status"], "unknown");
    assert_eq!(receipt["meta"]["completed"], false);
    assert_eq!(
        receipt["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(receipt["meta"]["resourceId"], ITEM);
    assert_eq!(receipt["meta"]["observedResource"]["id"], ITEM);
    assert_eq!(receipt["meta"]["remoteReplayed"], false);
    assert_eq!(receipt["meta"]["httpStatus"], 200);
    assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
    assert_eq!(
        receipt["meta"]["submission"]["resource"]["type"],
        "Eventhouse"
    );
    assert_eq!(receipt["meta"]["observation"]["httpStatus"], 200);
    assert_anticipated_effects(&receipt);
    harness.assert_posts(1);
}

#[tokio::test]
async fn mismatched_final_resource_keeps_distinct_initial_acceptance_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(vec![
        listing(vec![]),
        accepted(Some(resource())),
        response(200, json!({"status":"Succeeded"})),
        response(
            200,
            json!({"id":OP,"workspaceId":WS,"displayName":"other","type":"KQLDatabase"}),
        ),
    ]);
    let result = harness
        .execute(
            prepared(dir.path(), &["--wait", "--project", "displayName"], None).unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.report().exit_code(), 1);
    let receipt = delivered(result);
    assert_eq!(receipt["meta"]["status"], "unknown");
    assert_eq!(receipt["meta"]["completed"], false);
    assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
    assert_eq!(
        receipt["meta"]["submission"]["resource"]["displayName"],
        "Exact"
    );
    assert_eq!(
        receipt["meta"]["submission"]["resource"]["type"],
        "Eventhouse"
    );
    assert_eq!(receipt["meta"]["observedResource"]["id"], OP);
    assert_eq!(receipt["meta"]["observedResource"]["type"], "KQLDatabase");
    assert_eq!(
        receipt["meta"]["operationHandle"],
        format!("/operations/{OP}")
    );
    assert_eq!(receipt["meta"]["observation"]["httpStatus"], 200);
    assert_anticipated_effects(&receipt);
    harness.assert_posts(1);
}

#[tokio::test]
async fn malformed_no_wait_202_retains_initial_headers_and_exact_body_without_poll_or_replay() {
    use base64::Engine;
    let dir = tempfile::tempdir().unwrap();
    let raw = b"{";
    let harness = Harness::new(vec![
        listing(vec![]),
        AsyncRawResponse::from_bytes(StatusCode::Accepted, operation_headers(), raw.to_vec()),
    ]);
    let result = harness
        .execute(
            prepared(dir.path(), &["--project", "displayName"], None).unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.report().exit_code(), 1);
    let receipt = delivered(result);
    assert_eq!(receipt["meta"]["status"], "unknown");
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
    assert!(receipt["meta"]["submission"]["resource"].is_null());
    assert!(receipt["meta"]["resourceId"].is_null());
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(
                receipt["meta"]["unparsedAcceptanceBodyBase64"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap(),
        raw
    );
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(
                receipt["meta"]["unparsedResponseBodyBase64"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap(),
        raw
    );
    assert_eq!(receipt["meta"]["remoteReplayed"], false);
    assert_anticipated_effects(&receipt);
    assert_eq!(harness.wire.requests.lock().unwrap().len(), 2);
    harness.assert_posts(1);
}

#[tokio::test]
async fn complete_discovery_pages_are_required_and_untrusted_continuations_never_authorize() {
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(vec![
        response(200, json!({"value":[],"continuationToken":"next"})),
        listing(vec![
            json!({"id":OP,"workspaceId":WS,"displayName":"Exact","type":"Eventhouse","folderId":WS}),
            resource(),
        ]),
    ]);
    let result = harness
        .execute(
            prepared(dir.path(), &["--if-exists", "skip"], None).unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.report().value()["meta"]["status"], "unchanged");
    {
        let requests = harness.wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].url().query(), Some("recursive=false"));
        let query: Vec<_> = requests[1].url().query_pairs().into_owned().collect();
        assert_eq!(
            query,
            vec![
                ("recursive".to_owned(), "false".to_owned()),
                ("continuationToken".to_owned(), "next".to_owned())
            ]
        );
        assert!(
            requests[1]
                .url()
                .query()
                .is_some_and(|query| query.contains("continuationToken=next"))
        );
    }
    harness.assert_posts(0);
    let harness = Harness::new(vec![response(
        200,
        json!({"value":[],"continuationToken":"next","continuationUri":"https://foreign.invalid/next"}),
    )]);
    assert!(
        harness
            .execute(
                prepared(dir.path(), &[], None).unwrap(),
                &mut Approval::yes(),
                pending()
            )
            .await
            .is_err()
    );
    harness.assert_posts(0);
}

#[tokio::test]
async fn malformed_or_mismatched_results_never_claim_completed_creation() {
    let dir = tempfile::tempdir().unwrap();
    for reply in [
        response(
            201,
            json!({"id":ITEM,"workspaceId":WS,"type":"KQLDatabase","displayName":"Exact"}),
        ),
        response(
            201,
            json!({"id":ITEM,"workspaceId":ITEM,"type":"Eventhouse","displayName":"Exact"}),
        ),
        response(
            201,
            json!({"id":ITEM,"workspaceId":WS,"type":"Eventhouse","displayName":"other"}),
        ),
        response(201, json!({})),
        response(200, resource()),
        response(204, Value::Null),
        AsyncRawResponse::from_bytes(StatusCode::Created, Headers::new(), b"{".to_vec()),
        AsyncRawResponse::from_bytes(StatusCode::Accepted, Headers::new(), Vec::new()),
    ] {
        let harness = Harness::new(vec![listing(vec![]), reply]);
        let result = harness
            .execute(
                prepared(dir.path(), &[], None).unwrap(),
                &mut Approval::yes(),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(
            result.report().exit_code(),
            1,
            "{}",
            result.report().value()
        );
        assert_eq!(result.report().value()["meta"]["status"], "unknown");
        assert_eq!(
            result.report().value()["meta"]["mutationMayHaveOccurred"],
            true
        );
        harness.assert_posts(1);
    }
}

#[tokio::test]
async fn header_acceptance_survives_stream_failure_overflow_and_local_interruption() {
    let dir = tempfile::tempdir().unwrap();
    let broken = AsyncRawResponse::new(
        StatusCode::Accepted,
        operation_headers(),
        Box::pin(stream::iter([Err(azure_core::Error::with_message(
            ErrorKind::Io,
            "synthetic body failure",
        ))])),
    );
    let overflow = AsyncRawResponse::from_bytes(
        StatusCode::Accepted,
        operation_headers(),
        vec![b' '; MAX_BYTES + 1],
    );
    let suspended = AsyncRawResponse::new(
        StatusCode::Accepted,
        operation_headers(),
        Box::pin(stream::pending()),
    );
    for reply in [broken, overflow, suspended] {
        let harness = Harness::new(vec![listing(vec![]), reply]);
        let interrupt = async { tokio::time::sleep(Duration::from_millis(100)).await };
        let result = harness
            .execute(
                prepared(dir.path(), &[], None).unwrap(),
                &mut Approval::yes(),
                interrupt,
            )
            .await
            .unwrap();
        assert_eq!(result.report().exit_code(), 1);
        let receipt = delivered(result);
        assert_eq!(receipt["meta"]["status"], "unknown");
        assert_eq!(receipt["meta"]["httpStatus"], 202);
        assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(
            receipt["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(receipt["meta"]["resourceId"], Value::Null);
        assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(receipt["meta"]["remoteReplayed"], false);
        assert_anticipated_effects(&receipt);
        harness.assert_posts(1);
    }
}

#[tokio::test]
async fn deadline_and_cancellation_during_poll_retain_known_resource_and_operation() {
    let dir = tempfile::tempdir().unwrap();
    for deadline in [false, true] {
        let suspended =
            AsyncRawResponse::new(StatusCode::Ok, Headers::new(), Box::pin(stream::pending()));
        let mut harness =
            Harness::new(vec![listing(vec![]), accepted(Some(resource())), suspended]);
        if deadline {
            harness.http = harness
                .http
                .with_timeout(Duration::from_millis(25))
                .unwrap();
        }
        let interrupted = async {
            if deadline {
                pending::<()>().await
            } else {
                tokio::time::sleep(Duration::from_millis(25)).await
            }
        };
        let result = harness
            .execute(
                prepared(dir.path(), &["--wait"], None).unwrap(),
                &mut Approval::yes(),
                interrupted,
            )
            .await
            .unwrap();
        assert_eq!(result.report().exit_code(), 1);
        let receipt = delivered(result);
        assert_eq!(
            receipt["error"]["code"],
            if deadline { "TimedOut" } else { "Cancelled" }
        );
        assert_eq!(receipt["meta"]["status"], "unknown");
        assert_eq!(receipt["meta"]["resourceId"], ITEM);
        assert_eq!(
            receipt["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(receipt["meta"]["remoteReplayed"], false);
        assert_eq!(receipt["meta"]["remoteCancelled"], false);
        assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
        assert_eq!(
            receipt["meta"]["submission"]["resource"]["type"],
            "Eventhouse"
        );
        assert_eq!(receipt["meta"]["observation"]["httpStatus"], 200);
        assert_anticipated_effects(&receipt);
        harness.assert_posts(1);
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
async fn cancelled_immediate_approval_never_reaches_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(vec![listing(vec![])]);
    let result = harness
        .execute(
            prepared(dir.path(), &[], None).unwrap(),
            &mut WaitingApproval,
            tokio::time::sleep(Duration::from_millis(25)),
        )
        .await;
    assert!(matches!(result, Err(CreateCommandError::Interrupted)));
    harness.assert_posts(0);
    assert_eq!(harness.wire.requests.lock().unwrap().len(), 1);
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
}

#[tokio::test]
async fn output_race_projection_and_closed_writer_retain_success_no_replay() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("receipt.json");
    std::fs::write(&file, b"keep").unwrap();
    assert!(prepared(dir.path(), &["--output", file.to_str().unwrap()], None).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");
    std::fs::remove_file(&file).unwrap();
    for extra in [
        vec!["--output", file.to_str().unwrap()],
        vec![],
        vec!["--project", "length(displayName.foo)"],
    ] {
        let harness = Harness::new(vec![listing(vec![]), response(201, resource())]);
        let result = harness
            .execute(
                prepared(dir.path(), &extra, None).unwrap(),
                &mut Approval::yes(),
                pending(),
            )
            .await
            .unwrap();
        if extra.first() == Some(&"--output") {
            std::fs::write(&file, b"racer").unwrap();
        }
        let error = if extra.is_empty() {
            result.deliver(&mut BrokenWriter)
        } else {
            result.deliver(&mut Vec::new())
        }
        .unwrap_err();
        let CreateCommandError::Delivery { report, .. } = error else {
            panic!("remote evidence discarded")
        };
        assert_eq!(report.value()["meta"]["status"], "completed");
        assert_eq!(report.value()["meta"]["resourceId"], ITEM);
        assert_anticipated_effects(report.value());
        harness.assert_posts(1);
    }
    assert_eq!(std::fs::read(file).unwrap(), b"racer");
}

#[tokio::test]
async fn exact_new_receipt_file_commits_observed_resource_without_stdout_or_replay() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("new-receipt.json");
    let harness = Harness::new(vec![listing(vec![]), response(201, resource())]);
    let result = harness
        .execute(
            prepared(
                dir.path(),
                &[
                    "--output",
                    output.to_str().unwrap(),
                    "--project",
                    "displayName",
                ],
                None,
            )
            .unwrap(),
            &mut Approval::yes(),
            pending(),
        )
        .await
        .unwrap();
    assert!(!output.exists());
    let mut stdout = Vec::new();
    assert!(result.deliver(&mut stdout).unwrap().is_some());
    assert!(stdout.is_empty());
    let receipt: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    assert_eq!(receipt["data"], "Exact");
    assert_eq!(receipt["meta"]["resourceId"], ITEM);
    assert_eq!(receipt["meta"]["status"], "completed");
    harness.assert_posts(1);
}
