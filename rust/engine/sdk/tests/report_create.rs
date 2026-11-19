// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native Report create schema consumers; no service or model access.

#![deny(unnameable_test_items)]

use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_sdk::{
    items::create::{
        ConflictPolicy, CreateAction, CreateError, CreateInputValidator, CreateTarget,
        InputDisposition, LeafState, NativeId, Preflight, ResourceIdentity, SchemaRef, TargetScope,
        plan_create,
    },
    reports::create::{ReportCreateValidator, SCHEMA, capabilities},
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Value, json};
use std::{future::Future, num::NonZeroUsize, pin::Pin};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const OTHER_WS: &str = "33333333-3333-3333-3333-333333333333";
const MODEL: &str = "44444444-4444-4444-4444-444444444444";
const NAME: &str = "Report";

fn target() -> CreateTarget {
    CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WS).unwrap(),
        NAME,
        "Report",
    )
    .unwrap()
}

fn validator() -> ReportCreateValidator {
    ReportCreateValidator::new(NonZeroUsize::new(64 * 1024).unwrap())
}

fn part(path: &str, bytes: &[u8]) -> Value {
    json!({
        "path":path,"payloadType":"InlineBase64","payload":STANDARD.encode(bytes),
        "futurePart":{"keep":null}
    })
}

fn body() -> Value {
    let properties = json!({
        "$schema":"https://developer.microsoft.com/json-schemas/fabric/item/report/definitionProperties/2.0.0/schema.json",
        "version":"4.0",
        "datasetReference":{"byConnection":{"connectionString":
            format!("Data Source=powerbi://api.powerbi.com/v1.0/myorg/{OTHER_WS};Initial Catalog={MODEL}")
        }},
        "futureProperties":true
    });
    json!({
        "displayName":NAME,"description":"Native definition","folderId":null,
        "definition":{"format":"PBIR","futureDefinition":{"keep":null},"parts":[
            part("definition.pbir", &serde_json::to_vec(&properties).unwrap()),
            part("definition/version.json", br#"{"version":"2.0.0"}"#),
            part("definition/report.json", br#"{"futureReport":null}"#),
            part("definition/pages/page/page.json", br#"{"name":"page"}"#),
            part("StaticResources/Exact Image.bin", &[0,255,128,0])
        ]}
    })
}

#[test]
fn complete_native_request_keeps_binary_parts_extensions_and_inert_model_reference() {
    let supplied = body();
    let read = validator().inspect(&target(), &supplied).unwrap();
    assert_eq!(read.data(), &supplied);
    assert_eq!(
        read.part_bytes("StaticResources/Exact Image.bin").unwrap(),
        [0, 255, 128, 0]
    );
    assert!(!read.evidence().related_model_authorized);
    assert_eq!(
        read.evidence().model_reference.as_ref().unwrap()["byConnection"]["connectionString"],
        format!(
            "Data Source=powerbi://api.powerbi.com/v1.0/myorg/{OTHER_WS};Initial Catalog={MODEL}"
        )
    );
    assert_eq!(
        validator()
            .validate(&target(), &SchemaRef::new(SCHEMA).unwrap(), Some(&supplied))
            .unwrap(),
        InputDisposition::CreateOnly
    );
}

#[test]
fn legacy_is_native_json_not_pbix_and_by_path_is_not_resolved() {
    let properties = json!({
        "version":"1.0",
        "datasetReference":{"byPath":{"path":"../Existing.SemanticModel"}}
    });
    let supplied = json!({"displayName":NAME,"definition":{
        "format":"PBIR-Legacy","parts":[
            part("definition.pbir", &serde_json::to_vec(&properties).unwrap()),
            part("report.json", br#"{"futureLegacy":true}"#),
            part("StaticResources/image.bin", &[0xff])
        ]
    }});
    let read = validator().inspect(&target(), &supplied).unwrap();
    assert_eq!(read.data(), &supplied);
    assert!(!read.evidence().related_model_authorized);
    assert_eq!(
        read.evidence().format.layout.as_deref(),
        Some("PBIR-Legacy")
    );
}

#[test]
fn missing_null_unknown_fields_and_unadvertised_modes_are_rejected() {
    let adapter = validator();
    let schema = SchemaRef::new(SCHEMA).unwrap();
    assert!(adapter.validate(&target(), &schema, None).is_err());
    for supplied in [Value::Null, json!({}), json!([])] {
        assert!(adapter.inspect(&target(), &supplied).is_err());
    }
    for field in [
        "displayName",
        "description",
        "definition",
        "type",
        "creationPayload",
        "sensitivityLabelSettings",
        "unknown",
    ] {
        let mut supplied = body();
        supplied[field] = Value::Null;
        assert!(
            adapter.inspect(&target(), &supplied).is_err(),
            "must reject invalid or unadvertised {field}"
        );
    }
    let mut supplied = body();
    supplied["description"] = json!("x".repeat(257));
    assert!(adapter.inspect(&target(), &supplied).is_err());
    supplied = body();
    supplied["folderId"] = json!("not-a-uuid");
    assert!(adapter.inspect(&target(), &supplied).is_err());
}

#[test]
fn wrong_schema_kind_parent_scope_or_name_cannot_validate_input() {
    let supplied = body();
    assert!(matches!(
        validator().validate(
            &target(),
            &SchemaRef::new("unknown").unwrap(),
            Some(&supplied)
        ),
        Err(CreateError::InputRejected)
    ));
    for target in [
        CreateTarget::new(
            TargetScope::NamedLeaf,
            NativeId::new(WS).unwrap(),
            NAME,
            "Notebook",
        ),
        CreateTarget::new(
            TargetScope::NamedLeaf,
            NativeId::new("parent").unwrap(),
            NAME,
            "Report",
        ),
        CreateTarget::new(
            TargetScope::ParentCollection,
            NativeId::new(WS).unwrap(),
            NAME,
            "Report",
        ),
        CreateTarget::new(
            TargetScope::NamedLeaf,
            NativeId::new(WS).unwrap(),
            "Different",
            "Report",
        ),
    ] {
        assert!(validator().inspect(&target.unwrap(), &supplied).is_err());
    }
}

#[test]
fn partial_unknown_and_mixed_definition_evidence_never_validate_as_create_input() {
    for path in [
        "definition.pbir",
        "definition/version.json",
        "definition/report.json",
        "definition/pages/page/page.json",
    ] {
        let mut supplied = body();
        supplied["definition"]["parts"]
            .as_array_mut()
            .unwrap()
            .retain(|part| part["path"] != path);
        assert!(validator().inspect(&target(), &supplied).is_err());
    }
    for properties in [
        json!({"version":"future","datasetReference":{"future":true}}),
        json!({"$schema":"https://example.invalid/future","version":"4.0",
            "datasetReference":{"byPath":{"path":"../Existing.SemanticModel"}}}),
        json!({"version":"4.0","datasetReference":{"byConnection":{}}}),
        json!({"version":"4.0"}),
    ] {
        let mut supplied = body();
        supplied["definition"]["parts"][0] =
            part("definition.pbir", &serde_json::to_vec(&properties).unwrap());
        assert!(validator().inspect(&target(), &supplied).is_err());
    }
    for extra in [
        part("report.json", b"{}"),
        part("definition/report.json", b"{}"),
    ] {
        let mut supplied = body();
        supplied["definition"]["parts"]
            .as_array_mut()
            .unwrap()
            .push(extra);
        assert!(validator().inspect(&target(), &supplied).is_err());
    }
    for value in [json!("PBIX"), Value::Null] {
        let mut supplied = body();
        supplied["definition"]["format"] = value;
        assert!(validator().inspect(&target(), &supplied).is_err());
    }
    for bytes in [b"null".as_slice(), b"{malformed", &[0xff]] {
        let mut supplied = body();
        supplied["definition"]["parts"][2] = part("definition/report.json", bytes);
        assert!(validator().inspect(&target(), &supplied).is_err());
    }
}

#[test]
fn exact_body_byte_budget_is_inclusive() {
    let supplied = body();
    let size = serde_json::to_vec(&supplied).unwrap().len();
    assert!(
        ReportCreateValidator::new(NonZeroUsize::new(size).unwrap())
            .inspect(&target(), &supplied)
            .is_ok()
    );
    assert!(
        ReportCreateValidator::new(NonZeroUsize::new(size - 1).unwrap())
            .inspect(&target(), &supplied)
            .is_err()
    );
}

#[derive(Default)]
struct Capture {
    requests: Vec<ResourceOperation>,
    outcome: Option<ResourceOutcome>,
    failure: Option<ResourceError>,
}

impl ResourceOperations for Capture {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        self.requests.push(operation);
        let result = if let Some(error) = self.failure.take() {
            Err(error)
        } else {
            Ok(self.outcome.clone().unwrap())
        };
        Box::pin(std::future::ready(result))
    }
}

fn existing() -> Preflight {
    Preflight::Leaf(LeafState::Existing(
        ResourceIdentity::new(
            NativeId::new(MODEL).unwrap(),
            NativeId::new(WS).unwrap(),
            "Report",
        )
        .unwrap(),
    ))
}

#[test]
fn native_policy_support_rejects_overwrite_and_duplicate_even_when_missing() {
    for preflight in [Preflight::Leaf(LeafState::Missing), existing()] {
        for policy in [ConflictPolicy::Overwrite, ConflictPolicy::Duplicate] {
            assert!(matches!(
                plan_create(
                    policy,
                    target(),
                    preflight.clone(),
                    &capabilities(),
                    Some(body()),
                    &validator()
                ),
                Err(CreateError::UnsupportedPolicy)
            ));
        }
    }
    assert!(matches!(
        plan_create(
            ConflictPolicy::Error,
            target(),
            existing(),
            &capabilities(),
            Some(body()),
            &validator()
        ),
        Err(CreateError::AlreadyExists)
    ));
    let plan = plan_create(
        ConflictPolicy::Skip,
        target(),
        existing(),
        &capabilities(),
        Some(body()),
        &validator(),
    )
    .unwrap();
    assert!(matches!(plan.action(), CreateAction::Skip { .. }));
    let mut invalid = body();
    invalid["definition"] = Value::Null;
    assert!(matches!(
        plan_create(
            ConflictPolicy::Skip,
            target(),
            existing(),
            &capabilities(),
            Some(invalid),
            &validator()
        ),
        Err(CreateError::InputRejected)
    ));
}

#[tokio::test]
async fn native_create_delegates_one_exact_report_post_with_original_body_and_completion() {
    let supplied = body();
    let operation = Some("55555555-5555-5555-5555-555555555555".to_owned());
    for policy in [ConflictPolicy::Error, ConflictPolicy::Skip] {
        for completion in [
            CompletionPolicy::ReturnAccepted,
            CompletionPolicy::WaitForResult,
        ] {
            let plan = plan_create(
                policy,
                target(),
                Preflight::Leaf(LeafState::Missing),
                &capabilities(),
                Some(supplied.clone()),
                &validator(),
            )
            .unwrap();
            let mut capture = Capture {
                outcome: Some(ResourceOutcome::Accepted {
                    operation_id: operation.clone(),
                }),
                ..Capture::default()
            };
            let outcome = validator()
                .submit(&mut capture, &plan, completion)
                .await
                .unwrap();
            assert_eq!(
                outcome,
                ResourceOutcome::Accepted {
                    operation_id: operation.clone()
                }
            );
            assert_eq!(capture.requests.len(), 1);
            assert_eq!(capture.requests[0].method(), ResourceMethod::Post);
            assert_eq!(
                capture.requests[0].path(),
                format!("/v1/workspaces/{WS}/reports")
            );
            assert_eq!(capture.requests[0].body(), Some(&supplied));
            assert_eq!(capture.requests[0].completion(), completion);
        }
    }
}

#[tokio::test]
async fn unchanged_plan_is_not_a_write_and_mutation_failure_is_not_replayed() {
    let plan = plan_create(
        ConflictPolicy::Skip,
        target(),
        existing(),
        &capabilities(),
        Some(body()),
        &validator(),
    )
    .unwrap();
    let mut capture = Capture::default();
    assert!(
        validator()
            .submit(&mut capture, &plan, CompletionPolicy::ReturnAccepted)
            .await
            .is_err()
    );
    assert!(capture.requests.is_empty());
    let plan = plan_create(
        ConflictPolicy::Error,
        target(),
        Preflight::Leaf(LeafState::Missing),
        &capabilities(),
        Some(body()),
        &validator(),
    )
    .unwrap();
    capture.failure = Some(ResourceError::input());
    assert!(
        validator()
            .submit(&mut capture, &plan, CompletionPolicy::ReturnAccepted)
            .await
            .is_err()
    );
    assert_eq!(capture.requests.len(), 1);
}
