// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! YAML syntax primitive is not assumed to implement PyYAML source semantics.

use fabric_deploy::{
    deployment_config::ConfigurationError,
    yaml::{YamlLimits, YamlPolicy, decode},
};
use serde_json::{Value, json};

#[test]
fn protected_config_duplicate_and_merge_receipts_remain_exact() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/deploy/yaml.json")).unwrap();
    for case in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["operation"] == "safe_yaml")
    {
        assert_eq!(
            decode(
                case["input"].as_str().unwrap(),
                YamlPolicy::Config,
                YamlLimits::default()
            )
            .unwrap(),
            case["expected"],
            "{}",
            case["id"]
        );
    }
}

#[test]
fn protected_parameter_duplicate_merge_and_unicode_boundaries_remain_distinct() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/deploy/yaml.json")).unwrap();
    for case in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["operation"] == "parameter_yaml")
    {
        let result = decode(
            case["input"].as_str().unwrap(),
            YamlPolicy::Parameter,
            YamlLimits::default(),
        );
        if case.get("expected").is_some() {
            assert_eq!(result.unwrap(), case["expected"], "{}", case["id"]);
        } else {
            let error = result.unwrap_err();
            assert!(
                matches!(error, ConfigurationError::Invalid(_)),
                "{}",
                case["id"]
            );
            assert!(
                error
                    .to_string()
                    .contains(case["error"]["contains"].as_str().unwrap()),
                "{}: {error}",
                case["id"]
            );
        }
    }
}

#[test]
fn source_implicit_scalar_subset_and_quoted_spelling_are_not_yaml12_typing() {
    assert_eq!(
        decode(
            "[on, OFF, yes, null, ~, 012, 0x10, 1:20, 1e3, 1.0, 'on', '012']",
            YamlPolicy::Config,
            YamlLimits::default()
        )
        .unwrap(),
        json!([
            true, false, true, null, null, 10, 16, 80, "1e3", 1.0, "on", "012"
        ])
    );
}

#[test]
fn additive_actual_pyyaml_scalar_cases_preserve_types_or_fail_explicit_domain() {
    let fixture: Value = serde_json::from_str(include_str!("yaml_scalar_source.json")).unwrap();
    assert_eq!(fixture["pyyaml"], "6.0.3");
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 57);
    for case in cases {
        let input = case["input"].as_str().unwrap();
        let result = decode(input, YamlPolicy::Config, YamlLimits::default());
        if case["unsupportedNativeDomain"] == true {
            assert!(
                matches!(result, Err(ConfigurationError::Unsupported(_))),
                "{input}: {result:?}"
            );
        } else {
            assert_eq!(
                result.unwrap_or_else(|error| panic!("{input}: {error}")),
                case["expected"],
                "{input}"
            );
        }
    }
}

#[test]
fn non_json_scalar_domains_fail_typed_instead_of_coercion() {
    for input in [
        ".inf",
        ".nan",
        "2026-01-02",
        "18446744073709551616",
        "!!timestamp 2026-01-02",
        "!!python/object {}",
    ] {
        assert!(
            matches!(
                decode(input, YamlPolicy::Config, YamlLimits::default()),
                Err(ConfigurationError::Unsupported(_))
            ),
            "{input}"
        );
    }
}

#[test]
fn aliases_cannot_escape_expansion_depth_or_byte_bounds() {
    let input = "a: &a [long, long]\nb: &b [*a, *a]\nc: [*b, *b]\n";
    assert!(matches!(
        decode(
            input,
            YamlPolicy::Config,
            YamlLimits {
                max_nodes: 28,
                ..Default::default()
            }
        ),
        Err(ConfigurationError::Limit)
    ));
    assert!(matches!(
        decode(
            "[[[[1]]]]",
            YamlPolicy::Config,
            YamlLimits {
                max_depth: 2,
                ..Default::default()
            }
        ),
        Err(ConfigurationError::Limit)
    ));
    assert!(matches!(
        decode("a: &a [*a]", YamlPolicy::Config, YamlLimits::default()),
        Err(ConfigurationError::Unsupported(_))
    ));
}

#[test]
fn malformed_multiple_documents_and_custom_tags_are_not_ignored() {
    for input in ["{core:", "---\na: 1\n---\nb: 2\n", "a: !unsupported x"] {
        assert!(decode(input, YamlPolicy::Config, YamlLimits::default()).is_err());
    }
}
