// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Independent source contexts, ordered pipelines and item-specific schemas.

use fabric_deploy::parameter_transforms::{
    DynamicVariable, DynamicVariableKind, ParameterFileHost, ParameterItem, ParameterProgram,
    ParameterResolution, ParameterResolver, PathFilterOptions, TransformBudget, TransformError,
    TransformLimits, encode_dynamic_name, expand_parameter_environment, matches_parameter_filters,
    merge_parameter_template, prepare_parameter_document, resolve_parameter_paths,
};
use serde_json::{Value, json};
use std::{cell::Cell, collections::BTreeMap};

const GUID: &str = "11111111-1111-1111-1111-111111111111";

#[test]
fn dynamic_name_encoding_consumes_unchanged_original_source_receipt() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../../tests/fixtures/deploy/ids.json")).unwrap();
    let case = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "current-workspace-name-url-encoded")
        .unwrap();
    let result = encode_dynamic_name(
        "Synthetic / Workspace",
        &mut TransformBudget::new(TransformLimits::default(), &|| false),
    )
    .unwrap();
    assert_eq!(json!(result), case["expected"]);
}

#[test]
fn all_twenty_six_path_filters_match_actual_source_filesystem_capture() {
    assert_path_capture(include_str!("path_filter_source.json"));
}

#[test]
fn all_twenty_six_posix_path_filters_match_actual_source_filesystem_capture() {
    assert_path_capture(include_str!("path_filter_posix_source.json"));
}

fn assert_path_capture(receipt: &str) {
    let fixture: Value = serde_json::from_str(receipt).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 26);
    let options = PathFilterOptions {
        windows_paths: fixture["windowsPaths"].as_bool().unwrap(),
        case_sensitive: fixture["caseSensitive"].as_bool().unwrap(),
    };
    let root = if options.windows_paths {
        "C:\\source"
    } else {
        "/source"
    };
    let files: Vec<String> = fixture["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().into())
        .collect();
    for case in fixture["cases"].as_array().unwrap() {
        let pattern = case["pattern"].as_str().unwrap().replace("{ROOT}", root);
        let mut result = resolve_parameter_paths(
            root,
            &[pattern],
            &files,
            options,
            &mut TransformBudget::new(TransformLimits::default(), &|| false),
        )
        .unwrap()
        .paths;
        result.sort();
        assert_eq!(json!(result), case["expected"], "{}", case["pattern"]);
    }
}

struct Resolver {
    dynamic_calls: Cell<usize>,
}

struct TracedResolver {
    events: Vec<String>,
    workspace_name: String,
}

impl ParameterResolver for TracedResolver {
    fn resolve<'a>(&'a mut self, variable: &'a DynamicVariable) -> ParameterResolution<'a> {
        Box::pin(async move {
            self.events.push(format!("begin:{}", variable.expression));
            tokio::task::yield_now().await;
            let value = match variable.kind {
                DynamicVariableKind::Item => "22222222-2222-2222-2222-222222222222".to_owned(),
                DynamicVariableKind::Workspace if variable.workspace_name.is_some() => {
                    "22222222-2222-2222-2222-222222222222".to_owned()
                }
                DynamicVariableKind::Workspace if variable.attribute == "id" => GUID.into(),
                DynamicVariableKind::Workspace => self.workspace_name.clone(),
            };
            self.events.push(format!("end:{}", variable.expression));
            Ok(Value::String(value))
        })
    }

    fn resolve_paths(&self, filters: &[String]) -> Result<Vec<String>, TransformError> {
        Ok(filters.to_vec())
    }
}

fn assert_send<T: Send>(_: &T) {}

#[tokio::test(flavor = "current_thread")]
async fn seventeen_actual_source_metadata_traces_are_awaited_conditionally_without_cache() {
    let fixture: Value =
        serde_json::from_str(include_str!("parameter_resolution_source.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 17);
    for case in fixture["cases"].as_array().unwrap() {
        let program = ParameterProgram::with_default_limits(&case["parameters"], "dev").unwrap();
        let mut resolver = TracedResolver {
            events: Vec::new(),
            workspace_name: case["workspace_name"].as_str().unwrap().into(),
        };
        let mut budget = TransformBudget::new(TransformLimits::default(), &|| false);
        let future = program.apply_for_item(
            case["input"].as_str().unwrap(),
            ParameterItem {
                item_type: "Notebook",
                item_name: "Book",
                file_path: "content.py",
            },
            &mut resolver,
            &mut budget,
        );
        assert_send(&future);
        let output = future.await.unwrap();
        assert_eq!(output, case["expected"].as_str().unwrap(), "{}", case["id"]);
        let expected: Vec<String> = case["calls"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|call| {
                let call = call.as_str().unwrap();
                [format!("begin:{call}"), format!("end:{call}")]
            })
            .collect();
        assert_eq!(resolver.events, expected, "{}", case["id"]);
    }
}

impl ParameterResolver for Resolver {
    fn resolve<'a>(&'a mut self, variable: &'a DynamicVariable) -> ParameterResolution<'a> {
        Box::pin(async move {
            self.dynamic_calls.set(self.dynamic_calls.get() + 1);
            assert_eq!(variable.kind, DynamicVariableKind::Workspace);
            assert_eq!(variable.attribute, "id");
            tokio::task::yield_now().await;
            Ok(Value::String(GUID.into()))
        })
    }

    fn resolve_paths(&self, filters: &[String]) -> Result<Vec<String>, TransformError> {
        Ok(filters.to_vec())
    }
}

#[test]
fn all_thirty_three_injected_contexts_match_actual_source() {
    let fixture: Value =
        serde_json::from_str(include_str!("parameter_context_source.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 33);
    for case in fixture["cases"].as_array().unwrap() {
        let mut budget = TransformBudget::new(TransformLimits::default(), &|| false);
        match case["kind"].as_str().unwrap() {
            "dynamic" => {
                let result = DynamicVariable::parse(case["variable"].as_str().unwrap());
                if case["expected"].is_null() {
                    assert!(result.is_err(), "{}", case["variable"]);
                } else {
                    let result = result.unwrap();
                    assert_eq!(
                        json!({
                            "kind": match result.kind { DynamicVariableKind::Workspace => "workspace", DynamicVariableKind::Item => "item" },
                            "attribute": result.attribute, "workspace_name": result.workspace_name,
                            "item_type": result.item_type, "item_name": result.item_name,
                        }),
                        case["expected"],
                        "{}",
                        case["variable"]
                    );
                }
            }
            "filter" => {
                let paths: Option<Vec<String>> = case["paths"].as_array().map(|paths| {
                    paths
                        .iter()
                        .map(|path| path.as_str().unwrap().into())
                        .collect()
                });
                assert_eq!(
                    matches_parameter_filters(
                        case.get("type"),
                        case.get("name"),
                        paths.as_deref(),
                        ParameterItem {
                            item_type: "Notebook",
                            item_name: "Book",
                            file_path: "content.py"
                        }
                    ),
                    case["expected"].as_bool().unwrap(),
                    "{case}"
                );
            }
            "merge" => {
                assert_eq!(
                    merge_parameter_template(&case["base"], &case["template"], &mut budget)
                        .unwrap(),
                    case["expected"]
                );
            }
            "environment" => {
                let values = BTreeMap::from([
                    ("DEFINED".into(), "resolved".into()),
                    ("EMPTY".into(), String::new()),
                ]);
                assert_eq!(
                    expand_parameter_environment(
                        case["input"].as_str().unwrap(),
                        case["enabled"].as_bool().unwrap(),
                        &values,
                        &mut budget
                    )
                    .unwrap(),
                    case["expected"].as_str().unwrap()
                );
            }
            _ => panic!("Unknown source case"),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn all_thirty_four_source_pipelines_and_schemas_match() {
    let fixture: Value =
        serde_json::from_str(include_str!("parameter_pipeline_source.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 34);
    for case in fixture["cases"].as_array().unwrap() {
        let result = ParameterProgram::with_default_limits(&case["parameters"], "dev");
        if case["kind"] == "schema" {
            assert_eq!(
                result.is_ok(),
                case["valid"].as_bool().unwrap(),
                "{}: {result:?}",
                case["id"]
            );
            if let Ok(program) = result {
                assert_eq!(program.source_document(), &case["parameters"]);
                for key in ["spark_pool", "semantic_model_binding"] {
                    if let Some(value) = case["normalized"].get(key) {
                        assert_eq!(&program.item_settings()[key], value, "{}", case["id"]);
                    }
                }
            }
        } else {
            let program = result.unwrap();
            assert_eq!(program.source_document(), &case["parameters"]);
            let mut resolver = Resolver {
                dynamic_calls: Cell::new(0),
            };
            let actual = program
                .apply_scoped(
                    case["input"].as_str().unwrap(),
                    ParameterItem {
                        item_type: "Notebook",
                        item_name: "Book",
                        file_path: case["file"].as_str().unwrap(),
                    },
                    &mut resolver,
                    &mut TransformBudget::new(TransformLimits::default(), &|| false),
                )
                .await
                .unwrap();
            assert_eq!(actual, case["expected"].as_str().unwrap(), "{}", case["id"]);
            if case["id"] == "ordered-4" || case["id"] == "ordered-7" {
                assert_eq!(
                    resolver.dynamic_calls.get(),
                    0,
                    "No-match must not resolve dynamic replacements"
                );
            }
        }
    }
}

struct Templates;
impl ParameterFileHost for Templates {
    fn template(&self, reference: &str) -> Result<Option<Value>, TransformError> {
        Ok(match reference {
            "first.yml" => Some(
                json!({"find_replace": [{"find_value": "middle", "replace_value": {"dev": "target"}}]}),
            ),
            "nested.yml" => Some(json!({"extend": ["deeper.yml"]})),
            _ => None,
        })
    }
}

struct EdgeTemplates;
impl ParameterFileHost for EdgeTemplates {
    fn template(&self, reference: &str) -> Result<Option<Value>, TransformError> {
        Ok((reference == "template.yml").then(|| json!({"find_replace": []})))
    }
}

#[test]
fn eighteen_actual_source_loader_and_ignored_gateway_shapes_match() {
    let fixture: Value =
        serde_json::from_str(include_str!("parameter_schema_edges_source.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 18);
    for case in fixture["cases"].as_array().unwrap() {
        let prepared = prepare_parameter_document(
            &case["document"],
            &EdgeTemplates,
            &mut TransformBudget::new(TransformLimits::default(), &|| false),
        );
        let result = prepared
            .and_then(|prepared| ParameterProgram::with_default_limits(&prepared.document, "dev"));
        assert_eq!(
            result.is_ok(),
            case["valid"].as_bool().unwrap(),
            "{}",
            case["id"]
        );
        if let Ok(program) = result {
            if let Some(expected) = case["expected"].get("semantic_model_binding") {
                assert_eq!(
                    &program.item_settings()["semantic_model_binding"],
                    expected,
                    "{}",
                    case["id"]
                );
                program
                    .require_item_settings_consumed(&["semantic_model_binding"])
                    .unwrap();
            }
            if case["id"] == "empty-gateway-only" {
                program.require_item_settings_consumed(&[]).unwrap();
                assert_eq!(program.source_document()["gateway_binding"], json!([]));
            }
        }
    }
}

#[test]
fn templates_are_loaded_once_in_order_and_bad_templates_are_reported() {
    let prepared = prepare_parameter_document(
        &json!({
            "find_replace": [{"find_value": "source", "replace_value": {"dev": "middle"}}],
            "extend": ["first.yml", "first.yml", "missing.yml", "nested.yml"],
        }),
        &Templates,
        &mut TransformBudget::new(TransformLimits::default(), &|| false),
    )
    .unwrap();
    assert_eq!(
        prepared.skipped_templates,
        ["first.yml", "missing.yml", "nested.yml"]
    );
    assert!(prepared.document.get("extend").is_none());
    let program = ParameterProgram::with_default_limits(&prepared.document, "dev").unwrap();
    assert_eq!(
        program
            .apply(
                "source",
                "content.py",
                &mut TransformBudget::new(TransformLimits::default(), &|| false)
            )
            .unwrap(),
        "target"
    );
}

#[test]
fn unresolved_environment_tokens_and_contextless_filters_cannot_silently_disappear() {
    let inactive = json!({"find_replace": [{"find_value": "source", "replace_value": {"prod": "$ENV:UNSET"}}]});
    assert!(ParameterProgram::with_default_limits(&inactive, "dev").is_err());
    let program = ParameterProgram::from_document_with_features(
        &inactive,
        "dev",
        true,
        &mut TransformBudget::new(TransformLimits::default(), &|| false),
    )
    .unwrap();
    assert_eq!(program.skipped().len(), 1);
    let active =
        json!({"find_replace": [{"find_value": "source", "replace_value": {"dev": "$ENV:UNSET"}}]});
    assert!(
        ParameterProgram::from_document_with_features(
            &active,
            "dev",
            true,
            &mut TransformBudget::new(TransformLimits::default(), &|| false)
        )
        .is_err()
    );
    let program = ParameterProgram::with_default_limits(&json!({"find_replace": [{"find_value": "source", "replace_value": {"dev": "target"}, "item_name": "Book"}]}), "dev").unwrap();
    assert!(matches!(
        program.apply(
            "source",
            "content.py",
            &mut TransformBudget::new(TransformLimits::default(), &|| false)
        ),
        Err(TransformError::Unsupported(_))
    ));
}
