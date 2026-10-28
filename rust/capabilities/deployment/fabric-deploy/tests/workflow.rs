// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public deployment workflow against injected offline credentials and requests.

#![deny(unnameable_test_items)]

#[path = "workflow_support/additional.rs"]
mod additional;
#[path = "workflow_support/environment_scalars.rs"]
mod environment_scalars;

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use fabric_deploy::{
    deployment_config::{DeploymentConfig, ITEM_TYPES},
    model::{ItemFile, RepositoryItem},
    planner::{EngineContext, pipeline_references, topological_order},
    repository::DeploymentRepository,
    workflow::{WorkflowFailure, deploy_repository},
};
use fabric_sdk::{Audience, Client, CredentialSelection};
use fabric_sdk_operations::{
    OperationErrorKind, OperationExecutionError, OperationExecutor, OperationLimits,
    OperationRequest, OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    future::{Future, pending},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const SECOND: &str = "44444444-4444-4444-4444-444444444444";

#[derive(Debug)]
struct Credential;

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        Ok(AccessToken::new(
            "offline-deployment-test",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Default)]
struct Executor {
    replies: Mutex<VecDeque<(u16, Value)>>,
    requests: Mutex<Vec<(ResourceMethod, String, Value)>>,
    waits: Mutex<Vec<Duration>>,
}

impl Executor {
    fn reply(&self, status: u16, value: Value) {
        self.replies.lock().unwrap().push_back((status, value));
    }
    fn calls(&self) -> Vec<(ResourceMethod, String, Value)> {
        self.requests.lock().unwrap().clone()
    }
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.requests.lock().unwrap().push((
                request.method(),
                format!(
                    "{}{}",
                    request.url().path(),
                    request
                        .url()
                        .query()
                        .map(|query| format!("?{query}"))
                        .unwrap_or_default()
                ),
                if request.body().is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(request.body()).unwrap()
                },
            ));
            let (status, body) = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected offline request");
            Ok(OperationResponse {
                status,
                body: serde_json::to_vec(&body).unwrap(),
                headers: BTreeMap::new(),
            })
        })
    }
    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.waits.lock().unwrap().push(delay);
        Box::pin(async { Ok(()) })
    }
}

fn client() -> Client {
    Client::new(
        Arc::new(Credential),
        CredentialSelection::new("offline-account", "offline-tenant", Audience::Fabric).unwrap(),
    )
}

fn config(types: &[&str], unpublish: bool) -> DeploymentConfig {
    DeploymentConfig::from_document(&json!({
        "core": {"workspace_id": WORKSPACE, "repository_directory": ".", "item_types_in_scope": types},
        "unpublish": {"skip": !unpublish},
        "features": ["disable_workspace_folder_publish"],
    }), "test", None).unwrap()
}

fn item(item_type: &str, name: &str, logical: &str) -> RepositoryItem {
    RepositoryItem {
        name: name.into(),
        item_type: item_type.into(),
        description: "source fixture".into(),
        logical_id: logical.into(),
        path: format!("{name}.{item_type}"),
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
    }
}

#[tokio::test]
async fn every_source_publisher_dispatches_real_create_and_update_bodies_and_hooks() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/deploy_workflow/publisher_calls.json"
    ))
    .unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        let item_type = case["type"].as_str().unwrap();
        let update = case["update"].as_bool().unwrap();
        let executor = Executor::default();
        executor.reply(200, json!({"capacityId": "assigned"}));
        executor.reply(200, json!({"value": if update { json!([{"id": ITEM, "displayName": "Example", "type": item_type}]) } else { json!([]) }}));
        if matches!(item_type, "DataPipeline" | "KQLQueryset" | "KQLDashboard") {
            executor.reply(200, json!({"value": []}));
        }
        if item_type == "Environment" {
            executor.reply(200, json!({"value": []}));
        }
        executor.reply(
            if update { 200 } else { 201 },
            if update {
                Value::Null
            } else {
                json!({"id": ITEM})
            },
        );
        if item_type == "Lakehouse" {
            executor.reply(
                200,
                json!({"properties": {"sqlEndpointProperties": {"provisioningStatus": "Success"}}}),
            );
        }
        if item_type == "Environment" {
            executor.reply(202, Value::Null);
            executor.reply(200, json!({"value": [{"displayName": "Example", "properties": {"publishDetails": {"state": "success"}}}]}));
        }
        let config = config(&[item_type], false);
        let mut item = item(item_type, "Example", "33333333-3333-3333-3333-333333333333");
        if item_type == "DataPipeline" {
            item.files.push(ItemFile::new(
                "pipeline-content.json".into(),
                b"{}".to_vec(),
            ));
        }
        let result = deploy_repository(
            &client(),
            &executor,
            &config,
            DeploymentRepository {
                items: vec![item],
                folders: Vec::new(),
            },
            EngineContext::from_config(&config, false).unwrap(),
            OperationLimits::default(),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(result.published.len(), 1, "{item_type}");
        assert_eq!(result.published[0].outcome.item_id, ITEM, "{item_type}");
        let calls = executor.calls();
        let mutation = calls
            .iter()
            .find(|call| call.0 != ResourceMethod::Get)
            .unwrap();
        let mut expected = case["calls"][0]["body"].clone();
        if item_type == "DataPipeline" {
            let parts = expected
                .pointer_mut("/definition/parts")
                .unwrap()
                .as_array_mut()
                .unwrap();
            parts.push(json!({"path":"pipeline-content.json","payload":"e30=","payloadType":"InlineBase64"}));
        }
        assert_eq!(mutation.2, expected, "{item_type} update={update}");
        assert!(executor.replies.lock().unwrap().is_empty(), "{item_type}");
    }
    assert_eq!(ITEM_TYPES.len(), 33);
}

#[test]
fn nested_pipeline_references_and_cycles_match_captured_actual_source_functions() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/deploy_workflow/publisher_calls.json"
    ))
    .unwrap();
    for case in oracle["pipeline_cases"].as_array().unwrap() {
        assert_eq!(
            json!(pipeline_references(&case["content"])),
            case["references"]
        );
    }
    for case in oracle["sort_cases"].as_array().unwrap() {
        let graph = case["graph"].as_object().unwrap();
        let indices: Vec<_> = (0..graph.len()).collect();
        let lookup: HashMap<_, _> = graph
            .keys()
            .enumerate()
            .map(|(index, key)| (key.as_str(), index))
            .collect();
        let edges: Vec<_> = graph
            .iter()
            .enumerate()
            .flat_map(|(index, (_, references))| {
                references
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|reference| (index, lookup[reference.as_str().unwrap()]))
                    .collect::<Vec<_>>()
            })
            .collect();
        let result = topological_order(&indices, &edges);
        if case["result"].is_object() {
            assert!(result.is_err());
        } else {
            let mut result = result.unwrap();
            if case["mode"] == "Deployed" {
                result.reverse();
            }
            assert_eq!(
                json!(
                    result
                        .iter()
                        .map(|index| graph.keys().nth(*index).unwrap())
                        .collect::<Vec<_>>()
                ),
                case["result"]
            );
        }
    }
}

#[tokio::test]
async fn source_type_order_and_new_logical_references_use_actual_returned_ids() {
    let executor = Executor::default();
    executor.reply(200, json!({"capacityId": "assigned"}));
    executor.reply(200, json!({"value":[]}));
    executor.reply(201, json!({"id":ITEM}));
    executor.reply(201, json!({"id":SECOND}));
    let config = config(&["Notebook", "CopyJob"], false);
    let first = item("Notebook", "First", "33333333-3333-3333-3333-333333333333");
    let mut second = item("CopyJob", "Second", "55555555-5555-5555-5555-555555555555");
    second.files[1].bytes = first.logical_id.as_bytes().to_vec();
    let result = deploy_repository(
        &client(),
        &executor,
        &config,
        DeploymentRepository {
            items: vec![second, first],
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
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        ["First", "Second"]
    );
    assert_eq!(
        executor.calls()[3].2["definition"]["parts"][1]["payload"],
        "MjIyMjIyMjItMjIyMi0yMjIyLTIyMjItMjIyMjIyMjIyMjIy"
    );
}

#[tokio::test]
async fn partial_failure_retains_completed_ids_and_never_replays_writes() {
    let executor = Executor::default();
    executor.reply(200, json!({"capacityId":"assigned"}));
    executor.reply(200, json!({"value":[]}));
    executor.reply(201, json!({"id":ITEM}));
    executor.reply(500, json!({"errorCode":"InjectedFailure"}));
    let config = config(&["CopyJob"], false);
    let result = deploy_repository(
        &client(),
        &executor,
        &config,
        DeploymentRepository {
            items: vec![
                item("CopyJob", "First", "logical-first"),
                item("CopyJob", "Second", "logical-second"),
            ],
            folders: Vec::new(),
        },
        EngineContext::from_config(&config, false).unwrap(),
        OperationLimits::default(),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(result.partial.published.len(), 1);
    assert!(
        matches!(result.failure, WorkflowFailure::Operation(error) if error.mutation_may_have_occurred)
    );
    assert_eq!(
        executor
            .calls()
            .iter()
            .filter(|call| call.0 == ResourceMethod::Post)
            .count(),
        2
    );
}

#[tokio::test]
async fn cancellation_before_first_request_has_no_side_effects() {
    let executor = Executor::default();
    let config = config(&["CopyJob"], false);
    let result = deploy_repository(
        &client(),
        &executor,
        &config,
        DeploymentRepository::default(),
        EngineContext::from_config(&config, false).unwrap(),
        OperationLimits::default(),
        async {},
    )
    .await
    .unwrap_err();
    assert!(
        matches!(result.failure, WorkflowFailure::Operation(error) if error.kind == OperationErrorKind::Cancelled)
    );
    assert!(executor.calls().is_empty());
}

#[tokio::test]
async fn real_report_kql_and_environment_preprocessors_match_captured_source_artifacts() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/deploy_workflow/publisher_calls.json"
    ))
    .unwrap();
    for case in oracle["preprocess_cases"].as_array().unwrap() {
        let kind = case["type"].as_str().unwrap();
        let executor = Executor::default();
        let deployed = json!({"value": [
            {"id": ITEM, "displayName": "Database", "type": "KQLDatabase"},
            {"id": ITEM, "displayName": "Model", "type": "SemanticModel"},
        ]});
        executor.reply(200, json!({"capacityId": "assigned"}));
        executor.reply(200, deployed.clone());
        if matches!(kind, "KQLQueryset" | "KQLDashboard") {
            executor.reply(200, deployed);
            executor.reply(200, json!({"properties": {"queryServiceUri": "https://synthetic.kusto.fabric.microsoft.com"}}));
        }

        if kind == "Environment" {
            executor.reply(200, json!({"value": []}));
        }
        executor.reply(201, json!({"id": SECOND}));
        if kind == "Environment" {
            executor.reply(202, Value::Null);
            executor.reply(200, json!({"value": [{"displayName":"Example","properties":{"publishDetails":{"state":"success"}}}]}));
        }
        let mut source = item(kind, "Example", "55555555-5555-5555-5555-555555555555");
        source.files[1] = ItemFile::new(
            case["path"].as_str().unwrap().into(),
            case["input"].as_str().unwrap().as_bytes().to_vec(),
        );
        let dependency = if kind == "Report" {
            item(
                "SemanticModel",
                "Model",
                "33333333-3333-3333-3333-333333333333",
            )
        } else {
            item(
                "KQLDatabase",
                "Database",
                "33333333-3333-3333-3333-333333333333",
            )
        };
        let config = config(&[kind], false);
        deploy_repository(
            &client(),
            &executor,
            &config,
            DeploymentRepository {
                items: vec![source, dependency],
                folders: Vec::new(),
            },
            EngineContext::from_config(&config, false).unwrap(),
            OperationLimits::default(),
            pending(),
        )
        .await
        .unwrap();
        let calls = executor.calls();
        let mutation = calls
            .iter()
            .find(|call| call.0 == ResourceMethod::Post)
            .unwrap();
        let parts = mutation.2["definition"]["parts"].as_array().unwrap();
        let part = parts
            .iter()
            .find(|part| part["path"] == case["path"])
            .unwrap();
        let actual =
            String::from_utf8(STANDARD.decode(part["payload"].as_str().unwrap()).unwrap()).unwrap();
        let expected = case["output"]
            .as_str()
            .unwrap()
            .replace("33333333-3333-3333-3333-333333333333", ITEM);
        assert_eq!(actual, expected, "{kind}");
        assert!(executor.replies.lock().unwrap().is_empty(), "{kind}");
    }
}

#[tokio::test]
async fn variable_library_hooks_read_prepared_settings_in_standard_and_bulk_modes() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/deploy_workflow/post_publish_source.json"
    ))
    .unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        let bulk = case["bulk"].as_bool().unwrap();
        let executor = Executor::default();
        executor.reply(200, json!({"capacityId":"assigned"}));
        executor.reply(200, json!({"value":[]}));
        executor.reply(if bulk {200} else {201}, if bulk {
                        json!({"importItemDefinitionsDetails":[{
                            "itemType":"VariableLibrary","itemDisplayName":"Example","itemId":ITEM,"operationType":"Create",
                        }]})
                    } else { json!({"id":ITEM}) });
        executor.reply(200, Value::Null);
        let mut config = DeploymentConfig::from_document(&json!({
                        "core":{"workspace_id":WORKSPACE,"repository_directory":".","item_types_in_scope":["VariableLibrary"],"parameter":"p.yml"},
                        "unpublish":{"skip":true},"features":["disable_workspace_folder_publish"],
                    }),"test",None).unwrap();
        config
            .resolve_parameters(&json!({"find_replace":[{
                "find_value":"dev","replace_value":{"test":"test"},
            }]}))
            .unwrap();
        let mut source = item(
            "VariableLibrary",
            "Example",
            "33333333-3333-3333-3333-333333333333",
        );
        source.files[0].bytes = serde_json::to_vec(&json!({
            "metadata":{"displayName":"Example","type":"VariableLibrary"},
            "config":{"logicalId":source.logical_id},
        }))
        .unwrap();
        source.files[1] = ItemFile::new(
            "settings.json".into(),
            oracle["input_settings"]
                .as_str()
                .unwrap()
                .as_bytes()
                .to_vec(),
        );
        deploy_repository(
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
        let calls = executor.calls();
        let parts = if bulk {
            &calls[2].2["definitionParts"]
        } else {
            &calls[2].2["definition"]["parts"]
        };
        let settings = parts
            .as_array()
            .unwrap()
            .iter()
            .find(|part| part["path"].as_str().unwrap().ends_with("settings.json"))
            .unwrap();
        assert_eq!(
            String::from_utf8(
                STANDARD
                    .decode(settings["payload"].as_str().unwrap())
                    .unwrap()
            )
            .unwrap(),
            oracle["prepared_settings"].as_str().unwrap()
        );
        assert_eq!(calls[3].0, ResourceMethod::Patch);
        assert_eq!(calls[3].2, case["events"][1]["body"]);
        assert!(
            case["events"][1]["url"]
                .as_str()
                .unwrap()
                .ends_with(&calls[3].1)
        );
    }
}

#[tokio::test]
async fn aggregate_transformed_definition_limit_fails_before_publication() {
    let executor = Executor::default();
    executor.reply(200, json!({"capacityId":"assigned"}));
    executor.reply(200, json!({"value":[]}));
    let mut config = DeploymentConfig::from_document(&json!({
                    "core":{"workspace_id":WORKSPACE,"repository_directory":".","item_types_in_scope":["CopyJob"],"parameter":"p.yml"},
                    "unpublish":{"skip":true},"features":["disable_workspace_folder_publish"],
                }),"test",None).unwrap();
    config
        .resolve_parameters(&json!({"find_replace":[{
            "find_value":"x","replace_value":{"test":"x".repeat(150)},
        }]}))
        .unwrap();
    let mut source = item("CopyJob", "Example", "copy-logical");
    source.files[1] = ItemFile::new("content.json".into(), br#"{"v":"x"}"#.to_vec());
    source
        .files
        .push(ItemFile::new("resource.txt".into(), b"x".to_vec()));
    let context = EngineContext::from_config(&config, false)
        .unwrap()
        .with_definition_limits(fabric_deploy::notebook::DefinitionLimits {
            max_bytes: std::num::NonZeroUsize::new(300).unwrap(),
        });
    let error = deploy_repository(
        &client(),
        &executor,
        &config,
        DeploymentRepository {
            items: vec![source],
            folders: Vec::new(),
        },
        context,
        OperationLimits::default(),
        pending(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.failure,
        WorkflowFailure::Configuration(fabric_deploy::deployment_config::ConfigurationError::Limit)
    ));
    assert!(error.partial.effects.is_empty());
    assert_eq!(executor.calls().len(), 2);
}

#[tokio::test]
async fn excluded_pipeline_still_runs_source_metadata_pre_hook_without_definition_reads() {
    let source: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/deploy_workflow/refresh_source.json"
    ))
    .unwrap();
    let executor = Executor::default();
    executor.reply(200, json!({"capacityId":"assigned"}));
    executor.reply(200, json!({"value":[]}));
    executor.reply(200, json!({"value":[]}));
    let config = DeploymentConfig::from_document(&json!({
                    "core":{"workspace_id":WORKSPACE,"repository_directory":".","item_types_in_scope":["DataPipeline"]},
                    "publish":{"items_to_include":["Absent.DataPipeline"]},"unpublish":{"skip":true},
                    "features":["enable_experimental_features","enable_items_to_include","disable_workspace_folder_publish"],
                }),"test",None).unwrap();
    let result = deploy_repository(
        &client(),
        &executor,
        &config,
        DeploymentRepository {
            items: vec![item("DataPipeline", "Excluded", "excluded-logical")],
            folders: Vec::new(),
        },
        EngineContext::from_config(&config, false).unwrap(),
        OperationLimits::default(),
        pending(),
    )
    .await
    .unwrap();
    assert!(result.published.is_empty());
    assert_eq!(
        executor.calls().len(),
        2 + source["excluded_pipeline"]["events"]
            .as_array()
            .unwrap()
            .len()
    );
    assert!(
        executor.calls()[2].1.ends_with(
            source["excluded_pipeline"]["events"][0]["path"]
                .as_str()
                .unwrap()
        )
    );
    assert!(
        executor
            .calls()
            .iter()
            .all(|call| call.0 == ResourceMethod::Get)
    );
}

#[tokio::test]
async fn type_pre_hook_refresh_does_not_retarget_an_initially_absent_item() {
    let source: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/deploy_workflow/refresh_source.json"
    ))
    .unwrap();
    let source = &source["initially_absent_queryset"];
    let executor = Executor::default();
    executor.reply(200, json!({"capacityId":"assigned"}));
    executor.reply(200, json!({"value":[]}));
    executor.reply(200,json!({"value":[{"id":source["fresh_cache_id"],"displayName":"Example","type":"KQLQueryset"}]}));
    executor.reply(201, json!({"id":ITEM}));
    let config = config(&["KQLQueryset"], false);
    let result = deploy_repository(
        &client(),
        &executor,
        &config,
        DeploymentRepository {
            items: vec![item("KQLQueryset", "Example", "queryset-logical")],
            folders: Vec::new(),
        },
        EngineContext::from_config(&config, false).unwrap(),
        OperationLimits::default(),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(
        result.published[0].outcome.item_id,
        source["returned_id"].as_str().unwrap()
    );
    assert!(
        source["events"][1]["url"]
            .as_str()
            .unwrap()
            .ends_with(&executor.calls()[3].1)
    );
    assert!(!executor.calls()[3].1.contains("updateDefinition"));
}

#[tokio::test]
async fn scoped_dynamic_no_match_never_resolves_missing_metadata() {
    let executor = Executor::default();
    executor.reply(200, json!({"capacityId":"assigned"}));
    executor.reply(200, json!({"value":[]}));
    executor.reply(201, json!({"id":ITEM}));
    let mut config=DeploymentConfig::from_document(&json!({
                    "core":{"workspace_id":WORKSPACE,"repository_directory":".","item_types_in_scope":["CopyJob"],"parameter":"p.yml"},
                    "unpublish":{"skip":true},"features":["disable_workspace_folder_publish"],
                }),"test",None).unwrap();
    config
        .resolve_parameters(&json!({"find_replace":[{
            "find_value":"ABSENT","replace_value":{"test":"$items.Notebook.Missing.$id"},
            "item_type":"CopyJob",
        }]}))
        .unwrap();
    let result = deploy_repository(
        &client(),
        &executor,
        &config,
        DeploymentRepository {
            items: vec![item("CopyJob", "Example", "copy-logical")],
            folders: Vec::new(),
        },
        EngineContext::from_config(&config, false).unwrap(),
        OperationLimits::default(),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(result.published[0].outcome.item_id, ITEM);
    assert_eq!(executor.calls().len(), 3);
}

#[tokio::test]
async fn scoped_dynamic_occurrences_refresh_metadata_without_replaying_mutations() {
    let executor = Executor::default();
    executor.reply(200, json!({"capacityId":"assigned"}));
    executor.reply(200, json!({"value":[]}));
    for _ in 0..2 {
        executor.reply(
            200,
            json!({"value":[{"id":SECOND,"displayName":"Target","type":"Notebook"}]}),
        );
    }

    executor.reply(201, json!({"id":ITEM}));
    let mut config=DeploymentConfig::from_document(&json!({
                    "core":{"workspace_id":WORKSPACE,"repository_directory":".","item_types_in_scope":["CopyJob"],"parameter":"p.yml"},
                    "unpublish":{"skip":true},"features":["disable_workspace_folder_publish"],
                }),"test",None).unwrap();
    config
        .resolve_parameters(&json!({"find_replace":[
            {"find_value":"old","replace_value":{"test":"$items.Notebook.Target.$id"}},
            {"find_value":"second","replace_value":{"test":"$items.Notebook.Target.$id"}},
        ]}))
        .unwrap();
    let mut source = item("CopyJob", "Example", "copy-logical");
    source.files[1].bytes = br#"{"value":"old second"}"#.to_vec();
    deploy_repository(
        &client(),
        &executor,
        &config,
        DeploymentRepository {
            items: vec![source],
            folders: Vec::new(),
        },
        EngineContext::from_config(&config, false).unwrap(),
        OperationLimits::default(),
        pending(),
    )
    .await
    .unwrap();
    let calls = executor.calls();
    assert_eq!(calls.len(), 5);
    assert!(calls[..4].iter().all(|call| call.0 == ResourceMethod::Get));
    assert_eq!(calls[4].0, ResourceMethod::Post);
    let actual = String::from_utf8(
        STANDARD
            .decode(
                calls[4].2["definition"]["parts"][1]["payload"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(actual, format!("{{\"value\":\"{SECOND} {SECOND}\"}}"));
}

#[tokio::test]
async fn source_feature_list_scope_failure_keeps_completed_effects_without_globals() {
    let executor = Executor::default();
    executor.reply(200, json!({"capacityId":"assigned"}));
    executor.reply(200, json!({"value":[]}));
    executor.reply(201, json!({"id":ITEM}));
    let config=DeploymentConfig::from_document(&json!({
                        "core":{"workspace_id":WORKSPACE,"repository_directory":".","item_types_in_scope":["CopyJob"]},
                        "unpublish":{"skip":true},"features":["disable_workspace_folder_publish"],
                        "constants":{"FEATURE_FLAG":{"test":["disable_workspace_folder_publish"]}},
                    }),"test",None).unwrap();
    let error = deploy_repository(
        &client(),
        &executor,
        &config,
        DeploymentRepository {
            items: vec![item("CopyJob", "Example", "copy-logical")],
            folders: Vec::new(),
        },
        EngineContext::from_config(&config, false).unwrap(),
        OperationLimits::default(),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.partial.published[0].outcome.item_id, ITEM);
    assert_eq!(error.partial.effects.len(), 1);
    assert!(matches!(error.failure, WorkflowFailure::Configuration(_)));
    assert_eq!(executor.calls().len(), 3);
}
