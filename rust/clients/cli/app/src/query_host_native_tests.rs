// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

#[path = "../../../../engine/data-plane/aihub-query/examples/support/mod.rs"]
mod support;

use super::*;
use crate::{
    parser::{Invocation, parse_args},
    query_host::result::ConvertedQuery,
    tabular_export::FileSink,
};
use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, Method, Request, headers::Headers},
    time::OffsetDateTime,
};
use fabric_sdk::{Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin};
use fabric_sdk_http::ExecutionOptions;
use fabric_sdk_transport::TokioTimer;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::json;
use std::{
    collections::VecDeque,
    ffi::OsString,
    future::{pending, ready},
    io,
    path::Path,
    sync::Mutex,
};

const WS: &str = "11111111-1111-4111-8111-111111111111";
const ITEM: &str = "22222222-2222-4222-8222-222222222222";
const PARENT: &str = "33333333-3333-4333-8333-333333333333";
const SOURCE: &str = "44444444-4444-4444-8444-444444444444";
const EXECUTION: &str = "55555555-5555-4555-8555-555555555555";
const RESULT: &str = "66666666-6666-4666-8666-666666666666";
const KUSTO_SCOPE: &str = "https://api.kusto.windows.net/.default";

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<String>>,
    kusto_delay: Duration,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.scopes
            .lock()
            .unwrap()
            .extend(scopes.iter().map(|scope| (*scope).to_owned()));
        if scopes.contains(&KUSTO_SCOPE) && !self.kusto_delay.is_zero() {
            tokio::time::sleep(self.kusto_delay).await;
        }
        Ok(AccessToken::new(
            "synthetic-offline-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
    session: bool,
}
fn reply(value: Value) -> Reply {
    Reply {
        status: 200,
        body: serde_json::to_vec(&value).unwrap(),
        session: false,
    }
}
fn rpc(id: u8, result: Value) -> Reply {
    reply(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

struct Http {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Request>>,
}
impl std::fmt::Debug for Http {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OfflineNativeQueryHttp")
    }
}
impl Http {
    fn new(replies: impl IntoIterator<Item = Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::default(),
        })
    }
}

#[async_trait]
impl HttpClient for Http {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        let response = if request.url().host_str() == Some("login.microsoftonline.com") {
            assert_eq!(request.method(), Method::Post);
            assert_eq!(request.url().path(), format!("/{WS}/oauth2/v2.0/token"));
            let body = Bytes::from(request.body());
            let form = azure_core::http::Url::parse(&format!(
                "https://form.example.test/?{}",
                std::str::from_utf8(body.as_ref()).unwrap()
            ))
            .unwrap();
            let values = form
                .query_pairs()
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(values.get("client_id").unwrap().as_ref(), ITEM);
            let scope = values.get("scope").unwrap().as_ref();
            let token = if scope == KUSTO_SCOPE {
                "synthetic-kusto-token"
            } else {
                assert_eq!(scope, Audience::Fabric.scope());
                "synthetic-fabric-token"
            };
            reply(json!({"access_token":token,"expires_in":3600,"token_type":"Bearer"}))
        } else {
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected HTTP, query replay, fallback or remote cancellation")
        };
        let mut headers = Headers::new();
        headers.insert("content-type", "application/json");
        if response.session {
            headers.insert("mcp-session-id", "synthetic-session");
        }
        Ok(AsyncRawResponse::new(
            response.status.into(),
            headers,
            Box::pin(futures_util::stream::once(async move {
                Ok(Bytes::from(response.body))
            })),
        ))
    }
}

struct NoInput;
impl InputReader for NoInput {
    fn read(
        &mut self,
        _: fabric_cli_query_commands::InputSource<'_>,
        _: usize,
    ) -> io::Result<Vec<u8>> {
        panic!("inline native query must not read another source")
    }
}

fn host(
    http: Arc<Http>,
    credential: Arc<Credential>,
    settings: Option<Map<String, Value>>,
) -> NativeQueryHost {
    host_with_credential(
        http,
        credential,
        settings,
        "selected-account",
        "selected-tenant",
    )
}

fn host_with_credential(
    http: Arc<Http>,
    credential: Arc<dyn TokenCredential>,
    settings: Option<Map<String, Value>>,
    account: &str,
    tenant: &str,
) -> NativeQueryHost {
    let endpoint = EndpointBinding::public(ServiceOrigin::Fabric);
    let client = Client::with_context(RequestContext::with_endpoint(
        credential,
        CredentialSelection::new(account, tenant, Audience::Fabric).unwrap(),
        endpoint.clone(),
    ));
    let metadata = HttpExecutor::new(
        http.clone(),
        Arc::new(TokioTimer),
        ExecutionOptions::default(),
    )
    .unwrap()
    .with_endpoint(endpoint);
    NativeQueryHost::new(client, metadata, http, Arc::new(TokioTimer), settings)
}

#[cfg(feature = "headless")]
#[tokio::test]
async fn actual_headless_provider_queries_kusto_with_separate_exact_audience_and_same_identity() {
    use fabric_identity::headless::{HeadlessCredential, HeadlessInput, ServiceIdentity};
    let http = Http::new([owner("KQLDatabase"), kql_metadata(), kql_result()]);
    let credential = HeadlessCredential::new(
        HeadlessInput::ClientSecret {
            identity: ServiceIdentity::new(WS, ITEM).unwrap(),
            secret: azure_core::credentials::Secret::new("synthetic-offline-client-secret"),
        },
        http.clone(),
    )
    .unwrap();
    let host = host_with_credential(http.clone(), credential, None, ITEM, WS);
    let deadline = NativeDeadline::new(Duration::from_secs(120)).unwrap();
    let canceled = AtomicBool::new(false);
    let delivery = host
        .dispatch(
            &command("KQLDatabase", None),
            QueryControl {
                deadline: &deadline,
                end: Instant::now() + Duration::from_secs(120),
                canceled: &canceled,
                request_id: "query;actual-headless-provider",
            },
            &mut NoInput,
            &mut FileSink::default(),
            pending(),
        )
        .await
        .unwrap();
    assert!(delivery.error.is_none());
    assert_eq!(delivery.identity.requested_artifact.item, ITEM);
    let requests = http.requests.lock().unwrap();
    let queries = requests
        .iter()
        .filter(|request| request.url().host_str() == Some("synthetic.kusto.fabric.microsoft.com"))
        .collect::<Vec<_>>();
    assert_eq!(queries.len(), 1);
    assert_eq!(
        queries[0]
            .headers()
            .get_str(&"authorization".into())
            .unwrap(),
        "Bearer synthetic-kusto-token"
    );
    for metadata in requests
        .iter()
        .filter(|request| request.url().host_str() == Some("api.fabric.microsoft.com"))
    {
        assert_eq!(
            metadata.headers().get_str(&"authorization".into()).unwrap(),
            "Bearer synthetic-fabric-token"
        );
    }
}

fn command(kind: &str, export: Option<(&str, &Path)>) -> NativeCommand {
    let mut args = vec![
        OsString::from("fab-rs"),
        OsString::from("query"),
        OsString::from(format!("/{WS}.Workspace/{ITEM}.{kind}")),
        OsString::from("--text"),
        OsString::from("opaque actual root query"),
    ];
    if kind == "KQLDatabase" {
        args.extend([OsString::from("--engine"), OsString::from(KQL_ENGINE)]);
    }
    if let Some((format, path)) = export {
        args.extend([
            OsString::from("--format"),
            OsString::from(format),
            OsString::from("--output"),
            path.as_os_str().to_owned(),
        ]);
    }
    let Invocation::NativeTabular(command) = parse_args(args).unwrap() else {
        panic!("actual root query expected");
    };
    command
}

fn owner(kind: &str) -> Reply {
    reply(json!({"id":ITEM,"workspaceId":WS,"displayName":"exact-native-database","type":kind}))
}
fn kql_metadata() -> Reply {
    reply(
        json!({"id":ITEM,"workspaceId":WS,"displayName":"exact-native-database","type":"KQLDatabase",
        "properties":{"parentEventhouseItemId":PARENT,"databaseType":"ReadWrite",
            "queryServiceUri":"https://synthetic.kusto.fabric.microsoft.com"}}),
    )
}
fn kql_result() -> Reply {
    reply(json!([
        {"FrameType":"DataSetHeader","Version":"v2.0","IsProgressive":false},
        {"FrameType":"DataTable","TableId":7,"TableKind":"PrimaryResult","TableName":"actual-native",
         "Columns":[{"ColumnName":"wide","ColumnType":"long"},{"ColumnName":"text","ColumnType":"string"},
            {"ColumnName":"instant","ColumnType":"datetime"}],
         "Rows":[[9007199254740993_u64,"","1970-01-01T00:00:00.0000001Z"],[null,null,null]]},
        {"FrameType":"DataSetCompletion","HasErrors":false,"Cancelled":false,"OneApiErrors":[]}
    ]))
}

fn settings() -> Map<String, Value> {
    json!({"query_connection":{
        "protocol":"mcp","service_binding":"dataagent-typed-query",
        "endpoint":format!("https://api.fabric.microsoft.com{}",crate::query_host::connection::MCP_PROFILE_PATH),
        "audience_scope":Audience::Fabric.scope(),
        "data_agents":[{"workspace_id":WS,"agent_id":ITEM,"datasource_id":SOURCE,"stage":"published"}]
    }}).as_object().unwrap().clone()
}
fn aihub_replies() -> Vec<Reply> {
    let mut initialize = rpc(
        1,
        json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},
        "serverInfo":{"name":"offline-native-fixture","version":"1"}}),
    );
    initialize.session = true;
    let text = format!(
        r#"{{"schemaVersion":1,"resultId":"{RESULT}","columns":[
        {{"name":"wide","engineType":"uint64","logicalType":"integer","encoding":"json-number"}},
        {{"name":"text","engineType":"string","logicalType":"string","encoding":"text"}}],
        "rows":[[18446744073709551615,""],[null,null]],"rowCount":2,"truncated":false,
        "maxRows":1000,"totalRowCount":null}}"#
    );
    vec![
        owner("DataAgent"),
        reply(json!({"value":[{"id":SOURCE,"displayName":"actual-source",
        "type":"FabricItem","itemReference":{"workspaceId":WS,"itemId":EXECUTION}}]})),
        initialize,
        Reply {
            status: 202,
            body: Vec::new(),
            session: false,
        },
        rpc(2, json!({"tools":[support::tool()]})),
        rpc(
            3,
            json!({"content":[support::typed(&text,"opaque:actual-result")]}),
        ),
    ]
}

#[tokio::test]
async fn actual_native_kql_context_sdk_read_policy_scope_and_all_three_codecs() {
    for format in ["json", "csv", "parquet"] {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("explicit-codec.data");
        let http = Http::new([owner("KQLDatabase"), kql_metadata(), kql_result()]);
        let credential = Arc::new(Credential {
            kusto_delay: Duration::from_millis(10),
            ..Credential::default()
        });
        let host = host(http.clone(), credential.clone(), None);
        let deadline = NativeDeadline::new(Duration::from_secs(120)).unwrap();
        let canceled = AtomicBool::new(false);
        let mut sink = FileSink::default();
        let delivery = host
            .dispatch(
                &command("KQLDatabase", Some((format, &output))),
                QueryControl {
                    deadline: &deadline,
                    end: Instant::now() + Duration::from_secs(120),
                    canceled: &canceled,
                    request_id: "query;native-context-test",
                },
                &mut NoInput,
                &mut sink,
                pending(),
            )
            .await
            .unwrap();
        assert!(delivery.error.is_none());
        let Some(ConvertedQuery::Kql(converted)) = &delivery.converted else {
            panic!("real KQL bridge expected");
        };
        assert!(converted.evidence.completed);
        assert!(converted.evidence.http_eof);
        assert!(!converted.evidence.production_verified);
        assert_eq!(
            converted.evidence.adapter_capability,
            fabric_sdk_query::QueryCapability::ReadOnlyKustoV2
        );
        assert_eq!(
            credential.scopes.lock().unwrap().as_slice(),
            [
                Audience::Fabric.scope(),
                Audience::Fabric.scope(),
                KUSTO_SCOPE
            ]
        );
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[2].method(), Method::Post);
        assert_eq!(
            requests[2].url().as_str(),
            "https://synthetic.kusto.fabric.microsoft.com/v2/rest/query"
        );
        let body: Value = serde_json::from_slice(Bytes::from(requests[2].body()).as_ref()).unwrap();
        assert_eq!(body["db"], "exact-native-database");
        assert_eq!(body["csl"], "opaque actual root query");
        let options = &body["properties"]["Options"];
        for flag in [
            "request_readonly",
            "request_readonly_hardline",
            "request_callout_disabled",
            "request_remote_entities_disabled",
            "request_external_data_disabled",
            "request_impersonation_disabled",
            "request_sandboxed_execution_disabled",
        ] {
            assert_eq!(options[flag], true);
        }
        assert_eq!(options["servertimeout"], "00:00:30.000");
        assert_eq!(options["results_progressive_enabled"], false);
        let bytes = std::fs::read(&output).unwrap();
        if format == "json" {
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["rows"][0], json!(["9007199254740993", "", "100"]));
        } else if format == "csv" {
            assert_eq!(
                String::from_utf8(bytes).unwrap(),
                "\"wide\",\"text\",\"instant\"\r\n\"9007199254740993\",\"\",\"100\"\r\n,,\r\n"
            );
        } else {
            let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes)).unwrap();
            assert_eq!(
                builder.schema().as_ref(),
                converted.datasets[0].table.schema().as_ref()
            );
            assert_eq!(
                builder
                    .build()
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
                    .as_slice(),
                converted.datasets[0].table.batches()
            );
        }
    }
}

#[tokio::test]
async fn actual_native_aihub_context_resolves_datasource_then_sends_exact_variants() {
    let http = Http::new(aihub_replies());
    let credential = Arc::new(Credential::default());
    let host = host(http.clone(), credential.clone(), Some(settings()));
    let deadline = NativeDeadline::new(Duration::from_secs(120)).unwrap();
    let canceled = AtomicBool::new(false);
    let delivery = host
        .dispatch(
            &command("DataAgent", None),
            QueryControl {
                deadline: &deadline,
                end: Instant::now() + Duration::from_secs(120),
                canceled: &canceled,
                request_id: "query;native-aihub-test",
            },
            &mut NoInput,
            &mut FileSink::default(),
            pending(),
        )
        .await
        .unwrap();
    assert!(delivery.error.is_none());
    assert_eq!(delivery.identity.requested_artifact.item, ITEM);
    assert_eq!(delivery.identity.execution_artifact.item, EXECUTION);
    let value: Value = serde_json::from_slice(delivery.presentation.as_ref().unwrap()).unwrap();
    assert_eq!(
        value["resultsets"][0]["rows"][0],
        json!(["18446744073709551615", ""])
    );
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    for request in &requests[2..] {
        assert_eq!(
            request.url().path(),
            crate::query_host::connection::MCP_PROFILE_PATH
        );
        assert_eq!(request.method(), Method::Post);
        let body: Value = serde_json::from_slice(Bytes::from(request.body()).as_ref()).unwrap();
        assert_eq!(
            body["params"]["_meta"]["variants"],
            "Fabric.Routing.Documents.DataAgent,Fabric.Routing.Documents.DataAgent.TypedQueryResults"
        );
    }
    let query: Value = serde_json::from_slice(Bytes::from(requests[5].body()).as_ref()).unwrap();
    assert_eq!(
        query["params"]["arguments"]["artifactId"],
        format!("{ITEM}/{SOURCE}")
    );
    assert!(query["params"]["arguments"].get("language").is_none());
    assert_eq!(
        credential.scopes.lock().unwrap().as_slice(),
        [Audience::Fabric.scope(); 3]
    );
}

#[tokio::test]
async fn actual_native_untrusted_rest_connection_fails_before_any_token_or_http() {
    let http = Http::new([]);
    let credential = Arc::new(Credential::default());
    let mut settings = settings();
    settings["query_connection"]["endpoint"] =
        json!("https://api.fabric.microsoft.com/v1/workspaces");
    let host = host(http.clone(), credential.clone(), Some(settings));
    let deadline = NativeDeadline::new(Duration::from_secs(120)).unwrap();
    let canceled = AtomicBool::new(false);
    let result = host
        .dispatch(
            &command("DataAgent", None),
            QueryControl {
                deadline: &deadline,
                end: Instant::now() + Duration::from_secs(120),
                canceled: &canceled,
                request_id: "query;invalid-profile",
            },
            &mut NoInput,
            &mut FileSink::default(),
            pending(),
        )
        .await;
    assert!(matches!(
        result,
        Err(NativeQueryError::Connection(
            QueryConnectionError::UnsupportedProfile
        ))
    ));
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(http.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn native_server_budget_exhausted_during_real_credential_wait_does_not_send_query() {
    let http = Http::new([owner("KQLDatabase"), kql_metadata()]);
    let credential = Arc::new(Credential {
        kusto_delay: Duration::from_secs(3),
        ..Credential::default()
    });
    let host = host(http.clone(), credential.clone(), None);
    let deadline = NativeDeadline::new(Duration::from_secs(32)).unwrap();
    let canceled = AtomicBool::new(false);
    let delivery = host
        .dispatch(
            &command("KQLDatabase", None),
            QueryControl {
                deadline: &deadline,
                end: Instant::now() + Duration::from_secs(32),
                canceled: &canceled,
                request_id: "query;budget-exhaustion",
            },
            &mut NoInput,
            &mut FileSink::default(),
            pending(),
        )
        .await
        .unwrap();
    assert!(delivery.error.is_some());
    assert!(delivery.presentation.is_none());
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    assert_eq!(
        credential
            .scopes
            .lock()
            .unwrap()
            .iter()
            .filter(|scope| scope.as_str() == KUSTO_SCOPE)
            .count(),
        1
    );
}

#[tokio::test]
async fn native_local_cancel_before_metadata_does_not_acquire_any_credential() {
    let http = Http::new([]);
    let credential = Arc::new(Credential::default());
    let host = host(http.clone(), credential.clone(), None);
    let deadline = NativeDeadline::new(Duration::from_secs(120)).unwrap();
    let canceled = AtomicBool::new(false);
    let result = host
        .dispatch(
            &command("KQLDatabase", None),
            QueryControl {
                deadline: &deadline,
                end: Instant::now() + Duration::from_secs(120),
                canceled: &canceled,
                request_id: "query;local-cancel",
            },
            &mut NoInput,
            &mut FileSink::default(),
            ready(()),
        )
        .await;
    assert!(matches!(result, Err(NativeQueryError::Canceled)));
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(http.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn native_no_clobber_preflight_precedes_query_scope_and_submission() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("existing.data");
    std::fs::write(&output, b"other-owner").unwrap();
    let http = Http::new([owner("KQLDatabase"), kql_metadata()]);
    let credential = Arc::new(Credential::default());
    let host = host(http.clone(), credential.clone(), None);
    let deadline = NativeDeadline::new(Duration::from_secs(120)).unwrap();
    let canceled = AtomicBool::new(false);
    let result = host
        .dispatch(
            &command("KQLDatabase", Some(("parquet", &output))),
            QueryControl {
                deadline: &deadline,
                end: Instant::now() + Duration::from_secs(120),
                canceled: &canceled,
                request_id: "query;no-clobber",
            },
            &mut NoInput,
            &mut FileSink::default(),
            pending(),
        )
        .await;
    assert!(matches!(
        result,
        Err(NativeQueryError::Host(QueryHostError::OutputPreflight))
    ));
    assert_eq!(
        credential.scopes.lock().unwrap().as_slice(),
        [Audience::Fabric.scope(); 2]
    );
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    assert_eq!(std::fs::read(&output).unwrap(), b"other-owner");
}

#[tokio::test]
async fn native_aihub_permission_failure_retains_status_without_data_or_kql_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("denied.csv");
    let mut replies = aihub_replies();
    let failure = replies.last_mut().unwrap();
    failure.status = 403;
    failure.body = br#"{"error":{"message":"synthetic-private-provider-text"}}"#.to_vec();
    let http = Http::new(replies);
    let credential = Arc::new(Credential::default());
    let host = host(http.clone(), credential.clone(), Some(settings()));
    let deadline = NativeDeadline::new(Duration::from_secs(120)).unwrap();
    let canceled = AtomicBool::new(false);
    let delivery = host
        .dispatch(
            &command("DataAgent", Some(("csv", &output))),
            QueryControl {
                deadline: &deadline,
                end: Instant::now() + Duration::from_secs(120),
                canceled: &canceled,
                request_id: "query;permission-denied",
            },
            &mut NoInput,
            &mut FileSink::default(),
            pending(),
        )
        .await
        .unwrap();
    assert!(delivery.error.is_some());
    assert_eq!(
        delivery.receipt.receipt["execution"]["metadata"]["httpStatus"],
        403
    );
    assert!(
        !delivery
            .receipt
            .receipt
            .to_string()
            .contains("synthetic-private-provider-text")
    );
    assert!(!output.exists());
    assert_eq!(http.requests.lock().unwrap().len(), 6);
    assert_eq!(
        credential.scopes.lock().unwrap().as_slice(),
        [Audience::Fabric.scope(); 3]
    );
}

#[tokio::test]
async fn native_kql_partial_result_never_becomes_a_success_file() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("partial.parquet");
    let mut result: Value = serde_json::from_slice(&kql_result().body).unwrap();
    result[2]["HasErrors"] = json!(true);
    result[2]["OneApiErrors"] = json!([{"code":"fixture-partial"}]);
    let http = Http::new([owner("KQLDatabase"), kql_metadata(), reply(result)]);
    let credential = Arc::new(Credential::default());
    let host = host(http.clone(), credential, None);
    let deadline = NativeDeadline::new(Duration::from_secs(120)).unwrap();
    let canceled = AtomicBool::new(false);
    let delivery = host
        .dispatch(
            &command("KQLDatabase", Some(("parquet", &output))),
            QueryControl {
                deadline: &deadline,
                end: Instant::now() + Duration::from_secs(120),
                canceled: &canceled,
                request_id: "query;partial",
            },
            &mut NoInput,
            &mut FileSink::default(),
            pending(),
        )
        .await
        .unwrap();
    assert!(delivery.error.is_some());
    assert_eq!(delivery.receipt.receipt["execution"]["status"], "partial");
    assert!(delivery.converted.is_none());
    assert!(!output.exists());
    assert_eq!(http.requests.lock().unwrap().len(), 3);
}
