// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_sdk_operations::FabricOperation;
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod, ResourceOperation};
use serde_json::json;

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
    let operation = ResourceOperation::encoded(
        ResourceMethod::Delete,
        &["items", "Équipe été", "%2e%2e", "%2f"],
        None,
        CompletionPolicy::PythonSynchronous,
    )
    .unwrap();
    assert_eq!(operation.method(), ResourceMethod::Delete);
    assert_eq!(
        operation.path(),
        "/v1/items/%C3%89quipe%20%C3%A9t%C3%A9/%252e%252e/%252f"
    );
    assert!(FabricOperation::from_resource(&operation).is_ok());
}

#[test]
fn unsafe_literal_components_fail_without_relaxing_raw_path_policy() {
    for path in [
        vec![],
        vec![""],
        vec!["."],
        vec![".."],
        vec!["a/b"],
        vec!["a\\b"],
        vec!["\0"],
        vec!["\r\n"],
    ] {
        assert!(
            ResourceOperation::encoded(
                ResourceMethod::Get,
                &path,
                None,
                CompletionPolicy::PythonSynchronous
            )
            .is_err()
        );
    }
}

#[test]
fn encoded_query_and_body_validation_use_the_same_operation_contract() {
    let operation = ResourceOperation::encoded(
        ResourceMethod::Get,
        &["items"],
        None,
        CompletionPolicy::PythonSynchronous,
    )
    .unwrap()
    .with_query(&[("continuationToken", "next+/=")])
    .unwrap();
    assert_eq!(
        operation.path(),
        "/v1/items?continuationToken=next%2B%2F%3D"
    );
    for (method, body) in [
        (ResourceMethod::Get, Some(json!({}))),
        (ResourceMethod::Post, Some(json!([]))),
    ] {
        assert!(
            ResourceOperation::encoded(
                method,
                &["items"],
                body,
                CompletionPolicy::PythonSynchronous
            )
            .is_err()
        );
    }
}
