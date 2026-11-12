// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native schema, shared planning and immutable operation contracts.

#![deny(unnameable_test_items)]

use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk_items::{
    create::{
        ConflictPolicy, CreateAction, CreateError, CreateInputValidator, CreatePlan, CreateTarget,
        DuplicateSupport, LeafState, NativeId, Preflight, ReplacementUnit, ResourceIdentity,
        SchemaRef, TargetScope, plan_create,
    },
    lakehouse_create::{
        LakehouseCreateInput, LakehouseCreateRequest, LakehouseCreateSchema, MAX_INPUT_BYTES,
        SCHEMA,
    },
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};

const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
const ITEM: &str = "22222222-2222-4222-8222-222222222222";
const LABEL: &str = "33333333-3333-4333-8333-333333333333";
const NAME: &str = "Lakehouse_1";

fn target() -> CreateTarget {
    CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WORKSPACE).unwrap(),
        NAME,
        "Lakehouse",
    )
    .unwrap()
}

fn existing() -> Preflight {
    Preflight::Leaf(LeafState::Existing(
        ResourceIdentity::new(
            NativeId::new(ITEM).unwrap(),
            NativeId::new(WORKSPACE).unwrap(),
            "Lakehouse",
        )
        .unwrap(),
    ))
}

fn plan(
    policy: ConflictPolicy,
    state: Preflight,
    supplied: Option<Value>,
) -> Result<CreatePlan, CreateError> {
    plan_create(
        policy,
        target(),
        state,
        &LakehouseCreateSchema::capabilities().unwrap(),
        supplied,
        &LakehouseCreateSchema,
    )
}

fn request(supplied: Option<Value>) -> LakehouseCreateRequest {
    LakehouseCreateRequest::from_plan(
        &plan(
            ConflictPolicy::Error,
            Preflight::Leaf(LeafState::Missing),
            supplied,
        )
        .unwrap(),
    )
    .unwrap()
}

fn definition(metadata: &[u8]) -> Value {
    json!({
        "displayName":NAME,
        "definition":{
            "format":"LakehouseDefinitionV1",
            "parts":[{
                "path":"lakehouse.metadata.json",
                "payload":STANDARD.encode(metadata),
                "payloadType":"InlineBase64"
            }]
        }
    })
}

#[test]
fn omitted_input_creates_only_exact_name_at_native_collection() {
    let request = request(None);
    assert_eq!(request.target(), &target());
    assert_eq!(request.body(), &json!({"displayName":NAME}));
    assert_eq!(request.expected_statuses(), &[201, 202]);
    for completion in [
        CompletionPolicy::ReturnAccepted,
        CompletionPolicy::WaitForResult,
    ] {
        let operation = request.operation(completion).unwrap();
        assert_eq!(operation.method(), ResourceMethod::Post);
        assert_eq!(
            operation.path(),
            format!("/v1/workspaces/{WORKSPACE}/lakehouses")
        );
        assert_eq!(operation.body(), Some(request.body()));
        assert_eq!(operation.completion(), completion);
    }
}

#[test]
fn native_metadata_retains_folder_null_description_and_label_strategy() {
    let body = json!({
        "displayName":NAME,
        "description":"",
        "folderId":null,
        "creationPayload":{"enableSchemas":true},
        "sensitivityLabelSettings":{
            "labelId":LABEL,
            "sensitivityLabelApplyStrategy":"ApplyOrFail"
        }
    });
    assert_eq!(request(Some(body.clone())).body(), &body);
    let body = json!({
        "displayName":NAME,
        "description":"x".repeat(256),
        "folderId":ITEM,
        "sensitivityLabelSettings":{
            "labelId":LABEL,
            "sensitivityLabelApplyStrategy":"Ignore"
        }
    });
    assert_eq!(request(Some(body.clone())).body(), &body);
}

#[test]
fn strict_input_retains_raw_bytes_without_newline_or_base64_normalization() {
    let raw = b" \r\n{\r\n  \"displayName\" : \"Lakehouse_1\", \"description\":\"\" }\r\n";
    let input = LakehouseCreateInput::from_bytes(raw).unwrap();
    assert_eq!(input.raw(), raw);
    assert_eq!(input.value(), &json!({"displayName":NAME,"description":""}));
    assert_eq!(request(Some(input.value().clone())).body(), input.value());
    let sensitive = format!("{input:?}");
    assert!(!sensitive.contains(NAME));
    assert!(!sensitive.contains("description"));
}

#[test]
fn input_rejects_empty_non_utf8_duplicate_keys_trailing_documents_and_bounds() {
    for raw in [
        &b""[..],
        &b"\xff"[..],
        &b" "[..],
        &b"{} {}"[..],
        &b"{\"displayName\":\"first\",\"displayName\":\"second\"}"[..],
        &b"{\"displayName\":\"first\",\"display\\u004eame\":\"second\"}"[..],
        &b"{\"nested\":{\"key\":1,\"key\":2}}"[..],
    ] {
        let error = LakehouseCreateInput::from_bytes(raw).unwrap_err();
        assert_eq!(error.code, "InvalidInput");
        assert!(!error.mutation_may_have_occurred);
    }
    assert!(LakehouseCreateInput::from_bytes(&vec![b' '; MAX_INPUT_BYTES + 1]).is_err());
    let mut at_limit = vec![b' '; MAX_INPUT_BYTES];
    at_limit[..2].copy_from_slice(b"{}");
    assert!(LakehouseCreateInput::from_bytes(&at_limit).is_ok());
}

#[test]
fn explicit_invalid_metadata_is_rejected_even_before_existing_skip() {
    for body in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"displayName":null}),
        json!({"displayName":"another"}),
        json!({"displayName":NAME,"type":"Lakehouse"}),
        json!({"displayName":NAME,"id":ITEM}),
        json!({"displayName":NAME,"description":null}),
        json!({"displayName":NAME,"description":"x".repeat(257)}),
        json!({"displayName":NAME,"folderId":false}),
        json!({"displayName":NAME,"folderId":"../items"}),
        json!({"displayName":NAME,"creationPayload":null}),
        json!({"displayName":NAME,"creationPayload":{}}),
        json!({"displayName":NAME,"creationPayload":{"enableSchemas":false}}),
        json!({"displayName":NAME,"creationPayload":{"enableSchemas":null}}),
        json!({"displayName":NAME,"creationPayload":{"enableSchemas":true,"other":true}}),
        json!({"displayName":NAME,"sensitivityLabelSettings":null}),
        json!({"displayName":NAME,"sensitivityLabelSettings":{}}),
        json!({"displayName":NAME,"sensitivityLabelSettings":{"labelId":"bad"}}),
        json!({"displayName":NAME,"sensitivityLabelSettings":{"labelId":LABEL,"sensitivityLabelApplyStrategy":null}}),
        json!({"displayName":NAME,"sensitivityLabelSettings":{"labelId":LABEL,"other":0}}),
    ] {
        assert_eq!(
            plan(ConflictPolicy::Skip, existing(), Some(body)),
            Err(CreateError::InputRejected)
        );
    }
}

#[test]
fn metadata_definition_is_validated_without_reencoding_native_payload() {
    for metadata in [br#"{}"#.as_slice(), br#"{"defaultSchema":"dbo"}"#] {
        let body = definition(metadata);
        assert_eq!(request(Some(body.clone())).body(), &body);
    }
    let mut body = definition(b"{\r\n\"defaultSchema\":\"dbo\"\r\n}\r\n");
    body["definition"].as_object_mut().unwrap().remove("format");
    assert_eq!(request(Some(body.clone())).body(), &body);
}

#[test]
fn metadata_definition_rejects_null_duplicates_unknown_schema_and_unsupported_parts() {
    let valid = definition(br#"{"defaultSchema":"dbo"}"#);
    let mut invalid = Vec::new();
    for metadata in [
        &b"null"[..],
        &b"[]"[..],
        &b""[..],
        &b"\xff"[..],
        &b"{\"defaultSchema\":null}"[..],
        &b"{\"defaultSchema\":1}"[..],
        &b"{\"unknown\":true}"[..],
        &b"{\"defaultSchema\":\"dbo\",\"defaultSchema\":\"other\"}"[..],
        &b"{}{}"[..],
    ] {
        invalid.push(definition(metadata));
    }
    for (pointer, value) in [
        ("/definition", Value::Null),
        ("/definition/format", Value::Null),
        ("/definition/format", json!("Other")),
        ("/definition/parts", json!([])),
        ("/definition/parts", Value::Null),
        (
            "/definition/parts/0/path",
            json!("../lakehouse.metadata.json"),
        ),
        ("/definition/parts/0/path", json!("shortcuts.metadata.json")),
        ("/definition/parts/0/path", json!("data-access-roles.json")),
        ("/definition/parts/0/path", json!("alm.settings.json")),
        ("/definition/parts/0/path", json!(".platform")),
        ("/definition/parts/0/payload", json!("not-base64")),
        ("/definition/parts/0/payloadType", json!("Other")),
        ("/definition/parts/0/payloadType", Value::Null),
    ] {
        let mut body = valid.clone();
        *body.pointer_mut(pointer).unwrap() = value;
        invalid.push(body);
    }
    let mut body = valid.clone();
    let part = body["definition"]["parts"][0].clone();
    body["definition"]["parts"]
        .as_array_mut()
        .unwrap()
        .push(part);
    invalid.push(body);
    for object in ["/definition", "/definition/parts/0"] {
        let mut body = valid.clone();
        body.pointer_mut(object)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), json!(true));
        invalid.push(body);
    }
    for body in invalid {
        assert_eq!(
            plan(ConflictPolicy::Skip, existing(), Some(body)),
            Err(CreateError::InputRejected)
        );
    }
}

#[test]
fn unsupported_policies_fail_for_missing_and_existing_targets() {
    for policy in [ConflictPolicy::Overwrite, ConflictPolicy::Duplicate] {
        for state in [Preflight::Leaf(LeafState::Missing), existing()] {
            assert_eq!(
                plan(policy, state, Some(json!({"displayName":NAME}))),
                Err(CreateError::UnsupportedPolicy)
            );
        }
    }
}

#[test]
fn exact_preflight_conflict_skip_missing_parent_and_ambiguity_remain_distinct() {
    assert_eq!(
        plan(ConflictPolicy::Error, existing(), None),
        Err(CreateError::AlreadyExists)
    );
    let skipped = plan(ConflictPolicy::Skip, existing(), None).unwrap();
    assert!(matches!(skipped.action(), CreateAction::Skip { .. }));
    assert!(LakehouseCreateRequest::from_plan(&skipped).is_err());
    for policy in [ConflictPolicy::Error, ConflictPolicy::Skip] {
        assert!(
            LakehouseCreateRequest::from_plan(
                &plan(policy, Preflight::Leaf(LeafState::Missing), None).unwrap()
            )
            .is_ok()
        );
        assert_eq!(
            plan(policy, Preflight::MissingParent, None),
            Err(CreateError::MissingParent)
        );
        assert!(plan(policy, Preflight::Leaf(LeafState::Ambiguous), None).is_err());
        assert!(plan(policy, Preflight::Leaf(LeafState::WrongKind), None).is_err());
    }
}

#[test]
fn target_schema_parent_and_identity_mismatches_are_rejected_offline() {
    let schema = LakehouseCreateSchema;
    let reference = SchemaRef::new(SCHEMA).unwrap();
    for (scope, workspace, name, kind) in [
        (TargetScope::ParentCollection, WORKSPACE, NAME, "Lakehouse"),
        (TargetScope::NamedLeaf, WORKSPACE, NAME, "Notebook"),
        (TargetScope::NamedLeaf, WORKSPACE, NAME, "lakehouse"),
        (TargetScope::NamedLeaf, "../workspace", NAME, "Lakehouse"),
        (TargetScope::NamedLeaf, "not-a-uuid", NAME, "Lakehouse"),
    ] {
        let target =
            CreateTarget::new(scope, NativeId::new(workspace).unwrap(), name, kind).unwrap();
        assert_eq!(
            schema.validate(&target, &reference, None),
            Err(CreateError::InputRejected)
        );
    }
    assert_eq!(
        schema.validate(&target(), &SchemaRef::new("other/v1").unwrap(), None),
        Err(CreateError::InputRejected)
    );
    let wrong_workspace = Preflight::Leaf(LeafState::Existing(
        ResourceIdentity::new(
            NativeId::new(ITEM).unwrap(),
            NativeId::new(LABEL).unwrap(),
            "Lakehouse",
        )
        .unwrap(),
    ));
    assert!(plan(ConflictPolicy::Skip, wrong_workspace, None).is_err());
}

#[test]
fn native_constructor_cannot_inherit_forged_overwrite_or_duplicate_capabilities() {
    let mut capabilities = LakehouseCreateSchema::capabilities().unwrap();
    capabilities.replacement = Some(ReplacementUnit::new("pretend/v1").unwrap());
    capabilities.duplicate = DuplicateSupport::FreshIdentityWithRepeatedNames;
    let overwritten = plan_create(
        ConflictPolicy::Overwrite,
        target(),
        Preflight::Leaf(LeafState::Missing),
        &capabilities,
        None,
        &LakehouseCreateSchema,
    )
    .unwrap();
    assert!(LakehouseCreateRequest::from_plan(&overwritten).is_err());
}

struct Recorder {
    operations: Vec<ResourceOperation>,
    result: Result<ResourceOutcome, ResourceError>,
}

impl ResourceOperations for Recorder {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.operations.push(operation);
        let result = self.result.clone();
        Box::pin(async move { result })
    }
}

#[tokio::test]
async fn native_execution_preserves_raw_metadata_handles_and_uncertain_errors() {
    let mut uncertain = ResourceError::input();
    uncertain.code = "Cancelled".into();
    uncertain.operation_id = Some("operation-not-resource-id".into());
    uncertain.mutation_may_have_occurred = true;
    let data = json!({
        "id":ITEM,"workspaceId":WORKSPACE,"displayName":NAME,"type":"Lakehouse",
        "properties":{"sqlEndpointProperties":{"id":LABEL,"provisioningStatus":"InProgress"}},
        "future":{"native":null}
    });
    for result in [
        Ok(ResourceOutcome::Completed {
            status: 201,
            data: Some(data),
        }),
        Ok(ResourceOutcome::Accepted {
            operation_id: Some("operation-not-resource-id".into()),
        }),
        Err(uncertain),
    ] {
        let mut recorder = Recorder {
            operations: Vec::new(),
            result: result.clone(),
        };
        assert_eq!(
            request(None)
                .execute(&mut recorder, CompletionPolicy::ReturnAccepted)
                .await,
            result
        );
        assert_eq!(recorder.operations.len(), 1);
        assert_eq!(recorder.operations[0].method(), ResourceMethod::Post);
    }
}
