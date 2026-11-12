// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline parser, shared native host, SDK/wire and actual executable contracts.
#![deny(unnameable_test_items)]

use fabric_cli::{
    create_command::{CreateCommandError, MAX_BYTES, PreparedCreateCommand, v2_create},
    spark_job_definition_create::EmptySparkJobDefinition,
};
use std::io::{self, Read};

#[cfg(feature = "native-host")]
#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
#[cfg(feature = "native-host")]
const OP: &str = "33333333-3333-3333-3333-333333333333";
const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact.SparkJobDefinition";

struct ForbiddenRead;
impl Read for ForbiddenRead {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("no implicit stdin access, including for unsupported policy")
    }
}

fn prepare(extra: &[&str], input: Option<&[u8]>) -> PreparedCreateCommand {
    let temp = tempfile::tempdir().unwrap();
    let command =
        v2_create::parse_args(["create", TARGET].into_iter().chain(extra.iter().copied())).unwrap();
    PreparedCreateCommand::preflight(
        command,
        &EmptySparkJobDefinition,
        temp.path(),
        &mut input.unwrap_or_default(),
        false,
    )
    .unwrap()
}

#[test]
fn omitted_and_proven_metadata_input_are_valid_without_consuming_ambient_stdin() {
    let temp = tempfile::tempdir().unwrap();
    let command = v2_create::parse_args(["create", TARGET]).unwrap();
    assert!(
        PreparedCreateCommand::preflight(
            command,
            &EmptySparkJobDefinition,
            temp.path(),
            &mut ForbiddenRead,
            false,
        )
        .is_ok()
    );
    for input in [
        "{}".to_owned(),
        r#"{"displayName":"Exact","type":"SparkJobDefinition","folderId":null}"#.into(),
        r#"{"description":""}"#.into(),
        serde_json::json!({"description":"x".repeat(256)}).to_string(),
        serde_json::json!({"description":"\u{00e9}".repeat(256)}).to_string(),
    ] {
        let _ = prepare(&["--input", "-"], Some(input.as_bytes()));
        std::fs::write(temp.path().join("input.json"), input).unwrap();
        let command = v2_create::parse_args(["create", TARGET, "--input", "input.json"]).unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &EmptySparkJobDefinition,
                temp.path(),
                &mut ForbiddenRead,
                false,
            )
            .is_ok()
        );
    }
}

#[test]
fn strict_bounded_unique_utf8_metadata_rejects_all_definition_and_runtime_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let mut invalid = vec![
        b"".to_vec(),
        b"null".to_vec(),
        b"[]".to_vec(),
        b"{".to_vec(),
        vec![0xff],
        b"{} {}".to_vec(),
        br#"{"displayName":"Exact","displayName":"Exact"}"#.to_vec(),
        br#"{"displayName":"Exact","\u0064isplayName":"Exact"}"#.to_vec(),
        br#"{"displayName":"Other"}"#.to_vec(),
        br#"{"displayName":null}"#.to_vec(),
        br#"{"type":"Notebook"}"#.to_vec(),
        br#"{"type":"sparkjobdefinition"}"#.to_vec(),
        br#"{"type":null}"#.to_vec(),
        br#"{"description":null}"#.to_vec(),
        br#"{"folderId":"22222222-2222-2222-2222-222222222222"}"#.to_vec(),
        serde_json::to_vec(&serde_json::json!({"description":"x".repeat(257)})).unwrap(),
        vec![b' '; MAX_BYTES + 1],
    ];
    for field in [
        "definition",
        "code",
        "mainFile",
        "mainDefinitionFile",
        "executableFile",
        "libraries",
        "executionParameters",
        "executionData",
        "defaultLakehouse",
        "compute",
        "sensitivityLabelSettings",
        "unknown",
    ] {
        invalid.push(serde_json::to_vec(&serde_json::json!({field:null})).unwrap());
    }
    for bytes in invalid {
        let command =
            v2_create::parse_args(["create", TARGET, "--input", "-", "--if-exists", "skip"])
                .unwrap();
        assert!(
            PreparedCreateCommand::preflight(
                command,
                &EmptySparkJobDefinition,
                temp.path(),
                &mut bytes.as_slice(),
                false,
            )
            .is_err(),
            "explicit invalid input must fail before discovery/credentials, even for skip"
        );
    }
}

#[test]
fn unsupported_target_profile_and_policy_fail_before_input_or_output_effects() {
    let temp = tempfile::tempdir().unwrap();
    for target in [
        "/Name.Workspace/Exact.SparkJobDefinition",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Notebook",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.SparkJobDefinition/Files/main.py",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.SparkJobDefinition/src/definition",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact%20Name.SparkJobDefinition",
        "/11111111-1111-1111-1111-111111111111.Workspace",
        "/workspaces/11111111-1111-1111-1111-111111111111/items/22222222-2222-2222-2222-222222222222",
    ] {
        let command = v2_create::parse_args(["create", target, "--input", "-"]).unwrap();
        assert!(matches!(
            PreparedCreateCommand::preflight(
                command,
                &EmptySparkJobDefinition,
                temp.path(),
                &mut ForbiddenRead,
                false,
            ),
            Err(CreateCommandError::Invalid)
        ));
    }
    for policy in [
        vec!["--force"],
        vec!["--if-exists", "overwrite"],
        vec!["--if-exists", "duplicate"],
    ] {
        for source in ["-", "missing.json"] {
            let command = v2_create::parse_args(
                ["create", TARGET, "--input", source, "--output", "new.json"]
                    .into_iter()
                    .chain(policy.iter().copied()),
            )
            .unwrap();
            assert!(matches!(
                PreparedCreateCommand::preflight(
                    command,
                    &EmptySparkJobDefinition,
                    temp.path(),
                    &mut ForbiddenRead,
                    false,
                ),
                Err(CreateCommandError::Policy(
                    fabric_sdk::items::create::CreateError::UnsupportedPolicy
                ))
            ));
        }
    }
    let command =
        v2_create::parse_args(["create", TARGET, "--profile", "ambient", "--input", "-"]).unwrap();
    assert!(matches!(
        PreparedCreateCommand::preflight(
            command,
            &EmptySparkJobDefinition,
            temp.path(),
            &mut ForbiddenRead,
            false,
        ),
        Err(CreateCommandError::Invalid)
    ));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn operation_rejects_substituted_raw_bytes_and_noncreate_plans() {
    use fabric_cli::create_command::CreateBinding;
    use fabric_sdk::items::create::{
        ConflictPolicy, CreateTarget, LeafState, NativeId, Preflight, ResourceIdentity,
        TargetScope, plan_create,
    };
    let target = CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WS).unwrap(),
        "Exact",
        "SparkJobDefinition",
    )
    .unwrap();
    let binding = EmptySparkJobDefinition;
    let plan = plan_create(
        ConflictPolicy::Error,
        target.clone(),
        Preflight::Leaf(LeafState::Missing),
        &binding.capabilities(),
        Some(serde_json::json!({})),
        &binding,
    )
    .unwrap();
    assert!(binding.operation(&plan, None, false).is_err());
    assert!(
        binding
            .operation(&plan, Some(br#"{"description":"changed"}"#), false)
            .is_err()
    );
    assert!(binding.operation(&plan, Some(b"{}"), false).is_ok());
    let plan = plan_create(
        ConflictPolicy::Skip,
        target,
        Preflight::Leaf(LeafState::Existing(
            ResourceIdentity::new(
                NativeId::new(ITEM).unwrap(),
                NativeId::new(WS).unwrap(),
                "SparkJobDefinition",
            )
            .unwrap(),
        )),
        &binding.capabilities(),
        None,
        &binding,
    )
    .unwrap();
    assert!(binding.operation(&plan, None, false).is_err());
}

#[cfg(feature = "native-host")]
mod native {
    #![deny(unnameable_test_items)]
    use super::*;
    use azure_core::http::{AsyncRawResponse, Body, Method, headers::Headers};
    use fabric_cli::create_command::{
        CreateApproval, CreateApprovalError, CreateApprovalRequest, CreateDelivery,
        CreateExecution, CreateTransportEvidence,
    };
    use fabric_sdk::{
        Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    };
    use fabric_sdk_http::{ExecutionOptions, ExecutionTrace, HttpExecutor};
    use serde_json::{Value, json};
    use std::{
        future::{Future, pending},
        io::Write,
        pin::Pin,
        sync::{Arc, atomic::Ordering},
        time::Duration,
    };
    use support::{ScriptedHttp, TestCredential, TestTimer, response};

    struct Approval {
        allow: bool,
        calls: usize,
        approved_body: Option<Vec<u8>>,
        approved_question: Option<String>,
    }
    impl CreateApproval for Approval {
        fn authorize<'a>(
            &'a mut self,
            request: CreateApprovalRequest<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
            self.calls += 1;
            assert_eq!(request.plan().target().name(), "Exact");
            assert_eq!(request.plan().target().parent_id().as_str(), WS);
            assert_eq!(request.plan().target().item_type(), "SparkJobDefinition");
            assert_eq!(request.context().selection().account(), "offline");
            assert_eq!(request.context().selection().tenant(), "synthetic");
            assert_eq!(request.context().selection().audience(), Audience::Fabric);
            assert_eq!(
                request.context().endpoint(),
                &EndpointBinding::public(ServiceOrigin::Fabric)
            );
            assert_eq!(
                request.remote_write(),
                matches!(
                    request.plan().action(),
                    fabric_sdk::items::create::CreateAction::Create { .. }
                ),
            );
            assert!(request.anticipated_effects().is_empty());
            let question = request.approval_question();
            assert!(question.contains("audience=Fabric"));
            assert!(question.contains("anticipatedEffects=[]"));
            if request.remote_write() {
                let operation = request.operation().unwrap();
                assert_eq!(
                    operation.method(),
                    fabric_sdk_workspaces::mutations::ResourceMethod::Post
                );
                assert_eq!(
                    operation.path(),
                    format!("/v1/workspaces/{WS}/sparkJobDefinitions")
                );
                assert_eq!(
                    operation.completion(),
                    if request.command().wait {
                        fabric_sdk_workspaces::mutations::CompletionPolicy::WaitForResult
                    } else {
                        fabric_sdk_workspaces::mutations::CompletionPolicy::ReturnAccepted
                    },
                );
                assert_eq!(
                    request.url().unwrap().as_str(),
                    format!(
                        "https://api.fabric.microsoft.com/v1/workspaces/{WS}/sparkJobDefinitions"
                    ),
                );
                assert!(question.contains("SparkJobDefinition create policy"));
                assert!(question.contains("method=POST"));
                assert!(
                    question.contains(
                        &std::str::from_utf8(operation.body())
                            .unwrap()
                            .escape_debug()
                            .to_string(),
                    )
                );
                self.approved_body = Some(operation.body().to_vec());
            } else {
                assert!(request.operation().is_none());
                assert!(request.url().is_none());
                assert!(question.contains("no native request (unchanged skip)"));
            }
            self.approved_question = Some(question);
            Box::pin(async move {
                if self.allow {
                    Ok(())
                } else {
                    Err(CreateApprovalError::Denied)
                }
            })
        }
    }
    struct Harness {
        wire: Arc<ScriptedHttp>,
        credential: Arc<TestCredential>,
        timer: Arc<TestTimer>,
        client: Client,
        http: HttpExecutor,
        trace: ExecutionTrace,
    }
    impl Harness {
        fn new(replies: Vec<AsyncRawResponse>) -> Self {
            let wire = ScriptedHttp::new(replies);
            let credential = Arc::new(TestCredential::default());
            let timer = Arc::new(TestTimer::default());
            let trace = ExecutionTrace::default();
            let context = RequestContext::with_endpoint(
                credential.clone(),
                CredentialSelection::new("offline", "synthetic", Audience::Fabric).unwrap(),
                EndpointBinding::public(ServiceOrigin::Fabric),
            );
            Self {
                http: HttpExecutor::new(wire.clone(), timer.clone(), ExecutionOptions::default())
                    .unwrap()
                    .with_trace(trace.clone()),
                client: Client::with_context(context),
                wire,
                credential,
                timer,
                trace,
            }
        }
        async fn execute(
            &self,
            prepared: PreparedCreateCommand,
            approval: &mut Approval,
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
                    &EmptySparkJobDefinition,
                    &self.client,
                    &self.http,
                    CreateExecution {
                        executor: self.http.clone(),
                        observe: &observe,
                    },
                    approval,
                    interrupted,
                )
                .await
        }
        fn assert_calls(&self, total: usize, posts: usize) {
            let requests = self.wire.requests.lock().unwrap();
            assert_eq!(requests.len(), total);
            assert_eq!(
                requests
                    .iter()
                    .filter(|r| r.method() == Method::Post)
                    .count(),
                posts
            );
            for request in requests.iter() {
                assert_eq!(request.url().host_str(), Some("api.fabric.microsoft.com"));
                assert!(
                    request.url().path() == format!("/v1/workspaces/{WS}/items")
                        || request.url().path()
                            == format!("/v1/workspaces/{WS}/sparkJobDefinitions")
                        || request
                            .url()
                            .path()
                            .starts_with(&format!("/v1/operations/{OP}"))
                );
                if request.url().path() == format!("/v1/workspaces/{WS}/items") {
                    assert_eq!(request.method(), Method::Get);
                    assert_eq!(request.url().query(), Some("recursive=false"));
                } else {
                    assert!(request.url().query().is_none());
                }
            }
        }
    }
    fn missing() -> AsyncRawResponse {
        response(200, r#"{"value":[]}"#, &[])
    }
    fn item(name: &str, kind: &str) -> Value {
        json!({"id":ITEM,"workspaceId":WS,"displayName":name,"type":kind})
    }
    fn with_folder(mut item: Value, folder: Option<Value>) -> Value {
        if let Some(folder) = folder {
            item["folderId"] = folder;
        }
        item
    }
    fn created() -> AsyncRawResponse {
        response(201, &item("Exact", "SparkJobDefinition").to_string(), &[])
    }
    fn accepted(body: &str) -> AsyncRawResponse {
        response(
            202,
            body,
            &[
                ("x-ms-operation-id", OP),
                (
                    "location",
                    &format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
                ),
                ("retry-after", "2"),
            ],
        )
    }
    fn approve() -> Approval {
        Approval {
            allow: true,
            calls: 0,
            approved_body: None,
            approved_question: None,
        }
    }

    #[tokio::test]
    async fn malformed_incomplete_and_duplicate_discovery_pages_cannot_authorize_creation() {
        for replies in [
            vec![response(200, "{", &[])],
            vec![response(202, "", &[])],
            vec![
                response(
                    200,
                    &json!({
                        "value":[item("Exact","SparkJobDefinition")],
                        "continuationToken":"next",
                    })
                    .to_string(),
                    &[],
                ),
                response(403, "", &[]),
            ],
            vec![
                response(
                    200,
                    &json!({
                        "value":[item("Exact","SparkJobDefinition")],
                        "continuationToken":"next",
                    })
                    .to_string(),
                    &[],
                ),
                response(
                    200,
                    &json!({
                        "value":[item("Exact","SparkJobDefinition")],
                    })
                    .to_string(),
                    &[],
                ),
            ],
        ] {
            let h = Harness::new(replies);
            let mut approval = approve();
            assert!(
                h.execute(
                    prepare(&["--if-exists", "skip"], None),
                    &mut approval,
                    pending(),
                )
                .await
                .is_err()
            );
            assert_eq!(approval.calls, 0);
            assert!(
                h.wire
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|r| r.method() == Method::Get)
            );
        }
    }

    #[tokio::test]
    async fn parser_native_host_sdk_issue_only_the_exact_minimal_typed_post() {
        for input in [
            None,
            Some(br#"{"description":"metadata only"}"#.as_slice()),
            Some(
                b" {\r\n \"description\" : \"literal\\u0020metadata\\n\\u001b[2J\" }\n".as_slice(),
            ),
        ] {
            let h = Harness::new(vec![missing(), created()]);
            let mut approval = approve();
            let options = if input.is_some() {
                vec!["--input", "-"]
            } else {
                vec![]
            };
            let delivery = h
                .execute(prepare(&options, input), &mut approval, pending())
                .await
                .unwrap();
            assert_eq!(delivery.report().exit_code(), 0);
            assert_eq!(delivery.report().value()["meta"]["status"], "completed");
            assert_eq!(delivery.report().value()["meta"]["resourceId"], ITEM);
            assert_eq!(approval.calls, 1);
            assert_eq!(h.credential.calls.load(Ordering::SeqCst), 2);
            h.assert_calls(2, 1);
            let requests = h.wire.requests.lock().unwrap();
            assert_eq!(
                requests[1].url().path(),
                format!("/v1/workspaces/{WS}/sparkJobDefinitions")
            );
            let Body::Bytes(bytes) = requests[1].body() else {
                panic!("native JSON bytes")
            };
            let mut expected =
                json!({"displayName":"Exact","type":"SparkJobDefinition","folderId":null});
            if let Some(raw) = input {
                expected["description"] =
                    serde_json::from_slice::<Value>(raw).unwrap()["description"].clone();
                assert_ne!(bytes.as_ref(), raw);
            }
            assert_eq!(serde_json::from_slice::<Value>(bytes).unwrap(), expected);
            assert_eq!(approval.approved_body.as_deref(), Some(bytes.as_ref()));
            let question = approval.approved_question.unwrap();
            assert!(!question.contains('\u{1b}'));
            assert!(!question.contains('\n'));
            assert_eq!(
                delivery.report().value()["meta"]["anticipatedEffects"],
                json!([])
            );
            assert_eq!(
                delivery.report().value()["meta"]["submission"]["httpStatus"],
                201
            );
        }
    }

    #[tokio::test]
    async fn exact_skip_collision_ambiguity_wrong_kind_case_and_discovery_error_never_write() {
        for (items, skip, success) in [
            (vec![item("Exact", "SparkJobDefinition")], true, true),
            (
                vec![
                    item("Exact", "SparkJobDefinition"),
                    item("Exact", "Notebook"),
                ],
                true,
                true,
            ),
            (vec![item("Exact", "SparkJobDefinition")], false, false),
            (vec![item("Exact", "SparkJobDefinition"); 2], true, false),
            (vec![item("Exact", "Notebook")], true, false),
            (vec![item("exact", "SparkJobDefinition")], true, false),
            (
                vec![json!({"id":"bad","displayName":"Exact","type":"SparkJobDefinition"})],
                true,
                false,
            ),
            (
                vec![
                    json!({"id":ITEM,"workspaceId":OP,"displayName":"Exact","type":"SparkJobDefinition"}),
                ],
                true,
                false,
            ),
        ] {
            let h = Harness::new(vec![response(
                200,
                &json!({"value":items}).to_string(),
                &[],
            )]);
            let mut approval = approve();
            let options = if skip {
                vec!["--if-exists", "skip", "--input", "-"]
            } else {
                vec![]
            };
            let result = h
                .execute(
                    prepare(&options, if skip { Some(b"{}") } else { None }),
                    &mut approval,
                    pending(),
                )
                .await;
            assert_eq!(result.is_ok(), success);
            if let Ok(delivery) = result {
                assert_eq!(delivery.report().value()["meta"]["status"], "unchanged");
                assert_eq!(
                    delivery.report().value()["meta"]["contentEqualityClaimed"],
                    false
                );
                assert_eq!(
                    delivery.report().value()["meta"]["mutationMayHaveOccurred"],
                    false
                );
                assert_eq!(delivery.report().value()["meta"]["remoteWrite"], false);
                assert_eq!(
                    delivery.report().value()["meta"]["anticipatedEffects"],
                    json!([])
                );
                assert!(approval.approved_body.is_none());
                assert_eq!(approval.calls, 1);
            } else {
                assert_eq!(approval.calls, 0);
            }
            h.assert_calls(1, 0);
        }
        let h = Harness::new(vec![response(403, r#"{"errorCode":"Denied"}"#, &[])]);
        assert!(
            h.execute(prepare(&[], None), &mut approve(), pending())
                .await
                .is_err()
        );
        h.assert_calls(1, 0);
    }

    #[tokio::test]
    async fn root_skip_selects_only_direct_items_and_retains_observed_folder_identity() {
        for folder in [None, Some(Value::Null)] {
            let root = with_folder(item("Exact", "SparkJobDefinition"), folder.clone());
            let mut nested = with_folder(item("Exact", "SparkJobDefinition"), Some(json!(WS)));
            nested["id"] = json!(OP);
            for rows in [
                vec![root.clone()],
                vec![nested.clone(), root.clone()],
                vec![root.clone(), nested.clone(), item("Exact", "Notebook")],
            ] {
                let h = Harness::new(vec![response(200, &json!({"value":rows}).to_string(), &[])]);
                let mut approval = approve();
                let delivery = h
                    .execute(
                        prepare(&["--if-exists", "skip"], None),
                        &mut approval,
                        pending(),
                    )
                    .await
                    .unwrap();
                let receipt = delivery.report().value();
                assert_eq!(receipt["meta"]["status"], "unchanged");
                assert_eq!(receipt["meta"]["resourceId"], ITEM);
                assert_eq!(receipt["meta"]["folderId"], Value::Null);
                assert_eq!(receipt["meta"]["folderIdObserved"], folder.is_some());
                assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], false);
                assert_eq!(approval.calls, 1);
                assert!(approval.approved_body.is_none());
                h.assert_calls(1, 0);
            }
        }
    }

    #[tokio::test]
    async fn nested_only_malformed_or_ambiguous_root_matches_fail_before_approval() {
        let root = item("Exact", "SparkJobDefinition");
        let nested = with_folder(root.clone(), Some(json!(OP)));
        let mut cases = vec![
            vec![nested.clone()],
            vec![nested.clone(), nested.clone()],
            vec![root.clone(), root.clone()],
            vec![nested, item("Exact", "Notebook")],
        ];
        for malformed in [
            json!("not-a-guid"),
            json!(17),
            json!(false),
            json!([]),
            json!({}),
        ] {
            cases.push(vec![with_folder(root.clone(), Some(malformed.clone()))]);
            cases.push(vec![
                root.clone(),
                with_folder(root.clone(), Some(malformed)),
            ]);
        }
        for rows in cases {
            for options in [vec![], vec!["--if-exists", "skip"]] {
                let h = Harness::new(vec![response(200, &json!({"value":rows}).to_string(), &[])]);
                let mut approval = approve();
                assert!(
                    h.execute(prepare(&options, None), &mut approval, pending())
                        .await
                        .is_err()
                );
                assert_eq!(approval.calls, 0);
                assert!(approval.approved_body.is_none());
                h.assert_calls(1, 0);
            }
        }
    }

    #[tokio::test]
    async fn root_discovery_keeps_recursive_false_on_every_page_and_opaque_token_once() {
        let token = "next +/=";
        let h = Harness::new(vec![
            response(
                200,
                &json!({"value":[],"continuationToken":token}).to_string(),
                &[],
            ),
            response(
                200,
                &json!({"value":[with_folder(
                    item("Exact", "SparkJobDefinition"), Some(Value::Null),
                )]})
                .to_string(),
                &[],
            ),
        ]);
        let mut approval = approve();
        let delivery = h
            .execute(
                prepare(&["--if-exists", "skip"], None),
                &mut approval,
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(delivery.report().value()["meta"]["status"], "unchanged");
        assert_eq!(delivery.report().value()["meta"]["folderIdObserved"], true);
        assert_eq!(approval.calls, 1);
        assert_eq!(h.credential.calls.load(Ordering::SeqCst), 2);
        let requests = h.wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].url().as_str(),
            format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/items?recursive=false"),
        );
        assert_eq!(
            requests[1].url().as_str(),
            format!(
                "https://api.fabric.microsoft.com/v1/workspaces/{WS}/items?recursive=false&continuationToken=next+%2B%2F%3D"
            ),
        );
        assert!(
            requests
                .iter()
                .all(|request| request.method() == Method::Get)
        );
    }

    #[tokio::test]
    async fn completed_root_scope_is_revalidated_for_201_and_lro_result_without_replay() {
        for wait in [false, true] {
            for folder in [
                None,
                Some(Value::Null),
                Some(json!(OP)),
                Some(json!("invalid")),
                Some(json!(false)),
                Some(json!({})),
            ] {
                let valid_root = folder.is_none() || folder.as_ref().is_some_and(Value::is_null);
                let data = with_folder(item("Exact", "SparkJobDefinition"), folder.clone());
                let mut replies = vec![missing()];
                if wait {
                    replies.push(accepted(""));
                    replies.push(response(
                        200,
                        r#"{"status":"Succeeded"}"#,
                        &[(
                            "location",
                            &format!("https://api.fabric.microsoft.com/v1/operations/{OP}/result"),
                        )],
                    ));
                }
                replies.push(response(
                    if wait { 200 } else { 201 },
                    &data.to_string(),
                    &[],
                ));
                let h = Harness::new(replies);
                let options = if wait { vec!["--wait"] } else { vec![] };
                let delivery = h
                    .execute(prepare(&options, None), &mut approve(), pending())
                    .await
                    .unwrap();
                let receipt = delivery.report().value();
                assert_eq!(
                    delivery.report().exit_code(),
                    u8::from(!valid_root),
                    "{receipt}"
                );
                assert_eq!(receipt["meta"]["completed"], valid_root);
                assert_eq!(
                    receipt["meta"]["status"],
                    if valid_root { "completed" } else { "unknown" }
                );
                assert_eq!(receipt["meta"]["folderIdObserved"], folder.is_some());
                assert_eq!(
                    receipt["meta"]["folderId"],
                    folder.clone().unwrap_or(Value::Null)
                );
                if wait {
                    assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
                    assert_eq!(receipt["meta"]["submission"]["folderIdObserved"], false);
                } else {
                    assert_eq!(receipt["meta"]["submission"]["httpStatus"], 201);
                    assert_eq!(
                        receipt["meta"]["submission"]["folderIdObserved"],
                        folder.is_some()
                    );
                    assert_eq!(
                        receipt["meta"]["submission"]["folderId"],
                        folder.unwrap_or(Value::Null)
                    );
                }
                assert_eq!(receipt["meta"]["resourceId"], ITEM);
                assert_eq!(receipt["meta"]["remoteReplayed"], false);
                assert_eq!(receipt["meta"]["remoteCancelled"], false);
                h.assert_calls(if wait { 4 } else { 2 }, 1);
            }
        }
    }

    #[tokio::test]
    async fn accepted_folder_evidence_is_protected_from_projection_without_completion_claim() {
        for folder in [
            None,
            Some(Value::Null),
            Some(json!(OP)),
            Some(json!({"future":"unknown"})),
        ] {
            let data = with_folder(item("Exact", "SparkJobDefinition"), folder.clone());
            let h = Harness::new(vec![missing(), accepted(&data.to_string())]);
            let delivery = h
                .execute(
                    prepare(&["--project", "missing"], None),
                    &mut approve(),
                    pending(),
                )
                .await
                .unwrap();
            assert_eq!(delivery.report().exit_code(), 0);
            let mut bytes = Vec::new();
            delivery.deliver(&mut bytes).unwrap();
            let receipt: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(receipt["meta"]["status"], "accepted");
            assert_eq!(receipt["meta"]["completed"], false);
            assert_eq!(receipt["meta"]["folderIdObserved"], folder.is_some());
            assert_eq!(
                receipt["meta"]["submission"]["folderIdObserved"],
                folder.is_some()
            );
            assert_eq!(
                receipt["meta"]["folderId"],
                folder.clone().unwrap_or(Value::Null)
            );
            assert_eq!(
                receipt["meta"]["submission"]["folderId"],
                folder.unwrap_or(Value::Null)
            );
            assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
            assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
            assert_eq!(
                receipt["meta"]["operationHandle"],
                format!("/operations/{OP}")
            );
            assert_eq!(receipt["meta"]["remoteReplayed"], false);
            h.assert_calls(2, 1);
        }
    }

    #[tokio::test]
    async fn denied_approval_and_wrong_audience_or_endpoint_have_zero_post_effects() {
        let h = Harness::new(vec![missing()]);
        let mut approval = Approval {
            allow: false,
            ..approve()
        };
        assert!(matches!(
            h.execute(prepare(&[], None), &mut approval, pending())
                .await,
            Err(CreateCommandError::Approval(CreateApprovalError::Denied))
        ));
        h.assert_calls(1, 0);
        assert!(approval.approved_body.is_some());
        for (audience, origin) in [
            (Audience::Arm, ServiceOrigin::Fabric),
            (Audience::Fabric, ServiceOrigin::Arm),
        ] {
            let mut h = Harness::new(vec![]);
            h.client = Client::with_context(RequestContext::with_endpoint(
                h.credential.clone(),
                CredentialSelection::new("offline", "synthetic", audience).unwrap(),
                EndpointBinding::public(origin),
            ));
            assert!(matches!(
                h.execute(prepare(&[], None), &mut approve(), pending())
                    .await,
                Err(CreateCommandError::Invalid)
            ));
            assert_eq!(h.credential.calls.load(Ordering::SeqCst), 0);
            h.assert_calls(0, 0);
        }
    }

    struct PendingApproval;
    impl CreateApproval for PendingApproval {
        fn authorize<'a>(
            &'a mut self,
            request: CreateApprovalRequest<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<(), CreateApprovalError>> + Send + 'a>> {
            assert!(request.operation().is_some());
            Box::pin(pending())
        }
    }

    #[tokio::test]
    async fn cancellation_during_exact_body_approval_submits_nothing() {
        let h = Harness::new(vec![missing()]);
        let observe = || CreateTransportEvidence {
            http_status: None,
            operation_id: None,
        };
        let result = prepare(&[], None)
            .execute(
                &EmptySparkJobDefinition,
                &h.client,
                &h.http,
                CreateExecution {
                    executor: h.http.clone(),
                    observe: &observe,
                },
                &mut PendingApproval,
                tokio::time::sleep(Duration::from_millis(20)),
            )
            .await;
        assert!(matches!(result, Err(CreateCommandError::Interrupted)));
        assert_eq!(h.credential.calls.load(Ordering::SeqCst), 1);
        h.assert_calls(1, 0);
    }

    #[tokio::test]
    async fn accepted_completed_rejected_malformed_and_raced_conflict_never_replay() {
        for (reply, status, exit) in [
            (accepted(""), "accepted", 0),
            (response(403, r#"{"errorCode":"Denied"}"#, &[]), "failed", 1),
            (
                response(409, r#"{"errorCode":"ItemDisplayNameAlreadyInUse"}"#, &[]),
                "failed",
                1,
            ),
            (
                response(429, r#"{"errorCode":"Throttled"}"#, &[("retry-after", "1")]),
                "failed",
                1,
            ),
            (response(201, "{", &[]), "unknown", 1),
            (
                response(201, &item("Other", "SparkJobDefinition").to_string(), &[]),
                "unknown",
                1,
            ),
            (
                response(201, &item("Exact", "Notebook").to_string(), &[]),
                "unknown",
                1,
            ),
            (
                response(202, "not-json", &[("x-ms-operation-id", OP)]),
                "unknown",
                1,
            ),
        ] {
            let h = Harness::new(vec![missing(), reply]);
            let delivery = h
                .execute(
                    prepare(&["--if-exists", "skip"], None),
                    &mut approve(),
                    pending(),
                )
                .await
                .unwrap();
            assert_eq!(
                delivery.report().exit_code(),
                exit,
                "{}",
                delivery.report().value()
            );
            assert_eq!(delivery.report().value()["meta"]["status"], status);
            assert_eq!(delivery.report().value()["meta"]["remoteReplayed"], false);
            assert_eq!(delivery.report().value()["meta"]["remoteCancelled"], false);
            if status == "accepted" {
                assert_eq!(
                    delivery.report().value()["meta"]["operationHandle"],
                    format!("/operations/{OP}")
                );
                assert!(delivery.report().value()["meta"]["resourceId"].is_null());
                assert_eq!(delivery.report().value()["meta"]["completed"], false);
            }
            h.assert_calls(2, 1);
        }
    }

    #[tokio::test]
    async fn shared_lro_honors_retry_after_and_preserves_acceptance_when_polling_fails() {
        for success in [true, false] {
            let mut replies = vec![missing(), accepted(r#"{"acceptedMarker":"kept"}"#)];
            if success {
                replies.push(response(
                    200,
                    r#"{"status":"Succeeded"}"#,
                    &[(
                        "location",
                        &format!("https://api.fabric.microsoft.com/v1/operations/{OP}/result"),
                    )],
                ));
                replies.push(response(
                    200,
                    &item("Exact", "SparkJobDefinition").to_string(),
                    &[],
                ));
            } else {
                replies.push(response(403, r#"{"errorCode":"Denied"}"#, &[]));
            }
            let h = Harness::new(replies);
            let delivery = h
                .execute(prepare(&["--wait"], None), &mut approve(), pending())
                .await
                .unwrap();
            let receipt = delivery.report().value();
            assert_eq!(
                receipt["meta"]["operationHandle"],
                format!("/operations/{OP}")
            );
            assert_eq!(receipt["meta"]["completed"], success);
            assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
            assert_eq!(
                receipt["meta"]["submission"]["operationHandle"],
                format!("/operations/{OP}")
            );
            assert_eq!(
                receipt["meta"]["observation"]["httpStatus"],
                if success { 200 } else { 403 }
            );
            assert_eq!(delivery.report().exit_code(), u8::from(!success));
            assert!(h.timer.waits.lock().unwrap().contains(&2));
            if success {
                assert_eq!(receipt["meta"]["resourceId"], ITEM);
            } else {
                assert_eq!(receipt["data"]["acceptedMarker"], "kept");
            }
            h.assert_calls(if success { 4 } else { 3 }, 1);
        }
    }

    #[tokio::test]
    async fn cancellation_deadline_and_body_failure_retain_operation_without_replay() {
        for body_failure in [false, true] {
            let mut headers = Headers::new();
            headers.insert("x-ms-operation-id", OP);
            let body = if body_failure {
                futures_util::stream::once(async {
                    Err(azure_core::Error::new(
                        azure_core::error::ErrorKind::Other,
                        "synthetic body failure",
                    ))
                })
                .boxed()
            } else {
                futures_util::stream::pending().boxed()
            };
            let reply = AsyncRawResponse::new(202.into(), headers, body);
            let h = Harness::new(vec![missing(), reply]);
            let delivery = h
                .execute(
                    prepare(&["--wait"], None),
                    &mut approve(),
                    tokio::time::sleep(Duration::from_millis(20)),
                )
                .await
                .unwrap();
            let receipt = delivery.report().value();
            assert_eq!(delivery.report().exit_code(), 1);
            assert_eq!(receipt["meta"]["status"], "unknown");
            assert_eq!(
                receipt["meta"]["operationHandle"],
                format!("/operations/{OP}")
            );
            assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], true);
            assert_eq!(receipt["meta"]["completed"], false);
            h.assert_calls(2, 1);
        }
        let h = Harness::new(vec![missing(), accepted("")]);
        h.timer.pending_wait.store(true, Ordering::SeqCst);
        let delivery = h
            .execute(
                prepare(&["--wait"], None),
                &mut approve(),
                tokio::time::sleep(Duration::from_millis(20)),
            )
            .await
            .unwrap();
        assert_eq!(
            delivery.report().value()["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(delivery.report().value()["meta"]["status"], "unknown");
        h.assert_calls(2, 1);
    }

    struct RealTimer;
    impl fabric_sdk_http::ExecutionTimer for RealTimer {
        fn sleep(&self, delay: Duration) -> fabric_sdk_http::ExecutionFuture<'_, ()> {
            Box::pin(tokio::time::sleep(delay))
        }
    }

    #[tokio::test]
    async fn actual_http_poll_timeout_retains_initial_acceptance_and_operation_id() {
        let accepted = response(
            202,
            r#"{"acceptedMarker":"kept"}"#,
            &[("x-ms-operation-id", OP), ("retry-after", "0")],
        );
        let pending_poll = AsyncRawResponse::new(
            200.into(),
            Headers::new(),
            futures_util::stream::pending().boxed(),
        );
        let mut h = Harness::new(vec![missing(), accepted, pending_poll]);
        h.http = HttpExecutor::new(
            h.wire.clone(),
            Arc::new(RealTimer),
            ExecutionOptions {
                timeout: Duration::from_millis(20),
                ..ExecutionOptions::default()
            },
        )
        .unwrap()
        .with_trace(h.trace.clone());
        let delivery = h
            .execute(prepare(&["--wait"], None), &mut approve(), pending())
            .await
            .unwrap();
        let receipt = delivery.report().value();
        assert_eq!(delivery.report().exit_code(), 1);
        assert_eq!(receipt["error"]["code"], "TimedOut");
        assert_eq!(
            receipt["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(receipt["data"]["acceptedMarker"], "kept");
        assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(receipt["meta"]["completed"], false);
        assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
        h.assert_calls(3, 1);
    }

    #[tokio::test]
    async fn initial_accepted_identity_survives_failed_cancelled_and_malformed_result() {
        for outcome in ["denied", "cancelled", "malformed"] {
            let mut replies = vec![
                missing(),
                accepted(
                    &with_folder(item("Exact", "SparkJobDefinition"), Some(Value::Null))
                        .to_string(),
                ),
            ];
            if outcome == "denied" {
                replies.push(response(403, "", &[]));
            } else if outcome == "malformed" {
                replies.push(response(
                    200,
                    r#"{"status":"Succeeded"}"#,
                    &[(
                        "location",
                        &format!("https://api.fabric.microsoft.com/v1/operations/{OP}/result"),
                    )],
                ));
                replies.push(response(200, "{", &[]));
            }
            let h = Harness::new(replies);
            if outcome == "cancelled" {
                h.timer.pending_wait.store(true, Ordering::SeqCst);
            }
            let interrupted = async {
                if outcome == "cancelled" {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                } else {
                    pending::<()>().await;
                }
            };
            let delivery = h
                .execute(prepare(&["--wait"], None), &mut approve(), interrupted)
                .await
                .unwrap();
            let receipt = delivery.report().value();
            assert_eq!(delivery.report().exit_code(), 1);
            assert_eq!(receipt["meta"]["completed"], false);
            assert_eq!(receipt["meta"]["resourceId"], ITEM);
            assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
            assert_eq!(receipt["meta"]["submission"]["resource"]["id"], ITEM);
            assert_eq!(receipt["meta"]["folderId"], Value::Null);
            assert_eq!(receipt["meta"]["folderIdObserved"], true);
            assert_eq!(receipt["meta"]["submission"]["folderId"], Value::Null);
            assert_eq!(receipt["meta"]["submission"]["folderIdObserved"], true);
            assert_eq!(
                receipt["meta"]["operationHandle"],
                format!("/operations/{OP}")
            );
            assert_eq!(receipt["meta"]["remoteReplayed"], false);
            assert_eq!(receipt["meta"]["remoteCancelled"], false);
            assert_eq!(receipt["meta"]["anticipatedEffects"], json!([]));
            if outcome == "malformed" {
                assert_eq!(receipt["meta"]["unparsedResponseBodyBase64"], "ew==");
            }
            h.assert_calls(
                match outcome {
                    "cancelled" => 2,
                    "denied" => 3,
                    _ => 4,
                },
                1,
            );
        }
    }

    #[tokio::test]
    async fn malformed_nowait_acceptance_retains_bytes_handle_and_nonzero_status() {
        for body in ["{", "not-json"] {
            let h = Harness::new(vec![missing(), accepted(body)]);
            let delivery = h
                .execute(prepare(&[], None), &mut approve(), pending())
                .await
                .unwrap();
            let receipt = delivery.report().value();
            assert_eq!(delivery.report().exit_code(), 1);
            assert_eq!(receipt["meta"]["status"], "unknown");
            assert_eq!(receipt["meta"]["completed"], false);
            assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
            assert_eq!(
                receipt["meta"]["operationHandle"],
                format!("/operations/{OP}")
            );
            assert!(receipt["meta"]["resourceId"].is_null());
            assert!(receipt["meta"]["unparsedAcceptanceBodyBase64"].is_string());
            assert_eq!(receipt["meta"]["remoteReplayed"], false);
            h.assert_calls(2, 1);
        }
    }

    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("closed pipe"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn projection_no_clobber_and_broken_output_preserve_effect_and_handle_evidence() {
        let h = Harness::new(vec![missing(), accepted("")]);
        let delivery = h
            .execute(
                prepare(&["--project", "missing"], None),
                &mut approve(),
                pending(),
            )
            .await
            .unwrap();
        let mut bytes = Vec::new();
        delivery.deliver(&mut bytes).unwrap();
        let receipt: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            receipt["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(receipt["meta"]["anticipatedEffects"], json!([]));
        let h = Harness::new(vec![missing(), accepted("")]);
        let delivery = h
            .execute(prepare(&[], None), &mut approve(), pending())
            .await
            .unwrap();
        let Err(CreateCommandError::Delivery { report, .. }) = delivery.deliver(&mut BrokenWriter)
        else {
            panic!("must retain unprojected remote evidence")
        };
        assert_eq!(
            report.value()["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        h.assert_calls(2, 1);

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("receipt.json");
        let command =
            v2_create::parse_args(["create", TARGET, "--output", path.to_str().unwrap()]).unwrap();
        let prepared = PreparedCreateCommand::preflight(
            command,
            &EmptySparkJobDefinition,
            temp.path(),
            &mut ForbiddenRead,
            false,
        )
        .unwrap();
        assert!(!path.exists());
        let h = Harness::new(vec![missing(), accepted("")]);
        let delivery = h
            .execute(prepared, &mut approve(), pending())
            .await
            .unwrap();
        std::fs::write(&path, b"raced local file").unwrap();
        let Err(CreateCommandError::Delivery { report, .. }) = delivery.deliver(&mut Vec::new())
        else {
            panic!("raced output must not overwrite")
        };
        assert_eq!(std::fs::read(&path).unwrap(), b"raced local file");
        assert_eq!(
            report.value()["meta"]["operationHandle"],
            format!("/operations/{OP}")
        );
        h.assert_calls(2, 1);
    }
    use futures_util::StreamExt;
}

#[cfg(feature = "headless")]
mod binary {
    #![deny(unnameable_test_items)]
    use super::*;
    use serde_json::Value;
    use std::{
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    fn command() -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
        command
            .env_remove("FAB_RUST_AUTH_MODE")
            .stdin(Stdio::null());
        command
    }
    #[test]
    fn actual_binary_help_and_approval_boundary_require_no_live_identity() {
        let help = command().args(["create", "--help"]).output().unwrap();
        assert!(help.status.success());
        let help = String::from_utf8(help.stdout).unwrap();
        assert!(help.contains("Selected native owner: SparkJobDefinition."));
        assert!(help.contains("Enabled policies: error and skip."));
        assert!(!help.contains("Selected native owner: Notebook"));
        let output = command().args(["create", TARGET]).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "ApprovalRequired");
        let root = command().arg("--help").output().unwrap();
        assert!(
            String::from_utf8(root.stdout)
                .unwrap()
                .contains("not a configured runnable job")
        );
    }

    #[test]
    fn actual_binary_rejects_invalid_input_and_policy_before_approval_or_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("invalid.json");
        std::fs::write(&input, br#"{"definition":{}}"#).unwrap();
        for extra in [
            vec!["--input", input.to_str().unwrap(), "--if-exists", "skip"],
            vec!["--if-exists", "overwrite", "--input", "does-not-exist.json"],
            vec!["--force", "--input", "does-not-exist.json"],
        ] {
            let output = command()
                .args(["create", TARGET])
                .args(extra)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2));
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["error"]["code"], "CreatePolicyRejected");
            assert_eq!(error["evidence"]["mutationMayHaveOccurred"], false);
        }
        for path in [
            "/bad.Workspace/Exact.SparkJobDefinition",
            "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Notebook",
        ] {
            let output = command().args(["create", path]).output().unwrap();
            assert_eq!(output.status.code(), Some(2));
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["error"]["code"], "InvalidInput");
        }
    }

    #[test]
    fn actual_binary_blocked_stdin_obeys_original_preflight_deadline() {
        let mut child = command()
            .args(["create", TARGET, "--input", "-", "--timeout", "100ms"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("owned input child exceeded preflight deadline");
            }
            thread::sleep(Duration::from_millis(10));
        }
        drop(input);
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "TimedOut");
    }

    #[test]
    fn actual_binary_closed_reader_before_spawn_fails_without_hanging_or_authenticating() {
        let (reader, writer) = io::pipe().unwrap();
        drop(reader);
        let mut command = command();
        command
            .args(["create", "--help"])
            .stdout(writer)
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        drop(command);
        let stderr = child.stderr.take().unwrap();
        let capture = thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.take(64 * 1024).read_to_end(&mut bytes).unwrap();
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("owned help child exceeded deadline");
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(!status.success());
        let error: Value = serde_json::from_slice(&capture.join().unwrap()).unwrap();
        assert_eq!(error["error"]["code"], "OutputFailed");
    }
}
