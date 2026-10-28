// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual standard and bulk Environment definition emission retains YAML scalars.

use super::*;

#[tokio::test]
async fn environment_publication_preserves_quoted_yaml_indicators_in_both_modes() {
    for bulk in [false, true] {
        let executor = Executor::default();
        executor.reply(200, json!({"capacityId":"assigned"}));
        executor.reply(200, json!({"value":[]}));
        executor.reply(200, json!({"value":[]}));
        if bulk {
            executor.reply(
                200,
                json!({"importItemDefinitionsDetails":[{
                    "itemType":"Environment","itemDisplayName":"Example",
                    "itemId":ITEM,"operationType":"Create",
                }]}),
            );
        } else {
            executor.reply(201, json!({"id":ITEM}));
        }
        executor.reply(202, Value::Null);
        executor.reply(200, json!({"value":[{"displayName":"Example","properties":{"publishDetails":{"state":"success"}}}]}));
        let config = config(&["Environment"], false);
        let mut source = item(
            "Environment",
            "Example",
            "33333333-3333-3333-3333-333333333333",
        );
        let settings = "instance_pool_id: 22222222-2222-2222-2222-222222222222\nspark_conf:\n  spark.custom.pattern: '*'\n  '<<': literal\n  anchor: '&value'\n  octal: '012'\n";
        source.files.push(ItemFile::new(
            "Setting/Sparkcompute.yml".into(),
            settings.as_bytes().to_vec(),
        ));
        let result = deploy_repository(
            &client(),
            &executor,
            &config,
            DeploymentRepository {
                items: vec![source],
                folders: Vec::new(),
            },
            EngineContext::from_config(&config, bulk).unwrap(),
            OperationLimits::default(),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(result.published[0].outcome.item_id, ITEM);
        let calls = executor.calls();
        let mutation = calls
            .iter()
            .find(|call| call.0 == ResourceMethod::Post)
            .unwrap();
        let parts = if bulk {
            mutation.2["definitionParts"].as_array().unwrap()
        } else {
            mutation.2["definition"]["parts"].as_array().unwrap()
        };
        let part = parts
            .iter()
            .find(|part| {
                part["path"]
                    .as_str()
                    .unwrap()
                    .ends_with("Setting/Sparkcompute.yml")
            })
            .unwrap();
        let emitted =
            String::from_utf8(STANDARD.decode(part["payload"].as_str().unwrap()).unwrap()).unwrap();
        assert_eq!(
            fabric_deploy::yaml::decode(
                &emitted,
                fabric_deploy::yaml::YamlPolicy::Config,
                Default::default()
            )
            .unwrap(),
            fabric_deploy::yaml::decode(
                settings,
                fabric_deploy::yaml::YamlPolicy::Config,
                Default::default()
            )
            .unwrap(),
        );
        assert!(executor.replies.lock().unwrap().is_empty());
    }
}
