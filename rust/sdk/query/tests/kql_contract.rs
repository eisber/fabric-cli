// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Direct offline request, binding, streaming and outcome contracts.

#![deny(unnameable_test_items)]

use azure_core::http::Url;
use fabric_sdk::runtime::{
    NativeDeadline, NativeEffect, NativeError, NativeFuture, NativeMethod, NativeRequest,
    NativeStreamBody, NativeStreamExecutor, NativeStreamLimits, NativeStreamResponse,
};
use fabric_sdk_query::{
    EngineBinding, EngineExecutor, KqlQuery, QueryCapability, QueryErrorKind, QueryLimits,
    query_kql,
};
use fabric_v2_contracts::ItemIdentity;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{pending, ready},
    sync::{Arc, Mutex},
    time::Duration,
};

const ROOT: &str = "https://offline.kusto.fabric.microsoft.com/";
const SCOPE: &str = "https://api.kusto.windows.net/.default";
const PRIVATE: &str = "private-query-sentinel";

fn binding(database: &str, scope: &str) -> EngineBinding {
    EngineBinding::kusto(
        "query.kql",
        "1",
        ItemIdentity::new(
            "11111111-1111-1111-1111-111111111111",
            "22222222-2222-2222-2222-222222222222",
        )
        .unwrap(),
        Url::parse(ROOT).unwrap(),
        database,
        scope,
        "offline-account",
        "offline-tenant",
    )
    .unwrap()
}

fn limits(rows: usize) -> QueryLimits {
    QueryLimits::new(1024 * 1024, 1024 * 1024, rows, 16, 128, 64).unwrap()
}

fn deadline() -> NativeDeadline {
    NativeDeadline::new(Duration::from_secs(60)).unwrap()
}

fn header() -> Value {
    json!({"FrameType":"DataSetHeader","Version":"v2.0","IsProgressive":false})
}

fn completion(errors: bool, cancelled: bool) -> Value {
    json!({"FrameType":"DataSetCompletion","HasErrors":errors,"Cancelled":cancelled,
        "OneApiErrors": if errors {json!([{"code":"E_QUERY_RESULT_SET_TOO_LARGE"}])} else {json!([])}})
}

fn table(id: u64, rows: Value) -> Value {
    json!({"FrameType":"DataTable","TableId":id,"TableKind":"PrimaryResult","TableName":"Native",
        "Columns":[{"ColumnName":"same","ColumnType":"long"},{"ColumnName":"same","ColumnType":"dynamic"}],
        "Rows":rows})
}

fn frames() -> Value {
    json!([
        header(),
        table(
            9,
            json!([[9007199254740993_u64, {"nested":[null,true,"x"]}]])
        ),
        table(4, json!([])),
        completion(false, false)
    ])
}

#[derive(Default)]
struct Observation {
    requests: Vec<NativeRequest>,
    opens: Vec<NativeStreamLimits>,
    reads: usize,
    drops: usize,
}

struct Body {
    fragments: VecDeque<Result<Vec<u8>, NativeError>>,
    observation: Arc<Mutex<Observation>>,
    pending_read: bool,
}

impl NativeStreamBody for Body {
    fn read<'a>(
        &'a mut self,
        max_bytes: usize,
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<Option<Vec<u8>>, NativeError>> {
        Box::pin(async move {
            deadline.remaining()?;
            self.observation.lock().unwrap().reads += 1;
            if self.pending_read {
                return pending().await;
            }
            let next = self.fragments.pop_front().transpose()?;
            if let Some(bytes) = &next {
                assert!(max_bytes > 0);
                return Ok(Some(bytes.clone()));
            }
            Ok(None)
        })
    }
}

impl Drop for Body {
    fn drop(&mut self) {
        self.observation.lock().unwrap().drops += 1;
    }
}

struct Script {
    binding: EngineBinding,
    capability: QueryCapability,
    fragments: Mutex<Option<VecDeque<Result<Vec<u8>, NativeError>>>>,
    observation: Arc<Mutex<Observation>>,
    status: u16,
    media: Option<String>,
    headers: BTreeMap<String, String>,
    open_error: Option<NativeError>,
    pending_read: bool,
}

impl Script {
    fn new(binding: EngineBinding, value: &Value) -> Self {
        let bytes = serde_json::to_vec(value).unwrap();
        Self {
            binding,
            capability: QueryCapability::ScriptedOffline,
            fragments: Mutex::new(Some(
                bytes.chunks(31).map(|bytes| Ok(bytes.to_vec())).collect(),
            )),
            observation: Arc::default(),
            status: 200,
            media: Some("application/json; charset=utf-8".into()),
            headers: BTreeMap::from([("x-ms-activity-id".into(), "native-activity".into())]),
            open_error: None,
            pending_read: false,
        }
    }
}

impl NativeStreamExecutor for Script {
    fn open<'a>(
        &'a self,
        request: &'a NativeRequest,
        limits: NativeStreamLimits,
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<NativeStreamResponse, NativeError>> {
        Box::pin(async move {
            deadline.remaining()?;
            let mut observation = self.observation.lock().unwrap();
            observation.requests.push(request.clone());
            observation.opens.push(limits);
            drop(observation);
            if let Some(error) = self.open_error {
                return Err(error);
            }
            Ok(NativeStreamResponse {
                status: self.status,
                content_type: self.media.clone(),
                headers: self.headers.clone(),
                body: Box::new(Body {
                    fragments: self.fragments.lock().unwrap().take().expect("query replay"),
                    observation: Arc::clone(&self.observation),
                    pending_read: self.pending_read,
                }),
            })
        })
    }
}

impl EngineExecutor for Script {
    fn binding(&self) -> &EngineBinding {
        &self.binding
    }
    fn query_capability(&self) -> QueryCapability {
        self.capability
    }
}

#[tokio::test]
async fn exact_single_post_preserves_all_primary_tables_types_order_and_evidence() {
    let binding = binding("exact-db", SCOPE);
    let script = Script::new(binding.clone(), &frames());
    let query = KqlQuery::new(PRIVATE, "query;offline-1").unwrap();
    let result = query_kql(
        &binding,
        &script,
        &query,
        limits(100),
        &deadline(),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(result.datasets.len(), 2);
    assert_eq!(result.datasets[0].table_id, 9);
    assert_eq!(result.datasets[1].table_id, 4);
    assert_eq!(result.datasets[1].ordinal, 1);
    assert!(result.datasets[1].rows.is_empty());
    assert_eq!(result.datasets[1].columns.len(), 2);
    assert_eq!(
        result.datasets[0].columns[0].name,
        result.datasets[0].columns[1].name
    );
    assert_eq!(result.datasets[0].columns[0].native_type, "long");
    assert_eq!(
        result.datasets[0].rows[0][0].as_u64(),
        Some(9007199254740993)
    );
    assert_eq!(
        result.datasets[0].rows[0][1],
        json!({"nested":[null,true,"x"]})
    );
    assert_eq!(result.evidence.binding, binding);
    assert_eq!(
        result.evidence.activity_id.as_deref(),
        Some("native-activity")
    );
    assert!(result.evidence.completed);
    assert!(!result.evidence.production_verified);
    assert_eq!(result.evidence.language, "kql");
    assert_eq!(result.evidence.protocol_version, "v2.0");
    let observation = script.observation.lock().unwrap();
    assert_eq!(observation.requests.len(), 1);
    assert_eq!(observation.drops, 1);
    assert!(observation.reads > 2);
    let request = &observation.requests[0];
    assert_eq!(request.method(), NativeMethod::Post);
    assert_eq!(request.effect(), NativeEffect::Read);
    assert_eq!(request.url().as_str(), format!("{ROOT}v2/rest/query"));
    assert_eq!(request.accept(), "application/json");
    assert_eq!(request.content_type(), "application/json; charset=utf-8");
    assert_eq!(
        request.headers()["x-ms-client-request-id"],
        "query;offline-1"
    );
    let body: Value = serde_json::from_slice(request.bytes().unwrap()).unwrap();
    assert_eq!(body["db"], "exact-db");
    assert_eq!(body["csl"], PRIVATE);
    assert_eq!(
        body["properties"]["Options"]["request_readonly_hardline"],
        true
    );
    assert_eq!(body["properties"]["Options"]["query_language"], "kql");
    assert_eq!(
        body["properties"]["Options"]["request_callout_disabled"],
        true
    );
    assert_eq!(
        body["properties"]["Options"]["request_remote_entities_disabled"],
        true
    );
    assert!(body["properties"].get("Parameters").is_none());
    for debug in [
        format!("{query:?}"),
        format!("{request:?}"),
        format!("{result:?}"),
        format!("{binding:?}"),
    ] {
        assert!(!debug.contains(PRIVATE));
        assert!(!debug.contains("exact-db"));
        assert!(!debug.contains(SCOPE));
    }
}

#[tokio::test]
async fn text_never_selects_another_language_endpoint_or_identity() {
    let binding = binding("db", SCOPE);
    for text in [
        "SELECT * FROM T",
        ".drop table T",
        "EVALUATE T",
        "set request_readonly=false;\nT",
    ] {
        let script = Script::new(binding.clone(), &frames());
        query_kql(
            &binding,
            &script,
            &KqlQuery::new(text, "id").unwrap(),
            limits(100),
            &deadline(),
            pending(),
        )
        .await
        .unwrap();
        let state = script.observation.lock().unwrap();
        let request = &state.requests[0];
        assert_eq!(request.url().as_str(), format!("{ROOT}v2/rest/query"));
        let body: Value = serde_json::from_slice(request.bytes().unwrap()).unwrap();
        assert_eq!(body["csl"], text);
        assert_eq!(body["properties"]["Options"]["query_language"], "kql");
        assert_eq!(
            body["properties"]["Options"]["request_readonly_hardline"],
            true
        );
        assert_eq!(state.requests.len(), 1);
    }
}

#[tokio::test]
async fn exact_binding_mismatch_and_unverified_production_fail_before_open() {
    let expected = binding("db", SCOPE);
    for supplied in [
        binding("other-db", SCOPE),
        binding("db", "https://other.kusto.windows.net/.default"),
    ] {
        let script = Script::new(supplied, &frames());
        let error = query_kql(
            &expected,
            &script,
            &KqlQuery::new("T", "id").unwrap(),
            limits(100),
            &deadline(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, QueryErrorKind::InvalidBinding);
        assert!(script.observation.lock().unwrap().requests.is_empty());
    }
    let mut script = Script::new(expected.clone(), &frames());
    script.capability = QueryCapability::UnverifiedProduction;
    let error = query_kql(
        &expected,
        &script,
        &KqlQuery::new(".drop table T", "id").unwrap(),
        limits(100),
        &deadline(),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::UnsupportedCapability);
    assert!(script.observation.lock().unwrap().requests.is_empty());
}

#[test]
fn unsafe_binding_and_input_are_rejected_not_normalized() {
    let owner = binding("db", SCOPE).owner().clone();
    for uri in [
        "http://localhost/",
        "https://user:password@host/",
        "https://host/v2/",
        "https://host/?query",
        "https://host/#fragment",
        "https://host:8443/",
    ] {
        assert!(
            EngineBinding::kusto(
                "id",
                "1",
                owner.clone(),
                Url::parse(uri).unwrap(),
                "db",
                SCOPE,
                "account",
                "tenant"
            )
            .is_err()
        );
    }
    for (db, scope) in [
        ("", SCOPE),
        (" db", SCOPE),
        ("db", ""),
        ("db", "http://host/.default"),
        ("db", "https://host/.default?token=secret"),
        ("db", "https://user:secret@host/.default"),
    ] {
        assert!(
            EngineBinding::kusto(
                "id",
                "1",
                owner.clone(),
                Url::parse(ROOT).unwrap(),
                db,
                scope,
                "account",
                "tenant"
            )
            .is_err()
        );
    }
    for text in ["", " \r\n", "T\0"] {
        assert!(KqlQuery::new(text, "id").is_err());
    }
    assert!(KqlQuery::new("T", "id\r\nHeader:evil").is_err());
    for duration in [
        Duration::ZERO,
        Duration::from_secs(3601),
        Duration::from_nanos(1),
    ] {
        assert!(
            KqlQuery::new("T", "id")
                .unwrap()
                .with_server_timeout(duration)
                .is_err()
        );
    }
    assert!(QueryLimits::new(0, 1, 1, 1, 1, 2).is_err());
    assert!(QueryLimits::new(1, 0, 1, 1, 1, 2).is_err());
    assert!(QueryLimits::new(1, 1, 1, 0, 1, 2).is_err());
    assert!(QueryLimits::new(1, 1, 1, 1, 1, 4097).is_err());
}

#[tokio::test]
async fn exact_serialized_request_budget_and_server_timeout_are_enforced() {
    let binding = binding("db", SCOPE);
    let query = KqlQuery::new("T\n\"escaped\"", "id")
        .unwrap()
        .with_server_timeout(Duration::from_millis(1234))
        .unwrap();
    let size = query
        .request(&binding, limits(100))
        .unwrap()
        .bytes()
        .unwrap()
        .len();
    let exact = QueryLimits::new(size, 1024 * 1024, 100, 16, 128, 64).unwrap();
    assert_eq!(
        query
            .request(&binding, exact)
            .unwrap()
            .bytes()
            .unwrap()
            .len(),
        size
    );
    let small = QueryLimits::new(size - 1, 1024 * 1024, 100, 16, 128, 64).unwrap();
    let script = Script::new(binding.clone(), &frames());
    assert_eq!(
        query_kql(&binding, &script, &query, small, &deadline(), pending())
            .await
            .unwrap_err()
            .kind,
        QueryErrorKind::RequestLimit
    );
    assert!(script.observation.lock().unwrap().requests.is_empty());
    let body: Value =
        serde_json::from_slice(query.request(&binding, exact).unwrap().bytes().unwrap()).unwrap();
    assert_eq!(
        body["properties"]["Options"]["servertimeout"],
        "00:00:01.234"
    );
    let short = NativeDeadline::new(Duration::from_millis(500)).unwrap();
    assert_eq!(
        query_kql(&binding, &script, &query, exact, &short, pending())
            .await
            .unwrap_err()
            .kind,
        QueryErrorKind::InvalidInput
    );
    assert!(script.observation.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn errors_after_primary_rows_and_backend_cancellation_keep_partial_non_success() {
    let binding = binding("db", SCOPE);
    for (errors, cancelled, kind) in [
        (true, false, QueryErrorKind::BackendFailure),
        (false, true, QueryErrorKind::BackendCancelled),
    ] {
        let value = json!([
            header(),
            table(0, json!([[1, null]])),
            completion(errors, cancelled)
        ]);
        let script = Script::new(binding.clone(), &value);
        let error = query_kql(
            &binding,
            &script,
            &KqlQuery::new("T", "id").unwrap(),
            limits(100),
            &deadline(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, kind);
        let partial = error.partial.unwrap();
        assert_eq!(
            partial.datasets[0].rows,
            json!([[1, null]])
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_array().unwrap().clone())
                .collect::<Vec<_>>()
        );
        assert!(!partial.evidence.completed);
        assert_eq!(partial.completion["HasErrors"], errors);
        assert_eq!(script.observation.lock().unwrap().requests.len(), 1);
    }
}

#[tokio::test]
async fn row_cap_is_total_across_tables_and_metadata_is_not_data() {
    let binding = binding("db", SCOPE);
    let mut metadata = table(8, json!([[0,{"warning":PRIVATE}]]));
    metadata["TableKind"] = json!("QueryCompletionInformation");
    let value = json!([
        header(),
        table(0, json!([[1, null], [2, null]])),
        metadata,
        table(1, json!([[3, null], [4, null]])),
        completion(false, false)
    ]);
    for cap in [0, 3] {
        let script = Script::new(binding.clone(), &value);
        let error = query_kql(
            &binding,
            &script,
            &KqlQuery::new("T", "id").unwrap(),
            limits(cap),
            &deadline(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, QueryErrorKind::RowLimit);
        let partial = error.partial.unwrap();
        assert_eq!(
            partial.datasets.iter().map(|t| t.rows.len()).sum::<usize>(),
            cap
        );
        assert_eq!(partial.metadata.len(), 1);
        assert_eq!(partial.metadata[0]["Rows"][0][1]["warning"], PRIVATE);
        assert!(partial.evidence.locally_truncated);
        assert!(!format!("{partial:?}").contains(PRIVATE));
        assert_eq!(script.observation.lock().unwrap().drops, 1);
    }
}

#[tokio::test]
async fn missing_terminal_progressive_duplicate_ids_and_bad_width_never_succeed() {
    let binding = binding("db", SCOPE);
    let mut progressive = header();
    progressive["IsProgressive"] = json!(true);
    let mut bad_width = table(0, json!([[1]]));
    let missing_type = {
        bad_width["Rows"] = json!([]);
        bad_width["Columns"][0]
            .as_object_mut()
            .unwrap()
            .remove("ColumnType");
        bad_width
    };
    for value in [
        json!([header(), table(0, json!([]))]),
        json!([progressive, completion(false, false)]),
        json!([
            header(),
            table(0, json!([])),
            table(0, json!([])),
            completion(false, false)
        ]),
        json!([header(), table(0, json!([[1]])), completion(false, false)]),
        json!([header(), missing_type, completion(false, false)]),
        json!([header(),{"FrameType":"TableFragment"},completion(false,false)]),
        json!({"error":{"code":"bad"}}),
    ] {
        let script = Script::new(binding.clone(), &value);
        let error = query_kql(
            &binding,
            &script,
            &KqlQuery::new("T", "id").unwrap(),
            limits(100),
            &deadline(),
            pending(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error.kind,
            QueryErrorKind::InvalidResponse | QueryErrorKind::UnsupportedResponse
        ));
        assert!(!error.evidence.unwrap().completed);
    }
}

#[tokio::test]
async fn byte_bounds_and_broken_streams_fail_without_replay() {
    let binding = binding("db", SCOPE);
    for failure in [NativeError::Transport, NativeError::TimedOut] {
        let script = Script::new(binding.clone(), &frames());
        script
            .fragments
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .push_front(Err(failure));
        let error = query_kql(
            &binding,
            &script,
            &KqlQuery::new("T", "id").unwrap(),
            limits(100),
            &deadline(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, QueryErrorKind::Native(failure));
        assert_eq!(script.observation.lock().unwrap().requests.len(), 1);
        assert_eq!(script.observation.lock().unwrap().drops, 1);
    }
    let script = Script::new(binding.clone(), &frames());
    let small = QueryLimits::new(1024 * 1024, 100, 100, 16, 128, 64).unwrap();
    let error = query_kql(
        &binding,
        &script,
        &KqlQuery::new("T", "id").unwrap(),
        small,
        &deadline(),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::ResponseLimit);
    assert_eq!(script.observation.lock().unwrap().requests.len(), 1);
    assert_eq!(script.observation.lock().unwrap().drops, 1);
}

#[tokio::test]
async fn http_error_preserves_bounded_provider_body_and_does_not_retry() {
    let binding = binding("db", SCOPE);
    for status in [202, 400, 403, 429, 503] {
        let mut script = Script::new(binding.clone(), &json!({"error":{"message":PRIVATE}}));
        script.status = status;
        let error = query_kql(
            &binding,
            &script,
            &KqlQuery::new("T", "id").unwrap(),
            limits(100),
            &deadline(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, QueryErrorKind::Http);
        assert_eq!(error.evidence.as_ref().unwrap().http_status, Some(status));
        assert!(error.partial.is_none());
        assert!(!format!("{error:?} {error}").contains(PRIVATE));
        assert_eq!(
            serde_json::from_slice::<Value>(error.provider_body.as_ref().unwrap()).unwrap()["error"]
                ["message"],
            PRIVATE
        );
        assert_eq!(script.observation.lock().unwrap().requests.len(), 1);
    }
}

#[tokio::test]
async fn invalid_media_and_ambiguous_headers_are_rejected() {
    let binding = binding("db", SCOPE);
    for media in [
        None,
        Some("text/csv"),
        Some("application/json; charset=utf-16"),
        Some("application/jsonp"),
    ] {
        let mut script = Script::new(binding.clone(), &frames());
        script.media = media.map(str::to_owned);
        assert!(
            query_kql(
                &binding,
                &script,
                &KqlQuery::new("T", "id").unwrap(),
                limits(100),
                &deadline(),
                pending()
            )
            .await
            .is_err()
        );
        assert_eq!(script.observation.lock().unwrap().reads, 0);
    }
    let mut script = Script::new(binding.clone(), &frames());
    script
        .headers
        .insert("X-MS-ACTIVITY-ID".into(), "duplicate".into());
    assert_eq!(
        query_kql(
            &binding,
            &script,
            &KqlQuery::new("T", "id").unwrap(),
            limits(100),
            &deadline(),
            pending()
        )
        .await
        .unwrap_err()
        .kind,
        QueryErrorKind::Native(NativeError::InvalidResponse)
    );
}

#[tokio::test]
async fn local_cancellation_before_open_and_during_body_releases_only_local_work() {
    let binding = binding("db", SCOPE);
    let query = KqlQuery::new("T", "id").unwrap();
    let script = Script::new(binding.clone(), &frames());
    let error = query_kql(
        &binding,
        &script,
        &query,
        limits(100),
        &deadline(),
        ready(()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Native(NativeError::Cancelled));
    assert!(script.observation.lock().unwrap().requests.is_empty());
    let mut script = Script::new(binding.clone(), &frames());
    script.pending_read = true;
    let observation = Arc::clone(&script.observation);
    let cancel = std::future::poll_fn(move |cx| {
        if observation.lock().unwrap().reads > 0 {
            std::task::Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    });
    let error = query_kql(&binding, &script, &query, limits(100), &deadline(), cancel)
        .await
        .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Native(NativeError::Cancelled));
    assert_eq!(script.observation.lock().unwrap().requests.len(), 1);
    assert_eq!(script.observation.lock().unwrap().drops, 1);
    assert!(!error.evidence.unwrap().completed);
}
