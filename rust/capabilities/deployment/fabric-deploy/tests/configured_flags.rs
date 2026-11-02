// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fixed actual-source captures precede the native expectations in this file.

use fabric_deploy::{
    deployment_config::{CommandOptions, DeploymentConfig},
    parameter_transforms::{ParameterProgram, TransformBudget, TransformLimits},
};
use serde_json::{Value, json};

#[test]
fn borrowed_options_expose_readonly_source_field_view_without_changing_legacy_config() {
    let fixture: Value =
        serde_json::from_str(include_str!("configured_flags_source.json")).unwrap();
    let case = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "filters-environment")
        .unwrap();
    let mut config = DeploymentConfig::from_document(&case["config"], "dev", None).unwrap();
    let mut options = CommandOptions::new(
        true,
        serde_json::Map::from_iter([
            ("host_app".into(), json!("wrapper/1")),
            ("unused_argument".into(), json!({"typed": false})),
        ]),
    );
    let snapshot = config.command_configuration(&options).unwrap();
    assert_eq!(snapshot.raw_document, config.document);
    assert_eq!(
        snapshot.workspace,
        fabric_deploy::deployment_config::WorkspaceTarget::Id(
            "11111111-1111-1111-1111-111111111111".into()
        )
    );
    assert_eq!(snapshot.repository_directory, "repository");
    assert!(snapshot.parameter_file_path.is_none());
    assert_eq!(snapshot.item_types, ["Notebook"]);
    assert_eq!(snapshot.publish.exclude_regex.as_deref(), Some("Book"));
    assert!(!snapshot.publish.skip);
    assert!(!snapshot.unpublish.skip);
    assert_eq!(
        snapshot.unpublish.items_to_include.as_ref().unwrap(),
        &["Book.Notebook"]
    );
    assert!(snapshot.features.contains(&"enable_bulk_publish".into()));
    assert!(
        snapshot
            .features
            .contains(&"enable_experimental_features".into())
    );
    assert_eq!(snapshot.kwargs["unused_argument"], json!({"typed": false}));
    let cloned = snapshot.clone();
    assert!(std::ptr::eq(&*snapshot, &*cloned));
    options.kwargs.insert("host_app".into(), json!("changed"));
    config.document["core"]["repository_directory"] = json!("changed");
    assert_eq!(snapshot.host_app(), Some(&json!("wrapper/1")));
    assert_eq!(
        snapshot.raw_document["core"]["repository_directory"],
        "repository"
    );
}

#[test]
fn selected_constant_getter_matches_eighteen_actual_source_cases_without_alias_recomputation() {
    let fixture: Value =
        serde_json::from_str(include_str!("selected_constants_source.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 18);
    for case in fixture["cases"].as_array().unwrap() {
        let config = DeploymentConfig::from_document(&json!({
            "core": { "workspace_id": "11111111-1111-1111-1111-111111111111", "repository_directory": "repository" },
            "constants": case["constants"],
        }), case["environment"].as_str().unwrap(), None).unwrap();
        assert_eq!(
            json!(config.selected_constants()),
            case["selected"],
            "{}",
            case["id"]
        );
        let command = config
            .command_configuration(CommandOptions {
                features: vec!["base".into()],
                constants: fixture["defaults"].as_object().unwrap().clone(),
                ..CommandOptions::default()
            })
            .unwrap();
        assert_eq!(
            json!(command.constants()),
            case["effective_before_cleanup"],
            "{}",
            case["id"]
        );
        assert_eq!(
            config.document["constants"], case["constants"],
            "{}",
            case["id"]
        );
    }
}

#[test]
fn config_raw_parameter_getter_never_reads_or_reuses_stale_source() {
    let fixture: Value =
        serde_json::from_str(include_str!("literal_parameter_source.json")).unwrap();
    let raw = &fixture["cases"][0]["parameters"];
    let mut config = DeploymentConfig::from_document(&json!({
        "core": { "workspace_id": "11111111-1111-1111-1111-111111111111", "repository_directory": "repository", "parameter": "parameter.yml" },
    }), "dev", None).unwrap();
    assert!(config.raw_parameter_document().is_none());
    config.resolve_parameters(raw).unwrap();
    assert_eq!(config.raw_parameter_document(), Some(raw));
    assert!(
        config
            .resolve_parameters(&json!({"find_replace": false}))
            .is_err()
    );
    assert!(config.raw_parameter_document().is_none());
}

#[test]
fn invalid_caller_flag_constant_is_a_typed_error_not_a_feature_query_panic() {
    let config = DeploymentConfig::from_document(&json!({
        "core": { "workspace_id": "11111111-1111-1111-1111-111111111111", "repository_directory": "repository" },
    }), "dev", None).unwrap();
    assert!(
        config
            .command_configuration(CommandOptions {
                constants: serde_json::Map::from_iter([("FEATURE_FLAG".into(), json!(false))]),
                ..CommandOptions::default()
            })
            .is_err()
    );
}

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
