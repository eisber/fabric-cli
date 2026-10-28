// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Selected endpoint/scope contracts through the actual shared HTTP executor.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, headers::Headers},
    time::OffsetDateTime,
};
use fabric_core::{Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin};
use fabric_deploy::{
    deployment_config::DeploymentConfig,
    model::{ItemFile, RepositoryItem},
    notebook::{DefinitionLimits, DeploymentError, LocalNotebook},
    planner::EngineContext,
    publish::{PublishFailure, publish_repository},
    repository::{DeploymentRepository, NotebookRepository},
    transforms::NotebookTransforms,
    workflow::deploy_repository,
};
use fabric_sdk::{
    Client,
    notebooks::{NotebookErrorKind, NotebookLimits},
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_operations::{OperationErrorKind, OperationLimits};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::pending,
    sync::{Arc, Mutex},
    time::Duration,
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OPERATION: &str = "44444444-4444-4444-4444-444444444444";
const HOSTS: &[&str] = &[
    "api.fabric.microsoft.com",
    "dailyapi.fabric.microsoft.com",
    "msitapi.fabric.microsoft.com",
];

#[derive(Debug, Default)]
struct Credential(Mutex<Vec<Vec<String>>>);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0
            .lock()
            .unwrap()
            .push(scopes.iter().map(|value| (*value).to_owned()).collect());
        Ok(AccessToken::new(
            "offline-selected-deployment",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug, Default)]
struct Wire {
    replies: Mutex<VecDeque<AsyncRawResponse>>,
    requests: Mutex<Vec<Request>>,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request or mutation replay"))
    }
}

impl Wire {
    fn reply(&self, status: u16, body: Value, headers: &[(&str, &str)]) {
        let mut values = Headers::new();
        for (name, value) in headers {
            values.insert((*name).to_owned(), (*value).to_owned());
        }
        self.replies
            .lock()
            .unwrap()
            .push_back(AsyncRawResponse::from_bytes(
                status.into(),
                values,
                serde_json::to_vec(&body).unwrap(),
            ));
    }
}

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(async {})
    }
}

struct Harness {
    client: Client,
    http: HttpExecutor,
    wire: Arc<Wire>,
    credential: Arc<Credential>,
}

impl Harness {
    fn new(host: &str) -> Self {
        let endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap();
        let credential = Arc::new(Credential::default());
        let erased: Arc<dyn TokenCredential> = credential.clone();
        let client = Client::with_context(RequestContext::with_endpoint(
            erased.clone(),
            CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
            endpoint.clone(),
        ));
        assert!(Arc::ptr_eq(client.credential(), &erased));
        let wire = Arc::new(Wire::default());
        let http = HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default())
            .unwrap()
            .with_endpoint(endpoint);
        Self {
            client,
            http,
            wire,
            credential,
        }
    }

    fn assert_bound(&self, host: &str) {
        let requests = self.wire.requests.lock().unwrap();
        assert!(!requests.is_empty());
        for request in requests.iter() {
            assert_eq!(request.url().scheme(), "https");
            assert_eq!(request.url().host_str(), Some(host));
            assert_eq!(request.url().port_or_known_default(), Some(443));
        }
        let scopes = self.credential.0.lock().unwrap();
        assert_eq!(
            scopes.len(),
            requests.len(),
            "every request uses the injected Fabric credential"
        );
        assert!(
            scopes
                .iter()
                .all(|scopes| scopes == &[Audience::Fabric.scope().to_owned()])
        );
        assert!(self.wire.replies.lock().unwrap().is_empty());
    }
}

fn config(kind: &str, folders_disabled: bool) -> DeploymentConfig {
    let mut document = json!({
        "core": {"workspace_id":WORKSPACE,"repository_directory":".","item_types_in_scope":[kind]},
        "unpublish": {"skip":true},
    });
    if folders_disabled {
        document["features"] = json!(["disable_workspace_folder_publish"]);
    }
    DeploymentConfig::from_document(&document, "test", None).unwrap()
}

fn notebook() -> LocalNotebook {
    LocalNotebook::from_files(
        vec![
            (
                ".platform".into(),
                serde_json::to_vec(&json!({
                    "metadata":{"displayName":"Example","type":"Notebook"},
                    "config":{"logicalId":"33333333-3333-3333-3333-333333333333"},
                }))
                .unwrap(),
            ),
            (
                "notebook-content.py".into(),
                b"# Fabric notebook source\nprint('offline')\n".to_vec(),
            ),
        ],
        DefinitionLimits::default(),
    )
    .unwrap()
}

#[tokio::test]
async fn configured_notebook_metadata_paging_create_poll_and_result_keep_selected_host_and_scope() {
    for &host in HOSTS {
        let harness = Harness::new(host);
        let root = format!("https://{host}");
        let operation = format!("{root}/v1/operations/{OPERATION}");
        let result = format!("{operation}/result");
        harness
            .wire
            .reply(200, json!({"capacityId":"assigned"}), &[]);
        harness.wire.reply(200, json!({"value":[],"continuationToken":"next","continuationUri":format!("{root}/v1/workspaces/{WORKSPACE}/items?continuationToken=next")}), &[]);
        harness.wire.reply(200, json!({"value":[]}), &[]);
        harness.wire.reply(
            202,
            Value::Null,
            &[
                ("x-ms-operation-id", OPERATION),
                ("location", &operation),
                ("retry-after", "0"),
            ],
        );
        harness
            .wire
            .reply(200, json!({"status":"Succeeded"}), &[("location", &result)]);
        harness.wire.reply(200, json!({"id":ITEM}), &[]);
        let config = config("Notebook", false);
        publish_repository(
            &harness.client,
            &harness.http,
            &config,
            NotebookRepository {
                notebooks: vec![notebook()],
            },
            OperationLimits::default(),
            pending(),
        )
        .await
        .unwrap();
        harness.assert_bound(host);
        let requests = harness.wire.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|request| request.url().to_string())
                .collect::<Vec<_>>(),
            [
                format!("{root}/v1/workspaces/{WORKSPACE}"),
                format!("{root}/v1/workspaces/{WORKSPACE}/items"),
                format!("{root}/v1/workspaces/{WORKSPACE}/items?continuationToken=next"),
                format!("{root}/v1/workspaces/{WORKSPACE}/items"),
                operation,
                result,
            ]
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method() == Method::Post)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn public_single_notebook_api_keeps_selected_continuation_and_lro_links() {
    for &host in HOSTS {
        let harness = Harness::new(host);
        let root = format!("https://{host}");
        let operation = format!("{root}/v1/operations/{OPERATION}");
        let result = format!("{operation}/result");
        harness.wire.reply(200, json!({"value":[],"continuationToken":"next","continuationUri":format!("{root}/v1/workspaces/{WORKSPACE}/items?continuationToken=next")}), &[]);
        harness.wire.reply(200, json!({"value":[]}), &[]);
        harness.wire.reply(
            202,
            Value::Null,
            &[
                ("x-ms-operation-id", OPERATION),
                ("location", &operation),
                ("retry-after", "0"),
            ],
        );
        harness
            .wire
            .reply(200, json!({"status":"Succeeded"}), &[("location", &result)]);
        harness.wire.reply(
            200,
            json!({"id":ITEM,"displayName":"Example","type":"Notebook"}),
            &[],
        );
        let prepared = notebook()
            .prepare(WORKSPACE, &NotebookTransforms::default())
            .unwrap();
        let outcome = prepared
            .deploy(
                &harness.client,
                &harness.http,
                NotebookLimits::default(),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.item_id, ITEM);
        harness.assert_bound(host);
    }
}

#[tokio::test]
async fn every_source_dispatch_context_keeps_all_requests_on_selected_profile_with_fabric_scope() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/deploy_workflow/publisher_calls.json"
    ))
    .unwrap();
    for &host in HOSTS {
        for case in oracle["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| case["update"] == false)
        {
            let kind = case["type"].as_str().unwrap();
            let harness = Harness::new(host);
            harness
                .wire
                .reply(200, json!({"capacityId":"assigned"}), &[]);
            harness.wire.reply(200, json!({"value":[]}), &[]);
            if matches!(kind, "DataPipeline" | "KQLQueryset" | "KQLDashboard") {
                harness.wire.reply(200, json!({"value":[]}), &[]);
            }

            #[tokio::test]
            async fn selected_notebook_rejects_public_continuation_without_fallback_or_mutation() {
                let host = "msitapi.fabric.microsoft.com";
                let harness = Harness::new(host);
                harness.wire.reply(200, json!({
                    "value":[],"continuationToken":"next",
                    "continuationUri":format!("https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items?continuationToken=next"),
                }), &[]);
                let prepared = notebook()
                    .prepare(WORKSPACE, &NotebookTransforms::default())
                    .unwrap();
                let error = prepared
                    .deploy(
                        &harness.client,
                        &harness.http,
                        NotebookLimits::default(),
                        pending(),
                    )
                    .await
                    .unwrap_err();
                assert!(
                    matches!(error, DeploymentError::Service(error) if error.kind == NotebookErrorKind::InvalidLink && !error.mutation_may_have_occurred)
                );
                harness.assert_bound(host);
                assert_eq!(harness.wire.requests.lock().unwrap().len(), 1);
            }

            #[tokio::test]
            async fn selected_publication_rejects_public_lro_link_and_retains_uncertain_write() {
                let host = "msitapi.fabric.microsoft.com";
                let harness = Harness::new(host);
                harness
                    .wire
                    .reply(200, json!({"capacityId":"assigned"}), &[]);
                harness.wire.reply(200, json!({"value":[]}), &[]);
                let wrong = format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}");
                harness.wire.reply(
                    202,
                    Value::Null,
                    &[
                        ("x-ms-operation-id", OPERATION),
                        ("location", &wrong),
                        ("retry-after", "0"),
                    ],
                );
                let config = config("Notebook", false);
                let error = publish_repository(
                    &harness.client,
                    &harness.http,
                    &config,
                    NotebookRepository {
                        notebooks: vec![notebook()],
                    },
                    OperationLimits::default(),
                    pending(),
                )
                .await
                .unwrap_err();
                assert!(
                    matches!(error.failure,PublishFailure::Operation(error) if error.kind == OperationErrorKind::InvalidLink && error.mutation_may_have_occurred)
                );
                harness.assert_bound(host);
                let requests = harness.wire.requests.lock().unwrap();
                assert_eq!(requests.len(), 3);
                assert_eq!(
                    requests
                        .iter()
                        .filter(|request| request.method() == Method::Post)
                        .count(),
                    1
                );
            }

            #[tokio::test]
            async fn workspace_name_default_unpublish_and_folder_pruning_keep_selected_profile() {
                for &host in HOSTS {
                    let harness = Harness::new(host);
                    let child = "55555555-5555-5555-5555-555555555555";
                    harness.wire.reply(
                        200,
                        json!({"value":[{"id":WORKSPACE,"displayName":"Target"}]}),
                        &[],
                    );
                    harness.wire.reply(
                        200,
                        json!({"value":[{"id":ITEM,"displayName":"Orphan","type":"Notebook"}]}),
                        &[],
                    );
                    harness.wire.reply(200, Value::Null, &[]);
                    harness.wire.reply(200, json!({"value":[]}), &[]);
                    harness.wire.reply(
                        200,
                        json!({"value":[
                            {"id":OPERATION,"displayName":"Parent"},
                            {"id":child,"displayName":"Child","parentFolderId":OPERATION},
                        ]}),
                        &[],
                    );
                    harness.wire.reply(200, Value::Null, &[]);
                    harness.wire.reply(200, Value::Null, &[]);
                    let config = DeploymentConfig::from_document(&json!({
                        "core":{"workspace":"Target","repository_directory":".","item_types_in_scope":["Notebook"]},
                        "publish":{"skip":true},
                    }),"test",None).unwrap();
                    deploy_repository(
                        &harness.client,
                        &harness.http,
                        &config,
                        DeploymentRepository::default(),
                        EngineContext::from_config(&config, false).unwrap(),
                        OperationLimits::default(),
                        pending(),
                    )
                    .await
                    .unwrap();
                    harness.assert_bound(host);
                    let requests = harness.wire.requests.lock().unwrap();
                    assert_eq!(requests.len(), 7);
                    assert_eq!(requests[0].url().path(), "/v1/workspaces");
                    assert_eq!(
                        requests[5].url().path(),
                        format!("/v1/workspaces/{WORKSPACE}/folders/{child}")
                    );
                    assert_eq!(
                        requests[6].url().path(),
                        format!("/v1/workspaces/{WORKSPACE}/folders/{OPERATION}")
                    );
                    assert_eq!(
                        requests
                            .iter()
                            .filter(|request| request.method() == Method::Delete)
                            .count(),
                        3
                    );
                }
            }

            #[tokio::test]
            async fn scoped_bulk_import_keeps_selected_profile_and_actual_returned_id() {
                for &host in HOSTS {
                    let harness = Harness::new(host);
                    harness
                        .wire
                        .reply(200, json!({"capacityId":"assigned"}), &[]);
                    harness.wire.reply(200, json!({"value":[]}), &[]);
                    harness.wire.reply(200,json!({"importItemDefinitionsDetails":[{
                        "itemType":"CopyJob","itemDisplayName":"Example","itemId":ITEM,"operationType":"Create",
                    }]}),&[]);
                    let config = config("CopyJob", true);
                    let item =
                        RepositoryItem::from_files(
                            "Example.CopyJob".into(),
                            vec![
                        ItemFile::new(".platform".into(),serde_json::to_vec(&json!({
                            "metadata":{"displayName":"Example","type":"CopyJob"},
                            "config":{"logicalId":"33333333-3333-3333-3333-333333333333"},
                        })).unwrap()),
                        ItemFile::new("content.json".into(),b"{}".to_vec()),
                    ],
                        )
                        .unwrap();
                    let result = deploy_repository(
                        &harness.client,
                        &harness.http,
                        &config,
                        DeploymentRepository {
                            items: vec![item],
                            folders: Vec::new(),
                        },
                        EngineContext::from_config(&config, true).unwrap(),
                        OperationLimits::default(),
                        pending(),
                    )
                    .await
                    .unwrap();
                    assert_eq!(result.published[0].outcome.item_id, ITEM);
                    harness.assert_bound(host);
                    let requests = harness.wire.requests.lock().unwrap();
                    assert_eq!(requests.len(), 3);
                    assert_eq!(
                        requests[2].url().path(),
                        format!("/v1/workspaces/{WORKSPACE}/items/bulkImportDefinitions")
                    );
                }

                #[tokio::test]
                async fn lakehouse_shortcut_pruning_uses_source_encoded_names_on_the_selected_http_path()
                 {
                    let oracle: Value = serde_json::from_str(include_str!(
                        "../../../tests/compatibility/deploy_workflow/shortcuts_source.json"
                    ))
                    .unwrap();
                    let harness = Harness::new("msitapi.fabric.microsoft.com");
                    harness
                        .wire
                        .reply(200, json!({"capacityId":"assigned"}), &[]);
                    harness.wire.reply(200, json!({"value":[]}), &[]);
                    harness.wire.reply(201, json!({"id":ITEM}), &[]);
                    harness.wire.reply(200,json!({"properties":{"sqlEndpointProperties":{"provisioningStatus":"Success"}}}),&[]);
                    let mut shortcuts = vec![json!({"path":"Tables","name":"Keep"})];
                    for case in oracle["cases"].as_array().unwrap() {
                        let (path, name) = case["shortcut_path"]
                            .as_str()
                            .unwrap()
                            .rsplit_once('/')
                            .unwrap();
                        shortcuts.push(json!({"path":path,"name":name}));
                    }
                    harness.wire.reply(200, json!({"value":shortcuts}), &[]);
                    for _ in oracle["cases"].as_array().unwrap() {
                        harness.wire.reply(200, Value::Null, &[]);
                    }
                    harness.wire.reply(200, json!({"name":"Keep"}), &[]);
                    let config = DeploymentConfig::from_document(&json!({
                        "core":{"workspace_id":WORKSPACE,"repository_directory":".","item_types_in_scope":["Lakehouse"]},
                        "unpublish":{"skip":true},
                        "features":["disable_workspace_folder_publish","enable_shortcut_publish"],
                    }),"test",None).unwrap();
                    let item = RepositoryItem::from_files("Example.Lakehouse".into(),vec![
                        ItemFile::new(".platform".into(),serde_json::to_vec(&json!({
                            "metadata":{"displayName":"Example","type":"Lakehouse"},
                            "config":{"logicalId":"33333333-3333-3333-3333-333333333333"},
                        })).unwrap()),
                        ItemFile::new("shortcuts.metadata.json".into(),serde_json::to_vec(&json!([{
                            "path":"Tables","name":"Keep","target":{"oneLake":{
                                "workspaceId":WORKSPACE,"itemId":fabric_deploy::transforms::DEFAULT_GUID,"path":"Tables/source",
                            }},
                        }])).unwrap()),
                    ]).unwrap();
                    deploy_repository(
                        &harness.client,
                        &harness.http,
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
                    harness.assert_bound("msitapi.fabric.microsoft.com");
                    let requests = harness.wire.requests.lock().unwrap();
                    let deletes: Vec<_> = requests
                        .iter()
                        .filter(|request| request.method() == Method::Delete)
                        .collect();
                    assert_eq!(deletes.len(), oracle["cases"].as_array().unwrap().len());
                    for (request, source) in deletes.iter().zip(oracle["cases"].as_array().unwrap())
                    {
                        assert_eq!(request.url().as_str(), source["url"].as_str().unwrap());
                    }
                }
            }
            if kind == "Environment" {
                harness.wire.reply(200, json!({"value":[]}), &[]);
            }
            harness.wire.reply(201, json!({"id":ITEM}), &[]);
            if kind == "Lakehouse" {
                harness.wire.reply(200, json!({"properties":{"sqlEndpointProperties":{"provisioningStatus":"Success"}}}), &[]);
            }
            if kind == "Environment" {
                harness.wire.reply(202, Value::Null, &[]);
                harness.wire.reply(200, json!({"value":[{"displayName":"Example","properties":{"publishDetails":{"state":"success"}}}]}), &[]);
            }
            let mut item = RepositoryItem {
                name: "Example".into(),
                item_type: kind.into(),
                description: "source fixture".into(),
                logical_id: "33333333-3333-3333-3333-333333333333".into(),
                path: format!("Example.{kind}"),
                folder_path: String::new(),
                files: vec![
                    ItemFile::new(
                        ".platform".into(),
                        br#"{"metadata":{"displayName":"Example"}}"#.to_vec(),
                    ),
                    ItemFile::new(
                        "content.json".into(),
                        br#"{"dataSources":[],"queryset":{}}"#.to_vec(),
                    ),
                ],
            };
            if kind == "DataPipeline" {
                item.files.push(ItemFile::new(
                    "pipeline-content.json".into(),
                    b"{}".to_vec(),
                ));
            }
            let config = config(kind, true);
            let outcome = deploy_repository(
                &harness.client,
                &harness.http,
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
            assert_eq!(outcome.published[0].outcome.item_id, ITEM, "{host} {kind}");
            harness.assert_bound(host);
        }
    }
}
