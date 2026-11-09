// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline public consumers of Report metadata, native parts and discovery.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk::{
    Audience, Client, CredentialSelection, TokenCredential,
    items::{ItemExecutionError, ItemExecutor, ItemMetadata, ItemRequest, ItemResponse},
    metadata::MetadataLimits,
    reports::{
        IncompleteReason, ReportDefinition, ReportDefinitionRead, ReportError, ReportMetadata,
        ReportTarget, read_definition, read_metadata, register,
    },
    representations::{DefinitionRead, RepresentationError, RepresentationLimits},
    v2::{Availability, Effect, Registry, Target, TargetKind, Verb},
};
use fabric_sdk_operations::{
    OperationErrorKind, OperationExecutionError, OperationExecutor, OperationRequest,
    OperationResponse,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{Future, pending, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const REPORT: &str = "22222222-2222-2222-2222-222222222222";
const MODEL: &str = "33333333-3333-3333-3333-333333333333";
const OP: &str = "44444444-4444-4444-4444-444444444444";

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
            .extend(scopes.iter().map(|scope| (*scope).into()));
        Ok(AccessToken::new(
            "offline-report",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Executor {
    calls: Mutex<Vec<String>>,
    metadata: Value,
    source: Value,
    status: u16,
    headers: BTreeMap<String, String>,
}

impl Default for Executor {
    fn default() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            metadata: metadata_value(),
            source: source(),
            status: 200,
            headers: BTreeMap::new(),
        }
    }
}

impl ItemExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            assert_eq!(
                request.url().path(),
                format!("/v1/workspaces/{WS}/reports/{REPORT}")
            );
            self.calls.lock().unwrap().push(request.url().to_string());
            Ok(ItemResponse {
                status: self.status,
                body: serde_json::to_vec(&self.metadata).unwrap(),
                headers: self.headers.clone(),
            })
        })
    }
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(
                request.method(),
                fabric_sdk_workspaces::mutations::ResourceMethod::Post
            );
            assert_eq!(
                request.url().path(),
                format!("/v1/workspaces/{WS}/reports/{REPORT}/getDefinition")
            );
            assert!(request.body().is_empty());
            self.calls.lock().unwrap().push(request.url().to_string());
            Ok(OperationResponse {
                status: self.status,
                body: if self.status == 202 {
                    Vec::new()
                } else {
                    serde_json::to_vec(&self.source).unwrap()
                },
                headers: self.headers.clone(),
            })
        })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("Report read returns acceptance; it must not install a poller");
    }
}

fn client() -> (Client, Arc<Credential>) {
    let credential = Arc::new(Credential::default());
    let client = Client::new(
        credential.clone(),
        CredentialSelection::new("offline-owner", WS, Audience::Fabric).unwrap(),
    );
    (client, credential)
}

fn target() -> ReportTarget {
    ReportTarget::new(WS, REPORT).unwrap()
}

fn metadata_value() -> Value {
    json!({"id":REPORT,"displayName":"Report","type":"Report","workspaceId":WS,"future":{"keep":null}})
}

fn metadata() -> ReportMetadata {
    ReportMetadata::from_metadata(target(), serde_json::from_value(metadata_value()).unwrap())
        .unwrap()
}

fn limits() -> RepresentationLimits {
    RepresentationLimits {
        max_response_bytes: NonZeroUsize::new(64 * 1024).unwrap(),
        max_result_bytes: NonZeroUsize::new(64 * 1024).unwrap(),
    }
}

fn part(path: &str, bytes: &[u8]) -> Value {
    json!({"path":path,"payload":STANDARD.encode(bytes),"payloadType":"InlineBase64","future":true})
}

fn source() -> Value {
    let binding = json!({
        "$schema":"https://developer.microsoft.com/json-schemas/fabric/item/report/definitionProperties/2.0.0/schema.json",
        "version":"4.0","datasetReference":{"byConnection":{"connectionString":format!("semanticmodelid={MODEL}")}}
    });
    json!({"futureEnvelope":true,"definition":{"format":"PBIR","parts":[
        part("definition.pbir", &serde_json::to_vec(&binding).unwrap()),
        part("definition/version.json", br#"{"version":"2.0.0"}"#),
        part("definition/report.json", br#"{"futureReport":null}"#),
        part("definition/pages/page/page.json", br#"{"name":"page"}"#),
        part("StaticResources/image.bin", &[0,255,128,0]),
        part(".platform", br#"{"futurePlatform":true}"#)
    ]}})
}

fn inspect(value: Value) -> Result<ReportDefinition, ReportError> {
    ReportDefinition::from_read(DefinitionRead::Completed { definition: value }, None)
}

#[test]
fn required_report_and_each_page_json_must_not_be_false_complete() {
    for path in [
        "definition/report.json",
        "definition/pages/page/page.json",
        "definition/pages/second/page.json",
    ] {
        for bytes in [
            b"null".as_slice(),
            b"{malformed",
            b"[]",
            b"false",
            b"42",
            &[0xff],
        ] {
            let mut value = source();
            if path == "definition/pages/second/page.json" {
                value["definition"]["parts"]
                    .as_array_mut()
                    .unwrap()
                    .push(part(path, bytes));
            } else {
                let selected = value["definition"]["parts"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|part| part["path"] == path)
                    .unwrap();
                *selected = part(path, bytes);
            }
            let read = inspect(value.clone()).unwrap();
            assert!(
                !read.complete(),
                "required {path} must not be complete for {bytes:?}"
            );
            assert!(!read.evidence().incomplete.is_empty());
            assert!(
                read.evidence()
                    .incomplete
                    .contains(&IncompleteReason::InvalidReportStructure)
            );
            assert_eq!(read.data(), &value);
            assert_eq!(read.part_bytes(path).unwrap(), bytes);
        }
    }
}

fn legacy_nullable_properties() -> Value {
    json!({
        "$schema":"https://developer.microsoft.com/json-schemas/fabric/item/report/definitionProperties/1.0.0/schema.json",
        "version":"4.0",
        "datasetReference":{"byConnection":{
            "connectionString":null,
            "pbiServiceModelId":null,
            "pbiModelVirtualServerName":"sobe_wowvirtualserver",
            "pbiModelDatabaseName":MODEL,
            "name":"EntityDataSource",
            "connectionType":"pbiServiceXmlaStyleLive"
        }}
    })
}

#[test]
fn legacy_schema_nullable_connection_is_valid_native_source_not_model_permission() {
    let mut value = source();
    let properties = legacy_nullable_properties();
    value["definition"]["parts"][0] =
        part("definition.pbir", &serde_json::to_vec(&properties).unwrap());
    let read = inspect(value.clone()).expect("official schema 1.0.0 permits null connectionString");
    assert!(read.complete());
    assert_eq!(read.data(), &value);
    assert_eq!(
        read.evidence().model_reference.as_ref(),
        Some(&properties["datasetReference"])
    );
    assert!(!read.evidence().related_model_authorized);
}

#[test]
fn legacy_connection_required_fields_use_their_exact_nullable_types() {
    for nullable in [
        "connectionString",
        "pbiServiceModelId",
        "pbiModelVirtualServerName",
        "pbiModelDatabaseName",
        "name",
        "connectionType",
    ] {
        let mut properties = legacy_nullable_properties();
        properties["datasetReference"]["byConnection"][nullable] = Value::Null;
        let mut value = source();
        value["definition"]["parts"][0] =
            part("definition.pbir", &serde_json::to_vec(&properties).unwrap());
        assert!(inspect(value).unwrap().complete());

        properties["datasetReference"]["byConnection"]
            .as_object_mut()
            .unwrap()
            .remove(nullable);
        let mut value = source();
        value["definition"]["parts"][0] =
            part("definition.pbir", &serde_json::to_vec(&properties).unwrap());
        assert!(matches!(inspect(value), Err(ReportError::InvalidResponse)));
    }
    for (field, value, valid) in [
        ("pbiServiceModelId", json!(42), true),
        ("pbiServiceModelId", json!("42"), false),
        ("pbiServiceModelId", json!(42.5), false),
        ("connectionString", json!(false), false),
        ("pbiModelDatabaseName", json!(42), false),
        ("connectionType", json!({}), false),
    ] {
        let mut properties = legacy_nullable_properties();
        properties["datasetReference"]["byConnection"][field] = value;
        let mut value = source();
        value["definition"]["parts"][0] =
            part("definition.pbir", &serde_json::to_vec(&properties).unwrap());
        assert_eq!(inspect(value).is_ok(), valid, "field {field}");
    }
}

#[test]
fn modern_connection_string_remains_strict_and_unknown_schema_is_inert_partial() {
    for connection in [Value::Null, json!(false), json!(42), json!("")] {
        let properties = json!({
            "$schema":"https://developer.microsoft.com/json-schemas/fabric/item/report/definitionProperties/2.0.0/schema.json",
            "version":"4.0","datasetReference":{"byConnection":{"connectionString":connection}}
        });
        let mut value = source();
        value["definition"]["parts"][0] =
            part("definition.pbir", &serde_json::to_vec(&properties).unwrap());
        assert!(matches!(inspect(value), Err(ReportError::InvalidResponse)));
    }
    let properties = json!({
        "$schema":"https://developer.microsoft.com/json-schemas/fabric/item/report/definitionProperties/99.0.0/schema.json",
        "version":"4.0","datasetReference":{"futureBinding":{"opaque":true}}
    });
    let mut value = source();
    value["definition"]["parts"][0] =
        part("definition.pbir", &serde_json::to_vec(&properties).unwrap());
    let read = inspect(value.clone()).unwrap();
    assert_eq!(read.data(), &value);
    assert_eq!(
        read.evidence().model_reference.as_ref(),
        Some(&properties["datasetReference"])
    );
    assert!(
        read.evidence()
            .incomplete
            .contains(&IncompleteReason::UnknownBindingSchema)
    );
    assert!(!read.complete());
    assert!(!read.evidence().related_model_authorized);
}

#[test]
fn required_objects_are_checked_without_reinterpreting_unrelated_json_resources() {
    let mut value = source();
    value["definition"]["parts"]
        .as_array_mut()
        .unwrap()
        .push(part("StaticResources/user.json", &[0xff, 0, 42]));
    let read = inspect(value.clone()).unwrap();
    assert!(read.complete());
    assert_eq!(read.data(), &value);
    assert_eq!(
        read.part_bytes("StaticResources/user.json").unwrap(),
        [0xff, 0, 42]
    );
}

#[tokio::test]
async fn metadata_is_one_typed_get_with_no_source_or_model_call() {
    let (client, credential) = client();
    let executor = Executor::default();
    let read = read_metadata(
        &client,
        &executor,
        &target(),
        MetadataLimits {
            max_response_bytes: limits().max_response_bytes,
            max_result_bytes: limits().max_result_bytes,
        },
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(read.data(), &metadata_value());
    assert_eq!(executor.calls.lock().unwrap().len(), 1);
    assert_eq!(
        *credential.0.lock().unwrap(),
        ["https://api.fabric.microsoft.com/.default"]
    );
    assert!(!format!("{read:?}").contains("future"));
}

#[test]
fn exact_owner_identity_is_validated_without_synthesizing_model_coordinates() {
    for (field, value) in [
        ("type", json!("PaginatedReport")),
        ("type", Value::Null),
        ("id", json!(MODEL)),
        ("workspaceId", json!(MODEL)),
    ] {
        let mut value_source = metadata_value();
        value_source[field] = value;
        let metadata: ItemMetadata = serde_json::from_value(value_source).unwrap();
        assert!(ReportMetadata::from_metadata(target(), metadata).is_err());
    }
    assert!(ReportTarget::new("ws/other", REPORT).is_err());
}

#[tokio::test]
async fn native_read_preserves_complete_parts_media_and_unknown_fields() {
    let (client, _) = client();
    let mut executor = Executor::default();
    executor.source["definition"]["parts"][4]["mediaType"] = json!("application/octet-stream");
    let ReportDefinitionRead::Completed(read) = read_definition(
        &client,
        &executor,
        &metadata(),
        Some("PBIR"),
        limits(),
        pending(),
    )
    .await
    .unwrap() else {
        panic!("not a complete transport response")
    };
    assert_eq!(read.data(), &executor.source);
    assert!(read.complete());
    assert_eq!(
        read.part_bytes("StaticResources/image.bin").unwrap(),
        [0, 255, 128, 0]
    );
    assert_eq!(
        read.evidence().parts[4].media_type.as_deref(),
        Some("application/octet-stream")
    );
    assert_eq!(read.evidence().version.as_deref(), Some("4.0"));
    assert_eq!(read.evidence().native_version.as_deref(), Some("2.0.0"));
    assert!(!read.evidence().related_model_authorized);
    assert_eq!(
        read.evidence().model_reference.as_ref().unwrap()["byConnection"]["connectionString"],
        format!("semanticmodelid={MODEL}")
    );
    assert!(executor.calls.lock().unwrap()[0].ends_with("/getDefinition?format=PBIR"));
    assert!(!format!("{read:?}").contains(MODEL));
}

#[tokio::test]
async fn unsupported_requested_formats_and_precancel_never_dispatch() {
    let (client, credential) = client();
    let executor = Executor::default();
    for format in ["PBIX", "RDL", "json", "", "PBIR?other=true"] {
        assert!(matches!(
            read_definition(
                &client,
                &executor,
                &metadata(),
                Some(format),
                limits(),
                pending()
            )
            .await,
            Err(ReportError::UnsupportedFormat)
        ));
    }
    assert!(
        read_definition(&client, &executor, &metadata(), None, limits(), ready(()))
            .await
            .is_err()
    );
    assert!(executor.calls.lock().unwrap().is_empty());
    assert!(credential.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn acceptance_returns_exact_shared_handle_without_polling_or_replay() {
    let (client, _) = client();
    let executor = Executor {
        status: 202,
        headers: BTreeMap::from([
            (
                "Location".into(),
                format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
            ),
            ("x-ms-operation-id".into(), OP.into()),
            ("Retry-After".into(), "1".into()),
        ]),
        ..Executor::default()
    };
    let ReportDefinitionRead::Accepted(read) =
        read_definition(&client, &executor, &metadata(), None, limits(), pending())
            .await
            .unwrap()
    else {
        panic!("accepted is not completed source");
    };
    assert!(matches!(&read, DefinitionRead::Accepted { operation_id } if operation_id == OP));
    assert!(read.accepted_handle().is_some());
    assert!(ReportDefinition::from_read(read, None).is_err());
    assert_eq!(executor.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn denied_definition_retains_status_without_metadata_fallback() {
    let (client, _) = client();
    let executor = Executor {
        status: 403,
        ..Executor::default()
    };
    let error = read_definition(&client, &executor, &metadata(), None, limits(), pending())
        .await
        .unwrap_err();
    assert!(
        matches!(error, ReportError::Representation(RepresentationError::Operation(ref error))
        if matches!(error.kind, OperationErrorKind::Http { status:403, .. }))
    );
    assert_eq!(executor.calls.lock().unwrap().len(), 1);
}

#[test]
fn unknown_versions_and_missing_parts_retain_partial_source_not_success() {
    for (path, bytes) in [
        ("definition.pbir", br#"{"version":"99.0","datasetReference":{"byPath":{"path":"../Model.SemanticModel"}}}"#.as_slice()),
        ("definition/version.json", br#"{"version":"99.0.0"}"#.as_slice()),
    ] {
        let mut value = source();
        let selected = value["definition"]["parts"].as_array_mut().unwrap().iter_mut().find(|part| part["path"] == path).unwrap();
        *selected = part(path, bytes);
        let read = inspect(value.clone()).unwrap();
        assert_eq!(read.data(), &value);
        assert!(!read.complete());
        assert!(read.evidence().incomplete.contains(&IncompleteReason::UnknownVersion));
    }
    let mut value = source();
    value["definition"]["parts"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    let read = inspect(value).unwrap();
    assert!(
        read.evidence()
            .incomplete
            .contains(&IncompleteReason::MissingDefinitionProperties)
    );
    assert!(
        read.evidence()
            .incomplete
            .contains(&IncompleteReason::MissingModelReference)
    );
    assert!(!read.evidence().related_model_authorized);
}

#[test]
fn future_format_or_encoding_is_opaque_partial_never_a_conversion() {
    let mut value = source();
    value["definition"]["format"] = json!("FutureReport");
    value["definition"]["parts"][4]["payloadType"] = json!("FutureEncoding");
    let read = inspect(value.clone()).unwrap();
    assert_eq!(read.data(), &value);
    assert!(
        read.evidence()
            .incomplete
            .contains(&IncompleteReason::UnknownFormat)
    );
    assert!(
        read.evidence()
            .incomplete
            .contains(&IncompleteReason::UnsupportedPayloadType)
    );
    assert!(read.part_bytes("StaticResources/image.bin").is_err());
}

#[test]
fn future_definition_version_preserves_unknown_binding_without_legacy_coercion() {
    let mut value = source();
    let reference = json!({"futureBinding":{"modelObject":"opaque","workspace":"not-inferred"}});
    let properties = json!({"version":"99.0","datasetReference":reference});
    value["definition"]["parts"][0] =
        part("definition.pbir", &serde_json::to_vec(&properties).unwrap());
    let read = inspect(value.clone()).unwrap();
    assert_eq!(read.data(), &value);
    assert_eq!(read.evidence().model_reference, Some(reference));
    assert!(!read.evidence().related_model_authorized);
    assert!(!read.complete());
    assert!(
        read.evidence()
            .incomplete
            .contains(&IncompleteReason::UnknownVersion)
    );
}

#[test]
fn legacy_report_is_native_source_not_pbix_or_enhanced_definition() {
    let value = json!({"definition":{"parts":[
        part("definition.pbir",br#"{"version":"1.0","datasetReference":{"byPath":{"path":"../Model.SemanticModel"}}}"#),
        part("report.json",br#"{"sections":[]}"#)
    ]}});
    let read = inspect(value.clone()).unwrap();
    assert!(read.complete());
    assert_eq!(
        read.evidence().format.layout.as_deref(),
        Some("PBIR-Legacy")
    );
    assert_eq!(read.data(), &value);
    assert!(
        ReportDefinition::from_read(
            DefinitionRead::Completed { definition: value },
            Some("PBIR")
        )
        .is_err()
    );
}

#[test]
fn malformed_parts_bindings_and_mixed_layout_are_errors_not_dropped_resources() {
    let mut corrupt = source();
    corrupt["definition"]["parts"][4]["payload"] = json!("invalid base64");
    assert!(inspect(corrupt).is_err());
    let mut duplicate = source();
    let extra = duplicate["definition"]["parts"][4].clone();
    duplicate["definition"]["parts"]
        .as_array_mut()
        .unwrap()
        .push(extra);
    assert!(matches!(
        inspect(duplicate),
        Err(ReportError::AmbiguousSource)
    ));
    let mut mixed = source();
    mixed["definition"]["parts"]
        .as_array_mut()
        .unwrap()
        .push(part("report.json", b"{}"));
    assert!(matches!(inspect(mixed), Err(ReportError::AmbiguousSource)));
    for binding in [
        br#"not json"#.as_slice(),
        br#"{"version":4}"#.as_slice(),
        br#"{"version":"4.0","datasetReference":{"byPath":{"path":"m"},"byConnection":{"connectionString":"m"}}}"#.as_slice(),
        br#"{"version":"4.0","datasetReference":{"byPath":{}}}"#.as_slice(),
    ] {
        let mut value = source();
        value["definition"]["parts"][0] = part("definition.pbir",binding);
        assert!(inspect(value).is_err());
    }
}

#[tokio::test]
async fn exact_metadata_result_budget_is_enforced_without_projection() {
    let (client, _) = client();
    let executor = Executor::default();
    let bytes = serde_json::to_vec(&executor.metadata).unwrap().len();
    for (limit, succeeds) in [(bytes, true), (bytes - 1, false)] {
        assert_eq!(
            read_metadata(
                &client,
                &executor,
                &target(),
                MetadataLimits {
                    max_response_bytes: limits().max_response_bytes,
                    max_result_bytes: NonZeroUsize::new(limit).unwrap(),
                },
                pending()
            )
            .await
            .is_ok(),
            succeeds
        );
    }
}

#[test]
fn registry_shares_facade_reads_and_does_not_advertise_authoring() {
    let mut registry = Registry::default();
    register(&mut registry).unwrap();
    for (facet, representation) in [
        ("item", "json"),
        ("definition", "native"),
        ("definition", "PBIR"),
        ("definition", "PBIR-Legacy"),
    ] {
        let target = Target {
            kind: TargetKind::Item,
            owner_type: "Report".into(),
            facet: facet.into(),
            representation: representation.into(),
            generation: None,
        };
        let common = registry.resolve(Verb::Get, &target, None).unwrap();
        let alias = registry
            .resolve(Verb::Get, &target, Some("report"))
            .unwrap();
        assert_eq!(common.operation.id(), alias.operation.id());
        assert_eq!(common.operation.effects(), [Effect::RemoteRead]);
        if facet == "definition" {
            assert!(matches!(
                common.binding.availability(),
                Availability::Conditional(_)
            ));
        }
        assert!(
            registry
                .resolve(Verb::Create, &target, Some("report"))
                .is_err()
        );
        let wrong = Target {
            owner_type: "SemanticModel".into(),
            ..target
        };
        assert!(registry.resolve(Verb::Get, &wrong, Some("report")).is_err());
    }
    let before = serde_json::to_value(registry.operations().collect::<Vec<_>>()).unwrap();
    assert!(register(&mut registry).is_err());
    assert_eq!(
        serde_json::to_value(registry.operations().collect::<Vec<_>>()).unwrap(),
        before
    );
}
