// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual root deploy arguments through the broad native workflow.

#![cfg(feature = "native-read")]
#![deny(unnameable_test_items)]

#[path = "deploy_support/extensions.rs"]
mod extensions;
mod family_support;
#[path = "deploy_support/warnings.rs"]
mod warnings;
use family_support::{Harness, success};
use serde_json::{Value, json};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";

fn input(host: &Harness, parameters: bool) {
    let repository = host.root.join("repository");
    let item = repository.join("Example.CopyJob");
    std::fs::create_dir_all(&item).unwrap();
    std::fs::write(
        item.join(".platform"),
        serde_json::to_vec(&json!({
            "metadata":{"displayName":"Example","type":"CopyJob"},
            "config":{"logicalId":"33333333-3333-3333-3333-333333333333"},
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(item.join("content.json"), br#"{"value":"original"}"#).unwrap();
    std::fs::write(host.root.join("deploy.yml"),format!(
        "core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\n  item_types_in_scope: [CopyJob]\n{}unpublish:\n  skip: true\nfeatures: [disable_workspace_folder_publish]\n",
        if parameters {"  parameter: parameter.yml\n"}else{""},
    )).unwrap();
}

fn metadata(host: &Harness) {
    host.reply(200, json!({"capacityId":"assigned"}));
    host.reply(200, json!({"value":[]}));
}

#[tokio::test]
async fn root_copy_job_reaches_real_broad_deploy_publication() {
    let mut host = Harness::new();
    input(&host, false);
    metadata(&host);
    host.reply(201, json!({"id":ITEM}));
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "-f"])
        .await;
    success(&output);
    assert!(
        output
            .write_to(&mut family_support::BrokenWriter, &mut Vec::new())
            .is_err()
    );
    assert!(
        matches!(&output.evidence, fabric_cli::families::FamilyEvidence::Deploy(Ok(result))
            if result.completed.len() == 1 && result.completed[0].outcome.item_id == ITEM)
    );
    let requests = host.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2]["body"]["type"], "CopyJob");
    assert!(
        requests[2]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/workspaces/{WORKSPACE}/items"))
    );
}

#[tokio::test]
async fn root_templates_and_source_false_string_flags_reach_processed_definition() {
    let mut host = Harness::new();
    input(&host, true);
    std::fs::write(host.root.join("parameter.yml"),
        "extend: [template.yml]\nfind_replace:\n  - find_value: original\n    replace_value: {test: changed}\n    is_regex: 'yes'\n").unwrap();
    std::fs::write(host.root.join("template.yml"),
        "find_replace:\n  - find_value: changed\n    replace_value: {test: final}\n    ignore_case: ' true '\n").unwrap();
    metadata(&host);
    host.reply(201, json!({"id":ITEM}));
    let output = host
        .execute(&[
            "deploy",
            "--config",
            "deploy.yml",
            "--target_env",
            "test",
            "-f",
        ])
        .await;
    success(&output);
    let requests = host.requests();
    let part = requests[2]["body"]["definition"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["path"] == "content.json")
        .unwrap();
    let expected = serde_json::to_value(
        fabric_sdk::notebooks::DefinitionPart::inline("content.json", br#"{"value":"final"}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(part["payload"], expected["payload"]);
}

#[tokio::test]
async fn root_bulk_flag_uses_real_bulk_import_and_returned_identity() {
    let mut host = Harness::new();
    input(&host, false);
    metadata(&host);
    host.reply(
        200,
        json!({"importItemDefinitionsDetails":[{
            "itemType":"CopyJob","itemDisplayName":"Example","itemId":ITEM,"operationType":"Create",
        }]}),
    );
    let output = host
        .execute(&["deploy", "--config", "deploy.yml", "--bulk_publish", "-f"])
        .await;
    success(&output);
    let requests = host.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]["url"]
            .as_str()
            .unwrap()
            .ends_with("/items/bulkImportDefinitions")
    );
    assert!(requests[2]["body"]["definitionParts"].is_array());
    assert_ne!(requests[2]["body"], Value::Null);
}

#[tokio::test]
async fn full_root_publication_and_bulk_preserve_the_callers_fabric_binding() {
    for hostname in [
        "api.fabric.microsoft.com",
        "dailyapi.fabric.microsoft.com",
        "msitapi.fabric.microsoft.com",
    ] {
        for bulk in [false, true] {
            let mut host = Harness::new();
            host.endpoint =
                fabric_sdk::EndpointBinding::with_host(fabric_sdk::ServiceOrigin::Fabric, hostname)
                    .unwrap();
            input(&host, false);
            metadata(&host);
            if bulk {
                host.reply(
                    200,
                    json!({"importItemDefinitionsDetails":[{
                        "itemType":"CopyJob","itemDisplayName":"Example",
                        "itemId":ITEM,"operationType":"Create",
                    }]}),
                );
            } else {
                host.reply(201, json!({"id":ITEM}));
            }

            let mut args = vec!["deploy", "--config", "deploy.yml", "-f"];
            if bulk {
                args.push("--bulk_publish");
            }
            let output = host.execute(&args).await;
            success(&output);
            let requests = host.requests();
            assert_eq!(requests.len(), 3);
            assert!(requests.iter().all(|request| {
                request["url"]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("https://{hostname}/v1/"))
            }));
            let scopes = host.credential.0.lock().unwrap();
            assert!(!scopes.is_empty());
            assert!(
                scopes
                    .iter()
                    .all(|scopes| scopes == &[fabric_sdk::Audience::Fabric.scope()])
            );
        }
    }
}

#[tokio::test]
async fn explicit_root_environment_inputs_expand_before_yaml_without_identity_selection() {
    let mut host = Harness::new();
    input(&host, true);
    let config = host.root.join("deploy.yml");
    let source = std::fs::read_to_string(&config).unwrap().replace(
        "features: [disable_workspace_folder_publish]",
        "features: [disable_workspace_folder_publish, enable_environment_variable_replacement]",
    );
    std::fs::write(config, source).unwrap();
    std::fs::write(
        host.root.join("parameter.yml"),
        "find_replace:\n  - find_value: original\n    replace_value: $ENV:REPLACEMENT\n",
    )
    .unwrap();
    metadata(&host);
    host.reply(201, json!({"id":ITEM}));
    let inputs = fabric_cli::deploy::DeployInputs {
        environment: std::collections::BTreeMap::from([(
            "REPLACEMENT".into(),
            "{test: final}".into(),
        )]),
    };
    let output = host
        .execute_with_capabilities_and_deploy_inputs(
            &[
                "deploy",
                "--config",
                "deploy.yml",
                "--target_env",
                "test",
                "-f",
            ],
            std::future::pending(),
            &family_support::TransferUiCapture::default(),
            &inputs,
        )
        .await;
    success(&output);
    let requests = host.requests();
    assert_eq!(requests.len(), 3);
    let part = requests[2]["body"]["definition"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["path"] == "content.json")
        .unwrap();
    let expected = serde_json::to_value(
        fabric_sdk::notebooks::DefinitionPart::inline("content.json", br#"{"value":"final"}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(part["payload"], expected["payload"]);
    assert!(
        host.credential
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|scopes| scopes == &[fabric_sdk::Audience::Fabric.scope()])
    );
}
