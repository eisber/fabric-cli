// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-supported operations that supersede the historical Notebook preview guard.

use super::*;

#[tokio::test]
async fn default_unpublish_executes_real_orphan_deletion_without_replaying_publication() {
    let mut host = Harness::new();
    input(&host);
    std::fs::write(
        host.root.join("deploy.yml"),
        format!(
            "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [Notebook]\nfeatures: [disable_workspace_folder_publish]\n"
        ),
    )
    .unwrap();
    metadata(&host, false);
    host.reply(201, json!({"id":ITEM}));
    host.reply(
        200,
        json!({"value":[
            {"id":ITEM,"displayName":"Book","type":"Notebook"},
            {"id":OPERATION,"displayName":"Orphan","type":"Notebook"},
        ]}),
    );
    host.reply(200, Value::Null);
    host.reply(
        200,
        json!({"value":[{"id":ITEM,"displayName":"Book","type":"Notebook"}]}),
    );
    host.reply(200, json!({"value":[]}));
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "-f"])
        .await;
    success(&output);
    let requests = host.requests();
    assert_eq!(requests.len(), 7);
    assert_eq!(
        requests
            .iter()
            .map(|request| request["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["GET", "GET", "POST", "GET", "DELETE", "GET", "GET"]
    );
    assert!(
        requests[4]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/items/{OPERATION}"))
    );
    assert!(
        matches!(&output.evidence, FamilyEvidence::Deploy(Ok(result))
            if result.completed.len() == 1 && result.completed[0].outcome.item_id == ITEM)
    );
}

#[tokio::test]
async fn mixed_source_item_scope_publishes_the_existing_notebook_without_a_scope_stub() {
    let mut host = Harness::new();
    input(&host);
    std::fs::write(
        host.root.join("deploy.yml"),
        format!(
            "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [Notebook, DataPipeline]\nunpublish:\n  skip: true\nfeatures: [disable_workspace_folder_publish]\n"
        ),
    )
    .unwrap();
    metadata(&host, false);
    host.reply(201, json!({"id":ITEM}));
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "-f"])
        .await;
    success(&output);
    let requests = host.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2]["method"], "POST");
    assert_eq!(requests[2]["body"]["type"], "Notebook");
    assert!(
        matches!(&output.evidence, FamilyEvidence::Deploy(Ok(result))
            if result.completed.len() == 1 && result.completed[0].outcome.item_id == ITEM)
    );
}
