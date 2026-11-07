// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline create-only copy through the real shared operation session.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_core::{Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin};
use fabric_deploy::definitions::{
    CopyDefinitionDestination, DefinitionCopyErrorKind as Kind, DefinitionLimits, DefinitionRead,
    PreparedDefinitionCopy, prepare_definition_copy,
};
use fabric_sdk_operations::{
    FabricOperation, OperationErrorKind, OperationExecutionError, OperationExecutor,
    OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

const WORKSPACE: &str = "abcdefab-abcd-abcd-abcd-abcdefabcdef";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const NEW_ITEM: &str = "55555555-5555-5555-5555-555555555555";
const SOURCE_LOGICAL_ID: &str = "66666666-6666-6666-6666-666666666666";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";
const FOLDER: &str = "44444444-4444-4444-4444-444444444444";
const SECRET: &str = "private-definition-marker";

fn destination(item_type: &str) -> CopyDefinitionDestination<'_> {
    CopyDefinitionDestination {
        workspace_id: WORKSPACE,
        display_name: "Copy destination",
        item_type,
        folder_id: Some(FOLDER),
    }
}

fn source(item_type: &str) -> DefinitionRead {
    let (format, mut parts) = if item_type == "SemanticModel" {
        (
            "TMSL",
            vec![
                json!({
                    "path": "model.bim",
                    "payload": STANDARD.encode(
                        br#"{"compatibilityLevel":1702,"model":{"culture":"en-US","tables":[]}}"#
                    ),
                    "payloadType": "InlineBase64",
                    "unknown": {"label": SECRET}
                }),
                json!({
                    "path": "definition.pbism",
                    "payload": STANDARD.encode(br#"{"version":"5.0","settings":{}}"#),
                    "payloadType": "InlineBase64"
                }),
            ],
        )
    } else {
        (
            "ipynb",
            vec![json!({
                "path": "notebook-content.ipynb",
                "payload": STANDARD.encode(
                    br#"{"nbformat":4,"nbformat_minor":5,"cells":[{"cell_type":"code","outputs":[{"text":"keep"}]}]}"#
                ),
                "payloadType": "InlineBase64",
                "unknown": {"label": SECRET}
            })],
        )
    };
    parts.push(json!({
        "path": ".platform",
        "payload": STANDARD.encode(serde_json::to_vec(&json!({
            "version": "2.0",
            "config": {"logicalId": SOURCE_LOGICAL_ID},
            "metadata": {
                "type": item_type,
                "displayName": "Conflicting source platform name",
                "description": "Original platform description"
            }
        })).unwrap()),
        "payloadType": "InlineBase64"
    }));
    DefinitionRead {
        item: json!({
            "id": ITEM,
            "logicalId": SOURCE_LOGICAL_ID,
            "workspaceId": WORKSPACE,
            "type": item_type,
            "description": "Original description \u{2603}",
            "sensitivityLabel": {"id": "not-propagated"},
            "definition": {
                "format": format,
                "parts": parts,
                "unknownDefinitionField": {"label": SECRET}
            }
        }),
        definition_access_denied: None,
    }
}

fn source_without_platform(item_type: &str) -> DefinitionRead {
    let mut source = source(item_type);
    let platform = source.item["definition"]["parts"]
        .as_array_mut()
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(platform["path"], ".platform");
    source
}

fn prepared(item_type: &str) -> PreparedDefinitionCopy {
    prepare_definition_copy(
        &source(item_type),
        item_type,
        destination(item_type),
        DefinitionLimits::default(),
    )
    .unwrap()
}

fn response(status: u16, body: Value) -> OperationResponse {
    OperationResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

#[derive(Debug, Default)]
struct Credential(AtomicUsize);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "offline-copy-only-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct CapturedRequest {
    method: ResourceMethod,
    host: String,
    path: String,
    query: Option<String>,
    body: Vec<u8>,
}

struct Harness {
    credential: Arc<Credential>,
    context: RequestContext,
    replies: Mutex<VecDeque<Result<OperationResponse, OperationExecutionError>>>,
    requests: Mutex<Vec<CapturedRequest>>,
    waits: AtomicUsize,
    cancelled: AtomicBool,
    stall: bool,
}

impl Harness {
    fn new(
        replies: impl IntoIterator<Item = Result<OperationResponse, OperationExecutionError>>,
    ) -> Self {
        Self::with_host("api.fabric.microsoft.com", replies)
    }

    fn with_host(
        host: &str,
        replies: impl IntoIterator<Item = Result<OperationResponse, OperationExecutionError>>,
    ) -> Self {
        let credential = Arc::new(Credential::default());
        Self {
            context: RequestContext::with_endpoint(
                credential.clone(),
                CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
                EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap(),
            ),
            credential,
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::default(),
            waits: AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
            stall: false,
        }
    }

    fn session<'a>(
        &'a self,
        cancelled: impl Future<Output = ()> + Send + 'a,
    ) -> OperationSession<'a, &'a Self> {
        OperationSession::with_context(&self.context, self, OperationLimits::default(), cancelled)
    }

    fn assert_no_io(&self) {
        assert!(self.requests.lock().unwrap().is_empty());
        assert_eq!(self.credential.0.load(Ordering::SeqCst), 0);
        assert_eq!(self.waits.load(Ordering::SeqCst), 0);
    }
}

impl OperationExecutor for &Harness {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.requests.lock().unwrap().push(CapturedRequest {
                method: request.method(),
                host: request.url().host_str().unwrap().to_owned(),
                path: request.url().path().to_owned(),
                query: request.url().query().map(str::to_owned),
                body: request.body().to_vec(),
            });
            if self.stall {
                self.cancelled.store(true, Ordering::SeqCst);
                poll_fn(|cx| {
                    cx.waker().wake_by_ref();
                    Poll::<()>::Pending
                })
                .await;
            }
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request, replay or fallback")
        })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.waits.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { panic!("create-only copy must never poll or wait") })
    }
}

fn assert_rejected(
    source: &DefinitionRead,
    source_type: &str,
    destination: CopyDefinitionDestination<'_>,
    limits: DefinitionLimits,
    expected: Kind,
) {
    let harness = Harness::new([]);
    let error = prepare_definition_copy(source, source_type, destination, limits).unwrap_err();
    assert_eq!(error.kind, expected);
    assert!(error.response.is_none());
    assert!(error.operation.is_none());
    harness.assert_no_io();
}

#[tokio::test]
async fn same_workspace_create_omits_platform_preserves_artifacts_and_notebook_outputs() {
    let original = source("Notebook");
    let snapshot = original.item.clone();
    let mut expected = original.item["definition"].clone();
    let platform = expected["parts"].as_array_mut().unwrap().pop().unwrap();
    assert_eq!(platform["path"], ".platform");
    let harness = Harness::new([Ok(response(
        201,
        json!({
            "id": NEW_ITEM, "type": "Notebook", "workspaceId": WORKSPACE,
            "displayName": "Copy destination", "folderId": FOLDER
        }),
    ))]);
    let receipt = prepare_definition_copy(
        &original,
        "Notebook",
        destination("Notebook"),
        DefinitionLimits::default(),
    )
    .unwrap()
    .execute(&mut harness.session(pending()))
    .await
    .unwrap();
    assert_eq!(receipt.response.status, 201);
    assert_eq!(receipt.item_id.as_deref(), Some(NEW_ITEM));
    assert_ne!(receipt.item_id.as_deref(), original.item["id"].as_str());
    assert!(receipt.platform_omitted);
    assert!(receipt.operation_id.is_none());
    assert!(receipt.warnings.is_empty());
    let requests = harness.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, ResourceMethod::Post);
    assert_eq!(requests[0].host, "api.fabric.microsoft.com");
    assert_eq!(
        requests[0].path,
        format!("/v1/workspaces/{WORKSPACE}/items")
    );
    assert_eq!(requests[0].query, None);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["definition"], expected);
    assert!(
        body["definition"]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|part| part["path"] != ".platform")
    );
    assert_eq!(body["type"], "Notebook");
    assert_eq!(body["displayName"], "Copy destination");
    assert_eq!(body["folderId"], FOLDER);
    assert_eq!(body["description"], original.item["description"]);
    assert!(body.get("sensitivityLabel").is_none());
    assert!(body.get("id").is_none());
    assert!(body.get("logicalId").is_none());
    assert_eq!(original.item, snapshot);
    assert_eq!(harness.waits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn preparation_bounds_the_final_ascii_request_without_changing_the_source() {
    let original = source_without_platform("Notebook");
    let snapshot = original.item.clone();
    let harness = Harness::new([Ok(response(201, json!({"id": ITEM})))]);
    prepare_definition_copy(
        &original,
        "Notebook",
        destination("Notebook"),
        DefinitionLimits::default(),
    )
    .unwrap()
    .execute(&mut harness.session(pending()))
    .await
    .unwrap();
    let length = harness.requests.lock().unwrap()[0].body.len();
    let exact = DefinitionLimits {
        max_bytes: NonZeroUsize::new(length).unwrap(),
        ..DefinitionLimits::default()
    };
    prepare_definition_copy(&original, "Notebook", destination("Notebook"), exact).unwrap();
    assert_rejected(
        &original,
        "Notebook",
        destination("Notebook"),
        DefinitionLimits {
            max_bytes: NonZeroUsize::new(length - 1).unwrap(),
            ..DefinitionLimits::default()
        },
        Kind::Limit,
    );
    assert_eq!(original.item, snapshot);
}

#[tokio::test]
async fn no_platform_input_keeps_all_parts_and_reports_false_without_changing_source() {
    for item_type in ["Notebook", "SemanticModel"] {
        let original = source_without_platform(item_type);
        let snapshot = original.item.clone();
        let harness = Harness::new([Ok(response(
            201,
            json!({"id": NEW_ITEM, "type": item_type, "workspaceId": WORKSPACE}),
        ))]);
        let receipt = prepare_definition_copy(
            &original,
            item_type,
            destination(item_type),
            DefinitionLimits::default(),
        )
        .unwrap()
        .execute(&mut harness.session(pending()))
        .await
        .unwrap();
        assert!(!receipt.platform_omitted);
        assert_eq!(receipt.item_id.as_deref(), Some(NEW_ITEM));
        let requests = harness.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["definition"], original.item["definition"]);
        assert_eq!(body["displayName"], "Copy destination");
        assert_eq!(body["type"], item_type);
        assert_eq!(original.item, snapshot);
    }
}

#[tokio::test]
async fn platform_omission_matches_only_the_exact_root_metadata_part() {
    let mut original = source("Notebook");
    let parts = original.item["definition"]["parts"].as_array_mut().unwrap();
    let platform = parts.pop().unwrap();
    for path in ["metadata/.platform", ".platform.backup"] {
        let mut artifact = platform.clone();
        artifact["path"] = json!(path);
        parts.push(artifact);
    }
    let expected_parts = parts.clone();
    parts.insert(0, platform);
    let snapshot = original.item.clone();
    let harness = Harness::new([Ok(response(201, json!({"id": NEW_ITEM})))]);
    let receipt = prepare_definition_copy(
        &original,
        "Notebook",
        destination("Notebook"),
        DefinitionLimits::default(),
    )
    .unwrap()
    .execute(&mut harness.session(pending()))
    .await
    .unwrap();
    assert!(receipt.platform_omitted);
    let requests = harness.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["definition"]["parts"], json!(expected_parts));
    assert_eq!(
        body["definition"]["unknownDefinitionField"],
        original.item["definition"]["unknownDefinitionField"]
    );
    assert_eq!(
        body["definition"]["format"],
        original.item["definition"]["format"]
    );
    assert_eq!(original.item, snapshot);
}

#[test]
fn platform_input_is_validated_and_bounded_before_omission() {
    let mut original = source("Notebook");
    original.item["definition"]["parts"][1]["payload"] = json!({"invalid": "shape"});
    assert_rejected(
        &original,
        "Notebook",
        destination("Notebook"),
        DefinitionLimits::default(),
        Kind::InvalidDefinition,
    );
    let mut original = source("Notebook");
    original.item["definition"]["parts"][1]["payload"] = json!(STANDARD.encode([0; 2048]));
    assert_rejected(
        &original,
        "Notebook",
        destination("Notebook"),
        DefinitionLimits {
            max_bytes: NonZeroUsize::new(1024).unwrap(),
            ..DefinitionLimits::default()
        },
        Kind::Limit,
    );
    assert_rejected(
        &source("Notebook"),
        "Notebook",
        destination("Notebook"),
        DefinitionLimits {
            max_files: NonZeroUsize::new(1).unwrap(),
            ..DefinitionLimits::default()
        },
        Kind::Limit,
    );
}

#[test]
fn platform_only_definition_is_rejected_without_metadata_only_creation() {
    let mut original = source("Notebook");
    let platform = original.item["definition"]["parts"][1].clone();
    original.item["definition"]["parts"] = json!([platform]);
    let snapshot = original.item.clone();
    assert_rejected(
        &original,
        "Notebook",
        destination("Notebook"),
        DefinitionLimits::default(),
        Kind::InvalidDefinition,
    );
    assert_eq!(original.item, snapshot);
}

#[tokio::test]
async fn semantic_model_create_omits_platform_preserves_artifacts_and_null_folder_without_labels() {
    let original = source("SemanticModel");
    let snapshot = original.item.clone();
    let mut expected = original.item["definition"].clone();
    let platform = expected["parts"].as_array_mut().unwrap().pop().unwrap();
    assert_eq!(platform["path"], ".platform");
    let harness = Harness::new([Ok(response(201, json!({"id": ITEM})))]);
    let mut target = destination("SemanticModel");
    target.folder_id = None;
    let receipt = prepare_definition_copy(
        &original,
        "SemanticModel",
        target,
        DefinitionLimits::default(),
    )
    .unwrap()
    .execute(&mut harness.session(pending()))
    .await
    .unwrap();
    assert!(receipt.platform_omitted);
    let requests = harness.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["type"], "SemanticModel");
    assert_eq!(body["definition"], expected);
    assert_eq!(body["displayName"], "Copy destination");
    assert!(body["folderId"].is_null());
    assert!(body.get("sensitivityLabel").is_none());
    assert!(body.get("logicalId").is_none());
    assert_eq!(original.item, snapshot);
}

#[test]
fn preparation_rejects_denied_missing_and_malformed_definitions_before_io() {
    for status in [401, 403] {
        let mut input = source("Notebook");
        input.definition_access_denied = Some(status);
        assert_rejected(
            &input,
            "Notebook",
            destination("Notebook"),
            DefinitionLimits::default(),
            Kind::DefinitionAccessDenied(status),
        );
    }
    for definition in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"parts": {}}),
        json!({"parts": []}),
        json!({"parts": [null]}),
        json!({"parts": [{"path": "part", "payload": {}, "payloadType": "InlineBase64"}]}),
        json!({"parts": [{"path": "part", "payload": "", "payloadType": ""}]}),
        json!({"format": {}, "parts": []}),
    ] {
        let mut input = source("Notebook");
        input.item["definition"] = definition;
        assert_rejected(
            &input,
            "Notebook",
            destination("Notebook"),
            DefinitionLimits::default(),
            Kind::InvalidDefinition,
        );
    }
    let mut input = source("Notebook");
    input.item.as_object_mut().unwrap().remove("definition");
    assert_rejected(
        &input,
        "Notebook",
        destination("Notebook"),
        DefinitionLimits::default(),
        Kind::InvalidDefinition,
    );
    input.item = Value::Null;
    assert_rejected(
        &input,
        "Notebook",
        destination("Notebook"),
        DefinitionLimits::default(),
        Kind::InvalidDefinition,
    );
    let mut input = source("Notebook");
    input.item["description"] = json!({"not": "text"});
    assert_rejected(
        &input,
        "Notebook",
        destination("Notebook"),
        DefinitionLimits::default(),
        Kind::InvalidDefinition,
    );
}

#[test]
fn preparation_rejects_unsupported_and_mismatched_types_before_io() {
    for unsupported in ["Lakehouse", "Report", "", "notebook"] {
        assert_rejected(
            &source("Notebook"),
            unsupported,
            destination("Notebook"),
            DefinitionLimits::default(),
            Kind::UnsupportedType,
        );
        assert_rejected(
            &source("Notebook"),
            "Notebook",
            destination(unsupported),
            DefinitionLimits::default(),
            Kind::UnsupportedType,
        );
    }
    assert_rejected(
        &source("Notebook"),
        "Notebook",
        destination("SemanticModel"),
        DefinitionLimits::default(),
        Kind::TypeMismatch,
    );
    assert_rejected(
        &source("SemanticModel"),
        "Notebook",
        destination("Notebook"),
        DefinitionLimits::default(),
        Kind::TypeMismatch,
    );
}

#[test]
fn preparation_rejects_invalid_identifiers_and_names_before_io() {
    for invalid in [
        "",
        "not-a-guid",
        "../escape",
        "11111111-1111-1111-1111-11111111111z",
    ] {
        let mut target = destination("Notebook");
        target.workspace_id = invalid;
        assert_rejected(
            &source("Notebook"),
            "Notebook",
            target,
            DefinitionLimits::default(),
            Kind::InvalidInput,
        );
        target = destination("Notebook");
        target.folder_id = Some(invalid);
        assert_rejected(
            &source("Notebook"),
            "Notebook",
            target,
            DefinitionLimits::default(),
            Kind::InvalidInput,
        );
        for field in ["id", "workspaceId"] {
            let mut input = source("Notebook");
            input.item[field] = json!(invalid);
            assert_rejected(
                &input,
                "Notebook",
                destination("Notebook"),
                DefinitionLimits::default(),
                Kind::InvalidInput,
            );
        }
    }
    for name in ["", " \t ", "folder/name", "folder\\name", "bad\nname", "\0"] {
        let mut target = destination("Notebook");
        target.display_name = name;
        assert_rejected(
            &source("Notebook"),
            "Notebook",
            target,
            DefinitionLimits::default(),
            Kind::InvalidInput,
        );
    }
}

#[test]
fn preparation_enforces_request_part_path_and_depth_limits_before_io() {
    for limits in [
        DefinitionLimits {
            max_bytes: NonZeroUsize::new(32).unwrap(),
            ..DefinitionLimits::default()
        },
        DefinitionLimits {
            max_files: NonZeroUsize::new(1).unwrap(),
            ..DefinitionLimits::default()
        },
        DefinitionLimits {
            max_path_bytes: NonZeroUsize::new(8).unwrap(),
            ..DefinitionLimits::default()
        },
    ] {
        let mut target = destination("Notebook");
        target.display_name = "copy";
        assert_rejected(&source("Notebook"), "Notebook", target, limits, Kind::Limit);
    }
    let mut input = source("Notebook");
    input.item["definition"]["parts"][0]["path"] = json!("definition/notebook.ipynb");
    assert_rejected(
        &input,
        "Notebook",
        destination("Notebook"),
        DefinitionLimits {
            max_depth: NonZeroUsize::new(1).unwrap(),
            ..DefinitionLimits::default()
        },
        Kind::Limit,
    );
    input.item["definition"]["parts"][0]["path"] = json!("../escape");
    assert_rejected(
        &input,
        "Notebook",
        destination("Notebook"),
        DefinitionLimits::default(),
        Kind::InvalidDefinition,
    );
    input.item["definition"]["parts"][0]["path"] = json!("part");
    input.item["definition"]["unknownDefinitionField"] = json!("x".repeat(4096));
    assert_rejected(
        &input,
        "Notebook",
        destination("Notebook"),
        DefinitionLimits {
            max_bytes: NonZeroUsize::new(1024).unwrap(),
            ..DefinitionLimits::default()
        },
        Kind::Limit,
    );
}

#[tokio::test]
async fn created_response_requires_valid_consistent_identity_and_retains_response() {
    for body in [
        json!({}),
        json!([]),
        json!({"id": null}),
        json!({"id": "../unsafe"}),
        json!({"id": ITEM, "type": "SemanticModel"}),
        json!({"id": ITEM, "workspaceId": ITEM}),
        json!({"id": ITEM, "workspaceId": null}),
    ] {
        let mut reply = response(201, body);
        reply.headers.insert("x-original".into(), "retained".into());
        let original = reply.body.clone();
        let harness = Harness::new([Ok(reply)]);
        let error = prepared("Notebook")
            .execute(&mut harness.session(pending()))
            .await
            .unwrap_err();
        assert_eq!(error.kind, Kind::InvalidResponse);
        let received = error.response.unwrap();
        assert_eq!(received.body, original);
        assert_eq!(received.headers["x-original"], "retained");
        assert!(error.operation.unwrap().mutation_may_have_occurred);
        assert_eq!(harness.requests.lock().unwrap().len(), 1);
    }
    let harness = Harness::new([Ok(OperationResponse {
        status: 201,
        body: b"not-json".to_vec(),
        headers: BTreeMap::new(),
    })]);
    let error = prepared("Notebook")
        .execute(&mut harness.session(pending()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, Kind::InvalidResponse);
    assert_eq!(error.response.unwrap().body, b"not-json");
}

#[tokio::test]
async fn created_response_rejects_destination_metadata_mismatch_without_replay() {
    for (folder_id, body) in [
        (
            Some(FOLDER),
            json!({"id": NEW_ITEM, "displayName": "Wrong destination"}),
        ),
        (Some(FOLDER), json!({"id": NEW_ITEM, "displayName": null})),
        (Some(FOLDER), json!({"id": NEW_ITEM, "displayName": 42})),
        (Some(FOLDER), json!({"id": NEW_ITEM, "folderId": ITEM})),
        (Some(FOLDER), json!({"id": NEW_ITEM, "folderId": null})),
        (
            Some(FOLDER),
            json!({"id": NEW_ITEM, "folderId": "not-a-guid"}),
        ),
        (Some(FOLDER), json!({"id": NEW_ITEM, "folderId": 42})),
        (None, json!({"id": NEW_ITEM, "folderId": FOLDER})),
    ] {
        let mut target = destination("Notebook");
        target.folder_id = folder_id;
        let prepared = prepare_definition_copy(
            &source("Notebook"),
            "Notebook",
            target,
            DefinitionLimits::default(),
        )
        .unwrap();
        let original = response(201, body);
        let harness = Harness::new([Ok(original.clone())]);
        let error = prepared
            .execute(&mut harness.session(pending()))
            .await
            .unwrap_err();
        assert_eq!(error.kind, Kind::InvalidResponse);
        let received = error.response.unwrap();
        assert_eq!(received.status, 201);
        assert_eq!(received.body, original.body);
        assert!(error.operation.unwrap().mutation_may_have_occurred);
        let requests = harness.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, ResourceMethod::Post);
        assert_eq!(
            requests[0].path,
            format!("/v1/workspaces/{WORKSPACE}/items")
        );
    }
}

#[tokio::test]
async fn created_response_accepts_matching_destination_metadata() {
    for (folder_id, body) in [
        (
            Some(FOLDER),
            json!({"id": NEW_ITEM, "displayName": "Copy destination", "folderId": FOLDER}),
        ),
        (
            Some(WORKSPACE),
            json!({"id": NEW_ITEM, "folderId": WORKSPACE.to_ascii_uppercase()}),
        ),
        (
            None,
            json!({"id": NEW_ITEM, "displayName": "Copy destination", "folderId": null}),
        ),
        (Some(FOLDER), json!({"id": NEW_ITEM})),
        (None, json!({"id": NEW_ITEM})),
    ] {
        let mut target = destination("Notebook");
        target.folder_id = folder_id;
        let prepared = prepare_definition_copy(
            &source("Notebook"),
            "Notebook",
            target,
            DefinitionLimits::default(),
        )
        .unwrap();
        let harness = Harness::new([Ok(response(201, body))]);
        let receipt = prepared
            .execute(&mut harness.session(pending()))
            .await
            .unwrap();
        assert_eq!(receipt.item_id.as_deref(), Some(NEW_ITEM));
        assert_eq!(harness.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn created_response_accepts_absent_optional_identity_and_case_insensitive_workspace() {
    for body in [
        json!({"id": ITEM}),
        json!({"id": ITEM, "workspaceId": WORKSPACE.to_ascii_uppercase(), "type": "Notebook"}),
    ] {
        let harness = Harness::new([Ok(response(201, body))]);
        let receipt = prepared("Notebook")
            .execute(&mut harness.session(pending()))
            .await
            .unwrap();
        assert_eq!(receipt.item_id.as_deref(), Some(ITEM));
        assert_eq!(harness.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn accepted_response_retains_operation_identity_without_polling_or_completion() {
    for header_only in [true, false] {
        let mut reply = response(202, json!({"id": ITEM, "private": SECRET}));
        reply.headers.insert(
            "Location".into(),
            format!("https://dailyapi.fabric.microsoft.com/v1/operations/{OPERATION}"),
        );
        if header_only {
            reply
                .headers
                .insert("x-ms-operation-id".into(), OPERATION.into());
        }
        let original = reply.clone();
        let harness = Harness::with_host("dailyapi.fabric.microsoft.com", [Ok(reply)]);
        let receipt = prepared("Notebook")
            .execute(&mut harness.session(pending()))
            .await
            .unwrap();
        assert_eq!(receipt.response.status, 202);
        assert_eq!(receipt.response.body, original.body);
        assert_eq!(receipt.response.headers, original.headers);
        assert_eq!(receipt.operation_id.as_deref(), Some(OPERATION));
        assert!(receipt.item_id.is_none());
        assert!(receipt.platform_omitted);
        assert!(!format!("{receipt:?}").contains(SECRET));
        assert_eq!(harness.requests.lock().unwrap().len(), 1);
        assert_eq!(
            harness.requests.lock().unwrap()[0].host,
            "dailyapi.fabric.microsoft.com"
        );
        assert_eq!(harness.waits.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn accepted_without_handle_retains_uncertain_response() {
    let original = response(202, json!({"message": "accepted without handle"}));
    let harness = Harness::new([Ok(original.clone())]);
    let error = prepared("Notebook")
        .execute(&mut harness.session(pending()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, Kind::AcceptedWithoutOperation);
    assert_eq!(error.response.unwrap().body, original.body);
    let evidence = error.operation.unwrap();
    assert!(evidence.operation_id.is_none());
    assert!(evidence.mutation_may_have_occurred);
    assert_eq!(harness.requests.lock().unwrap().len(), 1);
    assert_eq!(harness.waits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_accepted_link_retains_shared_operation_evidence_without_replay() {
    let mut reply = response(202, json!({}));
    reply
        .headers
        .insert("x-ms-operation-id".into(), OPERATION.into());
    reply.headers.insert(
        "Location".into(),
        format!("https://unselected.invalid/v1/operations/{OPERATION}"),
    );
    let harness = Harness::new([Ok(reply)]);
    let error = prepared("Notebook")
        .execute(&mut harness.session(pending()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, Kind::Operation);
    assert!(error.response.is_none());
    let operation = error.operation.unwrap();
    assert_eq!(operation.kind, OperationErrorKind::InvalidLink);
    assert_eq!(operation.operation_id.as_deref(), Some(OPERATION));
    assert!(operation.mutation_may_have_occurred);
    assert_eq!(harness.requests.lock().unwrap().len(), 1);
    assert_eq!(harness.waits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unexpected_success_status_retains_original_response_and_uncertainty() {
    for status in [200, 204] {
        let original = response(status, json!({"id": ITEM}));
        let harness = Harness::new([Ok(original.clone())]);
        let error = prepared("Notebook")
            .execute(&mut harness.session(pending()))
            .await
            .unwrap_err();
        assert_eq!(error.kind, Kind::UnexpectedStatus(status));
        assert_eq!(error.response.unwrap().body, original.body);
        assert!(error.operation.unwrap().mutation_may_have_occurred);
        assert_eq!(harness.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn conflict_retains_shared_service_evidence_without_replay() {
    let service = json!({"error": {"code": "ItemAlreadyExists", "message": SECRET}});
    let harness = Harness::new([Ok(response(409, service))]);
    let error = prepared("Notebook")
        .execute(&mut harness.session(pending()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, Kind::Operation);
    assert!(!format!("{error:?}").contains(SECRET));
    assert!(error.response.is_none());
    let operation = error.operation.unwrap();
    assert!(matches!(
        operation.kind,
        OperationErrorKind::Http { status: 409, .. }
    ));
    assert!(operation.mutation_may_have_occurred);
    assert!(operation.raw_service_error.is_some());
    let requests = harness.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, ResourceMethod::Post);
    assert!(requests[0].path.ends_with("/items"));
}

#[tokio::test]
async fn transport_failure_is_uncertain_and_never_replayed() {
    let harness = Harness::new([Err(OperationExecutionError::Failed)]);
    let error = prepared("Notebook")
        .execute(&mut harness.session(pending()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, Kind::Operation);
    assert!(error.response.is_none());
    let operation = error.operation.unwrap();
    assert_eq!(
        operation.kind,
        OperationErrorKind::Execution(OperationExecutionError::Failed)
    );
    assert!(operation.mutation_may_have_occurred);
    assert_eq!(harness.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancellation_before_dispatch_has_no_side_effects() {
    let harness = Harness::new([]);
    let error = prepared("Notebook")
        .execute(&mut harness.session(ready(())))
        .await
        .unwrap_err();
    let operation = error.operation.unwrap();
    assert_eq!(operation.kind, OperationErrorKind::Cancelled);
    assert!(!operation.mutation_may_have_occurred);
    harness.assert_no_io();
}

#[tokio::test]
async fn cancellation_after_dispatch_retains_uncertainty() {
    let mut harness = Harness::new([]);
    harness.stall = true;
    let cancelled = poll_fn(|_| {
        if harness.cancelled.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let error = prepared("Notebook")
        .execute(&mut harness.session(cancelled))
        .await
        .unwrap_err();
    let operation = error.operation.unwrap();
    assert_eq!(operation.kind, OperationErrorKind::Cancelled);
    assert!(operation.mutation_may_have_occurred);
    assert_eq!(harness.requests.lock().unwrap().len(), 1);
    assert_eq!(harness.waits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn fresh_session_requirement_preserves_prior_post_evidence() {
    let harness = Harness::new([Ok(response(200, json!({"definition": {"parts": []}})))]);
    let mut session = harness.session(pending());
    session
        .execute(
            FabricOperation::new(
                ResourceMethod::Post,
                &format!("/v1/workspaces/{WORKSPACE}/items/{ITEM}/getDefinition"),
                Vec::new(),
                CompletionPolicy::ReturnAccepted,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let error = prepared("Notebook")
        .execute(&mut session)
        .await
        .unwrap_err();
    let operation = error.operation.unwrap();
    assert_eq!(operation.kind, OperationErrorKind::InvalidInput);
    assert!(operation.mutation_may_have_occurred);
    assert_eq!(harness.requests.lock().unwrap().len(), 1);
}

#[test]
fn debug_never_exposes_definition_content() {
    let request = prepared("Notebook");
    let debug = format!("{request:?}");
    assert!(!debug.contains(SECRET));
    assert!(!debug.contains(&STANDARD.encode(SECRET)));
    assert!(!debug.contains("payload"));
}
