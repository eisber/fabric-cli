// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Independent workflow cases registered outside the preprocessing test body.

use super::*;

#[tokio::test]
async fn default_orphan_unpublish_preserves_repository_items_and_source_delete_policy() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/deploy_workflow/publisher_calls.json"
    ))
    .unwrap();
    for hard in [false, true] {
        let executor = Executor::default();
        executor.reply(
            200,
            json!({"value": [
                {"id": ITEM, "displayName": "Orphan", "type": "Notebook"},
                {"id": SECOND, "displayName": "Kept", "type": "Notebook"},
            ]}),
        );
        executor.reply(200, Value::Null);
        executor.reply(
            200,
            json!({"value": [{"id": SECOND, "displayName": "Kept", "type": "Notebook"}]}),
        );
        executor.reply(200, json!({"value": []}));
        let config = DeploymentConfig::from_document(&json!({
            "core": {"workspace_id":WORKSPACE,"repository_directory":".","item_types_in_scope":["Notebook"]},
            "publish": {"skip":true},
            "features": ["disable_workspace_folder_publish"],
        }), "test", None).unwrap();
        let mut context = EngineContext::from_config(&config, false).unwrap();
        if hard {
            context.features.insert("enable_hard_delete".into());
        }
        deploy_repository(
            &client(),
            &executor,
            &config,
            DeploymentRepository {
                items: vec![item("Notebook", "Kept", "kept")],
                folders: Vec::new(),
            },
            context,
            OperationLimits::default(),
            pending(),
        )
        .await
        .unwrap();
        let calls = executor.calls();
        let deleted: Vec<_> = calls
            .iter()
            .filter(|call| call.0 == ResourceMethod::Delete)
            .collect();
        assert_eq!(deleted.len(), 1);
        let source = &oracle["unpublish_cases"][usize::from(hard)]["calls"][0];
        assert!(source["url"].as_str().unwrap().ends_with(&deleted[0].1));
        assert!(executor.replies.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn exported_default_logical_ids_do_not_pair_distinct_new_items() {
    let executor = Executor::default();
    executor.reply(200, json!({"capacityId":"assigned"}));
    executor.reply(200, json!({"value":[]}));
    executor.reply(201, json!({"id":ITEM}));
    executor.reply(201, json!({"id":SECOND}));
    let config = config(&["CopyJob"], false);
    let result = deploy_repository(
        &client(),
        &executor,
        &config,
        DeploymentRepository {
            items: vec![
                item("CopyJob", "First", fabric_deploy::transforms::DEFAULT_GUID),
                item("CopyJob", "Second", fabric_deploy::transforms::DEFAULT_GUID),
            ],
            folders: Vec::new(),
        },
        EngineContext::from_config(&config, false).unwrap(),
        OperationLimits::default(),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(
        result
            .published
            .iter()
            .map(|item| item.outcome.item_id.as_str())
            .collect::<Vec<_>>(),
        [ITEM, SECOND]
    );
    assert!(
        executor.calls()[2..]
            .iter()
            .all(|call| call.1.ends_with("/items"))
    );
}

#[tokio::test]
async fn lakehouse_readiness_waits_use_actual_source_backoff_not_a_fixed_delay() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/deploy_workflow/waits_source.json"
    ))
    .unwrap();
    let executor = Executor::default();
    executor.reply(200, json!({"capacityId":"assigned"}));
    executor.reply(200, json!({"value":[]}));
    executor.reply(201, json!({"id":ITEM}));
    for _ in 0..3 {
        executor.reply(
            200,
            json!({"properties":{"sqlEndpointProperties":{"provisioningStatus":"InProgress"}}}),
        );
    }
    executor.reply(
        200,
        json!({"properties":{"sqlEndpointProperties":{"provisioningStatus":"Success"}}}),
    );
    let config = config(&["Lakehouse"], false);
    deploy_repository(
        &client(),
        &executor,
        &config,
        DeploymentRepository {
            items: vec![item("Lakehouse", "Example", "lakehouse-logical")],
            folders: Vec::new(),
        },
        EngineContext::from_config(&config, false).unwrap(),
        OperationLimits::default(),
        pending(),
    )
    .await
    .unwrap();
    let waits: Vec<_> = executor
        .waits
        .lock()
        .unwrap()
        .iter()
        .map(Duration::as_secs)
        .collect();
    assert_eq!(
        json!(waits),
        Value::Array(oracle["cases"][0]["seconds"].as_array().unwrap()[..3].to_vec())
    );
}
