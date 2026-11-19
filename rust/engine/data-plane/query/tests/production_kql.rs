// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic credentials and HTTP exercise the real production adapter, not an
//! executor reporting ScriptedOffline. No endpoint is contacted.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    error::ErrorKind,
    http::{AsyncRawResponse, HttpClient, Method, Request, Url, headers::Headers},
    time::OffsetDateTime,
};
use fabric_sdk::runtime::{
    NativeDeadline, NativeEffect, NativeEndpoint, NativeError, NativeMethod, NativeStreamExecutor,
    NativeStreamLimits,
};
use fabric_sdk_http::{ExecutionFuture, ExecutionTimer};
use fabric_sdk_query::{
    EngineBinding, EngineExecutor, KqlQuery, KustoCredentialContext, KustoQueryExecutor,
    QueryCapability, QueryDiagnosticKind, QueryErrorKind, QueryLimits, query_kql,
};
use fabric_v2_contracts::ItemIdentity;
use futures_util::Stream;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending, poll_fn, ready},
    pin::{Pin, pin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

const ROOT: &str = "https://offline.kusto.fabric.microsoft.com/";
const SCOPE: &str = "https://api.kusto.windows.net/.default";
const PRIVATE: &str = "private-query-and-provider-sentinel";
const TOKEN: &str = "synthetic-kusto-token";

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
    pending: bool,
    failed: bool,
    delay: Option<Duration>,
    drops: AtomicUsize,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert!(options.is_none(), "caller owns provider identity selection");
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|value| (*value).to_owned()).collect());
        let _drop = DropCount(&self.drops);
        if self.pending {
            pending::<()>().await;
        }
        if let Some(delay) = self.delay {
            std::thread::sleep(delay);
        }
        if self.failed {
            return Err(private_error());
        }
        Ok(AccessToken::new(
            TOKEN,
            OffsetDateTime::from_unix_timestamp(4_102_444_800).unwrap(),
        ))
    }
}

#[derive(Default)]
struct Timer {
    expired: Arc<AtomicBool>,
    drops: AtomicUsize,
}

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        assert!(!delay.is_zero());
        Box::pin(async move {
            let _drop = DropCount(&self.drops);
            poll_fn(|_| {
                if self.expired.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await
        })
    }
}

struct DropCount<'a>(&'a AtomicUsize);

impl Drop for DropCount<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct Probe {
    polls: AtomicUsize,
    drops: AtomicUsize,
}

enum Chunk {
    Bytes(Bytes),
    Broken,
    Pending,
}

struct Body {
    chunks: VecDeque<Chunk>,
    probe: Arc<Probe>,
}

impl Stream for Body {
    type Item = azure_core::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.probe.polls.fetch_add(1, Ordering::SeqCst);
        match self.chunks.pop_front() {
            Some(Chunk::Bytes(bytes)) => Poll::Ready(Some(Ok(bytes))),
            Some(Chunk::Broken) => Poll::Ready(Some(Err(private_error()))),
            Some(Chunk::Pending) => {
                self.chunks.push_front(Chunk::Pending);
                Poll::Pending
            }
            None => Poll::Ready(None),
        }
    }
}

impl Drop for Body {
    fn drop(&mut self) {
        self.probe.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct Wire {
    reply: Mutex<Option<AsyncRawResponse>>,
    requests: Mutex<Vec<Request>>,
    pending: bool,
    failed: bool,
    drops: AtomicUsize,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        let _drop = DropCount(&self.drops);
        if self.pending {
            return pending().await;
        }
        if self.failed {
            return Err(private_error());
        }
        Ok(self
            .reply
            .lock()
            .unwrap()
            .take()
            .expect("no retry, redirect, remote cancellation or replay"))
    }
}

fn private_error() -> azure_core::Error {
    azure_core::Error::with_message(ErrorKind::Io, PRIVATE)
}

fn response(
    status: u16,
    media: Option<&str>,
    chunks: impl IntoIterator<Item = Chunk>,
) -> (Arc<Wire>, Arc<Probe>) {
    let mut headers = Headers::new();
    if let Some(media) = media {
        headers.insert("content-type", media.to_owned());
    }
    headers.insert("x-ms-activity-id", "synthetic-activity");
    headers.insert("retry-after", "0");
    headers.insert("location", "https://not-followed.invalid/");
    let probe = Arc::new(Probe::default());
    let body = Body {
        chunks: chunks.into_iter().collect(),
        probe: probe.clone(),
    };
    (
        Arc::new(Wire {
            reply: Mutex::new(Some(AsyncRawResponse::new(
                status.into(),
                headers,
                Box::pin(body),
            ))),
            requests: Mutex::default(),
            pending: false,
            failed: false,
            drops: AtomicUsize::new(0),
        }),
        probe,
    )
}

fn header() -> Value {
    json!({"FrameType":"DataSetHeader","Version":"v2.0","IsProgressive":false})
}

fn completion(errors: bool, cancelled: bool) -> Value {
    json!({"FrameType":"DataSetCompletion","HasErrors":errors,"Cancelled":cancelled})
}

fn table(id: u64, rows: Value) -> Value {
    json!({"FrameType":"DataTable","TableId":id,"TableKind":"PrimaryResult","TableName":"Synthetic",
        "Columns":[{"ColumnName":"duplicate","ColumnType":"long"},
                   {"ColumnName":"duplicate","ColumnType":"dynamic"}],"Rows":rows})
}

fn frames() -> Value {
    json!([
        header(),
        table(
            8,
            json!([[9007199254740993_u64, {"null":null,"text":"\u{03bb}"}]])
        ),
        table(3, json!([])),
        completion(false, false)
    ])
}

fn chunks(value: &Value) -> Vec<Chunk> {
    serde_json::to_vec(value)
        .unwrap()
        .chunks(19)
        .map(|part| Chunk::Bytes(Bytes::from(part.to_vec())))
        .collect()
}

fn binding() -> EngineBinding {
    EngineBinding::kusto(
        "kql",
        "1",
        ItemIdentity::new(
            "11111111-1111-1111-1111-111111111111",
            "22222222-2222-2222-2222-222222222222",
        )
        .unwrap(),
        Url::parse(ROOT).unwrap(),
        "exact-db",
        SCOPE,
        "account",
        "tenant",
    )
    .unwrap()
}

fn limits() -> QueryLimits {
    QueryLimits::new(1024 * 1024, 1024 * 1024, 100, 8, 32, 32).unwrap()
}

fn deadline() -> NativeDeadline {
    NativeDeadline::new(Duration::from_secs(30)).unwrap()
}

fn query() -> KqlQuery {
    KqlQuery::new(".drop table T; // \u{03bb}", "query;synthetic")
        .unwrap()
        .with_server_timeout(Duration::from_secs(1))
        .unwrap()
}

fn configured(
    binding: &EngineBinding,
    credential: Arc<Credential>,
    wire: Arc<Wire>,
    timer: Arc<Timer>,
) -> KustoQueryExecutor {
    KustoQueryExecutor::new(
        binding.clone(),
        KustoCredentialContext::new(credential, "account", "tenant", SCOPE).unwrap(),
        wire,
        timer,
    )
    .unwrap()
}

fn stream_limits() -> NativeStreamLimits {
    NativeStreamLimits {
        max_fragment_bytes: 4096,
        max_body_bytes: 1024 * 1024,
    }
}

fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

#[tokio::test]
async fn production_adapter_uses_exact_credential_scope_request_bytes_and_all_tables() {
    let binding = binding();
    let credential = Arc::new(Credential::default());
    let (wire, probe) = response(
        200,
        Some("application/json; charset=utf-8"),
        chunks(&frames()),
    );
    let executor = configured(&binding, credential.clone(), wire.clone(), Arc::default());
    assert_eq!(
        executor.query_capability(),
        QueryCapability::ReadOnlyKustoV2
    );
    let query = query();
    let result = query_kql(
        &binding,
        &executor,
        &query,
        limits(),
        &deadline(),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(*credential.scopes.lock().unwrap(), [vec![SCOPE.to_owned()]]);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method(), Method::Post);
    assert_eq!(requests[0].url().as_str(), format!("{ROOT}v2/rest/query"));
    let expected = query.request(&binding, limits()).unwrap();
    assert_eq!(
        Bytes::from(requests[0].body()).as_ref(),
        expected.bytes().unwrap()
    );
    assert_eq!(
        requests[0]
            .headers()
            .get_str(&"x-ms-client-request-id".into())
            .unwrap(),
        "query;synthetic"
    );
    assert_eq!(result.datasets.len(), 2);
    assert_eq!(result.datasets[0].table_id, 8);
    assert_eq!(result.datasets[1].table_id, 3);
    assert!(result.datasets[1].rows.is_empty());
    assert_eq!(
        result.datasets[0].columns[0].name,
        result.datasets[0].columns[1].name
    );
    assert_eq!(result.datasets[0].columns[0].native_type, "long");
    assert_eq!(result.datasets[0].rows[0][0], json!(9007199254740993_u64));
    assert_eq!(
        result.evidence.adapter_capability,
        QueryCapability::ReadOnlyKustoV2
    );
    assert!(result.evidence.completed && result.evidence.http_eof);
    assert!(
        !result.evidence.production_verified,
        "fixtures are not tenant enforcement proof"
    );
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert!(!format!("{executor:?} {result:?}").contains(TOKEN));
}

#[tokio::test]
async fn mismatched_descriptor_or_credential_context_fails_before_credential_and_http() {
    let expected = binding();
    let credential = Arc::new(Credential::default());
    let (wire, _) = response(200, Some("application/json"), chunks(&frames()));
    for (account, tenant, scope) in [("other", "tenant", SCOPE), ("account", "other", SCOPE)] {
        let context =
            KustoCredentialContext::new(credential.clone(), account, tenant, scope).unwrap();
        assert!(
            KustoQueryExecutor::new(
                expected.clone(),
                context,
                wire.clone(),
                Arc::new(Timer::default())
            )
            .is_err()
        );
    }
    for scope in [
        "https://api.fabric.microsoft.com/.default",
        "https://management.azure.com/.default",
        "https://unsupported.invalid/.default",
        "",
    ] {
        assert!(
            KustoCredentialContext::new(credential.clone(), "account", "tenant", scope).is_err()
        );
    }
    let executor = configured(&expected, credential.clone(), wire.clone(), Arc::default());
    for changed in 0..8 {
        let owner = if changed == 2 {
            ItemIdentity::new(
                "33333333-3333-3333-3333-333333333333",
                "22222222-2222-2222-2222-222222222222",
            )
            .unwrap()
        } else {
            expected.owner().clone()
        };
        let supplied = EngineBinding::kusto(
            if changed == 0 { "other" } else { "kql" },
            if changed == 1 { "2" } else { "1" },
            owner,
            Url::parse(if changed == 3 {
                "https://other.invalid/"
            } else {
                ROOT
            })
            .unwrap(),
            if changed == 4 { "other-db" } else { "exact-db" },
            if changed == 5 {
                "https://other.invalid/.default"
            } else {
                SCOPE
            },
            if changed == 6 {
                "other-account"
            } else {
                "account"
            },
            if changed == 7 {
                "other-tenant"
            } else {
                "tenant"
            },
        )
        .unwrap();
        assert_eq!(
            query_kql(
                &supplied,
                &executor,
                &query(),
                limits(),
                &deadline(),
                pending()
            )
            .await
            .unwrap_err()
            .kind,
            QueryErrorKind::InvalidBinding
        );
    }
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn direct_open_cannot_bypass_origin_route_or_policy_preflight() {
    let binding = binding();
    let credential = Arc::new(Credential::default());
    let (wire, _) = response(200, Some("application/json"), chunks(&frames()));
    let executor = configured(&binding, credential.clone(), wire.clone(), Arc::default());
    let valid = query().request(&binding, limits()).unwrap();
    let body = valid.bytes().unwrap().to_vec();
    for (root, route, method, effect) in [
        (
            "https://other.invalid/",
            "query",
            NativeMethod::Post,
            NativeEffect::Read,
        ),
        (ROOT, "mgmt", NativeMethod::Post, NativeEffect::Read),
        (ROOT, "query", NativeMethod::Put, NativeEffect::Read),
        (ROOT, "query", NativeMethod::Post, NativeEffect::Execute),
    ] {
        let request = NativeEndpoint::new(Url::parse(root).unwrap())
            .unwrap()
            .request(method, &["v2", "rest", route], &[], None, effect)
            .unwrap()
            .with_bytes(body.clone(), "application/json; charset=utf-8", 1024 * 1024)
            .unwrap()
            .with_header("x-ms-client-request-id", "query;synthetic")
            .unwrap();
        assert_eq!(
            executor
                .open(&request, stream_limits(), &deadline())
                .await
                .unwrap_err(),
            NativeError::InvalidInput
        );
    }
    let mut changed: Value = serde_json::from_slice(&body).unwrap();
    changed["properties"]["Options"]["request_readonly_hardline"] = json!(false);
    let request = binding
        .endpoint()
        .request(
            NativeMethod::Post,
            &["v2", "rest", "query"],
            &[],
            None,
            NativeEffect::Read,
        )
        .unwrap()
        .with_bytes(
            serde_json::to_vec(&changed).unwrap(),
            "application/json; charset=utf-8",
            1024 * 1024,
        )
        .unwrap()
        .with_header("x-ms-client-request-id", "query;synthetic")
        .unwrap();
    assert!(
        executor
            .open(&request, stream_limits(), &deadline())
            .await
            .is_err()
    );
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn direct_open_requires_objects_at_every_policy_level_before_any_credential_or_http() {
    let binding = binding();
    let credential = Arc::new(Credential::default());
    let (wire, _) = response(200, Some("application/json"), chunks(&frames()));
    let executor = configured(&binding, credential.clone(), wire.clone(), Arc::default());
    let valid = query().request(&binding, limits()).unwrap();
    let body: Value = serde_json::from_slice(valid.bytes().unwrap()).unwrap();
    let properties = &body["properties"];
    let options = &properties["Options"];
    let positional_options = json!([
        "kql",
        true,
        true,
        true,
        true,
        true,
        true,
        true,
        false,
        "00:00:01.000"
    ]);
    let mut invalid: Vec<Vec<u8>> = [
        json!([body["db"], body["csl"], properties]),
        json!({"db":body["db"],"csl":body["csl"],"properties":[options]}),
        json!({"db":body["db"],"csl":body["csl"],"properties":{"Options":positional_options}}),
        Value::Null,
        json!({"db":body["db"],"csl":body["csl"],"properties":null}),
        json!({"db":body["db"],"csl":body["csl"],"properties":{"Options":null}}),
        json!({"db":body["db"],"csl":body["csl"],"properties":{}}),
        json!({"db":body["db"],"csl":body["csl"],"properties":{"Options":{}}}),
        json!({"db":body["db"],"csl":body["csl"],"properties":properties,"unknown":true}),
        json!({"db":body["db"],"csl":body["csl"],"properties":{"Options":options,"unknown":true}}),
    ]
    .iter()
    .map(|value| serde_json::to_vec(value).unwrap())
    .collect();
    let mut unknown_option = body.clone();
    unknown_option["properties"]["Options"]["unknown"] = json!(true);
    invalid.push(serde_json::to_vec(&unknown_option).unwrap());
    let text = String::from_utf8(valid.bytes().unwrap().to_vec()).unwrap();
    invalid.extend([
        text.replace(
            "\"db\":\"exact-db\"",
            "\"db\":\"exact-db\",\"db\":\"exact-db\"",
        )
        .into_bytes(),
        format!(
            r#"{{"db":{},"csl":{},"properties":{},"properties":{}}}"#,
            body["db"], body["csl"], properties, properties
        )
        .into_bytes(),
        format!(
            r#"{{"db":{},"csl":{},"properties":{{"Options":{},"Options":{}}}}}"#,
            body["db"], body["csl"], options, options
        )
        .into_bytes(),
        text.replace(
            "\"request_readonly\":true",
            "\"request_readonly\":true,\"request_readonly\":true",
        )
        .into_bytes(),
        format!("{text} null").into_bytes(),
        b"{malformed".to_vec(),
        vec![0xff],
    ]);
    for bytes in invalid {
        let request = binding
            .endpoint()
            .request(
                NativeMethod::Post,
                &["v2", "rest", "query"],
                &[],
                None,
                NativeEffect::Read,
            )
            .unwrap()
            .with_bytes(bytes, "application/json; charset=utf-8", 1024 * 1024)
            .unwrap()
            .with_header("x-ms-client-request-id", "query;synthetic")
            .unwrap();
        assert_eq!(
            executor
                .open(&request, stream_limits(), &deadline())
                .await
                .unwrap_err(),
            NativeError::InvalidInput
        );
        assert!(credential.scopes.lock().unwrap().is_empty());
        assert!(wire.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn direct_open_object_validation_never_reserializes_original_request_bytes() {
    let binding = binding();
    let credential = Arc::new(Credential::default());
    let (wire, _) = response(200, Some("application/json"), chunks(&frames()));
    let executor = configured(&binding, credential.clone(), wire.clone(), Arc::default());
    let valid = query().request(&binding, limits()).unwrap();
    let original = format!(
        " \n{}\n ",
        String::from_utf8(valid.bytes().unwrap().to_vec()).unwrap()
    )
    .into_bytes();
    let request = binding
        .endpoint()
        .request(
            NativeMethod::Post,
            &["v2", "rest", "query"],
            &[],
            None,
            NativeEffect::Read,
        )
        .unwrap()
        .with_bytes(
            original.clone(),
            "application/json; charset=utf-8",
            1024 * 1024,
        )
        .unwrap()
        .with_header("x-ms-client-request-id", "query;synthetic")
        .unwrap();
    let response = executor
        .open(&request, stream_limits(), &deadline())
        .await
        .unwrap();
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        Bytes::from(requests[0].body()).as_ref(),
        original.as_slice()
    );
    drop(response);
}

#[tokio::test]
async fn http_errors_redirects_and_transients_are_not_replayed_or_completed() {
    for status in [202, 301, 307, 400, 401, 403, 429, 500, 503] {
        let binding = binding();
        let credential = Arc::new(Credential::default());
        let bytes = serde_json::to_vec(&json!({"error":{"message":PRIVATE}})).unwrap();
        let (wire, probe) = response(
            status,
            Some("application/json"),
            [Chunk::Bytes(Bytes::from(bytes.clone()))],
        );
        let executor = configured(&binding, credential, wire.clone(), Arc::default());
        let error = query_kql(
            &binding,
            &executor,
            &query(),
            limits(),
            &deadline(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, QueryErrorKind::Http, "{status}");
        assert_eq!(error.evidence.as_ref().unwrap().http_status, Some(status));
        assert!(error.evidence.as_ref().unwrap().http_eof);
        assert_eq!(error.provider_body.unwrap(), bytes);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn cancellation_before_credentials_and_pending_credential_open_or_body_disposes_local_io() {
    let binding = binding();
    let credential = Arc::new(Credential::default());
    let (wire, _) = response(200, Some("application/json"), chunks(&frames()));
    let executor = configured(&binding, credential.clone(), wire.clone(), Arc::default());
    let error = query_kql(
        &binding,
        &executor,
        &query(),
        limits(),
        &deadline(),
        ready(()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Native(NativeError::Cancelled));
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(wire.requests.lock().unwrap().is_empty());
    for phase in 0..3 {
        let credential = Arc::new(Credential {
            pending: phase == 0,
            ..Default::default()
        });
        let (mut wire, probe) = response(200, Some("application/json"), [Chunk::Pending]);
        Arc::get_mut(&mut wire).unwrap().pending = phase == 1;
        let executor = configured(&binding, credential.clone(), wire.clone(), Arc::default());
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = {
            let cancelled = cancelled.clone();
            poll_fn(move |_| {
                if cancelled.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
        };
        let deadline = deadline();
        let query = query();
        let mut future = pin!(query_kql(
            &binding,
            &executor,
            &query,
            limits(),
            &deadline,
            cancel
        ));
        assert!(poll(future.as_mut()).is_pending());
        cancelled.store(true, Ordering::SeqCst);
        let Poll::Ready(Err(error)) = poll(future.as_mut()) else {
            panic!("local cancellation must finish")
        };
        assert_eq!(error.kind, QueryErrorKind::Native(NativeError::Cancelled));
        assert!(!error.evidence.unwrap().completed);
        assert_eq!(credential.drops.load(Ordering::SeqCst), 1);
        if phase != 0 {
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
        }
        if phase == 2 {
            assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn one_deadline_enforces_pending_credential_open_and_body() {
    let binding = binding();
    for phase in 0..3 {
        let timer = Arc::new(Timer::default());
        let credential = Arc::new(Credential {
            pending: phase == 0,
            ..Default::default()
        });
        let (mut wire, probe) = response(200, Some("application/json"), [Chunk::Pending]);
        Arc::get_mut(&mut wire).unwrap().pending = phase == 1;
        let executor = configured(&binding, credential.clone(), wire.clone(), timer.clone());
        let deadline = deadline();
        let query = query();
        let mut future = pin!(query_kql(
            &binding,
            &executor,
            &query,
            limits(),
            &deadline,
            pending()
        ));
        assert!(poll(future.as_mut()).is_pending());
        timer.expired.store(true, Ordering::SeqCst);
        let Poll::Ready(Err(error)) = poll(future.as_mut()) else {
            panic!("deadline must finish pending I/O")
        };
        assert!(matches!(
            error.kind,
            QueryErrorKind::Native(NativeError::TimedOut | NativeError::OutcomeUnknown)
        ));
        assert!(!error.evidence.unwrap().completed);
        if phase == 0 {
            assert!(wire.requests.lock().unwrap().is_empty());
        }
        if phase == 2 {
            assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn credential_returning_after_absolute_deadline_never_dispatches() {
    let binding = binding();
    let credential = Arc::new(Credential {
        delay: Some(Duration::from_millis(150)),
        ..Default::default()
    });
    let (wire, _) = response(200, Some("application/json"), chunks(&frames()));
    let executor = configured(&binding, credential.clone(), wire.clone(), Arc::default());
    let query = KqlQuery::new("T", "query;expired")
        .unwrap()
        .with_server_timeout(Duration::from_millis(1))
        .unwrap();
    let deadline = NativeDeadline::new(Duration::from_millis(100)).unwrap();
    let error = query_kql(&binding, &executor, &query, limits(), &deadline, pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, QueryErrorKind::Native(NativeError::TimedOut));
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn broken_or_non_eof_body_is_not_a_complete_native_result() {
    let binding = binding();
    let prefix = serde_json::to_vec(&frames()).unwrap();
    for broken in [false, true] {
        let (wire, probe) = response(
            200,
            Some("application/json"),
            [
                Chunk::Bytes(Bytes::from(prefix.clone())),
                if broken {
                    Chunk::Broken
                } else {
                    Chunk::Pending
                },
            ],
        );
        let executor = configured(&binding, Arc::default(), wire.clone(), Arc::default());
        let deadline = deadline();
        let query = query();
        let cancelled = poll_fn(|_| {
            if probe.polls.load(Ordering::SeqCst) > 1 {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let mut future = pin!(query_kql(
            &binding,
            &executor,
            &query,
            limits(),
            &deadline,
            cancelled
        ));
        let first = poll(future.as_mut());
        let result = if first.is_pending() {
            poll(future.as_mut())
        } else {
            first
        };
        let Poll::Ready(Err(error)) = result else {
            panic!("must not complete before EOF")
        };
        assert_eq!(error.provider_body.as_ref().unwrap(), &prefix);
        assert!(!error.evidence.as_ref().unwrap().http_eof);
        assert!(!error.evidence.unwrap().completed);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn production_inline_terminal_warning_error_cancel_and_missing_terminal_preserve_forensics() {
    let binding = binding();
    for case in 0..5 {
        let mut value = frames();
        let expected = match case {
            0 => {
                value[1]["Rows"].as_array_mut().unwrap().push(
                    json!({"OneApiErrors":[{"error":{"code":"Synthetic","message":PRIVATE}}]}),
                );
                QueryErrorKind::BackendFailure
            }
            1 => {
                value.as_array_mut().unwrap().insert(3, json!({
                    "FrameType":"DataTable","TableId":10,"TableKind":"QueryCompletionInformation","TableName":"Status",
                    "Columns":[{"ColumnName":"Level","ColumnType":"int"},{"ColumnName":"ClientRequestId","ColumnType":"string"},{"ColumnName":"Payload","ColumnType":"string"}],
                    "Rows":[[3,"query;synthetic",PRIVATE]]
                }));
                QueryErrorKind::BackendWarning
            }
            2 => {
                value[3] = completion(true, false);
                QueryErrorKind::BackendFailure
            }
            3 => {
                value[3] = completion(false, true);
                QueryErrorKind::BackendCancelled
            }
            _ => {
                value.as_array_mut().unwrap().pop();
                QueryErrorKind::InvalidResponse
            }
        };
        let bytes = serde_json::to_vec(&value).unwrap();
        let (wire, _) = response(200, Some("application/json"), chunks(&value));
        let executor = configured(&binding, Arc::default(), wire.clone(), Arc::default());
        let error = query_kql(
            &binding,
            &executor,
            &query(),
            limits(),
            &deadline(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, expected);
        assert!(!format!("{error:?} {error}").contains(PRIVATE));
        assert_eq!(error.provider_body.unwrap(), bytes);
        let partial = error.partial.unwrap();
        assert!(partial.evidence.http_eof && !partial.evidence.completed);
        assert_eq!(partial.datasets.len(), 2);
        if case == 0 {
            assert_eq!(
                partial.diagnostics[0].kind,
                QueryDiagnosticKind::InlineError
            );
        }
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn expired_deadline_missing_timeout_and_oversized_request_fail_before_credentials() {
    let binding = binding();
    let credential = Arc::new(Credential::default());
    let (wire, _) = response(200, Some("application/json"), chunks(&frames()));
    let executor = configured(&binding, credential.clone(), wire.clone(), Arc::default());
    let expired = NativeDeadline::new(Duration::from_nanos(1)).unwrap();
    std::thread::sleep(Duration::from_millis(1));
    assert_eq!(
        query_kql(&binding, &executor, &query(), limits(), &expired, pending())
            .await
            .unwrap_err()
            .kind,
        QueryErrorKind::Native(NativeError::TimedOut)
    );
    assert_eq!(
        query_kql(
            &binding,
            &executor,
            &KqlQuery::new("T", "id").unwrap(),
            limits(),
            &deadline(),
            pending()
        )
        .await
        .unwrap_err()
        .kind,
        QueryErrorKind::InvalidInput
    );
    assert_eq!(
        query_kql(
            &binding,
            &executor,
            &query(),
            QueryLimits::new(1, 1024, 10, 4, 4, 8).unwrap(),
            &deadline(),
            pending()
        )
        .await
        .unwrap_err()
        .kind,
        QueryErrorKind::RequestLimit
    );
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn timeout_is_revalidated_after_credentials_without_clamping_or_dispatch() {
    let binding = binding();
    let credential = Arc::new(Credential {
        delay: Some(Duration::from_millis(200)),
        ..Default::default()
    });
    let (wire, _) = response(200, Some("application/json"), chunks(&frames()));
    let executor = configured(&binding, credential.clone(), wire.clone(), Arc::default());
    let query = KqlQuery::new("T", "query;budget")
        .unwrap()
        .with_server_timeout(Duration::from_millis(400))
        .unwrap();
    let deadline = NativeDeadline::new(Duration::from_millis(500)).unwrap();
    let error = query_kql(&binding, &executor, &query, limits(), &deadline, pending())
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        QueryErrorKind::Native(NativeError::InvalidInput)
    );
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn credential_failure_and_uncertain_transmission_are_sanitized_without_fallback() {
    let binding = binding();
    for failed_credential in [true, false] {
        let credential = Arc::new(Credential {
            failed: failed_credential,
            ..Default::default()
        });
        let (mut wire, _) = response(200, Some("application/json"), chunks(&frames()));
        Arc::get_mut(&mut wire).unwrap().failed = !failed_credential;
        let executor = configured(&binding, credential.clone(), wire.clone(), Arc::default());
        let error = query_kql(
            &binding,
            &executor,
            &query(),
            limits(),
            &deadline(),
            pending(),
        )
        .await
        .unwrap_err();
        if !failed_credential {
            assert_eq!(
                error.kind,
                QueryErrorKind::Native(NativeError::OutcomeUnknown)
            );
        }
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
        assert_eq!(
            wire.requests.lock().unwrap().len(),
            usize::from(!failed_credential)
        );
        assert!(!format!("{error:?} {error}").contains(PRIVATE));
        assert!(!format!("{error:?} {error}").contains(TOKEN));
        assert!(!error.evidence.unwrap().completed);
    }
}

#[tokio::test]
async fn production_media_and_shape_byte_row_caps_never_return_complete_tables() {
    let binding = binding();
    for media in [
        None,
        Some("text/csv"),
        Some("application/json; charset=utf-16"),
    ] {
        let (wire, probe) = response(200, media, chunks(&frames()));
        let executor = configured(&binding, Arc::default(), wire, Arc::default());
        let error = query_kql(
            &binding,
            &executor,
            &query(),
            limits(),
            &deadline(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, QueryErrorKind::InvalidResponse);
        assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        assert!(!error.evidence.unwrap().completed);
    }
    for (limits, expected) in [
        (
            QueryLimits::new(4096, 1024 * 1024, 0, 8, 32, 32).unwrap(),
            QueryErrorKind::RowLimit,
        ),
        (
            QueryLimits::new(4096, 1024 * 1024, 100, 1, 32, 32).unwrap(),
            QueryErrorKind::ResponseLimit,
        ),
        (
            QueryLimits::new(4096, 1024 * 1024, 100, 8, 1, 32).unwrap(),
            QueryErrorKind::ResponseLimit,
        ),
        (
            QueryLimits::new(4096, 1024 * 1024, 100, 8, 32, 2).unwrap(),
            QueryErrorKind::ResponseLimit,
        ),
    ] {
        let (wire, _) = response(200, Some("application/json"), chunks(&frames()));
        let executor = configured(&binding, Arc::default(), wire.clone(), Arc::default());
        let error = query_kql(
            &binding,
            &executor,
            &query(),
            limits,
            &deadline(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, expected);
        assert!(error.provider_body.is_some());
        assert!(!error.evidence.unwrap().completed);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
    let (wire, probe) = response(200, Some("application/json"), chunks(&frames()));
    let executor = configured(&binding, Arc::default(), wire.clone(), Arc::default());
    let error = query_kql(
        &binding,
        &executor,
        &query(),
        QueryLimits::new(4096, 100, 100, 8, 32, 32).unwrap(),
        &deadline(),
        pending(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.kind,
        QueryErrorKind::ResponseLimit
            | QueryErrorKind::Native(NativeError::Limit | NativeError::OutcomeUnknown)
    ));
    assert!(!error.evidence.unwrap().completed);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}
