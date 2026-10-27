// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive immutable actual-source config consumers; old 44 fixtures unchanged.

use fabric_deploy::deployment_config::{ConfigurationError, DeploymentConfig};
use serde_json::{Value, json};

#[test]
fn selected_config_settings_match_actual_pinned_source() {
    let fixture: Value = serde_json::from_str(include_str!("config_source.json")).unwrap();
    assert_eq!(
        fixture["source"],
        "microsoft/fabric-cicd@5ca096c00a9a878c646c59fe537b883eba951329"
    );
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 12);
    for case in cases {
        let result = DeploymentConfig::from_document(
            &case["config"],
            case["environment"].as_str().unwrap(),
            Some(&case["overrides"]),
        );
        if case["expected"]["errors"].as_array().unwrap().is_empty() {
            let result = result.unwrap_or_else(|error| panic!("{}: {error}", case["id"]));
            assert_eq!(
                result.document, case["expected"]["config"],
                "{}",
                case["id"]
            );
            assert_eq!(
                Value::Object(result.workspace),
                case["expected"]["workspace"],
                "{}",
                case["id"]
            );
            assert_eq!(
                Value::Object(result.publish),
                case["expected"]["publish"],
                "{}",
                case["id"]
            );
            assert_eq!(
                Value::Object(result.unpublish),
                case["expected"]["unpublish"],
                "{}",
                case["id"]
            );
        } else {
            assert!(
                matches!(result, Err(ConfigurationError::Invalid(errors)) if !errors.is_empty()),
                "{}",
                case["id"]
            );
        }
    }
}

fn config() -> Value {
    json!({
        "core": {
            "workspace_id": "11111111-1111-1111-1111-111111111111",
            "repository_directory": "repository",
            "item_types_in_scope": ["Notebook"],
        },
        "unpublish": {"skip": true},
    })
}

#[test]
fn notebook_publish_requires_explicit_non_destructive_configuration() {
    DeploymentConfig::from_document(&config(), "N/A", None)
        .unwrap()
        .require_notebook_publish()
        .unwrap();
    let mut document = config();
    document.as_object_mut().unwrap().remove("unpublish");
    let error = DeploymentConfig::from_document(&document, "N/A", None)
        .unwrap()
        .require_notebook_publish()
        .unwrap_err();
    assert!(
        matches!(error, ConfigurationError::Unsupported(branch) if branch.starts_with("unpublish"))
    );
}

#[test]
fn unsupported_source_fields_never_disappear_into_success() {
    for (field, value) in [
        ("parameter", json!("parameter.yml")),
        ("item_types_in_scope", json!(["Notebook", "DataPipeline"])),
    ] {
        let mut document = config();
        document["core"][field] = value;
        assert!(matches!(
            DeploymentConfig::from_document(&document, "N/A", None)
                .unwrap()
                .require_notebook_publish(),
            Err(ConfigurationError::Unsupported(_))
        ));
    }
    for (section, value) in [
        ("features", json!(["enable_bulk_publish"])),
        ("constants", json!({"PARALLEL_MAX_WORKERS": 4})),
        ("publish", json!({"items_to_include": ["Book.Notebook"]})),
    ] {
        let mut document = config();
        document[section] = value;
        assert!(matches!(
            DeploymentConfig::from_document(&document, "N/A", None)
                .unwrap()
                .require_notebook_publish(),
            Err(ConfigurationError::Unsupported(_))
        ));
    }
}

#[test]
fn invalid_other_environment_is_not_hidden_by_selected_valid_environment() {
    let mut document = config();
    document["core"]["workspace_id"] = json!({
        "dev": "11111111-1111-1111-1111-111111111111",
        "prod": "not-a-guid",
    });
    assert!(matches!(
        DeploymentConfig::from_document(&document, "dev", None),
        Err(ConfigurationError::Invalid(_))
    ));
}

#[test]
fn config_does_not_invent_parameter_file_autodiscovery() {
    let selected = DeploymentConfig::from_document(&config(), "N/A", None).unwrap();
    assert!(!selected.workspace.contains_key("parameter_file_path"));
}

#[test]
fn source_forbidden_unpublish_fields_are_not_ignored_even_when_skipped() {
    let fixture: Value =
        serde_json::from_str(include_str!("operation_section_source.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 3);
    for case in fixture["cases"].as_array().unwrap() {
        let mut document = config();
        document["unpublish"] = case["input"].clone();
        let error = DeploymentConfig::from_document(&document, "dev", None).unwrap_err();
        let ConfigurationError::Invalid(errors) = error else {
            panic!("Expected source operation validation error");
        };
        assert_eq!(
            serde_json::to_value(errors).unwrap(),
            case["expectedErrors"]
        );
    }
}
