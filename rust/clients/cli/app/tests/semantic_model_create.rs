// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual create parser -> shared host -> SDK planner -> scripted native HTTP.
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, Body, HttpClient, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_cli::{
    create_command::{
        CreateApproval, CreateApprovalError, CreateApprovalRequest, CreateBinding,
        CreateCommandError, CreateDelivery, CreateExecution, CreateTransportEvidence,
        PreparedCreateCommand, v2_create,
    },
    semantic_model_create::BlankSemanticModelBinding,
};
use fabric_cli_mutations::BlankSemanticModelCreate;
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
use futures_util::{future::pending, stream};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::Future,
    io::{self, Read, Write},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const RESOURCE: &str = "22222222-2222-2222-2222-222222222222";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";
const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact.SemanticModel";

#[derive(Debug, Default)]
struct Credential(Mutex<usize>);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        *self.0.lock().unwrap() += 1;
        Ok(AccessToken::new("offline-only", OffsetDateTime::UNIX_EPOCH))
    }
}

enum Reply {
    Complete(u16, Headers, Vec<u8>),
    BrokenAccepted,
    PendingAccepted,
    Pending,
}

#[derive(Debug, Default)]
struct Wire {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<(String, String, Value)>>,
    raw_bodies: Mutex<Vec<Vec<u8>>>,
    queries: Mutex<Vec<Vec<(String, String)>>>,
    dispatched: tokio::sync::Notify,
}

impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("synthetic response")
    }
}

fn accepted_headers() -> Headers {
    let mut headers = Headers::new();
    headers.insert("x-ms-operation-id", OPERATION);
    headers.insert(
        "Location",
        format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}"),
    );
    headers.insert("Retry-After", "1");
    headers
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.url().host_str(), Some("api.fabric.microsoft.com"));
        let query = request.url().query_pairs().into_owned().collect::<Vec<_>>();
        if request.url().path() == format!("/v1/workspaces/{WORKSPACE}/items") {
            assert_eq!(request.method().to_string(), "GET");
            assert_eq!(
                query.iter().filter(|(key, _)| key == "recursive").count(),
                1,
            );
            assert!(
                query
                    .iter()
                    .any(|(key, value)| key == "recursive" && value == "false")
            );
            assert!(
                query
                    .iter()
                    .all(|(key, _)| matches!(key.as_str(), "recursive" | "continuationToken"))
            );
        } else {
            assert!(
                query.is_empty(),
                "root discovery parameters must not reach typed create or operation routes"
            );
        }
        self.queries.lock().unwrap().push(query);
        let body = match request.body() {
            Body::Bytes(bytes) if bytes.is_empty() => Value::Null,
            Body::Bytes(bytes) => serde_json::from_slice(bytes).unwrap(),
            _ => panic!("unexpected streaming request"),
        };
        let Body::Bytes(raw) = request.body() else {
            panic!("unexpected streaming request")
        };
        self.raw_bodies.lock().unwrap().push(raw.to_vec());
        self.requests.lock().unwrap().push((
            request.method().to_string(),
            request.url().path().into(),
            body,
        ));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected call, secondary effect or mutation replay");
        self.dispatched.notify_one();
        match reply {
            Reply::Complete(status, headers, body) => Ok(AsyncRawResponse::from_bytes(
                StatusCode::from(status),
                headers,
                body,
            )),
            Reply::BrokenAccepted => Ok(AsyncRawResponse::new(
                StatusCode::Accepted,
                accepted_headers(),
                Box::pin(stream::once(async {
                    Err(azure_core::Error::with_message(
                        azure_core::error::ErrorKind::Io,
                        "synthetic partial body",
                    ))
                })),
            )),
            Reply::PendingAccepted => Ok(AsyncRawResponse::new(
                StatusCode::Accepted,
                accepted_headers(),
                Box::pin(stream::pending()),
            )),
            Reply::Pending => pending().await,
        }
    }
}

#[derive(Default)]
struct Timer(Mutex<Vec<Duration>>);

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        // HTTP timeout races stay pending; short native poll delays are recorded.
        if delay >= Duration::from_secs(30) {
            Box::pin(pending())
        } else {
            self.0.lock().unwrap().push(delay);
            Box::pin(async {})
        }
    }
}

#[derive(Default)]
struct Approval {
    calls: usize,
    deny: bool,
    pending: bool,
    body: Option<Vec<u8>>,
    question: Option<String>,
    expected_name: Option<String>,
}

impl CreateApproval for Approval {
    fn authorize<'a>(
        &'a mut self,
        request: CreateApprovalRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
        self.calls += 1;
        let expected_name = self.expected_name.as_deref().unwrap_or("Exact");
        assert_eq!(request.plan().target().name(), expected_name);
        assert_eq!(request.plan().target().parent_id().as_str(), WORKSPACE);
        assert_eq!(request.plan().target().item_type(), "SemanticModel");
        assert_eq!(
            request.context().selection().account(),
            "owner@example.test"
        );
        assert_eq!(request.context().selection().tenant(), WORKSPACE);
        assert_eq!(request.context().selection().audience(), Audience::Fabric);
        assert_eq!(
            request.context().endpoint(),
            &EndpointBinding::public(ServiceOrigin::Fabric)
        );
        assert_eq!(
            request.remote_write(),
            matches!(request.plan().action(), CreateAction::Create { .. })
        );
        self.question = Some(request.approval_question());
        assert!(request.anticipated_effects().is_empty());
        if let Some(operation) = request.operation() {
            assert_eq!(
                operation.method(),
                fabric_sdk_workspaces::mutations::ResourceMethod::Post
            );
            assert_eq!(
                operation.path(),
                format!("/v1/workspaces/{WORKSPACE}/semanticModels")
            );
            assert_eq!(
                request.url().unwrap().as_str(),
                format!(
                    "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/semanticModels"
                )
            );
            self.body = Some(operation.body().to_vec());
            let body: Value = serde_json::from_slice(operation.body()).unwrap();
            assert_eq!(body["displayName"], expected_name);
            assert_eq!(body["definition"]["parts"].as_array().unwrap().len(), 5);
        } else {
            assert!(request.url().is_none());
            self.body = None;
        }
        let denied = self.deny;
        let wait = self.pending;
        Box::pin(async move {
            if wait {
                pending().await
            } else if denied {
                Err(CreateApprovalError::Denied)
            } else {
                Ok(())
            }
        })
    }
}

struct Host {
    wire: Arc<Wire>,
    credential: Arc<Credential>,
    timer: Arc<Timer>,
    approval: Approval,
}

impl Host {
    fn new() -> Self {
        Self {
            wire: Arc::default(),
            credential: Arc::default(),
            timer: Arc::default(),
            approval: Approval::default(),
        }
    }

    fn reply(&self, status: u16, body: Value) {
        self.wire.replies.lock().unwrap().push_back(Reply::Complete(
            status,
            Headers::new(),
            serde_json::to_vec(&body).unwrap(),
        ));
    }

    fn accepted(&self, body: Value) {
        self.wire.replies.lock().unwrap().push_back(Reply::Complete(
            202,
            accepted_headers(),
            serde_json::to_vec(&body).unwrap(),
        ));
    }

    async fn execute(
        &mut self,
        args: &[&str],
        raw: &[u8],
        audience: Audience,
        interrupted: impl Future<Output = ()> + Send,
    ) -> Result<CreateDelivery, CreateCommandError> {
        let temp = tempfile::tempdir().unwrap();
        let command = v2_create::parse_args(args).unwrap();
        let prepared = PreparedCreateCommand::preflight(
            command,
            &BlankSemanticModelBinding,
            temp.path(),
            &mut &raw[..],
            false,
        )?;
        let client = Client::with_context(RequestContext::with_endpoint(
            self.credential.clone(),
            CredentialSelection::new("owner@example.test", WORKSPACE, audience).unwrap(),
            EndpointBinding::public(ServiceOrigin::Fabric),
        ));
        let executor = HttpExecutor::new(
            self.wire.clone(),
            self.timer.clone(),
            ExecutionOptions {
                timeout: Duration::from_secs(30),
                max_delay: Duration::from_secs(60),
            },
        )
        .unwrap();
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
                &BlankSemanticModelBinding,
                &client,
                &executor,
                CreateExecution {
                    executor: executor.clone().with_trace(trace.clone()),
                    observe: &observe,
                },
                &mut self.approval,
                interrupted,
            )
            .await
    }

    async fn run(
        &mut self,
        tail: &[&str],
        raw: &[u8],
    ) -> Result<CreateDelivery, CreateCommandError> {
        let mut args = vec!["create", TARGET];
        args.extend_from_slice(tail);
        self.execute(&args, raw, Audience::Fabric, pending()).await
    }

    fn requests(&self) -> Vec<(String, String, Value)> {
        self.wire.requests.lock().unwrap().clone()
    }
}

fn resource() -> Value {
    json!({"id":RESOURCE,"workspaceId":WORKSPACE,"displayName":"Exact","type":"SemanticModel"})
}

#[tokio::test]
async fn guid_shaped_leaf_is_a_literal_new_name_not_an_existing_resource_lookup() {
    let mut host = Host::new();
    host.approval.expected_name = Some(OPERATION.into());
    host.reply(200, json!({"value":[]}));
    host.reply(
        201,
        json!({
            "id":RESOURCE,"workspaceId":WORKSPACE,"displayName":OPERATION,"type":"SemanticModel"
        }),
    );
    let target = format!("/{WORKSPACE}.Workspace/{OPERATION}.SemanticModel");
    let delivery = host
        .execute(&["create", &target], b"", Audience::Fabric, pending())
        .await
        .unwrap();
    assert_eq!(delivery.report().exit_code(), 0);
    let requests = host.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].1,
        format!("/v1/workspaces/{WORKSPACE}/semanticModels")
    );
    assert_eq!(requests[1].2["displayName"], OPERATION);
    let platform: Value = serde_json::from_slice(
        &STANDARD
            .decode(
                requests[1].2["definition"]["parts"][0]["payload"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(platform["metadata"]["displayName"], "Blank");
    assert_eq!(
        platform["config"]["logicalId"],
        "00000000-0000-0000-0000-000000000000"
    );
}

#[tokio::test]
async fn parser_to_native_post_preserves_all_five_pinned_parts_and_placeholders() {
    for (tail, raw, description) in [
        (vec![], b"".as_slice(), None),
        (
            vec!["--input", "-"],
            br#"{"displayName":"Exact","description":"Example"}"#.as_slice(),
            Some("Example"),
        ),
    ] {
        let mut host = Host::new();
        host.reply(200, json!({"value":[]}));
        host.reply(201, resource());
        let delivery = host.run(&tail, raw).await.unwrap();
        assert_eq!(delivery.report().exit_code(), 0);
        assert_eq!(delivery.report().value()["meta"]["completed"], true);
        assert_eq!(host.approval.calls, 1);
        let requests = host.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].0, "GET");
        assert_eq!(requests[0].1, format!("/v1/workspaces/{WORKSPACE}/items"));
        assert_eq!(requests[1].0, "POST");
        assert_eq!(
            requests[1].1,
            format!("/v1/workspaces/{WORKSPACE}/semanticModels")
        );
        let expected = BlankSemanticModelCreate::new("Exact", description)
            .unwrap()
            .into_payload();
        assert_eq!(requests[1].2, expected);
        assert_eq!(
            host.approval.body.as_deref(),
            Some(host.wire.raw_bodies.lock().unwrap()[1].as_slice())
        );
        let question = host.approval.question.as_ref().unwrap();
        let approved = std::str::from_utf8(host.approval.body.as_ref().unwrap()).unwrap();
        assert!(question.contains(&format!("body=\"{}\"", approved.escape_debug())));
        assert!(question.contains("SemanticModel create policy error"));
        assert!(question.contains("audience=Fabric"));
        assert!(question.contains("anticipatedEffects=[]"));
        let parts = requests[1].2["definition"]["parts"].as_array().unwrap();
        let files: &[(&str, &[u8])] = &[
            (
                ".platform",
                include_bytes!("../../commands/mutations/templates/Blank.SemanticModel/.platform"),
            ),
            (
                "definition.pbism",
                include_bytes!(
                    "../../commands/mutations/templates/Blank.SemanticModel/definition.pbism"
                ),
            ),
            (
                "definition/database.tmdl",
                include_bytes!(
                    "../../commands/mutations/templates/Blank.SemanticModel/definition/database.tmdl"
                ),
            ),
            (
                "definition/model.tmdl",
                include_bytes!(
                    "../../commands/mutations/templates/Blank.SemanticModel/definition/model.tmdl"
                ),
            ),
            (
                "definition/tables/Table.tmdl",
                include_bytes!(
                    "../../commands/mutations/templates/Blank.SemanticModel/definition/tables/Table.tmdl"
                ),
            ),
        ];
        assert_eq!(parts.len(), 5);
        for (part, (path, bytes)) in parts.iter().zip(files) {
            assert_eq!(part["path"], *path);
            assert_eq!(part["payloadType"], "InlineBase64");
            assert_eq!(
                STANDARD.decode(part["payload"].as_str().unwrap()).unwrap(),
                *bytes
            );
        }
        let platform: Value = serde_json::from_slice(
            &STANDARD
                .decode(parts[0]["payload"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(platform["metadata"]["displayName"], "Blank");
        assert_eq!(
            platform["config"]["logicalId"],
            "00000000-0000-0000-0000-000000000000"
        );
        let mut bytes = Vec::new();
        delivery.deliver(&mut bytes).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["meta"]["resourceId"],
            RESOURCE
        );
    }
}

#[tokio::test]
async fn explicit_input_is_strict_even_under_skip_and_rejects_all_model_authoring() {
    let inputs = [
        b"".as_slice(),
        b"null",
        b"[]",
        br#"["Exact"]"#,
        br#"["Exact","Description"]"#,
        b"{}",
        b"false",
        b"{",
        br#"{"displayName":"Wrong"}"#,
        br#"{"displayName":"Exact","description":null}"#,
        br#"{"displayName":"Exact","description":42}"#,
        br#"{"displayName":"Exact","displayName":"Exact"}"#,
        br#"{"displayName":"Exact","\u0064isplayName":"Exact"}"#,
        br#"{"displayName":"Exact","definition":{"parts":[]}}"#,
        br#"{"displayName":"Exact","creationPayload":{}}"#,
        br#"{"displayName":"Exact","connectionId":"any"}"#,
        br#"{"displayName":"Exact","semanticModelId":"any"}"#,
        br#"{"displayName":"Exact","source":"file"}"#,
        br#"{"displayName":"Exact","type":"SemanticModel"}"#,
        br#"{"displayName":"Exact","folderId":null}"#,
        br#"{"displayName":"Exact","unknown":true}"#,
    ];
    for input in inputs {
        let mut host = Host::new();
        assert!(
            host.run(&["--input", "-", "--if-exists", "skip"], input)
                .await
                .is_err()
        );
        assert!(host.requests().is_empty());
        assert_eq!(*host.credential.0.lock().unwrap(), 0);
        assert_eq!(host.approval.calls, 0);
    }
}

struct ForbiddenRead;
impl Read for ForbiddenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("invalid target/policy must reject before input I/O")
    }
}

#[test]
fn owner_guid_name_schema_and_unsupported_policy_are_offline_zero_effect_checks() {
    let temp = tempfile::tempdir().unwrap();
    for target in [
        "/Name.Workspace/Exact.SemanticModel",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Report",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.semanticmodel",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.SemanticModel/src/definition",
    ] {
        let command = v2_create::parse_args(["create", target, "--input", "-"]).unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &BlankSemanticModelBinding,
                temp.path(),
                &mut ForbiddenRead,
                false
            )
            .is_err()
        );
    }
    for policy in ["overwrite", "duplicate"] {
        let command =
            v2_create::parse_args(["create", TARGET, "--input", "-", "--if-exists", policy])
                .unwrap();
        assert!(matches!(
            PreparedCreateCommand::preflight(
                command,
                &BlankSemanticModelBinding,
                temp.path(),
                &mut ForbiddenRead,
                false
            ),
            Err(CreateCommandError::Policy(CreateError::UnsupportedPolicy))
        ));
    }
    let target = CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WORKSPACE).unwrap(),
        "Exact",
        "SemanticModel",
    )
    .unwrap();
    assert!(
        BlankSemanticModelBinding
            .validate(&target, &SchemaRef::new("other/v1").unwrap(), None)
            .is_err()
    );
    for array in [json!(["Exact"]), json!(["Exact", "Description"])] {
        assert!(
            BlankSemanticModelBinding
                .validate(
                    &target,
                    &BlankSemanticModelBinding.capabilities().schema,
                    Some(&array),
                )
                .is_err()
        );
    }
    for name in [" Exact", "Exact ", "Model*", &"a".repeat(257)] {
        let target = CreateTarget::new(
            TargetScope::NamedLeaf,
            NativeId::new(WORKSPACE).unwrap(),
            name,
            "SemanticModel",
        )
        .unwrap();
        assert!(
            BlankSemanticModelBinding
                .validate(
                    &target,
                    &BlankSemanticModelBinding.capabilities().schema,
                    None
                )
                .is_err()
        );
    }
}

#[test]
fn operation_cannot_substitute_raw_input_after_the_sole_sdk_plan() {
    let binding = BlankSemanticModelBinding;
    let target = CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WORKSPACE).unwrap(),
        "Exact",
        "SemanticModel",
    )
    .unwrap();
    let input = json!({"displayName":"Exact"});
    let plan = plan_create(
        ConflictPolicy::Error,
        target,
        Preflight::Leaf(LeafState::Missing),
        &binding.capabilities(),
        Some(input),
        &binding,
    )
    .unwrap();
    for raw in [
        None,
        Some(br#"{"displayName":"Other"}"#.as_slice()),
        Some(br#"{"displayName":"Exact","displayName":"Exact"}"#.as_slice()),
        Some(br#"{"displayName":"Exact","definition":{}}"#.as_slice()),
    ] {
        assert!(binding.operation(&plan, raw, false).is_err());
    }
    assert!(
        binding
            .operation(&plan, Some(br#"{ "displayName": "Exact" }"#), false)
            .is_ok()
    );
}

#[tokio::test]
async fn wrong_audience_stops_before_discovery_or_credentials() {
    let mut host = Host::new();
    assert!(
        host.execute(&["create", TARGET], b"", Audience::OneLake, pending())
            .await
            .is_err()
    );
    assert!(host.requests().is_empty());
    assert_eq!(*host.credential.0.lock().unwrap(), 0);
}

#[tokio::test]
async fn exact_skip_is_unchanged_but_ambiguity_case_collision_and_wrong_kind_fail() {
    let mut host = Host::new();
    host.reply(200, json!({"value":[resource()]}));
    let skipped = host
        .run(
            &["--if-exists", "skip", "--input", "-"],
            br#"{"displayName":"Exact"}"#,
        )
        .await
        .unwrap();
    assert_eq!(skipped.report().value()["meta"]["action"], "skip");
    assert_eq!(skipped.report().value()["meta"]["resourceId"], RESOURCE);
    assert_eq!(
        skipped.report().value()["meta"]["contentEqualityClaimed"],
        false
    );
    assert_eq!(host.requests().len(), 1);
    assert!(host.approval.body.is_none());
    assert!(
        host.approval
            .question
            .as_ref()
            .unwrap()
            .contains("no native request (unchanged skip)")
    );

    let mut host = Host::new();
    host.reply(
        200,
        json!({"value":[resource(), {
            "id":OPERATION,"displayName":"Exact","type":"Report","workspaceId":WORKSPACE
        }]}),
    );
    let skipped = host.run(&["--if-exists", "skip"], b"").await.unwrap();
    assert_eq!(skipped.report().value()["meta"]["resourceId"], RESOURCE);
    assert_eq!(host.requests().len(), 1);
    for rows in [
        json!([resource(), resource()]),
        json!([{"id":RESOURCE,"displayName":"exact","type":"SemanticModel"}]),
        json!([{"id":RESOURCE,"displayName":"Exact","type":"Report"}]),
        json!([{"id":RESOURCE,"displayName":"Exact","type":"SemanticModel","workspaceId":RESOURCE}]),
    ] {
        let mut host = Host::new();
        host.reply(200, json!({"value":rows}));
        assert!(host.run(&["--if-exists", "skip"], b"").await.is_err());
        assert_eq!(host.requests().len(), 1);
        assert_eq!(host.approval.calls, 0);
    }
}

#[tokio::test]
async fn root_skip_excludes_nested_matches_and_retains_observed_null_separately_from_omission() {
    for explicit_null in [false, true] {
        let mut root = resource();
        if explicit_null {
            root["folderId"] = Value::Null;
        }
        let mut nested = resource();
        nested["id"] = json!(OPERATION);
        nested["folderId"] = json!(RESOURCE);
        let mut host = Host::new();
        host.reply(200, json!({"value":[nested, root]}));
        let skipped = host.run(&["--if-exists", "skip"], b"").await.unwrap();
        let meta = &skipped.report().value()["meta"];
        assert_eq!(meta["status"], "unchanged");
        assert_eq!(meta["resourceId"], RESOURCE);
        assert_eq!(meta["folderId"], Value::Null);
        assert_eq!(meta["folderIdObserved"], explicit_null);
        assert_eq!(meta["remoteWrite"], false);
        assert_eq!(meta["anticipatedEffects"], json!([]));
        assert_eq!(host.requests().len(), 1);
        assert_eq!(host.approval.calls, 1);
        assert!(host.approval.body.is_none());
    }

    for folder in [
        json!(RESOURCE),
        json!("not-a-guid"),
        json!(42),
        json!({}),
        json!([]),
        json!(""),
    ] {
        for include_root in [false, true] {
            if include_root && folder == json!(RESOURCE) {
                continue;
            }
            let mut item = resource();
            item["folderId"] = folder.clone();
            let mut rows = vec![item];
            if include_root {
                rows.push(resource());
            }
            let mut host = Host::new();
            host.reply(200, json!({"value":rows}));
            assert!(host.run(&["--if-exists", "skip"], b"").await.is_err());
            assert_eq!(host.requests().len(), 1);
            assert_eq!(host.approval.calls, 0);
        }
    }
}

#[tokio::test]
async fn root_discovery_keeps_recursion_and_encoded_continuation_across_every_page() {
    let token = "next +/=";
    let mut host = Host::new();
    host.reply(200, json!({
        "value":[], "continuationToken":token,
        "continuationUri":format!("https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items")
    }));
    host.reply(200, json!({"value":[resource()]}));
    let skipped = host.run(&["--if-exists", "skip"], b"").await.unwrap();
    assert_eq!(skipped.report().value()["meta"]["resourceId"], RESOURCE);
    let queries = host.wire.queries.lock().unwrap();
    assert_eq!(queries[0], vec![("recursive".into(), "false".into())]);
    assert_eq!(
        queries[1],
        vec![
            ("recursive".into(), "false".into()),
            ("continuationToken".into(), token.into()),
        ]
    );
    assert_eq!(host.requests().len(), 2);
    drop(queries);

    let mut host = Host::new();
    host.reply(
        200,
        json!({"value":[resource()],"continuationToken":"next"}),
    );
    host.reply(200, json!({"value":[resource()]}));
    assert!(matches!(
        host.run(&["--if-exists", "skip"], b"").await,
        Err(CreateCommandError::Policy(CreateError::AmbiguousLeaf))
    ));
    assert_eq!(host.requests().len(), 2);
    assert_eq!(host.approval.calls, 0);

    let mut host = Host::new();
    host.reply(200, json!({"value":[],"continuationToken":"next"}));
    host.reply(403, json!({"errorCode":"Denied"}));
    assert!(matches!(
        host.run(&[], b"").await,
        Err(CreateCommandError::Discovery)
    ));
    assert_eq!(host.requests().len(), 2);
    assert_eq!(host.approval.calls, 0);

    let mut host = Host::new();
    for page in 0..10 {
        host.reply(
            200,
            json!({"value":[],"continuationToken":format!("page-{page}")}),
        );
    }
    assert!(matches!(
        host.run(&[], b"").await,
        Err(CreateCommandError::Discovery)
    ));
    assert_eq!(host.requests().len(), 10);
    assert_eq!(host.approval.calls, 0);
}

#[tokio::test]
async fn completed_native_create_revalidates_root_folder_and_preserves_returned_evidence() {
    for wait in [false, true] {
        for folder in [Value::Null, json!(RESOURCE), json!("bad-folder"), json!(42)] {
            let mut created = resource();
            created["folderId"] = folder.clone();
            let mut host = Host::new();
            host.reply(200, json!({"value":[]}));
            if wait {
                let mut initial = resource();
                initial["folderId"] = Value::Null;
                host.accepted(initial);
                host.reply(200, json!({"status":"Succeeded"}));
                host.reply(200, created);
            } else {
                host.reply(201, created);
            }
            let delivery = host
                .run(
                    if wait {
                        &["--wait", "--project", "displayName"]
                    } else {
                        &["--project", "displayName"]
                    },
                    b"",
                )
                .await
                .unwrap();
            let valid_root = folder.is_null();
            assert_eq!(delivery.report().exit_code(), u8::from(!valid_root));
            let mut bytes = Vec::new();
            delivery.deliver(&mut bytes).unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["data"], "Exact");
            assert_eq!(value["meta"]["completed"], valid_root);
            assert_eq!(value["meta"]["folderId"], folder);
            assert_eq!(value["meta"]["folderIdObserved"], true);
            assert_eq!(
                value["meta"]["submission"]["httpStatus"],
                if wait { 202 } else { 201 }
            );
            assert_eq!(
                value["meta"]["submission"]["folderId"],
                if wait { Value::Null } else { folder }
            );
            assert_eq!(value["meta"]["submission"]["folderIdObserved"], true);
            assert_eq!(host.requests().len(), if wait { 4 } else { 2 });
            assert_eq!(
                host.requests()
                    .iter()
                    .filter(|request| request.0 == "POST")
                    .count(),
                1
            );
        }
    }
}

#[tokio::test]
async fn accepted_folder_observations_stay_protected_when_later_poll_fails() {
    let mut initial = resource();
    initial["folderId"] = json!(RESOURCE);
    let mut host = Host::new();
    host.reply(200, json!({"value":[]}));
    host.accepted(initial);
    host.reply(500, json!({"errorCode":"SyntheticPollFailure"}));
    let failed = host
        .run(&["--wait", "--project", "displayName"], b"")
        .await
        .unwrap();
    assert_eq!(failed.report().exit_code(), 1);
    let mut bytes = Vec::new();
    failed.deliver(&mut bytes).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let meta = &value["meta"];
    assert_eq!(value["data"], "Exact");
    assert_eq!(meta["completed"], false);
    assert_eq!(meta["submission"]["httpStatus"], 202);
    assert_eq!(meta["submission"]["folderId"], RESOURCE);
    assert_eq!(meta["submission"]["folderIdObserved"], true);
    assert_eq!(meta["submission"]["resource"]["id"], RESOURCE);
    assert_eq!(
        meta["submission"]["operationHandle"],
        format!("/operations/{OPERATION}")
    );
    assert_eq!(meta["observation"]["httpStatus"], 500);
    assert_eq!(meta["remoteReplayed"], false);
    assert_eq!(host.requests().len(), 3);
}

#[tokio::test]
async fn existing_error_denial_and_raced_409_never_mutate_twice_or_become_skip() {
    let mut host = Host::new();
    host.reply(200, json!({"value":[resource()]}));
    assert!(host.run(&[], b"").await.is_err());
    assert_eq!(host.requests().len(), 1);

    let mut host = Host::new();
    host.approval.deny = true;
    host.reply(200, json!({"value":[]}));
    assert!(matches!(
        host.run(&[], b"").await,
        Err(CreateCommandError::Approval(CreateApprovalError::Denied))
    ));
    assert_eq!(host.requests().len(), 1);

    for code in ["ItemDisplayNameAlreadyInUse", "UnrelatedConflict"] {
        let mut host = Host::new();
        host.reply(200, json!({"value":[]}));
        host.reply(409, json!({"errorCode":code,"message":"synthetic"}));
        let failed = host.run(&["--if-exists", "skip"], b"").await.unwrap();
        assert_eq!(failed.report().exit_code(), 1);
        assert_eq!(failed.report().value()["meta"]["action"], "create");
        assert_eq!(failed.report().value()["meta"]["status"], "failed");
        assert_eq!(failed.report().value()["meta"]["httpStatus"], 409);
        assert_eq!(host.requests().len(), 2);
    }
}

#[tokio::test]
async fn expanded_body_approval_escapes_controls_and_cancellation_sends_no_post() {
    let raw = br#"{"displayName":"Exact","description":"Example\n\u001b[31m"}"#;
    for deny in [true, false] {
        let mut host = Host::new();
        host.approval.deny = deny;
        host.approval.pending = !deny;
        host.reply(200, json!({"value":[]}));
        let result = host
            .execute(
                &["create", TARGET, "--input", "-"],
                raw,
                Audience::Fabric,
                tokio::time::sleep(Duration::from_millis(25)),
            )
            .await;
        if deny {
            assert!(matches!(
                result,
                Err(CreateCommandError::Approval(CreateApprovalError::Denied))
            ));
        } else {
            assert!(matches!(result, Err(CreateCommandError::Interrupted)));
        }
        assert_eq!(host.requests().len(), 1);
        let approved = host.approval.body.as_ref().unwrap();
        let body: Value = serde_json::from_slice(approved).unwrap();
        assert_eq!(body["description"], "Example\n\u{1b}[31m");
        let question = host.approval.question.as_ref().unwrap();
        assert!(!question.contains('\n'));
        assert!(!question.contains('\u{1b}'));
        assert!(
            question.contains(
                &std::str::from_utf8(approved)
                    .unwrap()
                    .escape_debug()
                    .to_string()
            )
        );
    }
}

#[tokio::test]
async fn accepted_and_native_wait_distinguish_operation_from_resource_and_honor_retry_after() {
    for wait in [false, true] {
        let mut host = Host::new();
        host.reply(200, json!({"value":[]}));
        host.accepted(Value::Null);
        if wait {
            host.reply(200, json!({"status":"Succeeded"}));
            host.reply(200, resource());
        }
        let delivery = host
            .run(if wait { &["--wait"] } else { &[] }, b"")
            .await
            .unwrap();
        let meta = &delivery.report().value()["meta"];
        assert_eq!(delivery.report().exit_code(), 0);
        assert_eq!(meta["completed"], wait);
        assert_eq!(meta["operationHandle"], format!("/operations/{OPERATION}"));
        assert_eq!(meta["submission"]["httpStatus"], 202);
        assert_eq!(
            meta["submission"]["operationHandle"],
            format!("/operations/{OPERATION}")
        );
        assert_eq!(
            meta["resourceId"],
            if wait { json!(RESOURCE) } else { Value::Null }
        );
        let requests = host.requests();
        assert_eq!(requests.len(), if wait { 4 } else { 2 });
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.0 == "POST")
                .count(),
            1
        );
        if wait {
            assert_eq!(requests[2].1, format!("/v1/operations/{OPERATION}"));
            assert_eq!(requests[3].1, format!("/v1/operations/{OPERATION}/result"));
            assert!(
                host.timer
                    .0
                    .lock()
                    .unwrap()
                    .contains(&Duration::from_secs(1))
            );
        }
    }
}

#[tokio::test]
async fn partial_accepted_body_and_poll_failure_keep_reconciliation_evidence() {
    for broken_body in [true, false] {
        let mut host = Host::new();
        host.reply(200, json!({"value":[]}));
        if broken_body {
            host.wire
                .replies
                .lock()
                .unwrap()
                .push_back(Reply::BrokenAccepted);
        } else {
            host.accepted(resource());
            host.reply(500, json!({"errorCode":"SyntheticPollFailure"}));
        }
        let failed = host.run(&["--wait"], b"").await.unwrap();
        assert_eq!(failed.report().exit_code(), 1);
        let meta = &failed.report().value()["meta"];
        assert_eq!(meta["completed"], false);
        assert_eq!(meta["operationHandle"], format!("/operations/{OPERATION}"));
        assert_eq!(meta["mutationMayHaveOccurred"], true);
        assert_eq!(meta["submission"]["httpStatus"], 202);
        assert_eq!(
            meta["submission"]["operationHandle"],
            format!("/operations/{OPERATION}")
        );
        if !broken_body {
            assert_eq!(meta["submission"]["resource"]["id"], RESOURCE);
            assert_eq!(meta["observation"]["httpStatus"], 500);
        }
        assert_eq!(
            meta["resourceId"],
            if broken_body {
                Value::Null
            } else {
                json!(RESOURCE)
            }
        );
        assert_eq!(
            host.requests()
                .iter()
                .filter(|request| request.0 == "POST")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn accepted_response_limit_keeps_header_operation_id_without_body_or_replay() {
    let mut host = Host::new();
    host.reply(200, json!({"value":[]}));
    host.wire.replies.lock().unwrap().push_back(Reply::Complete(
        202,
        accepted_headers(),
        vec![b' '; fabric_cli::create_command::MAX_BYTES + 1],
    ));
    let failed = host.run(&[], b"").await.unwrap();
    assert_eq!(failed.report().exit_code(), 1);
    let meta = &failed.report().value()["meta"];
    assert_eq!(meta["completed"], false);
    assert_eq!(meta["httpStatus"], 202);
    assert_eq!(meta["operationHandle"], format!("/operations/{OPERATION}"));
    assert!(meta["resourceId"].is_null());
    assert_eq!(host.requests().len(), 2);
}

#[tokio::test]
async fn cancellation_or_total_deadline_after_acceptance_never_replays_or_rolls_back() {
    for pending_body in [true, false] {
        let mut host = Host::new();
        host.reply(200, json!({"value":[]}));
        if pending_body {
            host.wire
                .replies
                .lock()
                .unwrap()
                .push_back(Reply::PendingAccepted);
        } else {
            host.accepted(resource());
            host.wire.replies.lock().unwrap().push_back(Reply::Pending);
        }
        let failed = host
            .execute(
                &["create", TARGET, "--wait"],
                b"",
                Audience::Fabric,
                tokio::time::sleep(Duration::from_millis(25)),
            )
            .await
            .unwrap();
        assert_eq!(failed.report().exit_code(), 1);
        let meta = &failed.report().value()["meta"];
        assert_eq!(meta["operationHandle"], format!("/operations/{OPERATION}"));
        assert_eq!(meta["mutationMayHaveOccurred"], true);
        assert_eq!(meta["remoteCancelled"], false);
        assert_eq!(meta["remoteReplayed"], false);
        assert_eq!(meta["submission"]["httpStatus"], 202);
        if !pending_body {
            assert_eq!(meta["submission"]["resource"]["id"], RESOURCE);
        }
        assert_eq!(
            host.requests()
                .iter()
                .filter(|request| request.0 == "POST")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn unexpected_success_result_status_stays_unknown_with_original_typed_http_cause() {
    for status in [201, 202, 204, 400, 409, 500] {
        let mut host = Host::new();
        host.reply(200, json!({"value":[]}));
        let mut initial = resource();
        initial["folderId"] = Value::Null;
        host.accepted(initial);
        host.reply(200, json!({"status":"Succeeded"}));
        host.reply(
            status,
            json!({"errorCode":"UnexpectedResult","message":"synthetic"}),
        );
        let failed = host
            .run(&["--wait", "--project", "displayName"], b"")
            .await
            .unwrap();
        assert_eq!(failed.report().exit_code(), 1);
        assert!(matches!(
            &failed.report().error().unwrap().kind,
            fabric_sdk_operations::OperationErrorKind::Http { status: actual, .. } if *actual == status
        ));
        let mut bytes = Vec::new();
        failed.deliver(&mut bytes).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let meta = &value["meta"];
        assert_eq!(value["data"], "Exact");
        assert_eq!(
            value["error"]["code"],
            if status < 300 {
                "InvalidResponse"
            } else {
                "ServiceRejected"
            }
        );
        assert_eq!(
            meta["status"],
            if status < 300 { "unknown" } else { "failed" }
        );
        assert_eq!(meta["completed"], false);
        assert_eq!(meta["httpStatus"], status);
        assert_eq!(meta["observation"]["httpStatus"], status);
        assert_eq!(meta["submission"]["httpStatus"], 202);
        assert_eq!(meta["submission"]["resource"]["id"], RESOURCE);
        assert_eq!(meta["submission"]["folderId"], Value::Null);
        assert_eq!(meta["submission"]["folderIdObserved"], true);
        assert_eq!(
            meta["submission"]["operationHandle"],
            format!("/operations/{OPERATION}")
        );
        assert_eq!(meta["operationHandle"], format!("/operations/{OPERATION}"));
        assert_eq!(meta["resourceId"], RESOURCE);
        assert_eq!(meta["mutationMayHaveOccurred"], true);
        assert_eq!(meta["remoteReplayed"], false);
        assert_eq!(meta["remoteCancelled"], false);
        let requests = host.requests();
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests[1].1,
            format!("/v1/workspaces/{WORKSPACE}/semanticModels")
        );
        assert_eq!(requests[3].1, format!("/v1/operations/{OPERATION}/result"));
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.0 == "POST")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn duplicate_native_response_keys_reject_at_root_nested_and_escaped_names_without_replay() {
    let responses = [
        format!(
            r#"{{"id":"{RESOURCE}","id":"{OPERATION}","workspaceId":"{WORKSPACE}","displayName":"Exact","type":"SemanticModel"}}"#
        ),
        format!(
            r#"{{"id":"{RESOURCE}","\u0069d":"{OPERATION}","workspaceId":"{WORKSPACE}","displayName":"Exact","type":"SemanticModel"}}"#
        ),
        format!(
            r#"{{"id":"{RESOURCE}","workspaceId":"{WORKSPACE}","displayName":"Other","displayName":"Exact","type":"SemanticModel"}}"#
        ),
        format!(
            r#"{{"id":"{RESOURCE}","workspaceId":"{WORKSPACE}","displayName":"Exact","type":"SemanticModel","details":{{"value":1,"value":2}}}}"#
        ),
        format!(
            r#"{{"id":"{RESOURCE}","workspaceId":"{WORKSPACE}","displayName":"Exact","type":"SemanticModel","details":{{"value":1,"\u0076alue":2}}}}"#
        ),
    ];
    for mode in [201, 202, 200] {
        for raw in &responses {
            let mut host = Host::new();
            host.reply(200, json!({"value":[]}));
            if mode == 200 {
                let mut initial = resource();
                initial["folderId"] = Value::Null;
                host.accepted(initial);
                host.reply(200, json!({"status":"Succeeded"}));
            }
            host.wire.replies.lock().unwrap().push_back(Reply::Complete(
                mode,
                if mode == 202 {
                    accepted_headers()
                } else {
                    Headers::new()
                },
                raw.as_bytes().to_vec(),
            ));
            let failed = host
                .run(if mode == 200 { &["--wait"] } else { &[] }, b"")
                .await
                .unwrap();
            assert_eq!(failed.report().exit_code(), 1);
            assert!(matches!(
                &failed.report().error().unwrap().kind,
                fabric_sdk_operations::OperationErrorKind::InvalidResponse
            ));
            let mut bytes = Vec::new();
            failed.deliver(&mut bytes).unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            let meta = &value["meta"];
            assert_eq!(value["error"]["code"], "InvalidResponse");
            assert_eq!(meta["status"], "unknown");
            assert_eq!(meta["completed"], false);
            assert_eq!(meta["httpStatus"], mode);
            assert_eq!(
                meta["unparsedResponseBodyBase64"],
                STANDARD.encode(raw.as_bytes())
            );
            assert_eq!(meta["mutationMayHaveOccurred"], true);
            assert_eq!(meta["remoteReplayed"], false);
            assert_eq!(meta["remoteCancelled"], false);
            if mode == 200 {
                assert_eq!(value["data"], resource_with_null_folder());
                assert_eq!(meta["resourceId"], RESOURCE);
                assert_eq!(meta["submission"]["httpStatus"], 202);
                assert_eq!(meta["submission"]["resource"]["id"], RESOURCE);
                assert_eq!(meta["submission"]["folderId"], Value::Null);
                assert_eq!(meta["submission"]["folderIdObserved"], true);
                assert_eq!(
                    meta["submission"]["operationHandle"],
                    format!("/operations/{OPERATION}")
                );
            } else {
                assert!(value["data"].is_null());
                assert!(meta["resourceId"].is_null());
                assert_eq!(meta["submission"]["httpStatus"], mode);
                if mode == 202 {
                    assert_eq!(
                        meta["unparsedAcceptanceBodyBase64"],
                        STANDARD.encode(raw.as_bytes())
                    );
                }
            }
            if mode != 201 {
                assert_eq!(meta["operationHandle"], format!("/operations/{OPERATION}"));
            }
            let requests = host.requests();
            assert_eq!(requests.len(), if mode == 200 { 4 } else { 2 });
            assert_eq!(
                requests[1].1,
                format!("/v1/workspaces/{WORKSPACE}/semanticModels")
            );
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.0 == "POST")
                    .count(),
                1
            );
        }
    }
}

fn resource_with_null_folder() -> Value {
    let mut value = resource();
    value["folderId"] = Value::Null;
    value
}

#[tokio::test]
async fn malformed_final_and_no_wait_acceptance_retain_submission_outside_projection() {
    let mut host = Host::new();
    host.reply(200, json!({"value":[]}));
    host.accepted(resource());
    host.reply(200, json!({"status":"Succeeded"}));
    host.wire.replies.lock().unwrap().push_back(Reply::Complete(
        200,
        Headers::new(),
        b"{".to_vec(),
    ));
    let failed = host
        .run(&["--wait", "--project", "displayName"], b"")
        .await
        .unwrap();
    assert_eq!(failed.report().exit_code(), 1);
    let mut bytes = Vec::new();
    failed.deliver(&mut bytes).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["meta"]["completed"], false);
    assert_eq!(value["meta"]["submission"]["httpStatus"], 202);
    assert_eq!(value["meta"]["submission"]["resource"]["id"], RESOURCE);
    assert_eq!(
        value["meta"]["submission"]["operationHandle"],
        format!("/operations/{OPERATION}")
    );
    assert_eq!(host.requests().len(), 4);

    let mut host = Host::new();
    host.reply(200, json!({"value":[]}));
    host.wire.replies.lock().unwrap().push_back(Reply::Complete(
        202,
        accepted_headers(),
        b"{".to_vec(),
    ));
    let failed = host.run(&[], b"").await.unwrap();
    assert_eq!(failed.report().exit_code(), 1);
    let meta = &failed.report().value()["meta"];
    assert_eq!(meta["completed"], false);
    assert_eq!(meta["submission"]["httpStatus"], 202);
    assert_eq!(
        meta["submission"]["operationHandle"],
        format!("/operations/{OPERATION}")
    );
    assert_eq!(meta["unparsedAcceptanceBodyBase64"], STANDARD.encode(b"{"));
    assert_eq!(host.requests().len(), 2);
}

struct BrokenWriter;
impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("synthetic closed sink"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("synthetic flush failure"))
    }
}

#[tokio::test]
async fn data_projection_and_output_failures_preserve_unprojected_receipt_and_no_clobber() {
    let mut host = Host::new();
    host.reply(200, json!({"value":[]}));
    host.reply(201, resource());
    let delivery = host.run(&["--project", "displayName"], b"").await.unwrap();
    let mut bytes = Vec::new();
    delivery.deliver(&mut bytes).unwrap();
    let output: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(output["data"], "Exact");
    assert_eq!(output["meta"]["resourceId"], RESOURCE);

    let mut host = Host::new();
    host.reply(200, json!({"value":[]}));
    host.accepted(resource());
    let delivery = host.run(&[], b"").await.unwrap();
    let Err(CreateCommandError::Delivery { report, .. }) = delivery.deliver(&mut BrokenWriter)
    else {
        panic!("writer failure must retain the original report")
    };
    assert_eq!(report.value()["meta"]["resourceId"], RESOURCE);
    assert_eq!(
        report.value()["meta"]["operationHandle"],
        format!("/operations/{OPERATION}")
    );
    assert_eq!(host.requests().len(), 2);

    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("receipt.json"), b"keep").unwrap();
    let command = v2_create::parse_args(["create", TARGET, "--output", "receipt.json"]).unwrap();
    assert!(
        PreparedCreateCommand::preflight(
            command,
            &BlankSemanticModelBinding,
            temp.path(),
            &mut ForbiddenRead,
            false
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(temp.path().join("receipt.json")).unwrap(),
        b"keep"
    );
}

#[tokio::test]
async fn malformed_created_result_and_input_bounds_do_not_prove_completion_or_replay() {
    for response in [
        json!({"id":OPERATION}),
        json!({"id":RESOURCE,"workspaceId":RESOURCE,"displayName":"Exact","type":"SemanticModel"}),
        json!({"id":RESOURCE,"workspaceId":WORKSPACE,"displayName":"Other","type":"SemanticModel"}),
        json!({"id":RESOURCE,"workspaceId":WORKSPACE,"displayName":"Exact","type":"Report"}),
    ] {
        let mut host = Host::new();
        host.reply(200, json!({"value":[]}));
        host.reply(201, response);
        let delivery = host.run(&[], b"").await.unwrap();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(delivery.report().value()["meta"]["completed"], false);
        assert_eq!(host.requests().len(), 2);
    }
    let mut host = Host::new();
    assert!(
        host.run(
            &["--input", "-"],
            &vec![b' '; fabric_cli::create_command::MAX_BYTES + 1]
        )
        .await
        .is_err()
    );
    assert!(host.requests().is_empty());
}
