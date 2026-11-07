// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The facade shares discovery types and constraints with the standalone SDK.
#![deny(unnameable_test_items)]

use fabric_sdk::discovery::{Operation, TargetKind, TargetSelector, discover, discover_for_facade};
use serde_json::json;

#[test]
fn facade_and_standalone_contracts_are_identical() {
    for (operation, path) in [
        (Operation::Create, "/New.Workspace"),
        (Operation::Update, "/Existing.Workspace"),
        (Operation::Delete, "/Existing.Workspace"),
        (Operation::Run, "/Existing.Workspace/Analysis.Notebook"),
    ] {
        let target = TargetSelector::parse(path).unwrap();
        let facade = discover(operation, &target).unwrap();
        let standalone = fabric_sdk_discovery::discover(operation, &target).unwrap();
        assert_eq!(
            serde_json::to_value(&facade).unwrap(),
            serde_json::to_value(&standalone).unwrap()
        );
        assert_eq!(facade.input_schema(), standalone.input_schema());
        assert_eq!(facade.mcp_tool(), standalone.mcp_tool());
    }
}

#[test]
fn facade_narrowing_cannot_change_or_broaden_the_contract() {
    let target = TargetSelector::parse("/New.Workspace").unwrap();
    let root = discover(Operation::Create, &target).unwrap();
    let alias = discover_for_facade(Operation::Create, &target, TargetKind::Workspace).unwrap();
    assert_eq!(
        serde_json::to_value(&root).unwrap(),
        serde_json::to_value(&alias).unwrap()
    );
    assert!(discover_for_facade(Operation::Create, &target, TargetKind::Notebook).is_err());
}

#[test]
fn facade_validation_uses_the_same_required_fields() {
    let target = TargetSelector::parse("/New.Workspace").unwrap();
    let contract = discover(Operation::Create, &target).unwrap();
    assert!(contract.validate_input(None).is_err());
    assert!(contract.validate_input(Some(&json!({}))).is_err());
    assert!(
        contract
            .validate_input(Some(&json!({"displayName": "New"})))
            .is_ok()
    );
    assert!(
        contract
            .validate_input(Some(&json!({"displayName": null})))
            .is_err()
    );
}
