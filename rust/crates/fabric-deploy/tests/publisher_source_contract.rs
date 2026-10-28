// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_deploy::{
    deployment_config::ITEM_TYPES,
    model::{ItemFile, RepositoryItem},
    processors::{Processor, mutation_body},
};
use serde_json::Value;
use std::collections::HashSet;

#[test]
fn all_source_publishers_preserve_actual_common_mutation_bodies() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/deploy_workflow/publisher_calls.json"
    ))
    .unwrap();
    let mut covered = HashSet::new();
    for case in oracle["cases"].as_array().unwrap() {
        let item_type = case["type"].as_str().unwrap();
        covered.insert(item_type);
        let processor = Processor::for_type(item_type).unwrap();
        let mut item = RepositoryItem {
            name: "Example".into(),
            item_type: item_type.into(),
            description: "source fixture".into(),
            logical_id: "33333333-3333-3333-3333-333333333333".into(),
            path: format!("Example.{item_type}"),
            folder_path: String::new(),
            files: vec![
                ItemFile::new(
                    ".platform".into(),
                    br#"{"metadata": {"displayName": "Example"}}"#.to_vec(),
                ),
                ItemFile::new(
                    "content.json".into(),
                    br#"{"dataSources": [], "queryset": {}}"#.to_vec(),
                ),
            ],
        };
        // The actual KQL Dashboard processor serializes even an empty sources list.
        if processor == Processor::KqlDashboard {
            item.files[1].bytes = b"{\n  \"dataSources\": [],\n  \"queryset\": {}\n}".to_vec();
        }
        let body = mutation_body(
            processor,
            &item,
            case["update"].as_bool().unwrap(),
            "",
            false,
        )
        .unwrap();
        assert_eq!(body, case["calls"][0]["body"], "{item_type}");
    }
    assert_eq!(covered, ITEM_TYPES.iter().copied().collect());
    assert_eq!(oracle["cases"].as_array().unwrap().len(), 66);
}

#[test]
fn unsupported_source_item_type_is_real_validation() {
    assert!(Processor::for_type("Dashboard").is_err());
    assert!(Processor::for_type("notebook").is_err());
}
