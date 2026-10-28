// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fixed actual-source captures precede the native expectations in this file.

use fabric_deploy::{
    deployment_config::{CommandOptions, DeploymentConfig},
    parameter_transforms::{ParameterProgram, TransformBudget, TransformLimits},
};
use serde_json::{Value, json};

#[test]
fn thirty_additional_config_values_and_caller_flags_match_actual_source() {
    let fixture: Value =
        serde_json::from_str(include_str!("config_value_edges_source.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 30);
    for case in fixture["cases"].as_array().unwrap() {
        if case["kind"] == "config" {
            let result = DeploymentConfig::from_document(
                &case["config"],
                case["environment"].as_str().unwrap(),
                Some(&case["overrides"]),
            );
            assert_eq!(
                result.is_ok(),
                case["valid"].as_bool().unwrap(),
                "{}",
                case["id"]
            );
            if let Ok(config) = result {
                assert_eq!(
                    config.document, case["expected"]["document"],
                    "{}",
                    case["id"]
                );
                assert_eq!(
                    json!(config.workspace),
                    case["expected"]["workspace"],
                    "{}",
                    case["id"]
                );
                assert_eq!(
                    json!(config.publish),
                    case["expected"]["publish"],
                    "{}",
                    case["id"]
                );
                assert_eq!(
                    json!(config.unpublish),
                    case["expected"]["unpublish"],
                    "{}",
                    case["id"]
                );
                if case["id"] == "all-item-types" {
                    assert_eq!(config.item_types.len(), 33);
                    assert_eq!(
                        json!(config.item_types),
                        case["expected"]["workspace"]["item_types_in_scope"]
                    );
                }
            }
        } else {
            let config = DeploymentConfig::from_document(&json!({
                "core": { "workspace_id": "11111111-1111-1111-1111-111111111111", "repository_directory": "repository" },
            }), "dev", None).unwrap();
            let caller_flags = case["flags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_owned())
                .collect();
            let command = config
                .command_configuration(CommandOptions {
                    features: caller_flags,
                    ..CommandOptions::default()
                })
                .unwrap();
            let mut features = command.features().to_vec();
            features.sort();
            assert_eq!(json!(features), case["expected"]);
        }
    }
}

#[test]
fn all_twenty_two_source_environment_flags_and_scoped_overrides_match() {
    let fixture: Value = serde_json::from_str(include_str!("flag_protocol_source.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 22);
    let base = json!({
        "core": { "workspace_id": "11111111-1111-1111-1111-111111111111", "repository_directory": "repository" },
        "unpublish": { "skip": true },
    });
    for case in fixture["cases"].as_array().unwrap() {
        let mut document = base.clone();
        if case["kind"] == "environment-flag" {
            document["constants"] = json!({"VALID_ENABLE_FLAGS": case["flags"]});
            let config = DeploymentConfig::from_document(&document, "dev", None)
                .unwrap()
                .command_configuration(CommandOptions::default())
                .unwrap();
            let result = config.environment_flag_enabled(case["input"].as_str().unwrap());
            assert_eq!(result.is_ok(), case["valid"].as_bool().unwrap(), "{case}");
            if let Ok(value) = result {
                assert_eq!(json!(value), case["expected"], "{case}");
            }
        } else {
            for (key, value) in case["config"].as_object().unwrap() {
                document[key] = value.clone();
            }
            let options = CommandOptions {
                features: vec!["base".into()],
                constants: serde_json::Map::from_iter([("VERSION".into(), json!("1.4.0"))]),
                ..CommandOptions::default()
            };
            let config = DeploymentConfig::from_document(&document, "dev", None)
                .unwrap()
                .command_configuration(options.clone())
                .unwrap();
            let mut features = config.features().to_vec();
            features.sort();
            assert_eq!(json!(features), case["expected"]["features"]);
            assert_eq!(config.constants()["VERSION"], case["expected"]["version"]);
            let restored = DeploymentConfig::from_document(&base, "dev", None)
                .unwrap()
                .command_configuration(options)
                .unwrap();
            assert_eq!(json!(restored.features()), case["restored"]["features"]);
            assert_eq!(restored.constants()["VERSION"], case["restored"]["version"]);
        }
    }
}

#[test]
fn configured_strings_and_all_config_fields_match_fifty_source_cases() {
    let fixture: Value =
        serde_json::from_str(include_str!("configured_flags_source.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 50);
    for case in fixture["cases"].as_array().unwrap() {
        let environment = case["environment"].as_str().unwrap();
        if case["kind"] == "parameter" {
            let result = ParameterProgram::with_default_limits(&case["parameters"], environment);
            assert_eq!(
                result.is_ok(),
                case["valid"].as_bool().unwrap(),
                "{}",
                case["id"]
            );
            if let Ok(program) = result {
                let result = program
                    .apply(
                        case["input"].as_str().unwrap(),
                        "content.py",
                        &mut TransformBudget::new(TransformLimits::default(), &|| false),
                    )
                    .unwrap();
                assert_eq!(result, case["expected"].as_str().unwrap(), "{}", case["id"]);
            }
        } else {
            let result = DeploymentConfig::from_document(
                &case["config"],
                environment,
                Some(&case["overrides"]),
            );
            assert_eq!(
                result.is_ok(),
                case["valid"].as_bool().unwrap(),
                "{}",
                case["id"]
            );
            if let Ok(config) = result {
                let expected = &case["expected"];
                assert_eq!(config.document, expected["document"], "{}", case["id"]);
                assert_eq!(
                    json!(config.workspace),
                    expected["workspace"],
                    "{}",
                    case["id"]
                );
                assert_eq!(json!(config.publish), expected["publish"], "{}", case["id"]);
                assert_eq!(
                    json!(config.unpublish),
                    expected["unpublish"],
                    "{}",
                    case["id"]
                );
                let command = config
                    .command_configuration(CommandOptions::default())
                    .unwrap();
                assert_eq!(
                    json!(command.features()),
                    expected["features"],
                    "{}",
                    case["id"]
                );
                assert_eq!(
                    json!(command.constants()),
                    expected["constants"],
                    "{}",
                    case["id"]
                );
            }
        }
    }
}

#[test]
fn command_configuration_keeps_globals_local_and_extensions_lossless() {
    let config = DeploymentConfig::from_document(
        &json!({
            "core": {"workspace": "Target", "repository_directory": "repository"},
            "features": ["enable_response_collection"],
            "constants": {"VERSION": {"dev": [1, false]}},
            "unknown": {"typed": [false, 1]},
        }),
        "dev",
        None,
    )
    .unwrap();
    let command = config
        .command_configuration(CommandOptions {
            bulk_publish: true,
            kwargs: serde_json::Map::from_iter([("host_app".into(), json!("caller/1"))]),
            ..CommandOptions::default()
        })
        .unwrap();
    assert_eq!(command.item_types().len(), 33);
    assert!(!command.unpublish().skip());
    assert!(command.feature_enabled("enable_bulk_publish"));
    assert!(command.feature_enabled("enable_experimental_features"));
    assert!(!command.feature_enabled("ENABLE_BULK_PUBLISH"));
    assert_eq!(command.constants()["VERSION"], json!([1, false]));
    assert_eq!(
        command.extensions()["unknown"],
        json!({"typed": [false, 1]})
    );
    assert!(command.require_no_extensions().is_err());
    assert_eq!(command.host_app(), Some(&json!("caller/1")));
    let next = config
        .command_configuration(CommandOptions::default())
        .unwrap();
    assert!(!next.feature_enabled("enable_bulk_publish"));
    assert!(next.host_app().is_none());
}
