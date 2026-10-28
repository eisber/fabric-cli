// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fixed actual-source captures precede the native expectations in this file.

use fabric_deploy::{
    deployment_config::{CommandOptions, DeploymentConfig},
    parameter_transforms::{ParameterProgram, TransformBudget, TransformLimits},
};
use serde_json::{Value, json};

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
