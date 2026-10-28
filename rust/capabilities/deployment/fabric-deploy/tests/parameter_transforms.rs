// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public bounded adapters against protected actual-source leaf expectations.

#![deny(unnameable_test_items)]

use fabric_deploy::parameter_transforms::{
    FindReplacement, JsonPathReplacement, ParameterProgram, TransformBudget, TransformError,
    TransformLimits,
};
use serde_json::Value;
use std::time::Duration;

#[test]
fn pinned_jsonpath_public_ast_supports_typed_operator_comparison() {
    use jsonpath_rust::{
        JsonPath,
        parser::{FilterExpression, JsonPathIndex, Operand},
    };

    fn filter(path: &JsonPath) -> &FilterExpression<Value> {
        let JsonPath::Chain(parts) = path else {
            panic!("expected parsed chain");
        };
        let [
            JsonPath::Root,
            JsonPath::Index(JsonPathIndex::Filter(filter)),
        ] = parts.as_slice()
        else {
            panic!("expected absolute filter");
        };
        filter
    }

    let first = JsonPath::try_from("$[?(@.n >= 2)]").unwrap();
    let same_operator = JsonPath::try_from("$[?(@.other >= 3)]").unwrap();
    let different_operator = JsonPath::try_from("$[?(@.n > 2)]").unwrap();
    let FilterExpression::Atom(Operand::Dynamic(_), first_sign, Operand::Static(value)) =
        filter(&first)
    else {
        panic!("expected dynamic/static comparison");
    };
    assert_eq!(value, &Value::from(2));
    let FilterExpression::Atom(_, same_sign, _) = filter(&same_operator) else {
        panic!("expected atom");
    };
    let FilterExpression::Atom(_, different_sign, _) = filter(&different_operator) else {
        panic!("expected atom");
    };
    assert_eq!(first_sign, same_sign);
    assert_ne!(first_sign, different_sign);
}

#[test]
fn bounded_public_ast_filters_preserve_all_six_comparison_operators() {
    for (operator, selected) in [
        ("==", vec![1]),
        ("!=", vec![0, 2]),
        ("<", vec![0]),
        (">", vec![2]),
        ("<=", vec![0, 1]),
        (">=", vec![1, 2]),
    ] {
        let active = || false;
        let mut budget = TransformBudget::new(TransformLimits::default(), &active);
        let query = JsonPathReplacement::compile(
            &format!("$.items[?(@.n {operator} 2)].value"),
            &mut budget,
        )
        .unwrap();
        let input = r#"{"items":[{"n":1,"value":"first"},{"n":2,"value":"second"},{"n":3,"value":"third"},{"value":"missing"}]}"#;
        let replacement = serde_json::json!({"changed": true});
        let output = query
            .apply(input, Some(&replacement), false, &mut budget)
            .unwrap();
        let mut expected: Value = serde_json::from_str(input).unwrap();
        for index in selected {
            expected["items"][index]["value"] = replacement.clone();
        }
        assert_eq!(
            output,
            fabric_core_json::json_dumps(&expected),
            "{operator}"
        );
    }
}

#[test]
fn public_ast_still_rejects_advanced_filters_before_content_or_writes() {
    for expression in [
        "$.items[?(@.n ~= '2')]",
        "$.items[?(@.n)]",
        "$.items[?(@.n >= 2 && @.n < 4)]",
        "$.items[?(@.n == @.other)]",
    ] {
        let active = || false;
        let mut budget = TransformBudget::new(TransformLimits::default(), &active);
        assert!(
            matches!(
                JsonPathReplacement::compile(expression, &mut budget),
                Err(TransformError::Unsupported(_))
            ),
            "{expression}"
        );
    }
}

fn flag(value: Option<&Value>) -> bool {
    value.is_some_and(|value| {
        value == true
            || value
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    })
}

fn replacement(rule: &Value) -> Option<&Value> {
    let mapping = rule["replace_value"].as_object().unwrap();
    mapping.get("dev").or_else(|| {
        (mapping.len() == 1)
            .then(|| mapping.iter().next().unwrap())
            .filter(|(key, _)| key.to_lowercase() == "_all_")
            .map(|(_, value)| value)
    })
}

fn apply(case: &Value, budget: &mut TransformBudget<'_>) -> Result<String, TransformError> {
    let content = case["input"].as_str().unwrap();
    if case["operation"] == "key_value" {
        let rule = &case["parameter"];
        return JsonPathReplacement::compile(rule["find_key"].as_str().unwrap(), budget)?.apply(
            content,
            replacement(rule),
            case["yaml"] == true,
            budget,
        );
    }
    let mut content = content.to_owned();
    let parameters = &case["parameters"];
    if let Some(rules) = parameters["key_value_replace"].as_array() {
        for rule in rules {
            let query = JsonPathReplacement::compile(rule["find_key"].as_str().unwrap(), budget)?;
            let json = serde_json::from_str::<Value>(&content).is_ok();
            let is_yaml = case["file_name"]
                .as_str()
                .is_some_and(|name| name.ends_with(".yml") || name.ends_with(".yaml"));
            if json || is_yaml {
                content = query.apply(&content, replacement(rule), !json && is_yaml, budget)?;
            }
        }
    }
    if let Some(rules) = parameters["find_replace"].as_array() {
        for rule in rules {
            let regex = FindReplacement::compile(
                rule["find_value"].as_str().unwrap(),
                flag(rule.get("is_regex")),
                flag(rule.get("ignore_case")),
                budget,
            )?;
            let filter = rule
                .get("item_type")
                .is_none_or(|value| value == "Notebook");
            let value = replacement(rule).and_then(Value::as_str).unwrap_or("");
            content = regex.apply(&content, value, filter, budget)?;
        }
    }
    Ok(content)
}

#[test]
fn all_protected_transform_cases_match_unchanged_leaf_receipts() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/deploy/transforms.json"
    ))
    .unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 18);
    for case in fixture["cases"].as_array().unwrap() {
        let cancellation = || false;
        let mut budget = TransformBudget::new(TransformLimits::default(), &cancellation);
        let actual = apply(case, &mut budget);
        if case.get("error").is_some() {
            let error = actual.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(case["error"]["contains"].as_str().unwrap()),
                "{}: {error}",
                case["id"]
            );
        } else {
            assert_eq!(
                actual.unwrap_or_else(|error| panic!("{}: {error}", case["id"])),
                case["expected"],
                "{}",
                case["id"]
            );
        }
    }
}

#[test]
fn additive_source_edges_match_or_remain_explicit_unported_branches() {
    let fixture: Value =
        serde_json::from_str(include_str!("transform_adapter_source.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 25);
    for case in fixture["cases"].as_array().unwrap() {
        let cancellation = || false;
        let mut budget = TransformBudget::new(TransformLimits::default(), &cancellation);
        let actual = apply(case, &mut budget);
        if matches!(
            case["id"].as_str().unwrap(),
            "inline-ignore-case" | "negative-index"
        ) || case["expected"].get("error").is_some()
        {
            assert!(actual.is_err(), "{}", case["id"]);
        } else {
            assert_eq!(
                actual.unwrap_or_else(|error| panic!("{}: {error}", case["id"])),
                case["expected"]["value"],
                "{}",
                case["id"]
            );
        }
    }
}

#[test]
fn regex_capture_validation_precedes_excluded_filter_and_empty_replacement() {
    let cancellation = || false;
    let mut budget = TransformBudget::new(TransformLimits::default(), &cancellation);
    assert!(matches!(
        FindReplacement::compile("(a)(b)", true, false, &mut budget),
        Err(TransformError::Invalid(_))
    ));
    let regex = FindReplacement::compile("id=(x*);", true, false, &mut budget).unwrap();
    assert!(matches!(
        regex.apply("id=;", "", true, &mut budget),
        Err(TransformError::Invalid(_))
    ));
    assert_eq!(
        regex.apply("id=;", "new", false, &mut budget).unwrap(),
        "id=;"
    );
}

#[test]
fn cancellation_deadline_limits_and_backtracking_never_return_partial_success() {
    let cancelled = || true;
    let mut budget = TransformBudget::new(TransformLimits::default(), &cancelled);
    assert!(matches!(
        FindReplacement::compile("(a)", true, false, &mut budget),
        Err(TransformError::Cancelled)
    ));
    let active = || false;
    let mut budget = TransformBudget::new(
        TransformLimits {
            max_duration: Duration::ZERO,
            ..Default::default()
        },
        &active,
    );
    assert!(matches!(
        JsonPathReplacement::compile("$.a", &mut budget),
        Err(TransformError::TimedOut)
    ));
    let mut budget = TransformBudget::new(
        TransformLimits {
            max_matches: 1,
            ..Default::default()
        },
        &active,
    );
    let query = JsonPathReplacement::compile("$.items[*]", &mut budget).unwrap();
    assert!(matches!(
        query.apply("{\"items\":[1,2]}", Some(&Value::Null), false, &mut budget),
        Err(TransformError::Limit)
    ));
    let mut budget = TransformBudget::new(
        TransformLimits {
            max_depth: 2,
            ..Default::default()
        },
        &active,
    );
    let query = JsonPathReplacement::compile("$..id", &mut budget).unwrap();
    assert!(matches!(
        query.apply(
            "{\"a\":{\"b\":{\"id\":1}}}",
            Some(&Value::Null),
            false,
            &mut budget
        ),
        Err(TransformError::Limit)
    ));
    let mut budget = TransformBudget::new(
        TransformLimits {
            max_bytes: 64,
            ..Default::default()
        },
        &active,
    );
    let regex = FindReplacement::compile("(a)", true, false, &mut budget).unwrap();
    assert!(matches!(
        regex.apply("aa", &"x".repeat(40), true, &mut budget),
        Err(TransformError::Limit)
    ));
    let mut budget = TransformBudget::new(
        TransformLimits {
            max_backtracks: 1,
            ..Default::default()
        },
        &active,
    );
    let regex = FindReplacement::compile("(x+x+)+(?>y)", true, false, &mut budget).unwrap();
    assert!(matches!(
        regex.apply("xxxxxxxxxxy", "x", true, &mut budget),
        Err(TransformError::Limit)
    ));
}

#[test]
fn no_match_preserves_bytes_and_duplicate_paths_do_not_create_fields() {
    let active = || false;
    let mut budget = TransformBudget::new(TransformLimits::default(), &active);
    let query = JsonPathReplacement::compile("$.missing", &mut budget).unwrap();
    assert_eq!(
        query
            .apply("{ \"a\": 1 }\r\n", Some(&Value::Null), false, &mut budget)
            .unwrap(),
        "{ \"a\": 1 }\r\n"
    );
    let query = JsonPathReplacement::compile("$['a','a']", &mut budget).unwrap();
    assert_eq!(
        query
            .apply("{\"a\":1}", Some(&Value::from("new")), false, &mut budget)
            .unwrap(),
        "{\"a\": \"new\"}"
    );
}

#[test]
fn actual_full_schema_programs_preserve_types_order_and_literal_capture_replacements() {
    let fixture: Value =
        serde_json::from_str(include_str!("transform_program_source.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 4);
    for case in fixture["cases"].as_array().unwrap() {
        let active = || false;
        let mut budget = TransformBudget::new(TransformLimits::default(), &active);
        let program =
            ParameterProgram::from_document(&case["parameters"], "dev", &mut budget).unwrap();
        let actual = program
            .apply(
                case["input"].as_str().unwrap(),
                "notebook-content.py",
                &mut budget,
            )
            .unwrap();
        assert_eq!(actual, case["expected"], "{}", case["id"]);
    }
}

#[test]
fn full_schema_still_rejects_leaf_boolean_flags_and_type_mixed_replacements() {
    for document in [
        serde_json::json!({"find_replace":[{"find_value":"(old)","is_regex":true,"replace_value":{"dev":"new"}}]}),
        serde_json::json!({"key_value_replace":[{"find_key":"$.a","replace_value":{"dev":true,"prod":1}}]}),
        serde_json::json!({"key_value_replace":[{"find_key":"$.a","replace_value":{"dev":1,"prod":1.0}}]}),
    ] {
        assert!(ParameterProgram::with_default_limits(&document, "dev").is_err());
    }
    for pattern in [
        r"(?<name>old)",
        r"(\u{0061})",
        r"(\x{61})",
        r"(\p{L})",
        r"([a&&b])",
    ] {
        let mut budget = TransformBudget::new(TransformLimits::default(), &|| false);
        assert!(
            matches!(
                FindReplacement::compile(pattern, true, false, &mut budget),
                Err(TransformError::Unsupported(_))
            ),
            "{pattern}"
        );
    }
}

#[test]
fn controlled_plan_and_parameter_resolution_keep_cancellation_typed_before_writes() {
    use fabric_deploy::{
        deployment_config::{ConfigurationError, DeploymentConfig},
        notebook::DeploymentError,
        publish::{NotebookPublishPlan, PublishFailure},
        repository::NotebookRepository,
    };
    let mut config = DeploymentConfig::from_document(
        &serde_json::json!({
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
    let document = serde_json::json!({"find_replace":[{"find_value":"(old)","is_regex":"true","replace_value":{"dev":"new"}}]});
    config.resolve_parameters(&document).unwrap();
    let cancelled = || true;
    let mut budget = TransformBudget::new(TransformLimits::default(), &cancelled);
    assert!(
        matches!(config.resolve_parameters_controlled(&document, &mut budget),
        Err(ConfigurationError::Transform(error)) if *error == TransformError::Cancelled)
    );
    assert!(config.parameters().is_none());
    config.resolve_parameters(&document).unwrap();
    let result = NotebookPublishPlan::prepare_controlled(
        &config,
        NotebookRepository {
            notebooks: Vec::new(),
        },
        &[],
        TransformLimits::default(),
        &cancelled,
    );
    assert!(matches!(result, Err(error)
        if matches!(&error.failure, PublishFailure::Notebook(DeploymentError::Transform(error)) if **error == TransformError::Cancelled)));
}
