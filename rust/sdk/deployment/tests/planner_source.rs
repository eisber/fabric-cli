// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-captured selective dependency ordering and planner boundary contracts.

#![deny(unnameable_test_items)]

use fabric_deploy::{
    deployment_config::DeploymentConfig,
    model::{ItemFile, RepositoryItem},
    planner::{DeploymentPlan, EngineContext},
    repository::DeploymentRepository,
};
use serde_json::{Value, json};

fn oracle() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/compatibility/deploy_workflow/pipeline_selection_source.json"
    ))
    .unwrap()
}

fn repository(source: &Value) -> DeploymentRepository {
    DeploymentRepository {
        items: source["definitions"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, definition)| {
                RepositoryItem::from_files(
                    format!("{name}.DataPipeline"),
                    vec![
                        ItemFile::new(
                            ".platform".into(),
                            serde_json::to_vec(&json!({
                                "metadata":{"displayName":name,"type":"DataPipeline"},
                                "config":{"logicalId":source["logical_ids"][name]},
                            }))
                            .unwrap(),
                        ),
                        ItemFile::new(
                            "pipeline-content.json".into(),
                            serde_json::to_vec(definition).unwrap(),
                        ),
                    ],
                )
                .unwrap()
            })
            .collect(),
        folders: Vec::new(),
    }
}

fn configuration(selection: Value) -> DeploymentConfig {
    DeploymentConfig::from_document(&json!({
        "core":{"workspace_id":"11111111-1111-1111-1111-111111111111","repository_directory":".","item_types_in_scope":["DataPipeline"]},
        "publish":{"items_to_include":selection},"unpublish":{"skip":true},
        "features":["enable_experimental_features","enable_items_to_include","disable_workspace_folder_publish"],
    }),"test",None).unwrap()
}

#[test]
fn selected_pipeline_still_validates_full_repository_dependency_graph_like_source() {
    let source = oracle();
    assert_eq!(source["selected"], json!(["Selected"]));
    assert_eq!(source["order"], json!({"error":"cycle"}));
    let config = configuration(source["items_to_include"].clone());
    let result = DeploymentPlan::prepare(
        &config,
        &EngineContext::from_config(&config, false).unwrap(),
        repository(&source),
    );
    assert!(
        matches!(result,Err(fabric_deploy::deployment_config::ConfigurationError::Invalid(errors)) if errors.iter().any(|message| message.contains("cycle")))
    );
}

#[test]
fn no_selected_pipelines_does_not_run_the_source_ordered_publisher() {
    let source = oracle();
    let config = configuration(json!(["Absent.DataPipeline"]));
    let plan = DeploymentPlan::prepare(
        &config,
        &EngineContext::from_config(&config, false).unwrap(),
        repository(&source),
    )
    .unwrap();
    assert!(plan.order.is_empty());
}
