// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use azure_core::http::Url;
use fabric_cli_query_commands::{
    InputReader, InputSource, KqlRoute, PreparedKql, QueryInput, ResolvedRoute, TextSource,
    prepare_input,
};
use fabric_sdk::runtime::{
    NativeDeadline, NativeEffect, NativeError, NativeFuture, NativeMethod, NativeRequest,
    NativeStreamBody, NativeStreamExecutor, NativeStreamLimits, NativeStreamResponse,
};
use fabric_sdk::v2::ItemIdentity;
use fabric_sdk_query::{
    EngineBinding, EngineExecutor, QueryCapability, QueryErrorKind, QueryLimits,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{pending, ready},
    io,
    sync::Mutex,
    time::Duration,
};

fn binding(database: &str) -> EngineBinding {
    EngineBinding::kusto(
        "exact-advertised-binding",
        "1",
        ItemIdentity::new(
            "11111111-1111-1111-1111-111111111111",
            "22222222-2222-2222-2222-222222222222",
        )
        .unwrap(),
        Url::parse("https://offline.kusto.example.test/").unwrap(),
        database,
        "https://exact-audience.example.test/.default",
        "exact-account",
        "exact-tenant",
    )
    .unwrap()
}

struct NoInput;

impl InputReader for NoInput {
    fn read(&mut self, _: InputSource<'_>, _: usize) -> io::Result<Vec<u8>> {
        panic!("inline query must not read files or stdin")
    }
}

fn prepared(binding: &EngineBinding) -> PreparedKql {
    let route = ResolvedRoute::Kql(KqlRoute {
        engine: "advertised-engine".into(),
        binding: binding.clone(),
    });
    let input = prepare_input(
        QueryInput {
            text: TextSource::Inline("opaque private query; not rewritten".into()),
            parameters: None,
        },
        &route,
        &mut NoInput,
    )
    .unwrap();
    PreparedKql::new(
        route,
        input,
        "query;fixture-1",
        Duration::from_secs(10),
        QueryLimits::new(1024 * 1024, 1024 * 1024, 100, 8, 32, 32).unwrap(),
    )
    .unwrap()
}

struct Body(Option<Vec<u8>>);

impl NativeStreamBody for Body {
    fn read<'a>(
        &'a mut self,
        _: usize,
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<Option<Vec<u8>>, NativeError>> {
        Box::pin(async move {
            deadline.remaining()?;
            Ok(self.0.take())
        })
    }
}

struct Offline {
    binding: EngineBinding,
    requests: Mutex<Vec<NativeRequest>>,
    response: Mutex<Option<Vec<u8>>>,
    capability: QueryCapability,
}

impl Offline {
    fn new(binding: EngineBinding, errors: bool) -> Self {
        let response = json!([
            {"FrameType":"DataSetHeader","Version":"v2.0","IsProgressive":false},
            {"FrameType":"DataTable","TableId":7,"TableKind":"PrimaryResult","TableName":"first",
             "Columns":[{"ColumnName":"same","ColumnType":"long"},
                        {"ColumnName":"same","ColumnType":"string"}],
             "Rows":[[9007199254740993_u64,null]]},
            {"FrameType":"DataTable","TableId":9,"TableKind":"PrimaryResult","TableName":"empty",
             "Columns":[{"ColumnName":"declared","ColumnType":"datetime"}],"Rows":[]},
            {"FrameType":"DataSetCompletion","HasErrors":errors,"Cancelled":false,
             "OneApiErrors":if errors {json!([{"code":"fixture-partial"}])} else {json!([])}}
        ]);
        Self {
            binding,
            requests: Mutex::default(),
            response: Mutex::new(Some(serde_json::to_vec(&response).unwrap())),
            capability: QueryCapability::ScriptedOffline,
        }
    }
}

impl NativeStreamExecutor for Offline {
    fn open<'a>(
        &'a self,
        request: &'a NativeRequest,
        _: NativeStreamLimits,
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<NativeStreamResponse, NativeError>> {
        Box::pin(async move {
            deadline.remaining()?;
            self.requests.lock().unwrap().push(request.clone());
            Ok(NativeStreamResponse {
                status: 200,
                content_type: Some("application/json".into()),
                headers: BTreeMap::new(),
                body: Box::new(Body(Some(
                    self.response.lock().unwrap().take().expect("query replay"),
                ))),
            })
        })
    }
}

impl EngineExecutor for Offline {
    fn binding(&self) -> &EngineBinding {
        &self.binding
    }

    fn query_capability(&self) -> QueryCapability {
        self.capability
    }
}

fn deadline() -> NativeDeadline {
    NativeDeadline::new(Duration::from_secs(60)).unwrap()
}

#[tokio::test]
async fn actual_sdk_request_preserves_native_sets_schema_and_read_policy() {
    let binding = binding("exact-database");
    let executor = Offline::new(binding.clone(), false);
    let result = prepared(&binding)
        .execute(&executor, &deadline(), pending())
        .await
        .unwrap();
    assert_eq!(result.datasets.len(), 2);
    assert_eq!(result.datasets[0].table_id, 7);
    assert_eq!(result.datasets[0].columns[0].name, "same");
    assert_eq!(result.datasets[0].columns[1].name, "same");
    assert_eq!(result.datasets[0].rows[0][0], json!(9007199254740993_u64));
    assert_eq!(result.datasets[0].rows[0][1], Value::Null);
    assert_eq!(result.datasets[1].table_id, 9);
    assert_eq!(result.datasets[1].columns[0].native_type, "datetime");
    assert!(result.datasets[1].rows.is_empty());
    assert!(result.evidence.completed);
    assert_eq!(result.evidence.binding, binding);
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method(), NativeMethod::Post);
    assert_eq!(request.effect(), NativeEffect::Read);
    assert_eq!(request.url().path(), "/v2/rest/query");
    let body: Value = serde_json::from_slice(request.bytes().unwrap()).unwrap();
    assert_eq!(body["db"], "exact-database");
    assert_eq!(body["csl"], "opaque private query; not rewritten");
    let options = &body["properties"]["Options"];
    assert_eq!(options["request_readonly"], true);
    assert_eq!(options["request_readonly_hardline"], true);
    assert_eq!(options["request_callout_disabled"], true);
    assert_eq!(options["request_remote_entities_disabled"], true);
    assert_eq!(options["request_external_data_disabled"], true);
    assert_eq!(options["request_impersonation_disabled"], true);
    assert_eq!(options["request_sandboxed_execution_disabled"], true);
    assert_eq!(options["results_progressive_enabled"], false);
    assert_eq!(options["servertimeout"], "00:00:10.000");
}

#[tokio::test]
async fn partial_native_result_is_not_success_and_is_not_replayed() {
    let binding = binding("exact-database");
    let executor = Offline::new(binding.clone(), true);
    let error = prepared(&binding)
        .execute(&executor, &deadline(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::BackendFailure);
    assert_eq!(error.partial.as_ref().unwrap().datasets.len(), 2);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn mismatched_binding_and_unverified_adapter_reject_before_dispatch() {
    let binding = binding("exact-database");
    let wrong = Offline::new(self::binding("another-database"), false);
    let error = prepared(&binding)
        .execute(&wrong, &deadline(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::InvalidBinding);
    assert!(wrong.requests.lock().unwrap().is_empty());
    let mut unverified = Offline::new(binding.clone(), false);
    unverified.capability = QueryCapability::UnverifiedProduction;
    let error = prepared(&binding)
        .execute(&unverified, &deadline(), pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::UnsupportedCapability);
    assert!(unverified.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ready_local_cancellation_does_not_dispatch_or_claim_remote_stop() {
    let binding = binding("exact-database");
    let executor = Offline::new(binding.clone(), false);
    let error = prepared(&binding)
        .execute(&executor, &deadline(), ready(()))
        .await
        .unwrap_err();
    assert!(matches!(error.kind, QueryErrorKind::Native(_)));
    assert!(executor.requests.lock().unwrap().is_empty());
}
