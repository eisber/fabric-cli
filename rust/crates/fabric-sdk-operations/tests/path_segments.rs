// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Validated operation components preserve route, payload and endpoint policies.

#![deny(unnameable_test_items)]

use fabric_sdk_operations::FabricOperation;
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};

#[test]
fn raw_constructor_still_rejects_encoded_paths() {
    assert!(
        FabricOperation::new(
            ResourceMethod::Delete,
            "/v1/items/My%20Shortcut",
            Vec::new(),
            CompletionPolicy::PythonSynchronous,
        )
        .is_err()
    );
}

#[test]
fn literal_segments_encode_unicode_spaces_and_percent_without_origin_or_traversal() {
    let operation = FabricOperation::from_path_segments(
        ResourceMethod::Delete,
        &["v1", "items", "Équipe été", "%2e%2e", "%2f"],
        &[],
        Vec::new(),
        CompletionPolicy::PythonSynchronous,
    )
    .unwrap();
    assert_eq!(operation.method(), ResourceMethod::Delete);
    assert_eq!(
        operation.path(),
        "/v1/items/%C3%89quipe%20%C3%A9t%C3%A9/%252e%252e/%252f"
    );
}

#[test]
fn unsafe_literal_components_fail_without_relaxing_raw_path_policy() {
    for path in [
        vec![],
        vec!["v1"],
        vec!["https:", "example"],
        vec!["v1", ""],
        vec!["v1", "."],
        vec!["v1", ".."],
        vec!["v1", "a/b"],
        vec!["v1", "a\\b"],
        vec!["v1", "\0"],
        vec!["v1", "\r\n"],
    ] {
        assert!(
            FabricOperation::from_path_segments(
                ResourceMethod::Get,
                &path,
                &[],
                Vec::new(),
                CompletionPolicy::PythonSynchronous
            )
            .is_err()
        );
    }
}

#[test]
fn encoded_query_and_body_validation_use_the_same_operation_contract() {
    let operation = FabricOperation::from_path_segments(
        ResourceMethod::Get,
        &["v1", "items"],
        &[("continuationToken", "next+/=")],
        Vec::new(),
        CompletionPolicy::PythonSynchronous,
    )
    .unwrap();
    assert_eq!(
        operation.path(),
        "/v1/items?continuationToken=next%2B%2F%3D"
    );
    for (method, body) in [
        (ResourceMethod::Get, b"{}".to_vec()),
        (ResourceMethod::Post, b"[]".to_vec()),
    ] {
        assert!(
            FabricOperation::from_path_segments(
                method,
                &["v1", "items"],
                &[],
                body,
                CompletionPolicy::PythonSynchronous
            )
            .is_err()
        );
    }
}
