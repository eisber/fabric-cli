// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Direct focused-package definition contracts, independent of the SDK facade.

#![deny(unnameable_test_items)]

use fabric_sdk_notebooks::{DefinitionPart, NotebookDefinition, NotebookErrorKind};
use serde_json::json;

#[test]
fn inline_definition_preserves_content_and_wire_names() {
    let definition = NotebookDefinition {
        format: Some("ipynb".into()),
        parts: vec![DefinitionPart::inline("notebook-content.ipynb", b"{}").unwrap()],
    };
    assert_eq!(
        serde_json::to_value(definition).unwrap(),
        json!({
            "format": "ipynb",
            "parts": [{
                "path": "notebook-content.ipynb",
                "payload": "e30=",
                "payloadType": "InlineBase64"
            }]
        })
    );
}

#[test]
fn inline_definition_rejects_unsafe_paths() {
    for path in [
        "",
        "/notebook",
        "../notebook",
        "a//b",
        "a\\b",
        "C:notebook",
        "a/./b",
    ] {
        assert!(matches!(
            DefinitionPart::inline(path, b"{}"),
            Err(NotebookErrorKind::InvalidInput)
        ));
    }
}
