// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! TABLE interpretation over the released OneLake client and HTTP adapter.

#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::Method,
    time::OffsetDateTime,
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_onelake::OneLakeError;
use fabric_sdk_onelake_http::OneLakeHttpExecutor;
use fabric_sdk_tables::*;
use serde_json::json;
use std::{
    future::pending,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};
use support::{ScriptedHttp, TestTimer, response};

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
            .extend(scopes.iter().map(|s| s.to_string()));
        Ok(AccessToken {
            token: "offline-table-storage".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}
fn client(credential: Arc<Credential>) -> TableClient {
    TableClient::new(
        credential,
        &CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        &CredentialSelection::new("offline", "tenant", Audience::OneLake).unwrap(),
    )
    .unwrap()
}
struct Legacy;
impl TableExecutor for Legacy {
    fn read<'a>(
        &'a self,
        _: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        Box::pin(async { panic!("selected OneLake SDK must not use legacy raw read") })
    }
    fn load<'a>(
        &'a self,
        _: TableRequest<'a>,
        _: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>> {
        Box::pin(async { panic!("schema does not submit load") })
    }
}
fn storage(network: Arc<ScriptedHttp>) -> OneLakeHttpExecutor {
    OneLakeHttpExecutor::new(Arc::new(
        HttpExecutor::new(
            network,
            Arc::new(TestTimer::default()),
            ExecutionOptions::default(),
        )
        .unwrap(),
    ))
}
fn table() -> TablePath {
    TablePath::new("ws", "item", "Tables/sales").unwrap()
}

#[tokio::test]
async fn schema_sorts_all_generic_onelake_pages_before_reading_newest_metadata() {
    let network = ScriptedHttp::new([
        response(
            200,
            r#"{"paths":[{"name":"item/Tables/sales/_delta_log/00001.json"}]}"#,
            &[("x-ms-continuation", "next+/=")],
        ),
        response(
            200,
            r#"{"paths":[{"name":"item/Tables/sales/_delta_log/00002.json"}]}"#,
            &[],
        ),
        response(
            200,
            r#"{"metaData":{"schemaString":"{\"fields\":[{\"name\":\"new\",\"type\":\"long\"}]}"}}"#,
            &[],
        ),
    ]);
    let storage = storage(network.clone());
    let executor = TableWithOneLake {
        tables: &Legacy,
        storage: &storage,
    };
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone());
    assert_eq!(
        client
            .schema(&executor, &table(), TableLimits::default(), pending())
            .await
            .unwrap(),
        json!([{"name":"new","type":"long"}])
    );
    let requests = network.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1]
            .url()
            .query_pairs()
            .find(|(k, _)| k == "continuationToken")
            .unwrap()
            .1,
        "next+/="
    );
    assert_eq!(
        requests[2].url().path(),
        "/ws/item/Tables/sales/_delta_log/00002.json"
    );
    assert_eq!(
        *credential.0.lock().unwrap(),
        [Audience::OneLake.scope(); 3],
        "no preflight/double token acquisition"
    );
}

#[tokio::test]
async fn all_pages_count_against_table_request_budget_and_no_partial_schema_escapes() {
    let network = ScriptedHttp::new([
        response(
            200,
            r#"{"paths":[{"name":"item/Tables/sales/_delta_log/00001.json"}]}"#,
            &[("x-ms-continuation", "next")],
        ),
        response(
            200,
            r#"{"paths":[{"name":"item/Tables/sales/_delta_log/00002.json"}]}"#,
            &[],
        ),
    ]);
    let storage = storage(network.clone());
    let executor = TableWithOneLake {
        tables: &Legacy,
        storage: &storage,
    };
    let result = client(Arc::new(Credential::default()))
        .schema(
            &executor,
            &table(),
            TableLimits {
                max_requests: NonZeroUsize::new(2).unwrap(),
                ..TableLimits::default()
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(result.kind, TableErrorKind::Limit);
    assert_eq!(network.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn repeated_storage_continuation_or_http_error_remains_precise_not_empty_success() {
    for fail_http in [false, true] {
        let second = if fail_http {
            response(403, "", &[("x-ms-error-code", "AuthorizationFailure")])
        } else {
            response(200, r#"{"paths":[]}"#, &[("x-ms-continuation", "next")])
        };
        let network = ScriptedHttp::new([
            response(200, r#"{"paths":[]}"#, &[("x-ms-continuation", "next")]),
            second,
        ]);
        let storage = storage(network.clone());
        let executor = TableWithOneLake {
            tables: &Legacy,
            storage: &storage,
        };
        let error = client(Arc::new(Credential::default()))
            .schema(&executor, &table(), TableLimits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            TableErrorKind::OneLake(Box::new(if fail_http {
                OneLakeError::Http {
                    status: 403,
                    code: Some("AuthorizationFailure".into()),
                    raw_service_error: None,
                }
            } else {
                OneLakeError::RepeatedContinuation
            }))
        );
        assert!(!error.mutation_may_have_occurred);
        assert_eq!(network.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn onelake_properties_and_generic_jobs_keep_separate_explicit_scopes() {
    let network = ScriptedHttp::new([
        response(200, "", &[("x-ms-resource-type", "file")]),
        support::accepted(),
        response(200, r#"{"id":"run","status":"Completed"}"#, &[]),
    ]);
    let storage = storage(network.clone());
    let executor = TableWithOneLake {
        tables: &Legacy,
        storage: &storage,
    };
    let credential = Arc::new(Credential::default());
    let tables = client(credential.clone());
    assert_eq!(
        tables
            .source_kind(
                &executor,
                &TablePath::new("ws", "item", "Files/a.csv").unwrap(),
                TableLimits::default(),
                pending()
            )
            .await
            .unwrap(),
        SourceKind::File
    );
    let jobs = support::sdk(network.clone(), Arc::new(TestTimer::default()));
    let maintenance = JobMaintenance {
        jobs: &jobs,
        wait: Default::default(),
        cancel_on_timeout: true,
        operations: None,
    };
    assert_eq!(
        maintenance
            .run(&table(), json!({"tableName":"sales"}), Box::pin(pending()))
            .await
            .unwrap(),
        MaintenanceOutcome::Completed {
            instance_id: "run".into()
        }
    );
    assert_eq!(*credential.0.lock().unwrap(), [Audience::OneLake.scope()]);
    let requests = network.requests.lock().unwrap();
    assert_eq!(requests[0].method(), Method::Head);
    assert_eq!(
        requests[0].url().host_str(),
        Some("onelake.dfs.fabric.microsoft.com")
    );
    assert_eq!(
        requests[1].url().host_str(),
        Some("api.fabric.microsoft.com")
    );
}

#[tokio::test]
async fn independent_paged_schema_cases_match_original_python_client_accumulation() {
    let manifest: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/storage-paging.json"
    ))
    .unwrap();
    for case in manifest["cases"].as_array().unwrap() {
        let mut replies = Vec::new();
        for page in case["pages"].as_array().unwrap() {
            let headers = page["continuation"]
                .as_str()
                .map(|token| vec![("x-ms-continuation", token)])
                .unwrap_or_default();
            replies.push(response(
                200,
                &json!({"paths":page["paths"]}).to_string(),
                &headers,
            ));
        }
        for path in case["expected_reads"].as_array().unwrap() {
            let name = path.as_str().unwrap().trim_start_matches("ws/");
            let text = case["logs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|log| log["name"] == name)
                .unwrap()["text"]
                .as_str()
                .unwrap();
            replies.push(response(200, text, &[]));
        }
        let network = ScriptedHttp::new(replies);
        let storage = storage(network.clone());
        let executor = TableWithOneLake {
            tables: &Legacy,
            storage: &storage,
        };
        let result = client(Arc::new(Credential::default()))
            .schema(&executor, &table(), TableLimits::default(), pending())
            .await
            .unwrap();
        assert_eq!(result, case["expected"], "{}", case["id"]);
        assert_eq!(
            network.requests.lock().unwrap().len(),
            case["expected_pages"].as_u64().unwrap() as usize
                + case["expected_reads"].as_array().unwrap().len()
        );
    }
}
