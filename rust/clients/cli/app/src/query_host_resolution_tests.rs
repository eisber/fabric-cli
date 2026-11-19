// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use crate::query_host::test_support as support;

use super::*;
use fabric_cli_query_commands::ResolvedRoute;
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    items::{ItemExecutionError, ItemRequest, ItemResponse},
    workspaces::{WorkspaceExecutionError, WorkspaceRequest, WorkspaceResponse},
};
use fabric_sdk_operations::{OperationExecutionError, OperationRequest, OperationResponse};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{pending, ready},
    pin::Pin,
    sync::{Arc, Mutex},
};

const WS: &str = "11111111-1111-4111-8111-111111111111";
const ITEM: &str = "22222222-2222-4222-8222-222222222222";
const PARENT: &str = "33333333-3333-4333-8333-333333333333";
const SOURCE: &str = "44444444-4444-4444-8444-444444444444";
const EXECUTION: &str = "55555555-5555-4555-8555-555555555555";

#[derive(Clone, Default)]
struct Metadata {
    responses: Arc<Mutex<VecDeque<Value>>>,
    paths: Arc<Mutex<Vec<String>>>,
}

impl Metadata {
    fn new(responses: impl IntoIterator<Item = Value>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
            ..Self::default()
        }
    }

    fn response(&self, path: &str) -> Vec<u8> {
        self.paths.lock().unwrap().push(path.into());
        serde_json::to_vec(
            &self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected metadata read or replay"),
        )
        .unwrap()
    }
}

impl ItemExecutor for Metadata {
    fn execute<'a>(
        &'a self,
        request: ItemRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method(), "GET");
            Ok(ItemResponse {
                status: 200,
                body: self.response(request.url().path()),
                headers: BTreeMap::new(),
            })
        })
    }
}

impl WorkspaceExecutor for Metadata {
    fn execute<'a>(
        &'a self,
        _: WorkspaceRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>> + Send + 'a>>
    {
        Box::pin(async { panic!("GUID workspace must not enumerate or guess another owner") })
    }
}

impl OperationExecutor for Metadata {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.method(), ResourceMethod::Get);
            assert!(request.body().is_empty());
            Ok(OperationResponse {
                status: 200,
                body: self.response(request.url().path()),
                headers: BTreeMap::new(),
            })
        })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(async { panic!("query metadata must not poll acceptance") })
    }
}

fn client() -> (Client, Arc<support::Credential>) {
    let credential = Arc::new(support::Credential::default());
    let client = Client::with_context(RequestContext::with_endpoint(
        credential.clone(),
        CredentialSelection::new("selected-account", "selected-tenant", Audience::Fabric).unwrap(),
        EndpointBinding::public(ServiceOrigin::Fabric),
    ));
    (client, credential)
}

fn owner_data(kind: &str) -> Value {
    json!({"id":ITEM,"workspaceId":WS,"displayName":"actual-native-database","type":kind})
}

fn path(kind: &str) -> String {
    format!("/{WS}.Workspace/{ITEM}.{kind}")
}
fn bound() -> NonZeroUsize {
    NonZeroUsize::new(1024 * 1024).unwrap()
}

#[tokio::test]
async fn actual_metadata_sdk_resolves_exact_kusto_context_without_native_query_or_scope_guess() {
    let (client, credential) = client();
    let mut typed = owner_data("KQLDatabase");
    typed["properties"] = json!({"queryServiceUri":"https://synthetic.kusto.fabric.microsoft.com",
        "parentEventhouseItemId":PARENT,"databaseType":"ReadWrite"});
    let metadata = Metadata::new([owner_data("KQLDatabase"), typed]);
    let owner = resolve_owner(&path("KQLDatabase"), &client, &metadata, bound(), pending())
        .await
        .unwrap();
    let target = resolve_kql(
        owner,
        &client,
        metadata.clone(),
        "advertised-kusto",
        "1",
        bound(),
        pending(),
    )
    .await
    .unwrap();
    let ResolvedRoute::Kql(route) = target.target.resolve(Some("advertised-kusto")).unwrap() else {
        panic!("artifact native KQL expected");
    };
    assert_eq!(route.binding.owner().workspace_id(), WS);
    assert_eq!(route.binding.owner().item_id(), ITEM);
    assert_eq!(route.binding.database(), "actual-native-database");
    assert_eq!(
        route.binding.endpoint().base().as_str(),
        "https://synthetic.kusto.fabric.microsoft.com/"
    );
    assert_eq!(
        route.binding.audience_scope(),
        "https://api.kusto.windows.net/.default"
    );
    assert_eq!(route.binding.account(), "selected-account");
    assert_eq!(route.binding.tenant(), "selected-tenant");
    assert_eq!(
        metadata.paths.lock().unwrap().as_slice(),
        [
            format!("/v1/workspaces/{WS}/items/{ITEM}"),
            format!("/v1/workspaces/{WS}/kqlDatabases/{ITEM}")
        ]
    );
    assert_eq!(
        credential.scopes.lock().unwrap().as_slice(),
        [Audience::Fabric.scope(), Audience::Fabric.scope()]
    );
}

#[tokio::test]
async fn source_type_mismatch_and_ambiguous_names_do_not_try_provider_endpoints() {
    let (client, _) = client();
    let wrong = Metadata::new([owner_data("DataAgent")]);
    assert!(matches!(
        resolve_owner(&path("KQLDatabase"), &client, &wrong, bound(), pending()).await,
        Err(QueryResolutionError::OwnerMismatch)
    ));
    assert_eq!(wrong.paths.lock().unwrap().len(), 1);
    let ambiguous = Metadata::new([json!({"value":[
        {"id":ITEM,"displayName":"name","type":"KQLDatabase"},
        {"id":PARENT,"displayName":"name","type":"KQLDatabase"}
    ]})]);
    let result = resolve_owner(
        &format!("/{WS}.Workspace/name.KQLDatabase"),
        &client,
        &ambiguous,
        bound(),
        pending(),
    )
    .await;
    assert!(matches!(result, Err(QueryResolutionError::Resolution(_))));
    assert_eq!(
        ambiguous.paths.lock().unwrap().as_slice(),
        [format!("/v1/workspaces/{WS}/items")]
    );
}

#[test]
fn unsupported_owner_or_query_specific_path_does_not_invent_new_grammar() {
    for path in [
        "/name.Workspace/item.Report",
        "/name.Workspace/item.KQLDatabase/src/definition",
        "/name.Workspace/item.DataAgent/query/provider",
        "/name.Workspace",
        "https://api.fabric.microsoft.com/",
    ] {
        assert!(parse_owner(path).is_err());
    }
}

fn connection(stage: &str) -> QueryConnection {
    let settings=json!({"query_connection":{
        "protocol":"mcp","service_binding":"dataagent-typed-query",
        "endpoint":format!("https://api.fabric.microsoft.com{}",crate::query_host::connection::MCP_PROFILE_PATH),
        "audience_scope":"https://api.fabric.microsoft.com/.default",
        "data_agents":[{"workspace_id":WS,"agent_id":ITEM,"datasource_id":SOURCE,"stage":stage}]
    }}).as_object().unwrap().clone();
    QueryConnection::from_settings(
        Some(&settings),
        &EndpointBinding::public(ServiceOrigin::Fabric),
    )
    .unwrap()
    .unwrap()
}

fn datasource() -> Value {
    json!({"id":SOURCE,"displayName":"configured-source","type":"FabricItem",
        "itemReference":{"workspaceId":WS,"itemId":EXECUTION}})
}

#[tokio::test]
async fn actual_explicit_stage_datasource_metadata_establishes_bound_artifact_without_engine_alias()
{
    for (stage, suffix, expected) in [
        ("published", "datasources", Stage::Production),
        ("staging", "staging/datasources", Stage::Sandbox),
    ] {
        let (client, _) = client();
        let metadata = Metadata::new([owner_data("DataAgent"), json!({"value":[datasource()]})]);
        let owner = resolve_owner(&path("DataAgent"), &client, &metadata, bound(), pending())
            .await
            .unwrap();
        let bound = resolve_data_agent(
            owner,
            &connection(stage),
            &client,
            &metadata,
            bound(),
            pending(),
        )
        .await
        .unwrap();
        let ResolvedRoute::AiHub(route) = bound.target.resolve(None).unwrap() else {
            panic!("real DataAgent route expected");
        };
        assert_eq!(route.agent.item, ITEM);
        assert_eq!(route.datasource, SOURCE);
        assert_eq!(route.bound_artifact.item, EXECUTION);
        assert_eq!(route.stage, expected);
        assert_eq!(route.engine, None);
        assert_eq!(
            metadata.paths.lock().unwrap()[1],
            format!("/v1/workspaces/{WS}/dataAgents/{ITEM}/{suffix}")
        );
        assert!(bound.target.resolve(Some("aihub")).is_err());
    }
}

#[tokio::test]
async fn incomplete_duplicate_or_reference_free_datasource_metadata_never_fabricates_a_target() {
    let mut no_reference = datasource();
    no_reference
        .as_object_mut()
        .unwrap()
        .remove("itemReference");
    for page in [
        json!({"value":[datasource()],"continuationUri":"https://never-follow.example.test"}),
        json!({"value":[datasource(),datasource()]}),
        json!({"value":[no_reference]}),
    ] {
        let (client, _) = client();
        let metadata = Metadata::new([owner_data("DataAgent"), page]);
        let owner = resolve_owner(&path("DataAgent"), &client, &metadata, bound(), pending())
            .await
            .unwrap();
        assert!(
            resolve_data_agent(
                owner,
                &connection("published"),
                &client,
                &metadata,
                bound(),
                pending()
            )
            .await
            .is_err()
        );
        assert_eq!(metadata.paths.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn canceled_owner_discovery_does_not_acquire_credentials_or_metadata() {
    let (client, credential) = client();
    let metadata = Metadata::default();
    assert!(
        resolve_owner(&path("KQLDatabase"), &client, &metadata, bound(), ready(()))
            .await
            .is_err()
    );
    assert!(metadata.paths.lock().unwrap().is_empty());
    assert!(credential.scopes.lock().unwrap().is_empty());
}
