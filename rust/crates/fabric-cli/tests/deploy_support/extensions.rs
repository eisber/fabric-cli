// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Root-command behavior for fields ignored by the actual source wrapper.

use super::*;

#[tokio::test]
async fn source_ignored_configuration_fields_do_not_block_real_publication() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../fabric-deploy/tests/config_extensions_execution_source.json"
    ))
    .unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 6);
    for case in fixture["cases"].as_array().unwrap() {
        assert_eq!(case["status"], "completed");
        assert_eq!(case["calls"], fixture["cases"][0]["calls"]);
        let mut host = Harness::new();
        input(&host, false);
        let path = host.root.join("deploy.yml");
        let mut config = fabric_deploy::yaml::decode(
            &std::fs::read_to_string(&path).unwrap(),
            fabric_deploy::yaml::YamlPolicy::Config,
            Default::default(),
        )
        .unwrap();
        if let Some(value) = case["document"].get("caller_metadata") {
            config["caller_metadata"] = value.clone();
        }
        for section in ["core", "publish", "unpublish"] {
            if let Some(value) = case["document"][section].get("caller_metadata") {
                let selected = config
                    .as_object_mut()
                    .unwrap()
                    .entry(section)
                    .or_insert_with(|| json!({}));
                selected["caller_metadata"] = value.clone();
            }
        }
        std::fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
        metadata(&host);
        host.reply(201, json!({"id":ITEM}));
        let output = host
            .execute(&["deploy", "--config", "deploy.yml", "-f"])
            .await;
        success(&output);
        let requests = host.requests();
        assert_eq!(requests.len(), 3, "{}", case["name"]);
        assert_eq!(requests[2]["body"]["type"], "CopyJob");
        assert!(
            matches!(&output.evidence, fabric_cli::families::FamilyEvidence::Deploy(Ok(result))
                if result.completed.len() == 1 && result.completed[0].outcome.item_id == ITEM)
        );
    }
}
