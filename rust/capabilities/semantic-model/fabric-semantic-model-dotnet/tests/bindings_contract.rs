// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use fabric_semantic_model_dotnet::{
    BindingError, CancellationToken, MAX_REQUEST_BYTES, validate_request_json,
};
use serde_json::json;

fn request() -> serde_json::Value {
    json!({
        "schemaVersion": "1",
        "format": "bim",
        "model": "{\"name\":\"offline\",\"compatibilityLevel\":1600,\"model\":{}}",
        "options": {},
        "evidence": {}
    })
}

#[test]
fn transport_accepts_a_standalone_bim_string() {
    assert_eq!(
        validate_request_json(&serde_json::to_vec(&request()).unwrap()),
        Ok(())
    );
}

#[test]
fn invalid_transport_never_becomes_an_empty_success() {
    for input in [b"not json".as_slice(), b"{}", b"null", b"[]", b"\xff"] {
        assert_eq!(
            validate_request_json(input),
            Err(BindingError::InvalidRequest)
        );
    }
    for field in ["options", "evidence"] {
        let mut value = request();
        value[field] = json!([]);
        assert_eq!(
            validate_request_json(&serde_json::to_vec(&value).unwrap()),
            Err(BindingError::InvalidRequest)
        );
    }
    let mut value = request();
    value["unexpected"] = json!(true);
    assert_eq!(
        validate_request_json(&serde_json::to_vec(&value).unwrap()),
        Err(BindingError::InvalidRequest)
    );
}

#[test]
fn transport_rejects_duplicate_fields_schema_and_tmdl() {
    assert_eq!(
        validate_request_json(
            br#"{"schemaVersion":"1","schemaVersion":"1","format":"bim","model":"{}","options":{},"evidence":{}}"#
        ),
        Err(BindingError::InvalidRequest)
    );
    let mut value = request();
    value["schemaVersion"] = json!("2");
    assert_eq!(
        validate_request_json(&serde_json::to_vec(&value).unwrap()),
        Err(BindingError::UnsupportedSchema)
    );
    value["schemaVersion"] = json!("1");
    value["format"] = json!("tmdl");
    assert_eq!(
        validate_request_json(&serde_json::to_vec(&value).unwrap()),
        Err(BindingError::UnsupportedFormat)
    );
}

#[test]
fn oversized_input_is_rejected_before_json_parsing() {
    assert_eq!(
        validate_request_json(&vec![b' '; MAX_REQUEST_BYTES + 1]),
        Err(BindingError::RequestTooLarge)
    );
}

#[test]
fn cloned_cancellation_state_is_shared() {
    let original = CancellationToken::new();
    let clone = original.clone();
    assert!(!original.is_cancelled());
    clone.cancel();
    assert!(original.is_cancelled());
}

#[cfg(not(feature = "dotnet"))]
#[test]
fn disabled_feature_does_not_inspect_or_load_configured_paths() {
    use fabric_semantic_model_dotnet::{Analyzer, HostOptions};
    assert_eq!(
        Analyzer::new(HostOptions::new("not-a-library", "not-a-directory")).unwrap_err(),
        BindingError::FeatureDisabled
    );
    assert_eq!(
        HostOptions::bundled("not-a-library").unwrap_err(),
        BindingError::FeatureDisabled
    );
}
