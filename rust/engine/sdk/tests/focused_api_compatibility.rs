// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Old public paths and focused owners expose the same nominal Rust types.

#![deny(unnameable_test_items)]

use fabric_sdk_notebooks::{DefinitionPart, NotebookDefinition};
use fabric_sdk_resource_contracts::{CompletionPolicy, Page, ResourceMethod, ResourceOperation};
use serde_json::json;

#[test]
fn notebook_facade_preserves_type_identity() {
    let direct = NotebookDefinition {
        format: None,
        parts: vec![DefinitionPart::inline("notebook-content.py", b"print(1)").unwrap()],
    };
    let facade: fabric_sdk::notebooks::NotebookDefinition = direct;
    let _: NotebookDefinition = facade;
}

#[test]
fn workspace_compatibility_paths_preserve_shared_type_identity() {
    let direct = ResourceOperation::new(
        ResourceMethod::Post,
        &["workspaces"],
        Some(json!({"displayName": "Offline"})),
        CompletionPolicy::ReturnAccepted,
    )
    .unwrap();
    let legacy: fabric_sdk_workspaces::mutations::ResourceOperation = direct;
    let _: ResourceOperation = legacy;

    let page = Page {
        value: vec![1],
        continuation_token: None,
        continuation_uri: None,
    };
    let legacy: fabric_sdk::Page<i32> = page;
    let _: Page<i32> = legacy;
}
