// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-pinned SDK contracts. The execution mock is not a production adapter.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::{Audience, CredentialSelection};
use fabric_sdk_tables::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{pending, ready},
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

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
            token: "offline-fixture-only".into(),
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

#[derive(Default)]
struct Executor {
    responses: Mutex<VecDeque<TableResponse>>,
    requests: Mutex<Vec<(String, String, Vec<u8>)>>,
}
impl Executor {
    fn record(&self, request: &TableRequest<'_>) {
        assert!(!format!("{request:?}").contains("offline-fixture-only"));
        assert_eq!(request.token().token.secret(), "offline-fixture-only");
        self.requests.lock().unwrap().push((
            request.method().into(),
            request.url().as_str().into(),
            request.body().into(),
        ));
    }
}
impl TableExecutor for Executor {
    fn read<'a>(
        &'a self,
        request: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        Box::pin(async move {
            self.record(&request);
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request"))
        })
    }
    fn load<'a>(
        &'a self,
        request: TableRequest<'a>,
        _cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>> {
        Box::pin(async move {
            self.record(&request);
            Ok(LoadOutcome::Completed)
        })
    }
}

fn response(body: impl Into<Vec<u8>>) -> TableResponse {
    TableResponse {
        status: 200,
        body: body.into(),
        headers: BTreeMap::new(),
    }
}

#[tokio::test]
async fn independent_source_schema_inventory_and_exact_storage_requests() {
    let inventory: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/cases.json"
    ))
    .unwrap();
    let branches: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/tables/branches.json"
    ))
    .unwrap();
    for case in inventory["cases"]
        .as_array()
        .unwrap()
        .iter()
        .chain(branches["cases"].as_array().unwrap().iter())
        .filter(|case| case["operation"] == "schema")
    {
        let logs = case["logs"].as_array().unwrap();
        let executor = Executor::default();
        executor.responses.lock().unwrap().push_back(response(
            serde_json::to_vec(&json!({
                "paths": logs.iter().map(|log| json!({"name": log["name"]})).collect::<Vec<_>>()
            }))
            .unwrap(),
        ));
        for path in case["expected_reads"].as_array().unwrap() {
            let path = path.as_str().unwrap().strip_prefix("ws/").unwrap();
            let log = logs.iter().find(|log| log["name"] == path).unwrap();
            executor
                .responses
                .lock()
                .unwrap()
                .push_back(response(log["text"].as_str().unwrap().as_bytes()));
        }
        let credential = Arc::new(Credential::default());
        let result = client(credential.clone())
            .schema(
                &executor,
                &TablePath::new("ws", "item", "Tables/sales").unwrap(),
                TableLimits::default(),
                pending(),
            )
            .await;
        if case.get("expected").is_some() {
            assert_eq!(result.unwrap(), case["expected"], "{}", case["id"]);
        } else {
            let kind = if case["error_code"] == "InvalidDeltaTable" {
                TableErrorKind::InvalidDeltaTable
            } else {
                TableErrorKind::InvalidResponse
            };
            assert_eq!(result.unwrap_err().kind, kind, "{}", case["id"]);
        }
        let requests = executor.requests.lock().unwrap();
        assert_eq!(requests[0].0, "GET");
        let url = azure_core::http::Url::parse(&requests[0].1).unwrap();
        assert_eq!(url.path(), "/ws/");
        assert_eq!(
            url.query_pairs().collect::<BTreeMap<_, _>>(),
            BTreeMap::from([
                ("recursive".into(), "false".into()),
                ("resource".into(), "filesystem".into()),
                ("directory".into(), "item/Tables/sales/_delta_log".into()),
                ("getShortcutMetadata".into(), "true".into()),
            ])
        );
        assert_eq!(
            requests
                .iter()
                .skip(1)
                .map(|r| r
                    .1
                    .strip_prefix("https://onelake.dfs.fabric.microsoft.com/")
                    .unwrap()
                    .to_string())
                .collect::<Vec<_>>(),
            case["expected_reads"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        );
        assert!(
            credential
                .0
                .lock()
                .unwrap()
                .iter()
                .all(|scope| scope == Audience::OneLake.scope())
        );
    }
}

#[tokio::test]
async fn load_uses_storage_head_then_actual_lakehouse_endpoint_and_fabric_scope() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone());
    let executor = Executor::default();
    executor.responses.lock().unwrap().push_back(TableResponse {
        headers: BTreeMap::from([("X-MS-RESOURCE-TYPE".into(), "directory".into())]),
        ..response("")
    });
    let table = TablePath::new("ws", "item", "Tables/dbo/sales").unwrap();
    let source = TablePath::new("other-ws", "other-item", "Files/data folder").unwrap();
    assert_eq!(
        client
            .load(
                &executor,
                &table,
                &source,
                &LoadOptions::default(),
                TableLimits::default(),
                pending()
            )
            .await
            .unwrap(),
        LoadOutcome::Completed
    );
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests[0].0, "HEAD");
    assert_eq!(
        requests[0].1,
        "https://onelake.dfs.fabric.microsoft.com/other-ws/other-item/Files/data%20folder"
    );
    assert_eq!(requests[1].0, "POST");
    assert_eq!(
        requests[1].1,
        "https://api.fabric.microsoft.com/v1/workspaces/ws/lakehouses/item/tables/sales/load"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[1].2).unwrap(),
        json!({
            "pathType":"Folder", "relativePath":"Files/data folder", "mode":"Overwrite", "formatOptions":{"format":"Csv"}, "recursive":true
        })
    );
    assert_eq!(
        *credential.0.lock().unwrap(),
        [Audience::OneLake.scope(), Audience::Fabric.scope()]
    );
}

#[tokio::test]
async fn cancellation_bounds_and_service_failures_are_not_empty_schema_success() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone());
    let executor = Executor::default();
    let table = TablePath::new("ws", "item", "Tables/sales").unwrap();
    let error = client
        .schema(&executor, &table, TableLimits::default(), ready(()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, TableErrorKind::Cancelled);
    assert!(executor.requests.lock().unwrap().is_empty());
    assert!(credential.0.lock().unwrap().is_empty());
    executor
        .responses
        .lock()
        .unwrap()
        .push_back(response(br#"{"paths":[]}"#));
    let error = client
        .schema(
            &executor,
            &table,
            TableLimits {
                max_response_bytes: NonZeroUsize::new(1).unwrap(),
                ..TableLimits::default()
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, TableErrorKind::Limit);
    executor.responses.lock().unwrap().push_back(TableResponse {
        status: 403,
        body: br#"{"errorCode":"Forbidden","message":"Denied"}"#.to_vec(),
        headers: BTreeMap::new(),
    });
    let error = client
        .schema(&executor, &table, TableLimits::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        TableErrorKind::Http {
            status: 403,
            code: Some("Forbidden".into()),
            message: Some("Denied".into())
        }
    );
    assert!(!error.mutation_may_have_occurred);
}

#[test]
fn explicit_scope_binding_and_safe_address_construction() {
    let credential = Arc::new(Credential::default());
    assert!(
        TableClient::new(
            credential,
            &CredentialSelection::new("a", "tenant", Audience::Fabric).unwrap(),
            &CredentialSelection::new("b", "tenant", Audience::OneLake).unwrap()
        )
        .is_err()
    );
    for (ws, item, path) in [
        ("", "item", "Tables/a"),
        ("ws/x", "item", "Tables/a"),
        ("ws", "item", "../a"),
        ("ws", "item", "Tables/a\n"),
        ("ws", "item", "Tables//a"),
    ] {
        assert!(TablePath::new(ws, item, path).is_err());
    }
}

#[tokio::test]
async fn unknown_missing_and_ambiguous_source_types_cannot_submit_a_load() {
    for (headers, kind) in [
        (
            BTreeMap::from([("x-ms-resource-type".into(), "unknown".into())]),
            TableErrorKind::InvalidInput,
        ),
        (
            BTreeMap::from([("x-ms-resource-type".into(), "File".into())]),
            TableErrorKind::InvalidInput,
        ),
        (BTreeMap::new(), TableErrorKind::InvalidResponse),
        (
            BTreeMap::from([
                ("x-ms-resource-type".into(), "file".into()),
                ("X-MS-RESOURCE-TYPE".into(), "directory".into()),
            ]),
            TableErrorKind::InvalidResponse,
        ),
    ] {
        let executor = Executor::default();
        executor.responses.lock().unwrap().push_back(TableResponse {
            headers,
            ..response("")
        });
        let credential = Arc::new(Credential::default());
        let result = client(credential.clone())
            .load(
                &executor,
                &TablePath::new("ws", "item", "Tables/sales").unwrap(),
                &TablePath::new("ws", "item", "Files/input.csv").unwrap(),
                &LoadOptions::default(),
                TableLimits::default(),
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(result.kind, kind);
        assert!(!result.mutation_may_have_occurred);
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
        assert_eq!(*credential.0.lock().unwrap(), [Audience::OneLake.scope()]);
    }
}

#[test]
fn inner_schema_parsing_is_separate_from_metadata_extraction() {
    assert_eq!(schema_fields(r#"{"fields":[]}"#).unwrap(), json!([]));
    assert_eq!(schema_fields(r#"{"fields":null}"#).unwrap(), Value::Null);
    assert_eq!(
        schema_fields("invalid").unwrap_err().kind,
        TableErrorKind::InvalidResponse
    );
    assert_eq!(
        schema_fields("{}").unwrap_err().kind,
        TableErrorKind::InvalidResponse
    );
}
