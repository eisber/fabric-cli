// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root parser -> central session -> released production family -> shared HTTP.

#![cfg(feature = "native-read")]

mod family_support;
use fabric_cli::families::FamilyEvidence;
use family_support::{Harness, success};
use serde_json::json;

#[tokio::test]
async fn root_open_launches_the_injected_browser_only_after_real_metadata() {
    let mut host = Harness::new();
    host.item("Notebook");
    let output = host
        .execute(&["open", "Demo.Workspace/Book.Notebook"])
        .await;
    success(&output);
    assert_eq!(host.effects.browser.len(), 1);
    assert!(host.effects.browser[0].starts_with("https://app.fabric.microsoft.com/groups/w/"));
    assert!(host.effects.browser[0].contains("n"));
    assert_eq!(host.requests().len(), 2);
}

#[tokio::test]
async fn root_exists_and_mkdir_use_resolved_ids_and_real_post() {
    let mut exists = Harness::new();
    exists.item("Notebook");
    let output = exists
        .execute(&["exists", "Demo.Workspace/Book.Notebook"])
        .await;
    assert!(success(&output).contains("true"));
    assert_eq!(exists.requests().len(), 2);
    let mut mkdir = Harness::new();
    mkdir.metadata(json!([]));
    mkdir.reply(
        201,
        json!({"id":"created","displayName":"Book","type":"Notebook"}),
    );
    let output = mkdir
        .execute(&["mkdir", "Demo.Workspace/Book.Notebook"])
        .await;
    success(&output);
    assert!(matches!(output.evidence, FamilyEvidence::Resource(Ok(_))));
    let requests = mkdir.requests();
    assert_eq!(requests.last().unwrap()["method"], "POST");
    assert!(
        requests.last().unwrap()["url"]
            .as_str()
            .unwrap()
            .ends_with("/workspaces/w/items")
    );
    assert_eq!(requests.last().unwrap()["body"]["type"], "Notebook");
}

#[tokio::test]
async fn root_api_and_find_reach_the_real_family_without_metadata_prefetch() {
    let mut api = Harness::new();
    api.reply(200, json!({"value":[{"id":"w"}]}));
    let output = api.execute(&["api", "workspaces"]).await;
    assert!(success(&output).contains("w"));
    assert_eq!(api.requests().len(), 1);
    let mut find = Harness::new();
    find.reply(200, json!({"value":[]}));
    let output = find.execute(&["find", "Book"]).await;
    success(&output);
    assert_eq!(find.requests().len(), 1);
    assert_eq!(find.requests()[0]["method"], "POST");
    assert!(
        find.requests()[0]["url"]
            .as_str()
            .unwrap()
            .ends_with("/catalog/search")
    );
}
