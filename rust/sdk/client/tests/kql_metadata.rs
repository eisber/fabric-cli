// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline native metadata and explicit context handoff contracts.
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    kql_databases::*,
    v2::{
        ContractError, ItemIdentity, Registry, ResolvedTarget, Target, TargetKind, Verb,
        WorkspaceIdentity,
    },
};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse,
};
use fabric_sdk_workspaces::mutations::{ResourceMethod, ResourceOutcome};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, ready},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

const WS: &str = "11111111-1111-4111-8111-111111111111";
const ITEM: &str = "22222222-2222-4222-8222-222222222222";
const PARENT: &str = "33333333-3333-4333-8333-333333333333";
const ORIGIN: &str = "https://synthetic.z5.kusto.fabric.microsoft.com";
const INGEST: &str = "https://ingest-synthetic.z5.kusto.fabric.microsoft.com";
const PRIVATE: &str = "offline-private-data";

#[derive(Debug, Default)]
struct Credential(Mutex<Vec<String>>);

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
            .extend(scopes.iter().map(|scope| (*scope).into()));
        Ok(AccessToken::new(PRIVATE, OffsetDateTime::UNIX_EPOCH))
    }
}

fn context(credential: Arc<Credential>) -> RequestContext {
    RequestContext::with_endpoint(
        credential,
        CredentialSelection::new("explicit-account", "explicit-tenant", Audience::Fabric).unwrap(),
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap(),
    )
}

fn target(owner: &str, representation: &str) -> ResolvedTarget {
    ResolvedTarget::new(
        "/workspace.Workspace/database.KQLDatabase",
        ItemIdentity::new(WS, ITEM).unwrap(),
        Target {
            kind: TargetKind::Item,
            owner_type: owner.into(),
            facet: "item".into(),
            representation: representation.into(),
            generation: None,
        },
        None,
    )
    .unwrap()
}

fn data() -> Value {
    json!({
        "id":ITEM,"workspaceId":WS,"type":"KQLDatabase","displayName":"Native.Name",
        "description":null,
        "properties":{
            "parentEventhouseItemId":PARENT,"databaseType":"ReadWrite",
            "queryServiceUri":ORIGIN,"ingestionServiceUri":INGEST,
            "future":{"null":null,"number":3}
        },
        "futureTopLevel":PRIVATE
    })
}

fn get(data: Value) -> Result<KqlDatabase, KqlError> {
    KqlDatabaseRequests::get(&target("KQLDatabase", "metadata"), &context(Arc::default()))?.decode(
        ResourceOutcome::Completed {
            status: 200,
            data: Some(data),
        },
    )
}

fn list_request(token: Option<&str>) -> KqlRequest<KqlDatabasePage> {
    KqlDatabaseRequests::list_page(
        &WorkspaceIdentity::new(WS).unwrap(),
        &context(Arc::default()),
        KqlListOptions {
            recursive: false,
            root_folder_id: None,
            continuation_token: token,
        },
    )
    .unwrap()
}

#[test]
fn exact_owner_context_route_and_alias_are_shared() {
    let credential = Arc::new(Credential::default());
    let ctx = context(credential.clone());
    let requested = target("KQLDatabase", "metadata");
    let request = KqlDatabaseRequests::get(&requested, &ctx).unwrap();
    assert_eq!(request.operation().method(), ResourceMethod::Get);
    assert_eq!(
        request.operation().path(),
        format!("/v1/workspaces/{WS}/kqlDatabases/{ITEM}")
    );
    assert!(request.operation().body().is_none());
    assert!(Arc::ptr_eq(
        request.context().credential(),
        ctx.credential()
    ));
    let mut registry = Registry::default();
    register(&mut registry).unwrap();
    let common = registry
        .resolve(Verb::Get, requested.selection(), None)
        .unwrap();
    let facade = registry
        .resolve(Verb::Get, requested.selection(), Some("kql-database"))
        .unwrap();
    assert_eq!(common.operation.id(), facade.operation.id());
    assert_eq!(registry.operations().count(), 2);
    assert!(credential.0.lock().unwrap().is_empty());
}

#[test]
fn only_metadata_operations_are_registered() {
    let mut registry = Registry::default();
    register(&mut registry).unwrap();
    for verb in [Verb::Create, Verb::Delete, Verb::Link, Verb::Update] {
        assert!(
            registry
                .resolve(
                    verb,
                    target("KQLDatabase", "metadata").selection(),
                    Some("kql-database")
                )
                .is_err()
        );
    }
    assert_eq!(
        registry
            .resolve(
                Verb::Get,
                target("Warehouse", "metadata").selection(),
                Some("kql-database")
            )
            .unwrap_err(),
        ContractError::WrongOwnerType
    );
    assert!(
        registry
            .resolve(
                Verb::Get,
                target("KQLDatabase", "definition").selection(),
                None
            )
            .is_err()
    );
}

#[test]
fn wrong_owner_media_and_identity_are_rejected_before_credentials() {
    let credential = Arc::new(Credential::default());
    let ctx = context(credential.clone());
    assert_eq!(
        KqlDatabaseRequests::get(&target("Warehouse", "metadata"), &ctx)
            .unwrap_err()
            .kind(),
        Some(KqlErrorKind::TargetMismatch)
    );
    assert_eq!(
        KqlDatabaseRequests::get(&target("KQLDatabase", "definition"), &ctx)
            .unwrap_err()
            .kind(),
        Some(KqlErrorKind::UnsupportedCapability)
    );
    for key in ["id", "workspaceId", "type"] {
        let mut bad = data();
        bad[key] = "wrong".into();
        assert_eq!(
            get(bad).unwrap_err().kind(),
            Some(KqlErrorKind::TargetMismatch)
        );
    }
    assert!(credential.0.lock().unwrap().is_empty());
}

#[test]
fn absent_null_unknown_and_native_types_remain_distinct() {
    let source = data();
    let metadata = get(source.clone()).unwrap();
    assert_eq!(metadata.data(), &source);
    assert_eq!(metadata.description(), &Observed::Null);
    assert!(!format!("{metadata:?}").contains(PRIVATE));
    let mut absent = data();
    absent.as_object_mut().unwrap().remove("description");
    absent.as_object_mut().unwrap().remove("properties");
    let absent = get(absent).unwrap();
    assert_eq!(absent.description(), &Observed::Missing);
    assert!(matches!(absent.properties(), Observed::Missing));
    assert_eq!(
        absent
            .kusto_context(
                DatabaseSelector::MetadataDisplayName,
                KustoResource::PublicService
            )
            .unwrap_err()
            .kind(),
        Some(KqlErrorKind::MissingBinding)
    );
    let mut unknown = data();
    unknown["properties"]["databaseType"] = "FutureKind".into();
    let unknown = get(unknown).unwrap();
    assert!(
        matches!(unknown.properties(),Observed::Value(p) if p.database_type==Observed::Value(DatabaseKind::Unknown("FutureKind".into())))
    );
    assert_eq!(
        unknown
            .kusto_context(
                DatabaseSelector::MetadataDisplayName,
                KustoResource::PublicService
            )
            .unwrap_err()
            .kind(),
        Some(KqlErrorKind::MissingBinding)
    );
}

#[test]
fn malformed_fields_and_envelopes_are_not_empty_success() {
    for malformed in [json!(null), json!([]), json!({}), json!({"value":[]})] {
        assert!(get(malformed).is_err());
    }
    for path in [
        "/properties",
        "/properties/databaseType",
        "/properties/queryServiceUri",
        "/properties/parentEventhouseItemId",
        "/displayName",
        "/description",
    ] {
        let mut malformed = data();
        *malformed.pointer_mut(path).unwrap() = json!(42);
        assert_eq!(
            get(malformed).unwrap_err().kind(),
            Some(KqlErrorKind::InvalidResponse)
        );
    }
}

#[test]
fn explicit_kusto_resource_and_native_selector_never_acquire_a_token() {
    let credential = Arc::new(Credential::default());
    let ctx = context(credential.clone());
    let metadata = KqlDatabaseRequests::get(&target("KQLDatabase", "metadata"), &ctx)
        .unwrap()
        .decode(ResourceOutcome::Completed {
            status: 200,
            data: Some(data()),
        })
        .unwrap();
    let native = metadata
        .kusto_context(
            DatabaseSelector::ConfirmedName("Confirmed.Native"),
            KustoResource::PublicService,
        )
        .unwrap();
    assert_eq!(native.database(), "Confirmed.Native");
    assert_eq!(native.identity().item_id(), ITEM);
    assert_eq!(native.parent_eventhouse().item_id(), PARENT);
    assert_eq!(native.scope(), "https://api.kusto.windows.net/.default");
    assert_eq!(
        native.query_endpoint().base().as_str(),
        format!("{ORIGIN}/")
    );
    assert!(
        matches!(native.ingestion_endpoint(),Observed::Value(endpoint) if endpoint.base().as_str()==format!("{INGEST}/"))
    );
    assert!(Arc::ptr_eq(
        native.metadata_context().credential(),
        ctx.credential()
    ));
    assert_eq!(native.metadata_context().selection(), ctx.selection());
    let cluster = metadata
        .kusto_context(
            DatabaseSelector::MetadataDisplayName,
            KustoResource::QueryOrigin,
        )
        .unwrap();
    assert_eq!(cluster.database(), "Native.Name");
    assert_eq!(cluster.scope(), format!("{ORIGIN}/.default"));
    assert_ne!(cluster.scope(), ctx.selection().audience().scope());
    assert!(!format!("{native:?}").contains(ORIGIN));
    assert!(credential.0.lock().unwrap().is_empty());
}

#[test]
fn database_shortcut_is_not_readwrite_or_an_ingestion_claim() {
    let mut source = data();
    source["properties"]["databaseType"] = "Shortcut".into();
    source["properties"]
        .as_object_mut()
        .unwrap()
        .remove("ingestionServiceUri");
    let native = get(source)
        .unwrap()
        .kusto_context(
            DatabaseSelector::MetadataDisplayName,
            KustoResource::PublicService,
        )
        .unwrap();
    assert_eq!(native.database_kind(), &DatabaseKind::Shortcut);
    assert!(matches!(native.ingestion_endpoint(), Observed::Missing));
}

#[test]
fn missing_properties_never_use_alternate_paths_or_parent_id() {
    for key in ["queryServiceUri", "parentEventhouseItemId", "databaseType"] {
        for null in [true, false] {
            let mut source = data();
            if null {
                source["properties"][key] = Value::Null;
            } else {
                source["properties"].as_object_mut().unwrap().remove(key);
            }
            source["properties"]["queryUri"] = ORIGIN.into();
            source["properties"]["databaseUrl"] = ORIGIN.into();
            let metadata = get(source).unwrap();
            assert_eq!(
                metadata
                    .kusto_context(
                        DatabaseSelector::MetadataDisplayName,
                        KustoResource::QueryOrigin
                    )
                    .unwrap_err()
                    .kind(),
                Some(KqlErrorKind::MissingBinding)
            );
        }
    }
}

#[test]
fn unsafe_query_uri_forms_cannot_become_network_destinations() {
    for uri in [
        "http://synthetic.kusto.fabric.microsoft.com",
        "https://evil.test",
        "https://synthetic.kusto.fabric.microsoft.com.evil.test",
        "https://synthetic.kusto.fabric.microsoft.com:443",
        "https://user@synthetic.kusto.fabric.microsoft.com",
        "https://synthetic.kusto.fabric.microsoft.com/query",
        "https://synthetic.kusto.fabric.microsoft.com/../",
        "https://synthetic.kusto.fabric.microsoft.com/?",
        "https://synthetic.kusto.fabric.microsoft.com/#",
        "https://synthetic.kusto.fabric.microsoft.com.",
        "https://synthetic%2ekusto.fabric.microsoft.com",
        "https://ingest-synthetic.kusto.fabric.microsoft.com",
        "https://-synthetic.kusto.fabric.microsoft.com",
        "https://kusto.fabric.microsoft.com",
        "https://Synthetic.kusto.fabric.microsoft.com",
    ] {
        let mut source = data();
        source["properties"]["queryServiceUri"] = uri.into();
        assert_eq!(
            get(source)
                .unwrap()
                .kusto_context(
                    DatabaseSelector::MetadataDisplayName,
                    KustoResource::PublicService
                )
                .unwrap_err()
                .kind(),
            Some(KqlErrorKind::InvalidInput),
            "{uri}"
        );
    }
}

#[test]
fn parent_ingestion_and_database_validation_does_not_guess_equivalence() {
    for parent in [ITEM, "bad", "00000000-0000-0000-0000-000000000000"] {
        let mut source = data();
        source["properties"]["parentEventhouseItemId"] = parent.into();
        assert!(
            get(source)
                .unwrap()
                .kusto_context(
                    DatabaseSelector::MetadataDisplayName,
                    KustoResource::PublicService
                )
                .is_err()
        );
    }
    let mut source = data();
    source["properties"]["ingestionServiceUri"] = ORIGIN.into();
    assert!(
        get(source)
            .unwrap()
            .kusto_context(
                DatabaseSelector::MetadataDisplayName,
                KustoResource::PublicService
            )
            .is_err()
    );
    for name in ["", " ", "bad\nname"] {
        assert!(
            get(data())
                .unwrap()
                .kusto_context(
                    DatabaseSelector::ConfirmedName(name),
                    KustoResource::PublicService
                )
                .is_err()
        );
    }
    assert!(
        get(data())
            .unwrap()
            .kusto_context(
                DatabaseSelector::ConfirmedName(&"a".repeat(1025)),
                KustoResource::PublicService
            )
            .is_err()
    );
}

#[test]
fn list_page_keeps_rows_order_unknowns_and_real_continuation() {
    let request = list_request(None);
    let token = "opaque+/= value";
    let uri = format!(
        "https://dailyapi.fabric.microsoft.com/v1/workspaces/{WS}/kqlDatabases?continuationToken=opaque%2B%2F%3D+value"
    );
    let page = request.decode(ResourceOutcome::Completed {status:200,data:Some(json!({"value":[data(),data()],"continuationToken":token,"continuationUri":uri,"unknown":true}))}).unwrap();
    assert_eq!(page.value.len(), 2);
    assert_eq!(page.coverage(), PageCoverage::Incomplete);
    assert_eq!(page.continuation_token, Observed::Value(token.into()));
    assert_eq!(page.data()["unknown"], true);
    assert!(!format!("{page:?}").contains(token));
    assert!(
        list_request(Some(token))
            .operation()
            .path()
            .ends_with("continuationToken=opaque%2B%2F%3D+value")
    );
    let terminal = list_request(None)
        .decode(ResourceOutcome::Completed {
            status: 200,
            data: Some(json!({"value":[]})),
        })
        .unwrap();
    assert_eq!(terminal.coverage(), PageCoverage::TerminalPage);
}

#[test]
fn list_never_accepts_changed_collection_origin_or_query() {
    for uri in [
        format!(
            "https://api.fabric.microsoft.com/v1/workspaces/{WS}/kqlDatabases?continuationToken=next"
        ),
        format!(
            "https://dailyapi.fabric.microsoft.com/v1/workspaces/{WS}/items?continuationToken=next"
        ),
        format!(
            "https://dailyapi.fabric.microsoft.com/v1/workspaces/{WS}/kqlDatabases?continuationToken=wrong"
        ),
        format!(
            "https://dailyapi.fabric.microsoft.com/v1/workspaces/{WS}/kqlDatabases?continuationToken=next&recursive=true"
        ),
        format!(
            "https://dailyapi.fabric.microsoft.com/v1/workspaces/{WS}/kqlDatabases?continuationToken=next&continuationToken=next"
        ),
        format!(
            "https://dailyapi.fabric.microsoft.com/v1/workspaces/{WS}/other/../kqlDatabases?continuationToken=next"
        ),
        format!(
            "https://dailyapi.fabric.microsoft.com/v1/workspaces/{WS}/kqlDatabases?continuationToken=next#fragment"
        ),
    ] {
        assert!(
            list_request(None)
                .decode(ResourceOutcome::Completed {
                    status: 200,
                    data: Some(
                        json!({"value":[],"continuationToken":"next","continuationUri":uri})
                    )
                })
                .is_err(),
            "{uri}"
        );
    }
    assert!(
        list_request(Some("next"))
            .decode(ResourceOutcome::Completed {
                status: 200,
                data: Some(json!({"value":[],"continuationToken":"next"}))
            })
            .is_err()
    );
    assert!(list_request(None).decode(ResourceOutcome::Completed {status:200,data:Some(json!({"value":[],"continuationUri":format!("https://dailyapi.fabric.microsoft.com/v1/workspaces/{WS}/kqlDatabases?continuationToken=next")}))}).is_err());
}

#[test]
fn list_requires_real_workspace_row_ids_and_native_input_types() {
    for token in ["", "bad\ntoken"] {
        assert!(
            KqlDatabaseRequests::list_page(
                &WorkspaceIdentity::new(WS).unwrap(),
                &context(Arc::default()),
                KqlListOptions {
                    recursive: false,
                    root_folder_id: None,
                    continuation_token: Some(token)
                }
            )
            .is_err()
        );
    }
    for body in [
        json!({"value":null}),
        json!({"value":{}}),
        json!({"value":[{}]}),
        json!({"value":[],"continuationToken":false}),
    ] {
        assert!(
            list_request(None)
                .decode(ResourceOutcome::Completed {
                    status: 200,
                    data: Some(body)
                })
                .is_err()
        );
    }
    let mut foreign = data();
    foreign["workspaceId"] = PARENT.into();
    assert!(
        list_request(None)
            .decode(ResourceOutcome::Completed {
                status: 200,
                data: Some(json!({"value":[foreign]}))
            })
            .is_err()
    );
}

#[test]
fn acceptance_and_wrong_statuses_are_not_complete_metadata() {
    let request =
        KqlDatabaseRequests::get(&target("KQLDatabase", "metadata"), &context(Arc::default()))
            .unwrap();
    let error = request
        .decode(ResourceOutcome::Accepted {
            operation_id: Some(PRIVATE.into()),
        })
        .unwrap_err();
    assert_eq!(error.kind(), Some(KqlErrorKind::UnexpectedAcceptance));
    assert!(
        matches!(&error,KqlError::Contract {operation_id:Some(id),status:Some(202),..} if id==PRIVATE)
    );
    assert!(!format!("{error:?}").contains(PRIVATE));
    for status in [201, 204, 403, 404, 429] {
        assert_eq!(
            request
                .decode(ResourceOutcome::Completed {
                    status,
                    data: Some(data())
                })
                .unwrap_err()
                .kind(),
            Some(KqlErrorKind::UnexpectedStatus)
        );
    }
}

#[derive(Clone, Default)]
struct Executor {
    responses: Arc<Mutex<VecDeque<OperationResponse>>>,
    calls: Arc<Mutex<Vec<(ResourceMethod, String, Vec<u8>)>>>,
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        assert_eq!(request.token().token.secret(), PRIVATE);
        self.calls.lock().unwrap().push((
            request.method(),
            request.url().to_string(),
            request.body().to_vec(),
        ));
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request");
        Box::pin(ready(Ok(response)))
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        panic!("metadata reads must not invent polling")
    }
}

fn response(status: u16, body: Value, media: &str) -> OperationResponse {
    OperationResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::from([("Content-Type".into(), media.into())]),
    }
}

#[tokio::test]
async fn shared_execution_uses_original_endpoint_credential_and_only_one_get() {
    let credential = Arc::new(Credential::default());
    let ctx = context(credential.clone());
    let executor = Executor::default();
    executor.responses.lock().unwrap().push_back(response(
        200,
        data(),
        "application/json; charset=utf-8",
    ));
    let result = KqlDatabaseRequests::get(&target("KQLDatabase", "metadata"), &ctx)
        .unwrap()
        .execute(executor.clone(), OperationLimits::default(), pending())
        .await
        .unwrap();
    assert_eq!(result.identity().item_id(), ITEM);
    assert_eq!(
        *credential.0.lock().unwrap(),
        vec!["https://api.fabric.microsoft.com/.default"]
    );
    let calls = executor.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        (
            ResourceMethod::Get,
            format!("https://dailyapi.fabric.microsoft.com/v1/workspaces/{WS}/kqlDatabases/{ITEM}"),
            Vec::new()
        )
    );
}

#[tokio::test]
async fn denied_failure_and_non_json_media_have_no_backend_fallback() {
    for status in [403, 404, 429] {
        let executor = Executor::default();
        executor.responses.lock().unwrap().push_back(response(
            status,
            json!({"errorCode":"Denied","message":PRIVATE}),
            "application/json",
        ));
        let error =
            KqlDatabaseRequests::get(&target("KQLDatabase", "metadata"), &context(Arc::default()))
                .unwrap()
                .execute(executor.clone(), OperationLimits::default(), pending())
                .await
                .unwrap_err();
        assert!(matches!(error, KqlError::Operation(_)));
        assert_eq!(executor.calls.lock().unwrap().len(), 1);
    }
    let executor = Executor::default();
    executor
        .responses
        .lock()
        .unwrap()
        .push_back(response(200, data(), "text/html"));
    let error =
        KqlDatabaseRequests::get(&target("KQLDatabase", "metadata"), &context(Arc::default()))
            .unwrap()
            .execute(executor.clone(), OperationLimits::default(), pending())
            .await
            .unwrap_err();
    assert_eq!(error.kind(), Some(KqlErrorKind::UnsupportedCapability));
    assert_eq!(executor.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancellation_before_send_never_acquires_credentials() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::default();
    let error = KqlDatabaseRequests::get(
        &target("KQLDatabase", "metadata"),
        &context(credential.clone()),
    )
    .unwrap()
    .execute(executor.clone(), OperationLimits::default(), ready(()))
    .await
    .unwrap_err();
    assert!(matches!(error, KqlError::Operation(_)));
    assert!(credential.0.lock().unwrap().is_empty());
    assert!(executor.calls.lock().unwrap().is_empty());
}
