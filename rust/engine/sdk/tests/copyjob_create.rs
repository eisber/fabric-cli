// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public metadata-only CopyJob schema and immutable plan admission.
#![deny(unnameable_test_items)]

use fabric_sdk::{
    copyjobs::create::{CopyJobCreate, MAX_INPUT_BYTES, SCHEMA},
    items::create::{
        ConflictPolicy, CreateError, CreateInputValidator, CreateTarget, LeafState, NativeId,
        Preflight, ReplacementUnit, SchemaRef, TargetScope, plan_create,
    },
};
use serde_json::{Value, json};

const WS: &str = "11111111-1111-1111-1111-111111111111";

fn target(workspace: &str, name: &str, owner: &str) -> CreateTarget {
    CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(workspace).unwrap(),
        name,
        owner,
    )
    .unwrap()
}

fn plan(value: Option<Value>) -> fabric_sdk::items::create::CreatePlan {
    plan_create(
        ConflictPolicy::Error,
        target(WS, "Exact", "CopyJob"),
        Preflight::Leaf(LeafState::Missing),
        &CopyJobCreate.capabilities(),
        value,
        &CopyJobCreate,
    )
    .unwrap()
}

#[test]
fn metadata_schema_is_explicit_and_never_advertises_definition_or_connectors() {
    let schema = CopyJobCreate.input_schema();
    assert_eq!(schema["required"], json!(["displayName"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"].as_object().unwrap().len(), 2);
    assert_eq!(schema["properties"]["description"]["maxLength"], 256);
}

#[test]
fn only_omitted_or_complete_explicit_metadata_can_be_planned() {
    for value in [
        None,
        Some(json!({"displayName":"Exact"})),
        Some(json!({"displayName":"Exact","description":""})),
        Some(json!({"displayName":"Exact","description":"x".repeat(256)})),
    ] {
        plan(value);
    }
    for value in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"displayName":null}),
        json!({"displayName":42}),
        json!({"displayName":"exact"}),
        json!({"displayName":"Exact","description":null}),
        json!({"displayName":"Exact","description":false}),
        json!({"displayName":"Exact","description":"x".repeat(257)}),
    ] {
        assert_eq!(
            CopyJobCreate.validate(
                &target(WS, "Exact", "CopyJob"),
                &SchemaRef::new(SCHEMA).unwrap(),
                Some(&value),
            ),
            Err(CreateError::InputRejected)
        );
    }
    for field in [
        "definition",
        "source",
        "destination",
        "connectionId",
        "jobMode",
        "schedule",
        "type",
        "folderId",
        "sensitivityLabelSettings",
        "defaultIdentity",
    ] {
        let mut value = json!({"displayName":"Exact"});
        value[field] = Value::Null;
        assert!(
            CopyJobCreate
                .validate(
                    &target(WS, "Exact", "CopyJob"),
                    &SchemaRef::new(SCHEMA).unwrap(),
                    Some(&value),
                )
                .is_err(),
            "{field} is outside metadata-only scope"
        );
    }
}

#[test]
fn schema_owner_workspace_and_literal_name_are_validated_without_io() {
    for invalid in [
        target("not-a-guid", "Exact", "CopyJob"),
        target(WS, "Exact", "DataPipeline"),
        target(WS, " Exact", "CopyJob"),
        target(WS, "Exact ", "CopyJob"),
        target(WS, "a/b", "CopyJob"),
        target(WS, "a%2fb", "CopyJob"),
        target(WS, ".", "CopyJob"),
        target(WS, &"x".repeat(257), "CopyJob"),
    ] {
        assert!(
            CopyJobCreate
                .validate(&invalid, &SchemaRef::new(SCHEMA).unwrap(), None)
                .is_err()
        );
    }
    assert!(
        CopyJobCreate
            .validate(
                &target(WS, "Exact", "CopyJob"),
                &SchemaRef::new("other/v1").unwrap(),
                None,
            )
            .is_err()
    );
    let collection = CreateTarget::new(
        TargetScope::ParentCollection,
        NativeId::new(WS).unwrap(),
        "Exact",
        "CopyJob",
    )
    .unwrap();
    assert!(
        CopyJobCreate
            .validate(&collection, &SchemaRef::new(SCHEMA).unwrap(), None)
            .is_err()
    );
}

#[test]
fn raw_bytes_must_match_the_planned_unique_key_object_without_substitution() {
    let explicit = plan(Some(json!({"displayName":"Exact"})));
    let omitted = plan(None);
    for wait in [false, true] {
        assert!(CopyJobCreate.operation(&omitted, None, wait).is_ok());
        assert!(
            CopyJobCreate
                .operation(
                    &explicit,
                    Some(b" { \"displayName\" : \"Exact\" }\r\n"),
                    wait
                )
                .is_ok()
        );
    }
    for bytes in [
        b"null".as_slice(),
        b"",
        br#"{"displayName":"Other"}"#,
        br#"{"displayName":"Exact","displayName":"Exact"}"#,
        br#"{"displayName":"Exact","\u0064isplayName":"Exact"}"#,
        br#"{"displayName":"Exact"} {}"#,
        br#"{"displayName":"Exact","definition":{}}"#,
        &[0xff],
    ] {
        assert!(
            CopyJobCreate
                .operation(&explicit, Some(bytes), false)
                .is_err()
        );
    }
    assert!(CopyJobCreate.operation(&explicit, None, false).is_err());
    assert!(
        CopyJobCreate
            .operation(&omitted, Some(br#"{"displayName":"Exact"}"#), false)
            .is_err()
    );
    assert!(
        CopyJobCreate
            .operation(&explicit, Some(&vec![b' '; MAX_INPUT_BYTES + 1]), false)
            .is_err()
    );
}

#[test]
fn provider_collision_is_not_overwrite_or_duplicate_capability() {
    for policy in [ConflictPolicy::Overwrite, ConflictPolicy::Duplicate] {
        assert_eq!(
            plan_create(
                policy,
                target(WS, "Exact", "CopyJob"),
                Preflight::Leaf(LeafState::Missing),
                &CopyJobCreate.capabilities(),
                None,
                &CopyJobCreate,
            ),
            Err(CreateError::UnsupportedPolicy)
        );
    }
}

#[test]
fn operation_rechecks_native_policy_even_if_another_planner_advertised_overwrite() {
    let mut broader_capabilities = CopyJobCreate.capabilities();
    broader_capabilities.replacement = Some(ReplacementUnit::new("other/full-unit/v1").unwrap());
    let foreign_plan = plan_create(
        ConflictPolicy::Overwrite,
        target(WS, "Exact", "CopyJob"),
        Preflight::Leaf(LeafState::Missing),
        &broader_capabilities,
        None,
        &CopyJobCreate,
    )
    .unwrap();
    assert!(matches!(
        CopyJobCreate.operation(&foreign_plan, None, false),
        Err(CreateError::UnsupportedPolicy)
    ));
}
