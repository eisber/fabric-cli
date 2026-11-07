// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline parent-owned Kusto protocol, bound and reconciliation contracts.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    error::ErrorKind,
    time::OffsetDateTime,
};
use fabric_sdk_tables::kql_database::*;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending, poll_fn, ready},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
    time::{Duration, Instant},
};

const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
const ITEM: &str = "22222222-2222-4222-8222-222222222222";
const EXTENT_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const EXTENT_B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const OPERATION: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const ZERO: &str = "00000000-0000-0000-0000-000000000000";
const ORIGIN: &str = "https://cluster.region.kusto.fabric.microsoft.com";
const SECRET: &str = "offline-secret-never-log";

#[derive(Debug, Default)]
struct Credential {
    calls: Mutex<Vec<String>>,
    fail: bool,
    pending: bool,
    trigger: Option<Arc<AtomicBool>>,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert!(options.is_none());
        self.calls
            .lock()
            .unwrap()
            .extend(scopes.iter().map(|s| s.to_string()));
        if let Some(trigger) = &self.trigger {
            trigger.store(true, Ordering::SeqCst);
        }
        if self.fail {
            return Err(azure_core::Error::with_message(ErrorKind::Other, SECRET));
        }
        if self.pending {
            poll_fn(|cx| {
                cx.waker().wake_by_ref();
                Poll::<()>::Pending
            })
            .await;
        }
        Ok(AccessToken {
            token: SECRET.into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}

struct Recorded {
    kind: KustoRequestKind,
    url: String,
    table: String,
    database: String,
    body: Value,
    body_bytes: usize,
    headers: Vec<(&'static str, &'static str)>,
}

#[derive(Default)]
struct Executor {
    responses: Mutex<VecDeque<Result<KustoResponse, KustoExecutionError>>>,
    calls: Mutex<Vec<Recorded>>,
    trigger: Option<Arc<AtomicBool>>,
    pending: bool,
}

impl Executor {
    fn new(responses: Vec<KustoResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(Ok).collect()),
            ..Self::default()
        }
    }
    fn failure(error: KustoExecutionError) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from([Err(error)])),
            ..Self::default()
        }
    }
}

impl KustoExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: KustoRequest<'a>,
    ) -> KustoFuture<'a, Result<KustoResponse, KustoExecutionError>> {
        Box::pin(async move {
            assert_eq!(request.method(), "POST");
            assert_eq!(request.token().token.secret(), SECRET);
            assert_eq!(request.binding().workspace_id(), WORKSPACE);
            assert_eq!(request.binding().item_id(), ITEM);
            assert!(request.deadline() > Instant::now());
            assert!(request.body().len() <= request.limits().max_request_bytes().get());
            assert!(request.limits().max_response_bytes().get() <= MAX_TRANSPORT_BYTES);
            let diagnostic = format!("{request:?}");
            assert!(!diagnostic.contains(SECRET));
            assert!(!diagnostic.contains(request.url().as_str()));
            self.calls.lock().unwrap().push(Recorded {
                kind: request.kind(),
                url: request.url().as_str().into(),
                table: request.table().name().into(),
                database: request.binding().database().into(),
                body: serde_json::from_slice(request.body()).unwrap(),
                body_bytes: request.body().len(),
                headers: request.headers(),
            });
            if let Some(trigger) = &self.trigger {
                trigger.store(true, Ordering::SeqCst);
            }
            if self.pending {
                poll_fn(|cx| {
                    cx.waker().wake_by_ref();
                    Poll::<()>::Pending
                })
                .await;
            }
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected dispatch")
        })
    }
}

fn binding(origin: &str) -> KustoBinding {
    KustoBinding::new(WORKSPACE, ITEM, "database'\\name", origin).unwrap()
}

fn client(credential: Arc<Credential>) -> KqlDatabaseClient {
    KqlDatabaseClient::new(credential, binding(ORIGIN))
}

fn table() -> KustoTable {
    KustoTable::new("table'\\] ; .drop database x").unwrap()
}

fn call_with(limits: KustoLimits) -> KustoCall<'static> {
    KustoCall::new(
        limits,
        Instant::now() + Duration::from_secs(60),
        pending(),
        pending(),
    )
    .unwrap()
}

fn call() -> KustoCall<'static> {
    call_with(KustoLimits::default())
}

fn signal(flag: Arc<AtomicBool>) -> impl Future<Output = ()> + Send {
    poll_fn(move |_| {
        if flag.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
}

fn limits(input: usize, request: usize, response: usize) -> KustoLimits {
    KustoLimits::new(input, request, response, 10_000, 64, 1024).unwrap()
}

fn response(value: Value) -> KustoResponse {
    KustoResponse {
        status: 200,
        body: serde_json::to_vec(&value).unwrap(),
    }
}

fn native_table(name: &str, columns: &[&str], rows: Value) -> Value {
    json!({
        "TableName": name,
        "Columns": columns.iter().map(|name| json!({
            "ColumnName": name, "ColumnType": "string"
        })).collect::<Vec<_>>(),
        "Rows": rows
    })
}

fn management() -> KustoResponse {
    response(json!({"Tables": [native_table("Table_0", &["Result"], json!([["value"]]))]}))
}

// Native v1 role mapping from the same pinned SDK's adminthenquery.json;
// ordinal order and all values here are synthetic.
fn v1_mapped_response(rows: Value, severity: i64) -> Value {
    let columns = |names: &[(&str, &str)]| {
        names
            .iter()
            .map(|(name, kind)| {
                json!({
                    "ColumnName": name, "DataType": kind
                })
            })
            .collect::<Vec<_>>()
    };
    json!({"Tables": [
        {
            "TableName": "Table_0",
            "Columns": columns(&[("Value", "Int64")]),
            "Rows": rows
        },
        {
            "TableName": "Table_1",
            "Columns": columns(&[("Value", "String")]),
            "Rows": [["{\"Visualization\":null}"]]
        },
        {
            "TableName": "Table_2",
            "Columns": columns(&[
                ("Timestamp", "DateTime"), ("Severity", "Int32"),
                ("SeverityName", "String"), ("StatusCode", "Int32"),
                ("StatusDescription", "String")
            ]),
            "Rows": [["2026-01-01T00:00:00Z", severity, "fixture", 0, "fixture"]]
        },
        {
            "TableName": "Table_3",
            "Columns": columns(&[
                ("Ordinal", "Int64"), ("Kind", "String"), ("Name", "String"),
                ("Id", "String"), ("PrettyName", "String")
            ]),
            "Rows": [
                [2, "QueryStatus", "QueryStatus", ZERO, ""],
                [0, "QueryResult", "PrimaryResult", EXTENT_A, ""],
                [1, "QueryProperties", "@ExtendedProperties", EXTENT_B, ""]
            ]
        }
    ]})
}

fn inline_extents(ids: &[&str]) -> KustoResponse {
    response(json!({
        "Tables": [native_table("Table_0", &["ExtentId"], json!(
            ids.iter().map(|id| vec![*id]).collect::<Vec<_>>()
        ))]
    }))
}

fn storage_extents(rows: Value) -> KustoResponse {
    response(json!({
        "Tables": [native_table(
            "Table_0",
            &["ExtentId", "ItemLoaded", "HasErrors", "OperationId"],
            rows
        )]
    }))
}

fn query_frames() -> Value {
    json!([
        {"FrameType": "DataSetHeader", "Version": "v2.0", "IsProgressive": false},
        {
            "FrameType": "DataTable", "TableId": 0, "TableKind": "PrimaryResult",
            "TableName": "PrimaryResult",
            "Columns": [{"ColumnName": "Value", "ColumnType": "long"}],
            "Rows": [[1]]
        },
        {"FrameType": "DataSetCompletion", "HasErrors": false, "Cancelled": false}
    ])
}

// Synthetic protocol shapes verified against Azure/azure-kusto-python commit
// 3f53425f7ff8d02de8fc0ea250d02dde30c7d258, azure-kusto-data/tests/input/
// query_partial_results_defer_is_true.json and query_partial_results_defer_is_false.json.
// Only the wire structure is reproduced; context and data are synthetic.
fn partial_query_prefix(rows: Value) -> Vec<Value> {
    vec![
        json!({"FrameType": "DataSetHeader", "Version": "v2.0", "IsProgressive": false}),
        json!({
            "FrameType": "DataTable", "TableId": 0, "TableKind": "QueryProperties",
            "TableName": "@ExtendedProperties",
            "Columns": [
                {"ColumnName": "TableId", "ColumnType": "int"},
                {"ColumnName": "Key", "ColumnType": "string"},
                {"ColumnName": "Value", "ColumnType": "dynamic"}
            ],
            "Rows": [[1, "Visualization", "{}"]]
        }),
        json!({
            "FrameType": "DataTable", "TableId": 1, "TableKind": "PrimaryResult",
            "TableName": "PrimaryResult",
            "Columns": [{"ColumnName": "x", "ColumnType": "long"}],
            "Rows": rows
        }),
    ]
}

fn deferred_partial_query(rows: Value) -> Value {
    let columns = [
        ("Timestamp", "datetime"),
        ("ClientRequestId", "string"),
        ("ActivityId", "guid"),
        ("SubActivityId", "guid"),
        ("ParentActivityId", "guid"),
        ("Level", "int"),
        ("LevelName", "string"),
        ("StatusCode", "int"),
        ("StatusCodeName", "string"),
        ("EventType", "int"),
        ("EventTypeName", "string"),
        ("Payload", "string"),
    ]
    .into_iter()
    .map(|(name, kind)| {
        json!({
            "ColumnName": name, "ColumnType": kind
        })
    })
    .collect::<Vec<_>>();
    let mut frames = partial_query_prefix(rows);
    frames.push(json!({
        "FrameType": "DataTable", "TableId": 2, "TableKind": "QueryCompletionInformation",
        "TableName": "QueryCompletionInformation", "Columns": columns,
        "Rows": [
            [
                "2026-01-01T00:00:00Z", "fixture", OPERATION, EXTENT_A, EXTENT_B,
                2, "Error", -2133196797i64, "E_QUERY_RESULT_SET_TOO_LARGE",
                1, "QueryLimitsExceeded", "{\"Count\":1,\"Text\":\"Synthetic truncation\"}"
            ],
            [
                "2026-01-01T00:00:00Z", "fixture", OPERATION, EXTENT_A, EXTENT_B,
                6, "Stats", 0, "S_OK", 0, "QueryResourceConsumption", "{}"
            ]
        ]
    }));
    frames.push(json!({
        "FrameType": "DataSetCompletion", "HasErrors": false, "Cancelled": false
    }));
    json!(frames)
}

fn native_row_failure() -> Value {
    json!({
        "OneApiErrors": [{
            "error": {
                "code": "LimitsExceeded", "message": SECRET,
                "@type": "Kusto.Data.Exceptions.KustoServicePartialQueryFailureLimitsExceededException",
                "@message": "Synthetic query limit failure", "@permanent": false
            }
        }]
    })
}

fn nondeferred_partial_query(mut rows: Vec<Value>) -> Value {
    let failure = native_row_failure();
    rows.push(failure.clone());
    let mut frames = partial_query_prefix(json!(rows));
    frames.push(json!({
        "FrameType": "DataSetCompletion", "HasErrors": true, "Cancelled": false,
        "OneApiErrors": failure["OneApiErrors"]
    }));
    json!(frames)
}

fn storage_load() -> KustoStorageLoad {
    KustoStorageLoad::new(
        vec![
            KustoStorageSource::impersonated(
                "https://onelake.blob.fabric.microsoft.com/workspace/item/Files/input.csv",
            )
            .unwrap(),
        ],
        KustoStorageFormat::Csv,
    )
    .unwrap()
}

#[test]
fn strict_public_cluster_binding_rejects_unsafe_urls_and_parent_ids() {
    for origin in [ORIGIN, "https://help.kusto.windows.net/"] {
        let binding = binding(origin);
        assert_eq!(binding.workspace_id(), WORKSPACE);
        assert_eq!(binding.item_id(), ITEM);
        assert!(!format!("{binding:?}").contains("database"));
    }
    for origin in [
        "http://cluster.kusto.windows.net",
        "https://kusto.windows.net",
        "https://cluster.kusto.windows.net.evil.example",
        "https://cluster.fabric.microsoft.com",
        "https://cluster.kusto.fabric.microsoft.com.evil.example",
        "https://cluster.kusto.windows.net@evil.example",
        "https://user:password@cluster.kusto.windows.net",
        "https://cluster.kusto.windows.net:443",
        "https://cluster.kusto.windows.net:8443",
        "https://cluster.kusto.windows.net/path",
        "https://cluster.kusto.windows.net/..",
        "https://cluster.kusto.windows.net/?x=y",
        "https://cluster.kusto.windows.net/#fragment",
        "https://cluster%2ekusto.windows.net",
        "https://cluster.kusto.windows.net.",
        "https://ingest-cluster.kusto.windows.net",
        "https://cluster..region.kusto.windows.net",
        "https://-cluster.kusto.windows.net",
        "https://cluster-.kusto.windows.net",
        "https://cluster.kusto.windows.net\\evil",
        " https://cluster.kusto.windows.net",
        "https://cluster.kusto.windows.net\n",
    ] {
        assert!(
            KustoBinding::new(WORKSPACE, ITEM, "db", origin).is_err(),
            "{origin}"
        );
    }
    for (workspace, item, database) in [
        ("not-a-guid", ITEM, "db"),
        (WORKSPACE, "item/name", "db"),
        (ZERO, ITEM, "db"),
        (WORKSPACE, ZERO, "db"),
        (WORKSPACE, ITEM, ""),
        (WORKSPACE, ITEM, "db\ninjected"),
    ] {
        assert!(KustoBinding::new(workspace, item, database, ORIGIN).is_err());
    }
}

#[test]
fn names_and_source_literals_are_bounded_and_not_diagnostic_data() {
    for name in ["", " ", "name\0", "name\n", "name\u{2028}"] {
        assert!(KustoTable::new(name).is_err());
    }
    assert!(KustoTable::new("x".repeat(1025)).is_err());
    assert!(KustoTable::new("x".repeat(1024)).is_ok());
    let table = table();
    assert!(!format!("{table:?}").contains(table.name()));
    assert!(KustoStorageLoad::new(vec![], KustoStorageFormat::Csv).is_err());
    assert!(
        KustoStorageLoad::new(
            (0..65)
                .map(|_| KustoStorageSource::impersonated(
                    "https://storage.blob.core.windows.net/container/file.csv"
                )
                .unwrap())
                .collect(),
            KustoStorageFormat::Csv
        )
        .is_err()
    );
    let parquet = KustoStorageLoad::new(
        vec![
            KustoStorageSource::impersonated(
                "https://storage.blob.core.windows.net/container/file.parquet",
            )
            .unwrap(),
        ],
        KustoStorageFormat::Parquet,
    )
    .unwrap();
    assert!(parquet.with_ignore_first_record().is_err());
    assert!(
        storage_load()
            .with_mapping_reference("bad\nmapping")
            .is_err()
    );
}

#[test]
fn storage_authority_is_explicit_and_never_accepts_arbitrary_locators() {
    for url in [
        "http://storage.blob.core.windows.net/container/file",
        "https://evil.example/container/file",
        "https://storage.blob.core.windows.net.evil.example/container/file",
        "https://user:pass@storage.blob.core.windows.net/container/file",
        "https://storage.blob.core.windows.net:443/container/file",
        "https://storage.blob.core.windows.net/container",
        "https://storage.blob.core.windows.net/container/",
        "https://storage.blob.core.windows.net/container/file#fragment",
        "https://storage.blob.core.windows.net/container/file;impersonate",
        "https://storage.blob.core.windows.net/container/file%3bimpersonate",
        "https://storage.blob.core.windows.net/container/file%0a",
        "https://storage.blob.core.windows.net/container/file%xx",
        "https://storage.blob.core.windows.net/container/a/../file",
        "https://storage.blob.core.windows.net/container/a/%2e%2e/file",
        "https://storage.blob.core.windows.net/container/a\\file",
        "https://storage.blob.core.windows.net/container/'file",
    ] {
        assert!(KustoStorageSource::impersonated(url).is_err(), "{url}");
    }
    assert!(
        KustoStorageSource::impersonated(
            "https://storage.blob.core.windows.net/container/file?sig=secret"
        )
        .is_err()
    );
    for query in ["", "?sp=r", "?sig=", "?sig=a&sig=b"] {
        let url = format!("https://storage.blob.core.windows.net/container/file{query}");
        assert!(KustoStorageSource::sas(&url).is_err());
    }
    let source = KustoStorageSource::sas(&format!(
        "https://storage.blob.core.windows.net/container/file?sp=r&sig={SECRET}%2B%2F%3D"
    ))
    .unwrap();
    assert!(!format!("{source:?}").contains(SECRET));
}

#[tokio::test]
async fn management_routes_preserve_parent_context_scope_and_safe_identifier_quotes() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone());
    let executor = Executor::new(vec![management(), management()]);
    client.schema(&table(), &executor, call()).await.unwrap();
    client
        .statistics(&table(), &executor, call())
        .await
        .unwrap();
    assert_eq!(
        *credential.calls.lock().unwrap(),
        vec![KUSTO_SCOPE, KUSTO_SCOPE]
    );
    let calls = executor.calls.lock().unwrap();
    assert_eq!(calls[0].kind, KustoRequestKind::Schema);
    assert_eq!(calls[1].kind, KustoRequestKind::Statistics);
    for request in calls.iter() {
        assert_eq!(request.url, format!("{ORIGIN}/v1/rest/mgmt"));
        assert_eq!(request.database, "database'\\name");
        assert_eq!(request.body["db"], "database'\\name");
        assert_eq!(request.table, table().name());
        assert!(request.headers.contains(&("x-ms-readonly", "true")));
    }
    assert_eq!(
        calls[0].body["csl"],
        ".show table ['table\\'\\\\] ; .drop database x'] schema as json"
    );
    assert_eq!(
        calls[1].body["csl"],
        ".show table ['table\\'\\\\] ; .drop database x'] details"
    );
    assert!(!format!("{client:?}").contains("database'"));
}

#[tokio::test]
async fn both_public_cluster_domains_use_the_fixed_service_audience() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::new(vec![management(), management()]);
    for origin in [ORIGIN, "https://help.kusto.windows.net"] {
        KqlDatabaseClient::new(credential.clone(), binding(origin))
            .schema(&table(), &executor, call())
            .await
            .unwrap();
    }
    assert_eq!(
        *credential.calls.lock().unwrap(),
        vec!["https://api.kusto.windows.net/.default"; 2]
    );
    let calls = executor.calls.lock().unwrap();
    assert_eq!(calls[0].url, format!("{ORIGIN}/v1/rest/mgmt"));
    assert_eq!(calls[1].url, "https://help.kusto.windows.net/v1/rest/mgmt");
}

#[tokio::test]
async fn query_is_verbatim_read_only_not_auto_routed_or_single_table_isolated() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone());
    let executor = Executor::new(vec![response(query_frames()), response(query_frames())]);
    for query in [
        "OtherTable | join (database('OtherDb').OtherTable) on Key\n| take 1",
        "  .drop table OtherTable",
    ] {
        let result = client
            .query(&table(), query, &executor, call())
            .await
            .unwrap();
        assert_eq!(result.completion, KustoCompletion::Succeeded);
        assert_eq!(result.tables[0].rows, vec![vec![json!(1)]]);
        assert_eq!(result.tables[0].column_types, vec!["long"]);
        let calls = executor.calls.lock().unwrap();
        let request = calls.last().unwrap();
        assert_eq!(request.kind, KustoRequestKind::Query);
        assert_eq!(request.url, format!("{ORIGIN}/v2/rest/query"));
        assert_eq!(request.body["csl"], query);
        assert_eq!(
            request.body["properties"]["Options"]["results_progressive_enabled"],
            false
        );
        assert!(request.headers.contains(&("x-ms-readonly", "true")));
    }
    assert_eq!(
        *credential.calls.lock().unwrap(),
        vec![KUSTO_SCOPE, KUSTO_SCOPE]
    );
}

#[tokio::test]
async fn inline_append_is_csv_verbatim_and_preserves_every_extent() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::new(vec![inline_extents(&[EXTENT_A, EXTENT_B])]);
    let csv = "name,value\n\"quoted'\\name\",42\n\"multi\nline\",43";
    let outcome = client(credential)
        .load_inline_csv(&table(), csv.as_bytes(), &executor, call())
        .await
        .unwrap();
    assert_eq!(outcome.completion, KustoCompletion::Succeeded);
    assert_eq!(outcome.extents.len(), 2);
    assert_eq!(outcome.extents[0].extent_id, EXTENT_A);
    assert_eq!(outcome.extents[1].extent_id, EXTENT_B);
    assert!(outcome.extents.iter().all(|e| e.operation_id.is_none()));
    let calls = executor.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].body["csl"],
        format!(".ingest inline into table ['table\\'\\\\] ; .drop database x'] <|\n{csv}")
    );
    assert!(
        !calls[0]
            .headers
            .iter()
            .any(|(name, _)| *name == "x-ms-readonly")
    );
}

#[tokio::test]
async fn typed_storage_pull_uses_existing_sources_and_explicit_native_options_only() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::new(vec![storage_extents(json!([
        [EXTENT_A, "source-a", false, OPERATION],
        [EXTENT_B, "source-b", false, OPERATION]
    ]))]);
    let load = KustoStorageLoad::new(
        vec![
            KustoStorageSource::impersonated(
                "https://onelake.blob.fabric.microsoft.com/ws/item/Files/data.csv",
            )
            .unwrap(),
            KustoStorageSource::sas(&format!(
                "https://storage.blob.core.windows.net/container/data.csv?sig={SECRET}"
            ))
            .unwrap(),
        ],
        KustoStorageFormat::Csv,
    )
    .unwrap()
    .with_ignore_first_record()
    .unwrap()
    .with_mapping_reference("mapping'\\name")
    .unwrap();
    assert!(!format!("{load:?}").contains(SECRET));
    let outcome = client(credential)
        .load_storage(&table(), &load, &executor, call())
        .await
        .unwrap();
    assert_eq!(outcome.completion, KustoCompletion::Succeeded);
    assert_eq!(outcome.extents.len(), 2);
    assert_eq!(outcome.extents[1].operation_id.as_deref(), Some(OPERATION));
    let calls = executor.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].kind, KustoRequestKind::StorageIngestion);
    let command = calls[0].body["csl"].as_str().unwrap();
    assert!(command.starts_with(".ingest into table ['table\\'\\\\] ; .drop database x'] (h'"));
    assert!(command.contains(";impersonate', h'https://storage.blob.core.windows.net"));
    assert!(command.ends_with(
        "with (format='csv', ignoreFirstRecord=true, ingestionMappingReference='mapping\\'\\\\name')"
    ));
    assert!(!command.contains(".ingest async"));
    assert!(!command.contains("overwrite"));
    assert!(!format!("{outcome:?}").contains("source-a"));
    assert!(!format!("{outcome:?}").contains(OPERATION));
}

#[tokio::test]
async fn real_async_storage_is_explicitly_unsupported_without_authentication() {
    let credential = Arc::new(Credential::default());
    let error = client(credential.clone())
        .load_storage_async(&table(), &storage_load())
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::Unsupported);
    assert!(!error.mutation_may_have_occurred);
    assert!(credential.calls.lock().unwrap().is_empty());
}

#[test]
fn all_byte_and_shape_limits_and_deadlines_are_finite() {
    for parameters in [
        (0, 100, 100, 1, 1, 1),
        (100, 0, 100, 1, 1, 1),
        (100, 100, 0, 1, 1, 1),
        (100, 100, 100, 0, 1, 1),
        (100, 100, 100, 1, 0, 1),
        (100, 100, 100, 1, 1, 0),
        (MAX_TRANSPORT_BYTES + 1, 100, 100, 1, 1, 1),
        (100, MAX_TRANSPORT_BYTES + 1, 100, 1, 1, 1),
        (100, 100, MAX_TRANSPORT_BYTES + 1, 1, 1, 1),
    ] {
        let (a, b, c, d, e, f) = parameters;
        assert!(KustoLimits::new(a, b, c, d, e, f).is_err());
    }
    assert_eq!(MAX_TRANSPORT_BYTES, 4_194_304);
    assert_eq!(
        KustoCall::new(
            KustoLimits::default(),
            Instant::now() - Duration::from_secs(1),
            pending(),
            pending(),
        )
        .unwrap_err()
        .kind,
        KustoErrorKind::TimedOut
    );
    assert_eq!(
        KustoCall::new(
            KustoLimits::default(),
            Instant::now() + Duration::from_secs(3601),
            pending(),
            pending(),
        )
        .unwrap_err()
        .kind,
        KustoErrorKind::InvalidInput
    );
}

#[tokio::test]
async fn raw_and_serialized_caps_are_distinct_and_fail_before_credential_access() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone());
    let executor = Executor::default();
    for csv in [
        b"12345".as_slice(),
        b"\xff".as_slice(),
        b"".as_slice(),
        b"\0".as_slice(),
    ] {
        assert!(
            client
                .load_inline_csv(&table(), csv, &executor, call_with(limits(4, 1024, 1024)))
                .await
                .is_err()
        );
    }
    for query in ["12345", "", " \n ", "a\0", "ééé"] {
        assert!(
            client
                .query(&table(), query, &executor, call_with(limits(4, 1024, 1024)))
                .await
                .is_err()
        );
    }
    let error = client
        .load_inline_csv(
            &table(),
            "\"".repeat(512).as_bytes(),
            &executor,
            call_with(limits(1024, 512, 1024)),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::InputLimit);
    assert!(!error.mutation_may_have_occurred);
    assert!(credential.calls.lock().unwrap().is_empty());
    assert!(executor.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn serialized_request_boundary_counts_json_escaping_and_management_syntax() {
    let credential = Arc::new(Credential::default());
    let client = client(credential.clone());
    let executor = Executor::new(vec![
        inline_extents(&[EXTENT_A]),
        inline_extents(&[EXTENT_A]),
    ]);
    let csv = b"\"value\",42";
    client
        .load_inline_csv(&table(), csv, &executor, call())
        .await
        .unwrap();
    let bytes = executor.calls.lock().unwrap()[0].body_bytes;
    client
        .load_inline_csv(
            &table(),
            csv,
            &executor,
            call_with(limits(csv.len(), bytes, 4096)),
        )
        .await
        .unwrap();
    let error = client
        .load_inline_csv(
            &table(),
            csv,
            &executor,
            call_with(limits(csv.len(), bytes - 1, 4096)),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::InputLimit);
    assert_eq!(credential.calls.lock().unwrap().len(), 2);
    assert_eq!(executor.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn shared_four_mib_is_serialized_transport_not_a_csv_service_guarantee() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::default();
    let csv = "\"".repeat(3 * 1024 * 1024);
    let error = client(credential.clone())
        .load_inline_csv(
            &table(),
            csv.as_bytes(),
            &executor,
            call_with(limits(MAX_TRANSPORT_BYTES, MAX_TRANSPORT_BYTES, 1024)),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::InputLimit);
    assert!(credential.calls.lock().unwrap().is_empty());
    assert!(executor.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn decoded_response_exact_byte_boundary_is_enforced_even_for_nonconforming_executor() {
    let credential = Arc::new(Credential::default());
    let client = client(credential);
    let body_bytes = management().body.len();
    let executor = Executor::new(vec![management(), management()]);
    client
        .schema(
            &table(),
            &executor,
            call_with(limits(1024, 4096, body_bytes)),
        )
        .await
        .unwrap();
    let error = client
        .schema(
            &table(),
            &executor,
            call_with(limits(1024, 4096, body_bytes - 1)),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::ResponseLimit);
    assert!(!error.mutation_may_have_occurred);
}

#[tokio::test]
async fn management_and_query_200_native_errors_are_not_success() {
    for value in [
        json!({"error": {"message": SECRET}}),
        json!({"Tables": [], "Exceptions": [{"message": SECRET}]}),
        json!({"Tables": [], "HasErrors": true}),
    ] {
        let executor = Executor::new(vec![response(value)]);
        let error = client(Arc::new(Credential::default()))
            .schema(&table(), &executor, call())
            .await
            .unwrap_err();
        assert_eq!(error.kind, KustoErrorKind::Service);
        assert!(!format!("{error:?} {error}").contains(SECRET));
    }
    for (has_rows, native_cancel) in [(true, false), (false, false), (true, true)] {
        let mut frames = query_frames();
        if !has_rows {
            frames[1]["Rows"] = json!([]);
        }
        frames[2]["HasErrors"] = json!(!native_cancel);
        frames[2]["Cancelled"] = json!(native_cancel);
        let executor = Executor::new(vec![response(frames)]);
        let error = client(Arc::new(Credential::default()))
            .query(&table(), "T", &executor, call())
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            if has_rows {
                KustoErrorKind::Partial
            } else {
                KustoErrorKind::Service
            }
        );
        assert!(!error.mutation_may_have_occurred);
        assert_eq!(
            error.result.unwrap().tables[0].rows.len(),
            usize::from(has_rows)
        );
    }
}

#[tokio::test]
async fn query_checks_all_frames_not_only_first_primary_result() {
    for (rows, expected) in [
        (json!([[1], [2]]), KustoErrorKind::Partial),
        (json!([]), KustoErrorKind::Service),
    ] {
        let frame_rows = rows.as_array().unwrap().clone();
        let frames = deferred_partial_query(rows);
        assert_eq!(frames[3]["Columns"][5]["ColumnName"], "Level");
        assert_eq!(frames[3]["Rows"][0][5], 2);
        assert_eq!(frames[3]["Rows"][0][8], "E_QUERY_RESULT_SET_TOO_LARGE");
        assert_eq!(frames[4]["HasErrors"], false);
        assert_eq!(frames[4]["Cancelled"], false);
        let executor = Executor::new(vec![response(frames)]);
        let error = client(Arc::new(Credential::default()))
            .query(&table(), "T", &executor, call())
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected);
        assert!(!error.mutation_may_have_occurred);
        let result = error.result.unwrap();
        assert_eq!(
            result.completion,
            if frame_rows.is_empty() {
                KustoCompletion::Failed
            } else {
                KustoCompletion::Partial
            }
        );
        assert_eq!(result.tables.len(), 3);
        assert_eq!(
            result.tables[0].rows,
            vec![vec![json!(1), json!("Visualization"), json!("{}")]]
        );
        assert_eq!(
            result.tables[1].rows,
            frame_rows
                .into_iter()
                .map(|row| row.as_array().unwrap().clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(result.tables[2].columns[5], "Level");
        assert_eq!(result.tables[2].rows[0][7], -2133196797i64);
    }
}

#[tokio::test]
async fn nondeferred_native_error_objects_preserve_collected_rows_tables_and_error_evidence() {
    for native_flags in [true, false] {
        for preceding_rows in [vec![json!([1]), json!([2])], vec![]] {
            let mut frames = nondeferred_partial_query(preceding_rows.clone());
            if !native_flags {
                frames[3]["HasErrors"] = json!(false);
                frames[3].as_object_mut().unwrap().remove("OneApiErrors");
            }
            let executor = Executor::new(vec![response(frames)]);
            let error = client(Arc::new(Credential::default()))
                .query(&table(), "T", &executor, call())
                .await
                .unwrap_err();
            assert_eq!(
                error.kind,
                if preceding_rows.is_empty() {
                    KustoErrorKind::Service
                } else {
                    KustoErrorKind::Partial
                }
            );
            assert!(!format!("{error:?} {error}").contains(SECRET));
            let result = error.result.unwrap();
            assert_ne!(result.completion, KustoCompletion::Succeeded);
            assert_eq!(result.tables.len(), 2);
            assert_eq!(result.tables[0].name, "@ExtendedProperties");
            assert_eq!(
                result.tables[1].rows,
                preceding_rows
                    .iter()
                    .map(|row| row.as_array().unwrap().clone())
                    .collect::<Vec<_>>()
            );
            assert_eq!(result.tables[1].errors, vec![native_row_failure()]);
            assert!(!format!("{result:?}").contains(SECRET));
        }
    }
}

#[tokio::test]
async fn v2_completion_level_and_v1_status_severity_have_separate_protocol_meanings() {
    let mut v2_wrong_shape = deferred_partial_query(json!([[1]]));
    v2_wrong_shape[3]["Columns"][5]["ColumnName"] = json!("Severity");
    let mut v2_success = deferred_partial_query(json!([[1]]));
    v2_success[3]["Rows"][0] = v2_success[3]["Rows"][1].clone();
    let executor = Executor::new(vec![response(v2_wrong_shape), response(v2_success)]);
    let client = client(Arc::new(Credential::default()));
    let error = client
        .query(&table(), "T", &executor, call())
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::UnknownOutcome);
    assert_eq!(error.result.unwrap().tables.len(), 3);
    let success = client
        .query(&table(), "T", &executor, call())
        .await
        .unwrap();
    assert_eq!(success.completion, KustoCompletion::Succeeded);
    let executor = Executor::new(vec![response(json!({
        "Tables": [
            native_table("Table_0", &["Value"], json!([[1]])),
            native_table("QueryStatus", &["Level"], json!([[2]]))
        ]
    }))]);
    let error = client
        .schema(&table(), &executor, call())
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::UnknownOutcome);
}

#[tokio::test]
async fn native_error_objects_consume_row_budget_and_malformed_objects_remain_invalid() {
    let executor = Executor::new(vec![response(nondeferred_partial_query(vec![json!([1])]))]);
    let bounded = KustoLimits::new(1024, 4096, 4096, 2, 64, 1024).unwrap();
    let error = client(Arc::new(Credential::default()))
        .query(&table(), "T", &executor, call_with(bounded))
        .await
        .unwrap_err();
    // One properties row, one data row and the error object total three rows.
    assert_eq!(error.kind, KustoErrorKind::ResponseLimit);
    for malformed in [
        json!({"unexpected": "object"}),
        json!({"OneApiErrors": []}),
        json!({"OneApiErrors": "not-an-error-array"}),
        json!({"OneApiErrors": [{"code": "LimitsExceeded", "message": SECRET}]}),
        json!({"OneApiErrors": [{"error": {"code": 123, "message": SECRET}}]}),
        json!({"OneApiErrors": [{"error": {"code": "LimitsExceeded", "message": 123}}]}),
    ] {
        let mut frames = query_frames();
        frames[1]["Rows"] = json!([[1], malformed]);
        let executor = Executor::new(vec![response(frames)]);
        let error = client(Arc::new(Credential::default()))
            .query(&table(), "T", &executor, call())
            .await
            .unwrap_err();
        assert_eq!(error.kind, KustoErrorKind::InvalidResponse);
    }
}

#[tokio::test]
async fn error_shaped_dynamic_cells_are_data_not_out_of_band_row_errors() {
    let mut frames = query_frames();
    frames[1]["Columns"][0]["ColumnType"] = json!("dynamic");
    frames[1]["Rows"] = json!([[native_row_failure()]]);
    let executor = Executor::new(vec![response(frames)]);
    let result = client(Arc::new(Credential::default()))
        .query(&table(), "T", &executor, call())
        .await
        .unwrap();
    assert_eq!(result.completion, KustoCompletion::Succeeded);
    assert_eq!(result.tables[0].rows, vec![vec![native_row_failure()]]);
    assert!(result.tables[0].errors.is_empty());
}

#[tokio::test]
async fn management_checks_native_query_status_and_late_error_envelopes() {
    let executor = Executor::new(vec![response(json!({
        "Tables": [
            native_table("Table_0", &["Value"], json!([[1]])),
            native_table("QueryStatus", &["Severity"], json!([[1]]))
        ]
    }))]);
    let error = client(Arc::new(Credential::default()))
        .schema(&table(), &executor, call())
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::Partial);
    assert_eq!(error.result.unwrap().tables.len(), 2);
}

#[tokio::test]
async fn v1_table_of_contents_resolves_diagnostics_by_ordinal_not_wire_name() {
    for (rows, expected) in [
        (json!([[1], [2]]), KustoErrorKind::Partial),
        (json!([]), KustoErrorKind::Service),
    ] {
        let executor = Executor::new(vec![response(v1_mapped_response(rows, 2))]);
        let error = client(Arc::new(Credential::default()))
            .schema(&table(), &executor, call())
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected);
        assert!(!error.mutation_may_have_occurred);
        let result = error.result.unwrap();
        assert_eq!(result.tables[0].name, "Table_0");
        assert_eq!(result.tables[0].kind.as_deref(), Some("PrimaryResult"));
        assert_eq!(result.tables[1].kind.as_deref(), Some("QueryProperties"));
        assert_eq!(result.tables[2].name, "Table_2");
        assert_eq!(
            result.tables[2].kind.as_deref(),
            Some("QueryCompletionInformation")
        );
        assert_eq!(result.tables[3].kind.as_deref(), Some("TableOfContents"));
    }
}

#[tokio::test]
async fn v1_success_and_multiple_primary_results_preserve_native_roles() {
    let mut multiple = v1_mapped_response(json!([[1]]), 4);
    multiple["Tables"][3]["Rows"][2][1] = json!("QueryResult");
    multiple["Tables"][3]["Rows"][2][2] = json!("PrimaryResult");
    let executor = Executor::new(vec![
        response(v1_mapped_response(json!([[1]]), 4)),
        response(multiple),
    ]);
    let client = client(Arc::new(Credential::default()));
    let result = client.schema(&table(), &executor, call()).await.unwrap();
    assert_eq!(result.completion, KustoCompletion::Succeeded);
    assert_eq!(result.tables[0].column_types, vec!["Int64"]);
    assert_eq!(result.tables[2].column_types[1], "Int32");
    let result = client.schema(&table(), &executor, call()).await.unwrap();
    assert_eq!(result.completion, KustoCompletion::Succeeded);
    assert_eq!(
        result
            .tables
            .iter()
            .filter(|table| { table.kind.as_deref() == Some("PrimaryResult") })
            .count(),
        2
    );
}

#[tokio::test]
async fn v1_invalid_or_incomplete_role_mapping_never_establishes_success() {
    let mut cases = Vec::new();
    for ordinal in [json!(-1), json!(4), json!("2"), json!(null)] {
        let mut body = v1_mapped_response(json!([[1]]), 2);
        body["Tables"][3]["Rows"][0][0] = ordinal;
        cases.push(body);
    }
    let mut body = v1_mapped_response(json!([[1]]), 2);
    body["Tables"][3]["Rows"][0][0] = json!(0);
    cases.push(body);
    let mut body = v1_mapped_response(json!([[1]]), 2);
    body["Tables"][3]["Rows"].as_array_mut().unwrap().pop();
    cases.push(body);
    for (column, value) in [
        (1, json!("FutureRole")),
        (2, json!("")),
        (3, json!("not-a-guid")),
        (4, json!(null)),
    ] {
        let mut body = v1_mapped_response(json!([[1]]), 2);
        body["Tables"][3]["Rows"][0][column] = value;
        cases.push(body);
    }
    let mut body = v1_mapped_response(json!([[1]]), 2);
    body["Tables"][3]["Columns"][0]["ColumnName"] = json!("NotOrdinal");
    cases.push(body);
    let mut body = v1_mapped_response(json!([[1]]), 2);
    body["Tables"][2]["TableKind"] = json!("PrimaryResult");
    cases.push(body);
    let mut body = v1_mapped_response(json!([[1]]), 2);
    body["Tables"].as_array_mut().unwrap().pop();
    cases.push(body);
    for body in cases {
        let executor = Executor::new(vec![response(body)]);
        let error = client(Arc::new(Credential::default()))
            .schema(&table(), &executor, call())
            .await
            .unwrap_err();
        assert_eq!(error.kind, KustoErrorKind::InvalidResponse);
    }
}

#[tokio::test]
async fn v1_mapped_diagnostics_require_severity_and_never_count_as_data() {
    let mut missing = v1_mapped_response(json!([[1]]), 2);
    missing["Tables"][2]["Columns"][1]["ColumnName"] = json!("Level");
    let executor = Executor::new(vec![response(missing)]);
    let error = client(Arc::new(Credential::default()))
        .statistics(&table(), &executor, call())
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::UnknownOutcome);
    assert_eq!(
        error.result.unwrap().tables[2].kind.as_deref(),
        Some("QueryCompletionInformation")
    );
}

#[tokio::test]
async fn v1_mapped_ingestion_failure_retains_only_primary_extent_receipts() {
    for severity in [2, 4] {
        let mut body = v1_mapped_response(json!([[EXTENT_A]]), severity);
        body["Tables"][0]["Columns"] = json!([{
            "ColumnName": "ExtentId", "DataType": "Guid"
        }]);
        body["Tables"][1]["Columns"] = json!([{
            "ColumnName": "ExtentId", "DataType": "Guid"
        }]);
        body["Tables"][1]["Rows"] = json!([[EXTENT_B]]);
        let executor = Executor::new(vec![response(body)]);
        let client = client(Arc::new(Credential::default()));
        let outcome = if severity == 2 {
            let error = client
                .load_inline_csv(&table(), b"a", &executor, call())
                .await
                .unwrap_err();
            assert_eq!(error.kind, KustoErrorKind::Partial);
            assert!(error.mutation_may_have_occurred);
            assert_eq!(error.result.unwrap().tables.len(), 4);
            *error.load.unwrap()
        } else {
            client
                .load_inline_csv(&table(), b"a", &executor, call())
                .await
                .unwrap()
        };
        assert_eq!(outcome.extents.len(), 1);
        assert_eq!(outcome.extents[0].extent_id, EXTENT_A);
    }
}

#[tokio::test]
async fn malformed_and_progressive_frames_never_establish_completion() {
    let mut cases = vec![
        json!([]),
        json!({}),
        json!([{"FrameType": "DataSetCompletion"}]),
    ];
    let mut frames = query_frames();
    frames.as_array_mut().unwrap().pop();
    cases.push(frames);
    let mut frames = query_frames();
    frames[0]["IsProgressive"] = json!(true);
    cases.push(frames);
    let mut frames = query_frames();
    frames[0]["Version"] = json!("v3.0");
    cases.push(frames);
    let mut frames = query_frames();
    frames[1]["FrameType"] = json!("TableFragment");
    cases.push(frames);
    let mut frames = query_frames();
    let duplicate = frames[1].clone();
    frames.as_array_mut().unwrap().insert(2, duplicate);
    cases.push(frames);
    let mut frames = query_frames();
    let duplicate = frames[2].clone();
    frames.as_array_mut().unwrap().insert(2, duplicate);
    cases.push(frames);
    let mut frames = query_frames();
    frames[1]["Rows"] = json!([[1, 2]]);
    cases.push(frames);
    let mut frames = query_frames();
    frames[1]["Columns"] = json!([
        {"ColumnName": "Value", "ColumnType": "long"},
        {"ColumnName": "Value", "ColumnType": "long"}
    ]);
    frames[1]["Rows"] = json!([[1, 2]]);
    cases.push(frames);
    for frames in cases {
        let executor = Executor::new(vec![response(frames)]);
        let error = client(Arc::new(Credential::default()))
            .query(&table(), "T", &executor, call())
            .await
            .unwrap_err();
        assert_eq!(error.kind, KustoErrorKind::InvalidResponse);
    }
}

#[tokio::test]
async fn missing_or_inconsistent_completion_evidence_is_never_success() {
    let mut frames = query_frames();
    frames[2].as_object_mut().unwrap().remove("Cancelled");
    let mut missing_has_errors = query_frames();
    missing_has_errors[2]
        .as_object_mut()
        .unwrap()
        .remove("HasErrors");
    let mut inconsistent = query_frames();
    inconsistent[2]["OneApiErrors"] = json!([{"message": SECRET}]);
    for (frames, expected) in [
        (frames, KustoErrorKind::UnknownOutcome),
        (missing_has_errors, KustoErrorKind::UnknownOutcome),
        (inconsistent, KustoErrorKind::Partial),
    ] {
        let executor = Executor::new(vec![response(frames)]);
        let error = client(Arc::new(Credential::default()))
            .query(&table(), "T", &executor, call())
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected);
    }
}

#[tokio::test]
async fn duplicate_json_properties_cannot_hide_error_or_completion_flags() {
    let original = String::from_utf8(response(query_frames()).body).unwrap();
    for replacement in [
        r#""HasErrors":true,"HasErrors":false"#,
        r#""HasErrors":true,"\u0048asErrors":false"#,
    ] {
        let body = original.replace(r#""HasErrors":false"#, replacement);
        assert_ne!(body, original);
        let executor = Executor::new(vec![KustoResponse {
            status: 200,
            body: body.into_bytes(),
        }]);
        let error = client(Arc::new(Credential::default()))
            .query(&table(), "T", &executor, call())
            .await
            .unwrap_err();
        assert_eq!(error.kind, KustoErrorKind::InvalidResponse);
    }
}

#[tokio::test]
async fn all_native_primary_result_tables_are_retained_in_order() {
    let mut frames = query_frames().as_array().unwrap().clone();
    let mut second = frames[1].clone();
    second["TableId"] = json!(1);
    second["Rows"] = json!([[2], [3]]);
    frames.insert(2, second);
    let executor = Executor::new(vec![response(json!(frames))]);
    let result = client(Arc::new(Credential::default()))
        .query(&table(), "T; OtherTable", &executor, call())
        .await
        .unwrap();
    assert_eq!(result.tables.len(), 2);
    assert_eq!(result.tables[1].rows, vec![vec![json!(2)], vec![json!(3)]]);
}

#[tokio::test]
async fn row_table_and_column_bounds_apply_to_the_entire_native_response() {
    for (response_body, max_rows, max_tables, max_columns) in [
        (
            json!({"Tables": [native_table("A", &["V"], json!([[1], [2]]))]}),
            1,
            2,
            2,
        ),
        (
            json!({"Tables": [
                native_table("A", &["V"], json!([[1]])),
                native_table("B", &["V"], json!([[2]]))
            ]}),
            1,
            2,
            2,
        ),
        (
            json!({"Tables": [
                native_table("A", &["V"], json!([])),
                native_table("B", &["V"], json!([]))
            ]}),
            2,
            1,
            2,
        ),
        (
            json!({"Tables": [native_table("A", &["A", "B"], json!([[1, 2]]))]}),
            2,
            2,
            1,
        ),
    ] {
        let executor = Executor::new(vec![response(response_body)]);
        let limits = KustoLimits::new(1024, 4096, 4096, max_rows, max_tables, max_columns).unwrap();
        let error = client(Arc::new(Credential::default()))
            .schema(&table(), &executor, call_with(limits))
            .await
            .unwrap_err();
        assert_eq!(error.kind, KustoErrorKind::ResponseLimit);
    }
}

#[tokio::test]
async fn storage_extent_error_and_zero_guid_receipts_are_truthful() {
    for (rows, expected, completion) in [
        (
            json!([
                [EXTENT_A, "a", false, OPERATION],
                [ZERO, "b", true, OPERATION]
            ]),
            KustoErrorKind::Partial,
            KustoCompletion::Partial,
        ),
        (
            json!([[ZERO, "a", true, OPERATION]]),
            KustoErrorKind::Service,
            KustoCompletion::Failed,
        ),
        (
            json!([[ZERO, "a", false, OPERATION]]),
            KustoErrorKind::UnknownOutcome,
            KustoCompletion::Unknown,
        ),
        (
            json!([[EXTENT_A, "a", true, OPERATION]]),
            KustoErrorKind::Partial,
            KustoCompletion::Partial,
        ),
    ] {
        let row_count = rows.as_array().unwrap().len();
        let executor = Executor::new(vec![storage_extents(rows)]);
        let error = client(Arc::new(Credential::default()))
            .load_storage(&table(), &storage_load(), &executor, call())
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected);
        assert!(error.mutation_may_have_occurred);
        let outcome = error.load.unwrap();
        assert_eq!(outcome.completion, completion);
        assert_eq!(outcome.extents.len(), row_count);
    }
}

#[tokio::test]
async fn empty_extent_set_missing_error_flag_and_invalid_receipts_do_not_succeed() {
    for (response, expected) in [
        (inline_extents(&[]), KustoErrorKind::UnknownOutcome),
        (inline_extents(&[ZERO]), KustoErrorKind::UnknownOutcome),
        (inline_extents(&[EXTENT_A]), KustoErrorKind::Partial),
        (
            inline_extents(&["invalid-extent"]),
            KustoErrorKind::InvalidResponse,
        ),
        (
            storage_extents(json!([[EXTENT_A, "source", false, ZERO]])),
            KustoErrorKind::InvalidResponse,
        ),
        (
            storage_extents(json!([[EXTENT_A, "source", "false", OPERATION]])),
            KustoErrorKind::InvalidResponse,
        ),
    ] {
        let executor = Executor::new(vec![response]);
        let error = client(Arc::new(Credential::default()))
            .load_storage(&table(), &storage_load(), &executor, call())
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected);
        assert!(error.mutation_may_have_occurred);
        assert!(error.result.is_some());
    }
}

#[tokio::test]
async fn load_native_completion_failure_preserves_extent_evidence() {
    let mut body: Value = serde_json::from_slice(&inline_extents(&[EXTENT_A]).body).unwrap();
    body["Tables"].as_array_mut().unwrap().push(native_table(
        "QueryStatus",
        &["Severity"],
        json!([[2]]),
    ));
    let executor = Executor::new(vec![response(body)]);
    let error = client(Arc::new(Credential::default()))
        .load_inline_csv(&table(), b"a", &executor, call())
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::Partial);
    assert!(error.mutation_may_have_occurred);
    assert_eq!(error.result.unwrap().tables.len(), 2);
    let outcome = error.load.unwrap();
    assert_eq!(outcome.completion, KustoCompletion::Partial);
    assert_eq!(outcome.extents[0].extent_id, EXTENT_A);
}

#[tokio::test]
async fn http_rejection_acceptance_redirect_and_malformed_200_are_not_completion() {
    for status in [202, 204, 301, 307, 400, 401, 403, 429, 500] {
        let executor = Executor::new(vec![KustoResponse {
            status,
            body: SECRET.as_bytes().into(),
        }]);
        let error = client(Arc::new(Credential::default()))
            .load_inline_csv(&table(), b"x", &executor, call())
            .await
            .unwrap_err();
        assert_eq!(error.kind, KustoErrorKind::Http { status });
        assert!(error.mutation_may_have_occurred);
        assert_eq!(executor.calls.lock().unwrap().len(), 1);
        assert!(!format!("{error:?} {error}").contains(SECRET));
    }
    let executor = Executor::new(vec![KustoResponse {
        status: 200,
        body: b"truncated".into(),
    }]);
    let error = client(Arc::new(Credential::default()))
        .load_inline_csv(&table(), b"x", &executor, call())
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::InvalidResponse);
    assert!(error.mutation_may_have_occurred);
}

#[tokio::test]
async fn dispatch_loss_is_never_replayed_and_only_proven_unsent_clears_mutation_evidence() {
    for (failure, expected, mutation) in [
        (
            KustoExecutionError::ResponseLost,
            KustoErrorKind::Execution,
            true,
        ),
        (
            KustoExecutionError::NotDispatched,
            KustoErrorKind::Execution,
            false,
        ),
        (
            KustoExecutionError::ResponseLimit,
            KustoErrorKind::ResponseLimit,
            true,
        ),
        (
            KustoExecutionError::TimedOut,
            KustoErrorKind::TimedOut,
            true,
        ),
    ] {
        let executor = Executor::failure(failure);
        let error = client(Arc::new(Credential::default()))
            .load_inline_csv(&table(), b"x", &executor, call())
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected);
        assert_eq!(error.mutation_may_have_occurred, mutation);
        assert_eq!(executor.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn provider_failure_is_redacted_and_pre_dispatch() {
    let credential = Arc::new(Credential {
        fail: true,
        ..Credential::default()
    });
    let executor = Executor::default();
    let error = client(credential.clone())
        .load_inline_csv(&table(), b"x", &executor, call())
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::Credential);
    assert!(!error.mutation_may_have_occurred);
    assert!(executor.calls.lock().unwrap().is_empty());
    assert!(!format!("{error:?} {error}").contains(SECRET));
}

#[tokio::test]
async fn ready_cancellation_wins_without_polling_credential_and_ready_deadline() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::default();
    let call = KustoCall::new(
        KustoLimits::default(),
        Instant::now() + Duration::from_secs(60),
        ready(()),
        ready(()),
    )
    .unwrap();
    let error = client(credential.clone())
        .load_inline_csv(&table(), b"x", &executor, call)
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::Cancelled);
    assert!(!error.mutation_may_have_occurred);
    assert!(credential.calls.lock().unwrap().is_empty());
    assert!(executor.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ready_deadline_bounds_authentication_without_dispatch() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::default();
    let call = KustoCall::new(
        KustoLimits::default(),
        Instant::now() + Duration::from_secs(60),
        pending(),
        ready(()),
    )
    .unwrap();
    let error = client(credential.clone())
        .schema(&table(), &executor, call)
        .await
        .unwrap_err();
    assert_eq!(error.kind, KustoErrorKind::TimedOut);
    assert!(!error.mutation_may_have_occurred);
    assert!(credential.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_or_deadline_during_and_immediately_after_auth_never_dispatches() {
    for (deadline, provider_pending) in [(false, false), (false, true), (true, false), (true, true)]
    {
        let trigger = Arc::new(AtomicBool::new(false));
        let credential = Arc::new(Credential {
            trigger: Some(trigger.clone()),
            pending: provider_pending,
            ..Credential::default()
        });
        let executor = Executor::default();
        let cancel: KustoFuture<'static, ()> = if deadline {
            Box::pin(pending())
        } else {
            Box::pin(signal(trigger.clone()))
        };
        let expired: KustoFuture<'static, ()> = if deadline {
            Box::pin(signal(trigger))
        } else {
            Box::pin(pending())
        };
        let call = KustoCall::new(
            KustoLimits::default(),
            Instant::now() + Duration::from_secs(60),
            cancel,
            expired,
        )
        .unwrap();
        let error = client(credential)
            .load_inline_csv(&table(), b"x", &executor, call)
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            if deadline {
                KustoErrorKind::TimedOut
            } else {
                KustoErrorKind::Cancelled
            }
        );
        assert!(!error.mutation_may_have_occurred);
        assert!(executor.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn cancellation_or_deadline_after_dispatch_never_claims_rollback_or_success() {
    for (deadline, transport_pending) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let trigger = Arc::new(AtomicBool::new(false));
        let executor = Executor {
            trigger: Some(trigger.clone()),
            pending: transport_pending,
            ..Executor::new(vec![inline_extents(&[EXTENT_A])])
        };
        let cancel: KustoFuture<'static, ()> = if deadline {
            Box::pin(pending())
        } else {
            Box::pin(signal(trigger.clone()))
        };
        let expired: KustoFuture<'static, ()> = if deadline {
            Box::pin(signal(trigger))
        } else {
            Box::pin(pending())
        };
        let call = KustoCall::new(
            KustoLimits::default(),
            Instant::now() + Duration::from_secs(60),
            cancel,
            expired,
        )
        .unwrap();
        let error = client(Arc::new(Credential::default()))
            .load_inline_csv(&table(), b"x", &executor, call)
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            if deadline {
                KustoErrorKind::TimedOut
            } else {
                KustoErrorKind::Cancelled
            }
        );
        assert!(error.mutation_may_have_occurred);
        assert_eq!(executor.calls.lock().unwrap().len(), 1);
    }
}
