// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline typed Notebook create contract. No network or ambient credentials.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk::{
    Audience, CredentialSelection, TokenCredential,
    items::create::{
        ConflictPolicy, CreateAction, CreateError, CreateInputValidator, CreatePlan, CreateSupport,
        CreateTarget, DuplicateSupport, InputDisposition, LeafState, NativeId, Preflight,
        ReplacementUnit, ResourceIdentity, SchemaRef, SkipSupport, TargetScope, plan_create,
    },
    notebook_create::{NotebookCreateValidator, SCHEMA, capabilities, operation},
};
use fabric_sdk_items::links::parse_link_json;
use fabric_sdk_operations::{
    FabricOperation, OperationError, OperationErrorKind, OperationExecutionError,
    OperationExecutor, OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, ready},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const ITEM: &str = "22222222-3333-4444-5555-666666666666";
const OPERATION: &str = "33333333-4444-5555-6666-777777777777";
const NAME: &str = "Exact notebook \u{e9} \u{1f4d3}";

fn target() -> CreateTarget {
    CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WORKSPACE).unwrap(),
        NAME,
        "Notebook",
    )
    .unwrap()
}

fn existing() -> Preflight {
    Preflight::Leaf(LeafState::Existing(
        ResourceIdentity::new(
            NativeId::new(ITEM).unwrap(),
            NativeId::new(WORKSPACE).unwrap(),
            "Notebook",
        )
        .unwrap(),
    ))
}

fn plan(policy: ConflictPolicy, supplied: Option<Value>) -> Result<CreatePlan, CreateError> {
    plan_create(
        policy,
        target(),
        Preflight::Leaf(LeafState::Missing),
        &capabilities(),
        supplied,
        &NotebookCreateValidator,
    )
}

fn part(path: &str) -> Value {
    json!({
        "path": path,
        "payload": STANDARD.encode(b"exact content\r\n"),
        "payloadType": "InlineBase64",
    })
}

fn body() -> Value {
    json!({
        "displayName": NAME,
        "description": "Exact metadata \u{e9}",
        "definition": {
            "parts": [
                part("notebook-content.py"),
                {
                    "path": ".platform",
                    "payload": STANDARD.encode(br#"{"metadata":{"displayName":"internal unchanged","logicalId":"retained"},"config":{"version":"2.0"}}"#),
                    "payloadType": "InlineBase64",
                },
                part("settings/runtime.json"),
            ],
        },
    })
}

#[test]
fn capabilities_advertise_only_native_rejection_and_exact_existing_skip() {
    let advertised = capabilities();
    assert_eq!(advertised.schema.as_str(), SCHEMA);
    assert_eq!(advertised.create, CreateSupport::RejectExisting);
    assert_eq!(advertised.skip, SkipSupport::ExactExistingUnchanged);
    assert_eq!(advertised.replacement, None);
    assert_eq!(advertised.duplicate, DuplicateSupport::Unsupported);
}

#[test]
fn planner_handles_supported_policies_without_implying_a_backend_guarantee() {
    for policy in [ConflictPolicy::Error, ConflictPolicy::Skip] {
        let planned = plan(policy, None).unwrap();
        assert_eq!(planned.policy(), policy);
        assert!(matches!(planned.action(), CreateAction::Create { .. }));
    }
    assert_eq!(
        plan_create(
            ConflictPolicy::Error,
            target(),
            existing(),
            &capabilities(),
            None,
            &NotebookCreateValidator,
        ),
        Err(CreateError::AlreadyExists)
    );
    let skipped = plan_create(
        ConflictPolicy::Skip,
        target(),
        existing(),
        &capabilities(),
        Some(body()),
        &NotebookCreateValidator,
    )
    .unwrap();
    assert!(matches!(skipped.action(), CreateAction::Skip { .. }));
    assert!(matches!(
        operation(&skipped, None, false),
        Err(CreateError::InputRejected)
    ));
    assert!(matches!(
        operation(&skipped, Some(b"{}"), true),
        Err(CreateError::InputRejected)
    ));
    for policy in [ConflictPolicy::Overwrite, ConflictPolicy::Duplicate] {
        for observed in [Preflight::Leaf(LeafState::Missing), existing()] {
            assert_eq!(
                plan_create(
                    policy,
                    target(),
                    observed,
                    &capabilities(),
                    Some(body()),
                    &NotebookCreateValidator,
                ),
                Err(CreateError::UnsupportedPolicy)
            );
        }
    }
    for (observed, expected) in [
        (Preflight::MissingParent, CreateError::MissingParent),
        (
            Preflight::Leaf(LeafState::Ambiguous),
            CreateError::AmbiguousLeaf,
        ),
        (
            Preflight::Leaf(LeafState::WrongKind),
            CreateError::WrongKind,
        ),
    ] {
        assert_eq!(
            plan_create(
                ConflictPolicy::Skip,
                target(),
                observed,
                &capabilities(),
                None,
                &NotebookCreateValidator,
            ),
            Err(expected)
        );
    }
}

#[test]
fn validator_accepts_only_the_exact_schema_kind_scope_and_guid_parent() {
    let validator = NotebookCreateValidator;
    let schema = capabilities().schema;
    assert_eq!(
        validator.validate(&target(), &schema, None),
        Ok(InputDisposition::CreateOnly)
    );
    assert_eq!(
        validator.validate(&target(), &SchemaRef::new("other/v1").unwrap(), None),
        Err(CreateError::InputRejected)
    );
    for (scope, parent, kind) in [
        (TargetScope::ParentCollection, WORKSPACE, "Notebook"),
        (TargetScope::NamedLeaf, WORKSPACE, "notebook"),
        (TargetScope::NamedLeaf, WORKSPACE, "Report"),
        (TargetScope::NamedLeaf, "opaque-native-id", "Notebook"),
        (TargetScope::NamedLeaf, "../workspace", "Notebook"),
        (
            TargetScope::NamedLeaf,
            "11111111-2222-3333-4444-55555555555z",
            "Notebook",
        ),
    ] {
        let target = CreateTarget::new(scope, NativeId::new(parent).unwrap(), NAME, kind).unwrap();
        assert_eq!(
            validator.validate(&target, &schema, None),
            Err(CreateError::InputRejected)
        );
    }
}

#[test]
fn definitions_accept_both_explicit_formats_and_the_omitted_service_default() {
    for format in [None, Some("ipynb"), Some("fabricGitSource")] {
        let mut supplied = body();
        if let Some(format) = format {
            supplied["definition"]["format"] = json!(format);
        }
        let planned = plan(ConflictPolicy::Error, Some(supplied.clone())).unwrap();
        let raw = serde_json::to_vec_pretty(&supplied).unwrap();
        assert!(operation(&planned, Some(&raw), false).is_ok());
        let CreateAction::Create { input } = planned.action() else {
            panic!("expected create");
        };
        assert_eq!(input.supplied(), Some(&supplied));
        assert_eq!(input.disposition(), &InputDisposition::CreateOnly);
    }
    for description in ["".into(), "\u{1f4d3}".repeat(256)] {
        let supplied = json!({"displayName": NAME, "description": description});
        assert!(plan(ConflictPolicy::Error, Some(supplied)).is_ok());
    }
}

#[test]
fn invalid_input_is_rejected_even_when_an_existing_leaf_would_be_skipped() {
    let mut invalid = vec![
        Value::Null,
        json!([]),
        json!([NAME]),
        json!([NAME, "description", {"parts": [part("content.py")]}]),
        json!("Notebook"),
        json!(false),
        json!({}),
        json!({"displayName": null}),
        json!({"displayName": 42}),
        json!({"displayName": "different"}),
        json!({"displayName": NAME, "description": null}),
        json!({"displayName": NAME, "description": []}),
        json!({"displayName": NAME, "description": "\u{1f4d3}".repeat(257)}),
        json!({"displayName": NAME, "definition": null}),
        json!({"displayName": NAME, "definition": []}),
        json!({"displayName": NAME, "definition": ["fabricGitSource", [part("content.py")]]}),
        json!({"displayName": NAME, "definition": {}}),
        json!({"displayName": NAME, "definition": {"parts": null}}),
        json!({"displayName": NAME, "definition": {"parts": {}}}),
        json!({"displayName": NAME, "definition": {"parts": []}}),
        json!({"displayName": NAME, "definition": {"parts": [null]}}),
        json!({"displayName": NAME, "definition": {"parts": [{}]}}),
        json!({"displayName": NAME, "definition": {"parts": [["content.py", "YQ==", "InlineBase64"]]}}),
    ];
    for field in [
        "type",
        "id",
        "workspaceId",
        "owner",
        "name",
        "folderId",
        "sensitivityLabelSettings",
        "future",
    ] {
        let mut value = json!({"displayName": NAME});
        value[field] = json!("must not substitute or silently drop");
        invalid.push(value);
    }
    for format in [
        Value::Null,
        json!(false),
        json!("IPYNB"),
        json!("other"),
        json!({"ipynb": null}),
        json!({"fabricGitSource": null}),
    ] {
        let mut value = body();
        value["definition"]["format"] = format;
        invalid.push(value);
    }
    let mut value = body();
    value["definition"]["future"] = json!(true);
    invalid.push(value);
    for field in ["path", "payload", "payloadType"] {
        for replacement in [Value::Null, json!(1), json!([])] {
            let mut value = body();
            value["definition"]["parts"][0][field] = replacement;
            invalid.push(value);
        }
        let mut value = body();
        value["definition"]["parts"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
        invalid.push(value);
    }
    for (field, replacement) in [
        ("future", json!(true)),
        ("payloadType", json!("Url")),
        ("payloadType", json!("inlineBase64")),
        ("payloadType", json!({"InlineBase64": null})),
        ("payload", json!("not-base64!")),
        ("payload", json!("YQ")),
        ("payload", json!("YR==")),
        ("payload", json!("YQ==\n")),
    ] {
        let mut value = body();
        value["definition"]["parts"][0][field] = replacement;
        invalid.push(value);
    }
    for path in [
        "",
        "/absolute.py",
        "C:/absolute.py",
        "notebook\\content.py",
        "../content.py",
        "folder/../content.py",
        "./content.py",
        "folder//content.py",
        "folder/",
        "content\0.py",
        "content\n.py",
    ] {
        let mut value = body();
        value["definition"]["parts"][0]["path"] = json!(path);
        invalid.push(value);
    }
    let mut duplicate_path = body();
    duplicate_path["definition"]["parts"][1] = duplicate_path["definition"]["parts"][0].clone();
    invalid.push(duplicate_path);
    for supplied in invalid {
        for (policy, observed) in [
            (ConflictPolicy::Error, Preflight::Leaf(LeafState::Missing)),
            (ConflictPolicy::Skip, existing()),
        ] {
            assert_eq!(
                plan_create(
                    policy,
                    target(),
                    observed,
                    &capabilities(),
                    Some(supplied.clone()),
                    &NotebookCreateValidator,
                ),
                Err(CreateError::InputRejected),
                "unexpected acceptance: {supplied}"
            );
        }
    }
}

#[test]
fn raw_body_must_be_present_exactly_when_explicit_input_was_planned() {
    let supplied = body();
    let planned = plan(ConflictPolicy::Error, Some(supplied.clone())).unwrap();
    assert!(matches!(
        operation(&planned, None, false),
        Err(CreateError::InputRejected)
    ));
    let mut changed = supplied;
    changed["description"] = json!("changed metadata");
    let changed_raw = serde_json::to_vec(&changed).unwrap();
    assert!(matches!(
        operation(&planned, Some(&changed_raw), false),
        Err(CreateError::InputRejected)
    ));
    changed["description"] = json!("Exact metadata \u{e9}");
    changed["definition"]["parts"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    let reordered_raw = serde_json::to_vec(&changed).unwrap();
    assert!(matches!(
        operation(&planned, Some(&reordered_raw), false),
        Err(CreateError::InputRejected)
    ));
    let absent = plan(ConflictPolicy::Error, None).unwrap();
    assert!(matches!(
        operation(&absent, Some(b"{}"), false),
        Err(CreateError::InputRejected)
    ));
}

#[test]
fn raw_bytes_reject_malformed_json_and_duplicate_decoded_keys_at_every_schema_depth() {
    let valid = json!({"displayName": NAME});
    let planned = plan(ConflictPolicy::Error, Some(valid)).unwrap();
    for raw in [
        b"".as_slice(),
        b"{",
        b"null",
        b"[]",
        b"\xff",
        b"{}{}",
        b"{\"displayName\":\"unrelated\"}",
    ] {
        assert!(matches!(
            operation(&planned, Some(raw), false),
            Err(CreateError::InputRejected)
        ));
    }
    for raw in [
        r#"{"displayName":"Exact notebook \u00e9 \ud83d\udcd3","displayName":"Exact notebook \u00e9 \ud83d\udcd3"}"#,
        r#"{"displayName":"Exact notebook \u00e9 \ud83d\udcd3","display\u004eame":"Exact notebook \u00e9 \ud83d\udcd3"}"#,
        r#"{"displayName":"Exact notebook \u00e9 \ud83d\udcd3","description":"first","description":"last"}"#,
        r#"{"displayName":"Exact notebook \u00e9 \ud83d\udcd3","definition":{"format":"ipynb","format":"fabricGitSource","parts":[{"path":"content.py","payload":"YQ==","payloadType":"InlineBase64"}]}}"#,
        r#"{"displayName":"Exact notebook \u00e9 \ud83d\udcd3","definition":{"parts":[{"path":"ignored.py","path":"content.py","payload":"YQ==","payloadType":"InlineBase64"}]}}"#,
        r#"{"displayName":"Exact notebook \u00e9 \ud83d\udcd3","definition":{"parts":[{"path":"content.py","payload":"Yg==","payload":"YQ==","payloadType":"InlineBase64"}]}}"#,
        r#"{"displayName":"Exact notebook \u00e9 \ud83d\udcd3","definition":{"parts":[{"path":"content.py","payload":"YQ==","payloadType":"InlineBase64","payloadType":"InlineBase64"}]}}"#,
    ] {
        // Even a caller that used serde_json's last-key-wins parser cannot bypass this check.
        let supplied: Value = serde_json::from_str(raw).unwrap();
        let planned = plan(ConflictPolicy::Error, Some(supplied)).unwrap();
        assert!(parse_link_json(raw).is_err());
        assert!(matches!(
            operation(&planned, Some(raw.as_bytes()), false),
            Err(CreateError::InputRejected)
        ));
    }
}

struct UntrustedValidator;

impl CreateInputValidator for UntrustedValidator {
    fn validate(
        &self,
        _: &CreateTarget,
        _: &SchemaRef,
        _: Option<&Value>,
    ) -> Result<InputDisposition, CreateError> {
        Ok(InputDisposition::CreateOnly)
    }
}

#[test]
fn operation_revalidates_instead_of_trusting_an_arbitrary_planner_validator() {
    let mut wrong_schema = capabilities();
    wrong_schema.schema = SchemaRef::new("other/v1").unwrap();
    for (target, advertised, supplied) in [
        (target(), wrong_schema, None),
        (
            CreateTarget::new(
                TargetScope::NamedLeaf,
                NativeId::new(WORKSPACE).unwrap(),
                NAME,
                "Report",
            )
            .unwrap(),
            capabilities(),
            None,
        ),
        (
            CreateTarget::new(
                TargetScope::NamedLeaf,
                NativeId::new("not-a-guid").unwrap(),
                NAME,
                "Notebook",
            )
            .unwrap(),
            capabilities(),
            None,
        ),
        (
            target(),
            capabilities(),
            Some(json!({"displayName": "wrong"})),
        ),
        (
            target(),
            capabilities(),
            Some(json!({"displayName": NAME, "type": "Notebook"})),
        ),
        (target(), capabilities(), Some(Value::Null)),
    ] {
        let raw = supplied
            .as_ref()
            .map(|value| serde_json::to_vec(value).unwrap());
        let planned = plan_create(
            ConflictPolicy::Error,
            target,
            Preflight::Leaf(LeafState::Missing),
            &advertised,
            supplied,
            &UntrustedValidator,
        )
        .unwrap();
        assert!(matches!(
            operation(&planned, raw.as_deref(), false),
            Err(CreateError::InputRejected)
        ));
    }
    let mut dishonest = capabilities();
    dishonest.replacement = Some(ReplacementUnit::new("pretend/v1").unwrap());
    let planned = plan_create(
        ConflictPolicy::Overwrite,
        target(),
        Preflight::Leaf(LeafState::Missing),
        &dishonest,
        None,
        &UntrustedValidator,
    )
    .unwrap();
    assert!(matches!(
        operation(&planned, None, false),
        Err(CreateError::UnsupportedPolicy)
    ));
}

#[derive(Debug)]
struct Credential;

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        Ok(AccessToken::new(
            "synthetic-notebook-create",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Call {
    method: ResourceMethod,
    url: String,
    body: Vec<u8>,
}

#[derive(Clone, Default)]
struct Executor {
    replies: Arc<Mutex<VecDeque<OperationResponse>>>,
    calls: Arc<Mutex<Vec<Call>>>,
    waits: Arc<Mutex<Vec<Duration>>>,
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.calls.lock().unwrap().push(Call {
            method: request.method(),
            url: request.url().to_string(),
            body: request.body().to_vec(),
        });
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected mutation replay or extra poll");
        Box::pin(ready(Ok(reply)))
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.waits.lock().unwrap().push(delay);
        Box::pin(ready(Ok(())))
    }
}

fn response(status: u16, body: &str) -> OperationResponse {
    OperationResponse {
        status,
        body: body.as_bytes().to_vec(),
        headers: BTreeMap::new(),
    }
}

fn accepted() -> OperationResponse {
    OperationResponse {
        headers: BTreeMap::from([
            (
                "Location".into(),
                format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}"),
            ),
            ("x-ms-operation-id".into(), OPERATION.into()),
            ("Retry-After".into(), "1".into()),
        ]),
        ..response(202, "")
    }
}

async fn run(
    operation: FabricOperation,
    replies: Vec<OperationResponse>,
) -> (Result<OperationResponse, OperationError>, Executor) {
    let executor = Executor::default();
    executor.replies.lock().unwrap().extend(replies);
    let selection =
        CredentialSelection::new("offline", "synthetic-tenant", Audience::Fabric).unwrap();
    let result = OperationSession::new(
        &Credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    )
    .execute(operation)
    .await;
    (result, executor)
}

#[tokio::test]
async fn explicit_json_preserves_raw_bytes_metadata_part_order_and_payloads() {
    let raw = br#" {
 "description" : "Exact metadata \u00e9",
 "definition": {
   "parts": [
     {"payloadType":"InlineBase64","payload":"cHJpbnQoJ2V4YWN0JykNCg==","path":"notebook-content.py"},
     {"path":".platform", "payload":"eyJtZXRhZGF0YSI6eyJkaXNwbGF5TmFtZSI6ImludGVybmFsIHVuY2hhbmdlZCJ9fQ==", "payloadType":"InlineBase64"},
     {"path":"settings/runtime.json","payload":"eyJsYWtlaG91c2UiOiJ1bmNoYW5nZWQifQ==","payloadType":"InlineBase64"}
   ]
 },
 "displayName":"Exact notebook \u00e9 \ud83d\udcd3"
}
 "#;
    let supplied = parse_link_json(std::str::from_utf8(raw).unwrap()).unwrap();
    let planned = plan(ConflictPolicy::Error, Some(supplied)).unwrap();
    let (result, executor) = run(
        operation(&planned, Some(raw), false).unwrap(),
        vec![response(201, r#"{"id":"created"}"#)],
    )
    .await;
    assert_eq!(result.unwrap().status, 201);
    assert_eq!(
        *executor.calls.lock().unwrap(),
        [Call {
            method: ResourceMethod::Post,
            url: format!("https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/notebooks"),
            body: raw.to_vec(),
        }]
    );
    assert!(executor.waits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn absent_input_serializes_only_the_exact_destination_display_name() {
    for name in [NAME, " leading and trailing ", ITEM] {
        let target = CreateTarget::new(
            TargetScope::NamedLeaf,
            NativeId::new(WORKSPACE.to_uppercase()).unwrap(),
            name,
            "Notebook",
        )
        .unwrap();
        let planned = plan_create(
            ConflictPolicy::Skip,
            target,
            Preflight::Leaf(LeafState::Missing),
            &capabilities(),
            None,
            &NotebookCreateValidator,
        )
        .unwrap();
        let (result, executor) = run(
            operation(&planned, None, false).unwrap(),
            vec![response(201, r#"{"id":"created"}"#)],
        )
        .await;
        assert_eq!(result.unwrap().status, 201);
        let calls = executor.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            serde_json::from_slice::<Value>(&calls[0].body).unwrap(),
            json!({"displayName": name})
        );
        assert_eq!(
            calls[0].url,
            format!("https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/notebooks")
        );
    }
}

#[tokio::test]
async fn default_returns_actual_acceptance_without_polling_or_claiming_completion() {
    let planned = plan(ConflictPolicy::Error, None).unwrap();
    let (result, executor) = run(operation(&planned, None, false).unwrap(), vec![accepted()]).await;
    let returned = result.unwrap();
    assert_eq!(returned.status, 202);
    assert_eq!(returned.headers["x-ms-operation-id"], OPERATION);
    assert_eq!(executor.calls.lock().unwrap().len(), 1);
    assert!(executor.waits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn wait_uses_shared_operation_polling_and_fetches_the_result_without_reposting() {
    let planned = plan(ConflictPolicy::Error, None).unwrap();
    let (result, executor) = run(
        operation(&planned, None, true).unwrap(),
        vec![
            accepted(),
            response(200, r#"{"status":"Succeeded"}"#),
            response(200, r#"{"id":"created","type":"Notebook"}"#),
        ],
    )
    .await;
    assert_eq!(
        result.unwrap().body,
        br#"{"id":"created","type":"Notebook"}"#
    );
    let calls = executor.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].method, ResourceMethod::Post);
    for (call, suffix) in calls[1..].iter().zip(["", "/result"]) {
        assert_eq!(call.method, ResourceMethod::Get);
        assert_eq!(
            call.url,
            format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}{suffix}")
        );
        assert!(call.body.is_empty());
    }
    assert_eq!(*executor.waits.lock().unwrap(), [Duration::from_secs(1)]);
}

#[tokio::test]
async fn documented_native_collision_is_a_service_failure_not_upsert_or_success() {
    let planned = plan(ConflictPolicy::Error, None).unwrap();
    let (result, executor) = run(
        operation(&planned, None, false).unwrap(),
        vec![response(
            409,
            r#"{"errorCode":"ItemDisplayNameAlreadyInUse","message":"synthetic conflict"}"#,
        )],
    )
    .await;
    assert!(matches!(
        result.unwrap_err().kind,
        OperationErrorKind::Http { status: 409, code: Some(code), .. }
            if code == "ItemDisplayNameAlreadyInUse"
    ));
    assert_eq!(executor.calls.lock().unwrap().len(), 1);
}
