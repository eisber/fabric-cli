// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline public CopyJob consumers; no ambient identity or live service.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk::{
    Audience, Client, CredentialSelection, TokenCredential,
    copyjobs::{
        CopyJobError, CopyJobListOptions, CopyJobTarget, decode_page, get_definition, get_metadata,
        list_operation, list_page, register, validate_definition,
    },
    items::{ItemErrorKind, ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse},
    representations::{DefinitionRead, RepresentationError, RepresentationLimits},
    v2::{ContractError, Effect, Registry, Target, TargetKind, Verb},
};
use fabric_sdk_operations::{
    OperationErrorKind, OperationExecutionError, OperationExecutor, OperationRequest,
    OperationResponse,
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OP: &str = "33333333-3333-3333-3333-333333333333";

#[derive(Debug, Default)]
struct Credential(Mutex<Vec<String>>);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0
            .lock()
            .unwrap()
            .extend(scopes.iter().map(|s| (*s).into()));
        Ok(AccessToken::new(
            "offline-copyjob",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

fn client(credential: &Arc<Credential>) -> Client {
    Client::new(
        credential.clone(),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    )
}

fn target() -> CopyJobTarget {
    CopyJobTarget::new(WS, ITEM).unwrap()
}

fn metadata() -> Value {
    json!({"id":ITEM,"workspaceId":WS,"type":"CopyJob","displayName":"Copy",
        "description":null,"future":{"integer":42,"enabled":false}})
}

fn definition() -> Value {
    json!({"futureEnvelope":true,"definition":{"future":null,"parts":[
        {"path":"copyjob-content.json","payloadType":"InlineBase64",
            "payload":STANDARD.encode(br#"{"properties":{"jobMode":"CDC","future":null},"activities":[]}"#)},
        {"path":".platform","payloadType":"InlineBase64","payload":STANDARD.encode(b"{\"metadata\":{}}")}
    ]}})
}

fn limits(bytes: usize) -> RepresentationLimits {
    RepresentationLimits {
        max_response_bytes: NonZeroUsize::new(bytes).unwrap(),
        max_result_bytes: NonZeroUsize::new(bytes).unwrap(),
    }
}

type Reply = Result<OperationResponse, OperationExecutionError>;

#[derive(Default)]
struct Executor {
    replies: Mutex<VecDeque<Reply>>,
    calls: Mutex<Vec<(ResourceMethod, String, Vec<u8>)>>,
}

impl Executor {
    fn response(status: u16, value: &Value) -> Self {
        let response = OperationResponse {
            status,
            body: serde_json::to_vec(value).unwrap(),
            headers: BTreeMap::new(),
        };
        Self {
            replies: Mutex::new(VecDeque::from([Ok(response)])),
            ..Self::default()
        }
    }

    fn take(&self, method: ResourceMethod, url: &str, body: &[u8]) -> Reply {
        self.calls
            .lock()
            .unwrap()
            .push((method, url.into(), body.into()));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected replay, fallback or poll")
    }
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Reply> + Send + 'a>> {
        Box::pin(async move { self.take(request.method(), request.url().as_str(), request.body()) })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("CopyJob read must not install an observer")
    }
}

impl ItemExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            let response = self
                .take(ResourceMethod::Get, request.url().as_str(), b"")
                .map_err(|_| ItemExecutionError::Failed)?;
            Ok(ItemResponse {
                status: response.status,
                body: response.body,
                headers: response.headers,
            })
        })
    }
}

#[tokio::test]
async fn metadata_is_one_native_get_with_exact_owner_and_no_definition() {
    let credential = Arc::new(Credential::default());
    let raw = metadata();
    let executor = Executor::response(200, &raw);
    let read = get_metadata(
        &client(&credential),
        &executor,
        &target(),
        limits(4096).max_response_bytes,
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(read.as_value(), &raw);
    assert!(!format!("{read:?}").contains("integer"));
    assert_eq!(
        executor.calls.lock().unwrap().as_slice(),
        &[(
            ResourceMethod::Get,
            format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/copyJobs/{ITEM}"),
            Vec::new()
        )]
    );
    assert_eq!(
        credential.0.lock().unwrap().as_slice(),
        ["https://api.fabric.microsoft.com/.default"]
    );
}

#[test]
fn malformed_concrete_identifiers_fail_without_execution() {
    for id in [
        "",
        "name",
        "../item",
        "11111111-1111-1111-1111-11111111111x",
        "a/b",
        "%2F",
    ] {
        assert!(CopyJobTarget::new(WS, id).is_err());
        assert!(CopyJobTarget::new(id, ITEM).is_err());
    }
}

#[tokio::test]
async fn metadata_rejects_wrong_or_malformed_identity_and_known_field_types() {
    for (field, value) in [
        ("id", json!(OP)),
        ("workspaceId", json!(OP)),
        ("type", json!("Notebook")),
        ("type", Value::Null),
        ("workspaceId", Value::Null),
        ("id", json!("item")),
        ("folderId", json!("folder")),
        ("logicalId", json!(7)),
        ("description", json!({})),
    ] {
        let mut raw = metadata();
        raw[field] = value;
        let executor = Executor::response(200, &raw);
        assert!(matches!(
            get_metadata(
                &client(&Arc::default()),
                &executor,
                &target(),
                limits(4096).max_response_bytes,
                pending()
            )
            .await,
            Err(CopyJobError::InvalidResponse)
        ));
        assert_eq!(executor.calls.lock().unwrap().len(), 1);
    }
    let mut raw = metadata();
    raw.as_object_mut().unwrap().remove("workspaceId");
    assert!(
        get_metadata(
            &client(&Arc::default()),
            &Executor::response(200, &raw),
            &target(),
            limits(4096).max_response_bytes,
            pending()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn denied_metadata_is_neither_absent_nor_source_fallback() {
    for status in [401, 403, 404] {
        let executor = Executor::response(status, &json!({"errorCode":"AccessDenied"}));
        let result = get_metadata(
            &client(&Arc::default()),
            &executor,
            &target(),
            limits(4096).max_response_bytes,
            pending(),
        )
        .await;
        assert!(
            matches!(result, Err(CopyJobError::Metadata(error)) if matches!(error.kind, ItemErrorKind::Http {status:observed,..} if observed==status))
        );
        assert_eq!(executor.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn definition_preserves_native_envelope_optional_parts_and_no_format_default() {
    let raw = definition();
    let executor = Executor::response(200, &raw);
    let read = get_definition(
        &client(&Arc::default()),
        &executor,
        &target(),
        None,
        limits(4096),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(read, DefinitionRead::Completed { definition: raw });
    assert_eq!(read.part_bytes(".platform").unwrap(), b"{\"metadata\":{}}");
    assert_eq!(
        executor.calls.lock().unwrap().as_slice(),
        &[(
            ResourceMethod::Post,
            format!(
                "https://api.fabric.microsoft.com/v1/workspaces/{WS}/copyJobs/{ITEM}/getDefinition"
            ),
            Vec::new()
        )]
    );
}

#[tokio::test]
async fn explicit_native_format_is_encoded_and_unsafe_format_precedes_credentials() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::response(200, &definition());
    get_definition(
        &client(&credential),
        &executor,
        &target(),
        Some("Future.Format-1"),
        limits(4096),
        pending(),
    )
    .await
    .unwrap();
    assert!(
        executor.calls.lock().unwrap()[0]
            .1
            .ends_with("?format=Future.Format-1")
    );
    let before = credential.0.lock().unwrap().len();
    for format in ["", "a/b", "x?format=y", "%2F", "é", "x\n", &"x".repeat(129)] {
        assert!(matches!(
            get_definition(
                &client(&credential),
                &executor,
                &target(),
                Some(format),
                limits(4096),
                pending()
            )
            .await,
            Err(CopyJobError::InvalidInput)
        ));
    }
    assert_eq!(credential.0.lock().unwrap().len(), before);
    assert_eq!(executor.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn definition_acceptance_retains_actual_handle_without_poll_or_replay() {
    let executor = Executor {
        replies: Mutex::new(VecDeque::from([Ok(OperationResponse {
            status: 202,
            body: Vec::new(),
            headers: BTreeMap::from([
                (
                    "Location".into(),
                    format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
                ),
                ("x-ms-operation-id".into(), OP.into()),
                ("Retry-After".into(), "30".into()),
            ]),
        })])),
        ..Executor::default()
    };
    let read = get_definition(
        &client(&Arc::default()),
        &executor,
        &target(),
        None,
        limits(4096),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(
        read,
        DefinitionRead::Accepted {
            operation_id: OP.into()
        }
    );
    assert!(read.accepted_handle().is_some());
    assert_eq!(executor.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn definition_denial_response_loss_and_unbound_acceptance_keep_failure_evidence() {
    for status in [401, 403, 404] {
        let executor = Executor::response(status, &json!({"errorCode":"Denied"}));
        assert!(matches!(
            get_definition(
                &client(&Arc::default()),
                &executor,
                &target(),
                None,
                limits(4096),
                pending()
            )
            .await,
            Err(CopyJobError::Definition(RepresentationError::Operation(_)))
        ));
        assert_eq!(executor.calls.lock().unwrap().len(), 1);
    }
    let executor = Executor {
        replies: Mutex::new(VecDeque::from([Err(OperationExecutionError::Failed)])),
        ..Executor::default()
    };
    let result = get_definition(
        &client(&Arc::default()),
        &executor,
        &target(),
        None,
        limits(4096),
        pending(),
    )
    .await;
    assert!(
        matches!(result,Err(CopyJobError::Definition(RepresentationError::Operation(error)))
        if matches!(error.kind,OperationErrorKind::Execution(_)))
    );
    assert_eq!(executor.calls.lock().unwrap().len(), 1);
    let executor = Executor::response(202, &Value::Null);
    assert!(
        get_definition(
            &client(&Arc::default()),
            &executor,
            &target(),
            None,
            limits(4096),
            pending()
        )
        .await
        .is_err()
    );
    assert_eq!(executor.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancellation_and_byte_limit_never_deliver_completed_source() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::default();
    assert!(
        get_definition(
            &client(&credential),
            &executor,
            &target(),
            None,
            limits(4096),
            ready(())
        )
        .await
        .is_err()
    );
    assert!(executor.calls.lock().unwrap().is_empty());
    assert!(credential.0.lock().unwrap().is_empty());
    let executor = Executor::response(200, &definition());
    assert!(
        get_definition(
            &client(&credential),
            &executor,
            &target(),
            None,
            limits(1),
            pending()
        )
        .await
        .is_err()
    );
    assert_eq!(executor.calls.lock().unwrap().len(), 1);
}

#[test]
fn native_definition_checks_canonical_part_and_content_without_rewriting_future_fields() {
    let raw = definition();
    let accepted =
        DefinitionRead::from_completed_result(&serde_json::to_vec(&raw).unwrap(), limits(4096))
            .unwrap();
    validate_definition(&accepted).unwrap();
    for (field, value) in [
        ("path", json!("CopyJobV1.json")),
        ("payload", json!("!")),
        ("payloadType", json!("External")),
    ] {
        let mut invalid = raw.clone();
        invalid["definition"]["parts"][0][field] = value;
        let read = DefinitionRead::from_completed_result(
            &serde_json::to_vec(&invalid).unwrap(),
            limits(4096),
        )
        .unwrap();
        assert!(validate_definition(&read).is_err());
    }
    for content in [
        b"null".as_slice(),
        b"{}",
        br#"{"properties":{},"activities":[]}"#,
        br#"{"properties":{"jobMode":"Batch"},"activities":[7]}"#,
    ] {
        let mut invalid = raw.clone();
        invalid["definition"]["parts"][0]["payload"] = json!(STANDARD.encode(content));
        let read = DefinitionRead::from_completed_result(
            &serde_json::to_vec(&invalid).unwrap(),
            limits(4096),
        )
        .unwrap();
        assert!(validate_definition(&read).is_err());
    }
    let mut duplicate = raw.clone();
    let part = duplicate["definition"]["parts"][0].clone();
    duplicate["definition"]["parts"]
        .as_array_mut()
        .unwrap()
        .push(part);
    let read = DefinitionRead::from_completed_result(
        &serde_json::to_vec(&duplicate).unwrap(),
        limits(4096),
    )
    .unwrap();
    assert!(matches!(
        validate_definition(&read),
        Err(CopyJobError::Definition(RepresentationError::Ambiguous))
    ));
}

struct Pages {
    calls: Vec<ResourceOperation>,
    outcome: Option<Result<ResourceOutcome, ResourceError>>,
}

impl ResourceOperations for Pages {
    fn execute<'a>(
        &'a mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + 'a>> {
        self.calls.push(operation);
        Box::pin(ready(self.outcome.take().expect("unexpected next page")))
    }
}

fn list_options() -> CopyJobListOptions {
    CopyJobListOptions {
        recursive: false,
        root_folder_id: None,
        continuation_token: None,
    }
}

#[tokio::test]
async fn list_one_page_preserves_records_and_incomplete_native_continuation() {
    let raw = json!({"value":[metadata(),metadata()],"continuationToken":"opaque+/=",
        "continuationUri":"https://untrusted.example/never-followed","future":false});
    let mut pages = Pages {
        calls: Vec::new(),
        outcome: Some(Ok(ResourceOutcome::Completed {
            status: 200,
            data: Some(raw.clone()),
        })),
    };
    let page = list_page(&mut pages, WS, &list_options()).await.unwrap();
    assert_eq!(page.as_value(), &raw);
    assert_eq!(page.records().len(), 2);
    assert!(page.has_continuation());
    assert_eq!(pages.calls.len(), 1);
    assert_eq!(
        pages.calls[0].path(),
        format!("/v1/workspaces/{WS}/copyJobs?recursive=false")
    );
    assert_eq!(pages.calls[0].method(), ResourceMethod::Get);
    assert_eq!(pages.calls[0].body(), None);
    assert_eq!(
        pages.calls[0].completion(),
        CompletionPolicy::ReturnAccepted
    );
    let options = CopyJobListOptions {
        recursive: true,
        root_folder_id: Some(ITEM.into()),
        continuation_token: Some("opaque+/=%".into()),
    };
    assert_eq!(
        list_operation(WS, &options).unwrap().path(),
        format!(
            "/v1/workspaces/{WS}/copyJobs?recursive=true&rootFolderId={ITEM}&continuationToken=opaque%2B%2F%3D%25"
        )
    );
}

#[test]
fn list_empty_singleton_malformed_and_accepted_outcomes_remain_distinct() {
    for rows in [vec![], vec![metadata()]] {
        let page = decode_page(
            WS,
            ResourceOutcome::Completed {
                status: 200,
                data: Some(json!({"value":rows})),
            },
        )
        .unwrap();
        assert_eq!(page.records().len(), rows.len());
        assert!(!page.has_continuation());
    }
    for raw in [
        json!({}),
        json!({"value":null}),
        json!({"value":{}}),
        json!({"value":[{}]}),
        json!({"value":[],"continuationToken":2}),
        json!({"value":[],"continuationUri":false}),
    ] {
        assert!(
            decode_page(
                WS,
                ResourceOutcome::Completed {
                    status: 200,
                    data: Some(raw)
                }
            )
            .is_err()
        );
    }
    for outcome in [
        ResourceOutcome::Accepted {
            operation_id: Some(OP.into()),
        },
        ResourceOutcome::Completed {
            status: 204,
            data: None,
        },
    ] {
        assert!(matches!(
            decode_page(WS, outcome),
            Err(CopyJobError::UnexpectedOutcome(_))
        ));
    }
    let mut options = list_options();
    for token in ["", "opaque\n"] {
        options.continuation_token = Some(token.into());
        assert!(list_operation(WS, &options).is_err());
    }
}

#[test]
fn registry_root_facade_and_schema_share_exact_read_bindings_only() {
    let mut registry = Registry::default();
    register(&mut registry).unwrap();
    assert_eq!(registry.operations().count(), 3);
    for (kind, facet, representation, verb) in [
        (TargetKind::Item, "item", "metadata", Verb::Get),
        (TargetKind::Item, "src", "definition", Verb::Get),
        (
            TargetKind::WorkspaceCollection,
            "items",
            "metadata",
            Verb::List,
        ),
    ] {
        let target = Target {
            kind,
            owner_type: "CopyJob".into(),
            facet: facet.into(),
            representation: representation.into(),
            generation: None,
        };
        let root = registry.resolve(verb, &target, None).unwrap();
        let facade = registry.resolve(verb, &target, Some("copy-job")).unwrap();
        assert_eq!(root.operation.id(), facade.operation.id());
        assert_eq!(root.operation.effects(), &[Effect::RemoteRead]);
        assert!(root.operation.input_schema().is_object());
        assert!(root.operation.output_schema().is_object());
        assert!(registry.resolve(Verb::Update, &target, None).is_err());
        let wrong = Target {
            owner_type: "Notebook".into(),
            ..target
        };
        assert!(matches!(
            registry.resolve(verb, &wrong, Some("copy-job")),
            Err(ContractError::WrongOwnerType)
        ));
    }
    assert!(register(&mut registry).is_err());
}
