// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual source schema/transform expectations, captured before native rules.

use fabric_deploy::{
    deployment_config::{ConfigurationError, DeploymentConfig},
    parameters::LiteralParameters,
    transforms::NotebookTransforms,
};
use serde_json::{Value, json};

#[test]
fn actual_source_literal_schema_and_transforms_match() {
    let fixture: Value =
        serde_json::from_str(include_str!("literal_parameter_source.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 10);
    for case in fixture["cases"].as_array().unwrap() {
        let parameters = LiteralParameters::from_document(
            &case["parameters"],
            case["environment"].as_str().unwrap(),
        );
        if case["valid"] == false {
            assert!(
                matches!(parameters, Err(ConfigurationError::Invalid(_))),
                "{}",
                case["id"]
            );
        } else if case["id"] == "missing-environment" {
            assert!(matches!(
                parameters,
                Err(ConfigurationError::Unsupported(_))
            ));
        } else {
            let parameters = parameters.unwrap();
            let transformed = NotebookTransforms {
                logical_ids: Vec::new(),
                literals: parameters.literals().to_vec(),
            }
            .apply(
                case["input"].as_str().unwrap(),
                "11111111-1111-1111-1111-111111111111",
                1024,
            )
            .unwrap();
            assert_eq!(
                transformed,
                case["expected"].as_str().unwrap(),
                "{}",
                case["id"]
            );
        }
    }
}

#[test]
fn unsupported_parameter_branches_fail_before_publication() {
    for document in [
        json!({"key_value_replace": []}),
        json!({"extend": ["template.yml"]}),
        json!({"find_replace":[{"find_value":"(source)","replace_value":{"dev":"target"},"is_regex":"true"}]}),
        json!({"find_replace":[{"find_value":"source","replace_value":{"dev":"target"},"ignore_case":"true"}]}),
        json!({"find_replace":[{"find_value":"source","replace_value":{"dev":"$workspace.$id"}}]}),
        json!({"find_replace":[{"find_value":"source","replace_value":{"dev":"target"},"file_path":"*.py"}]}),
    ] {
        assert!(matches!(
            LiteralParameters::from_document(&document, "dev"),
            Err(ConfigurationError::Unsupported(_))
        ));
    }
}

#[test]
fn configured_parameters_cannot_be_ignored_or_reused_after_failed_resolution() {
    let mut config = DeploymentConfig::from_document(
        &json!({
            "core": {
                "workspace_id":"11111111-1111-1111-1111-111111111111",
                "repository_directory":"repository",
                "item_types_in_scope":["Notebook"],
                "parameter":"parameter.yml",
            },
            "unpublish":{"skip":true},
        }),
        "dev",
        None,
    )
    .unwrap();
    assert!(matches!(
        config.require_notebook_publish(),
        Err(ConfigurationError::Unsupported(_))
    ));
    config
        .resolve_parameters(
            &json!({"find_replace":[{"find_value":"source","replace_value":{"dev":"target"}}]}),
        )
        .unwrap();
    config.require_notebook_publish().unwrap();
    assert!(
        config
            .resolve_parameters(&json!({"find_replace":false}))
            .is_err()
    );
    assert!(config.parameters().is_none());
    assert!(config.require_notebook_publish().is_err());
}

#[test]
fn parameter_debug_does_not_expose_find_or_replacement_values() {
    let parameters = LiteralParameters::from_document(&json!({
        "find_replace":[{"find_value":"synthetic-private-find","replace_value":{"dev":"synthetic-private-replacement"}}],
    }), "dev").unwrap();
    let debug = format!("{parameters:?}");
    assert!(!debug.contains("synthetic-private"));
    assert!(debug.contains("rule_count"));
}
