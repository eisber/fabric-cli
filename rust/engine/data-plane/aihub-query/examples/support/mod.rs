// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![allow(
    dead_code,
    reason = "Offline support is shared by the public example and integration tests."
)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    error::ErrorKind,
    http::{AsyncRawResponse, HttpClient, Request, Url, headers::Headers},
    time::OffsetDateTime,
};
use fabric_sdk_aihub_query::{
    AiHubQueryClient, AuthBinding, DataAgentTarget, ExecutionTimer, Limits, NativeDeadline,
    QueryRequest, QueryStage, TypedJson,
};
use fabric_sdk_http::ExecutionFuture;
use futures_util::Stream;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{pending, poll_fn},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

pub const AGENT: &str = "11111111-1111-4111-8111-111111111111";
pub const SOURCE: &str = "22222222-2222-4222-8222-222222222222";
pub const RESULT: &str = "33333333-3333-4333-8333-333333333333";
pub const ENDPOINT: &str = "https://aihub.example.invalid/mcp";
pub const SCOPE: &str = "https://audience.example.invalid/.default";

#[derive(Debug, Default)]
pub struct Credential {
    pub calls: AtomicUsize,
    pub fail: bool,
    pub suspend: bool,
    pub dropped: AtomicUsize,
    pub scopes: Mutex<Vec<String>>,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert!(options.is_none());
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.scopes
            .lock()
            .unwrap()
            .extend(scopes.iter().map(|s| (*s).into()));
        if self.fail {
            return Err(private_error());
        }
        if self.suspend {
            let _guard = Dropped(&self.dropped);
            pending::<()>().await;
        }
        Ok(AccessToken::new(
            "synthetic-offline-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug, Default)]
pub struct Timer {
    pub expired: AtomicBool,
    pub waits: Mutex<Vec<Duration>>,
}

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        self.waits.lock().unwrap().push(delay);
        Box::pin(poll_fn(|_| {
            if self.expired.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }))
    }
}

pub enum Reply {
    Response {
        status: u16,
        media: String,
        session: Option<String>,
        chunks: Vec<Chunk>,
    },
    Failure,
    Pending,
}

pub enum Chunk {
    Bytes(Vec<u8>),
    Pending,
    Failure,
}

#[derive(Default)]
pub struct Wire {
    pub replies: Mutex<VecDeque<Reply>>,
    pub requests: Mutex<Vec<Request>>,
    pub dropped: AtomicUsize,
    pub body_dropped: Arc<AtomicUsize>,
}

impl std::fmt::Debug for Wire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OfflineWire")
    }
}

impl Wire {
    pub fn new(replies: impl IntoIterator<Item = Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            ..Self::default()
        })
    }
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.url().as_str(), ENDPOINT);
        self.requests.lock().unwrap().push(request.clone());
        let reply = self.replies.lock().unwrap().pop_front().expect(
            "unexpected dispatch, redirect, retry, resource download or remote cancellation",
        );
        match reply {
            Reply::Failure => Err(private_error()),
            Reply::Pending => {
                let _guard = Dropped(&self.dropped);
                pending().await
            }
            Reply::Response {
                status,
                media,
                session,
                chunks,
            } => {
                let mut headers = Headers::new();
                headers.insert("content-type", media);
                if let Some(session) = session {
                    headers.insert("mcp-session-id", session);
                }
                Ok(AsyncRawResponse::new(
                    status.into(),
                    headers,
                    Box::pin(Body {
                        chunks: chunks.into(),
                        dropped: self.body_dropped.clone(),
                    }),
                ))
            }
        }
    }
}

struct Body {
    chunks: VecDeque<Chunk>,
    dropped: Arc<AtomicUsize>,
}

impl Stream for Body {
    type Item = azure_core::Result<Bytes>;
    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.chunks.pop_front() {
            Some(Chunk::Bytes(bytes)) => Poll::Ready(Some(Ok(bytes.into()))),
            Some(Chunk::Failure) => Poll::Ready(Some(Err(private_error()))),
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
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

struct Dropped<'a>(&'a AtomicUsize);
impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn private_error() -> azure_core::Error {
    azure_core::Error::with_message(ErrorKind::Io, "synthetic-sensitive-provider-text")
}

pub fn response(status: u16, body: Vec<u8>, media: &str, session: Option<&str>) -> Reply {
    Reply::Response {
        status,
        media: media.into(),
        session: session.map(str::to_owned),
        chunks: vec![Chunk::Bytes(body)],
    }
}

pub fn rpc(id: u64, result: Value) -> Reply {
    response(
        200,
        serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"result":result})).unwrap(),
        "application/json",
        None,
    )
}

pub fn initialize() -> Reply {
    response(
        200,
        serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"result":{
            "protocolVersion":"2025-06-18", "capabilities":{"tools":{}},
            "serverInfo":{"name":"offline-fixture","version":"1"},
        }}))
        .unwrap(),
        "application/json",
        Some("synthetic-session"),
    )
}

pub fn tool() -> Value {
    json!({"name":"execute_query","annotations":{"readOnlyHint":true,"destructiveHint":false},"inputSchema":{
        "type":"object","required":["artifactId","query"],
        "properties":{
            "artifactId":{"type":"string"}, "query":{"type":"string"},
            "language":{"type":["string","null"],"description":"Supported selector: 'jmespath'"},
            "filename":{"type":["string","null"]},
            "max_rows":{"type":["integer","null"],"minimum":1,"maximum":10000}
        }
    }})
}

pub fn setup() -> Vec<Reply> {
    vec![
        initialize(),
        response(202, vec![], "application/json", None),
        rpc(2, json!({"tools":[tool()]})),
    ]
}

pub fn rowset() -> String {
    format!(
        r#"{{
        "schemaVersion":1,"resultId":"{RESULT}",
        "columns":[
            {{"name":"value","engineType":"uint64","logicalType":"integer","encoding":"json-number"}},
            {{"name":"value","engineType":"decimal","logicalType":"decimal","encoding":"json-number"}},
            {{"name":"","engineType":"vendor-extension","logicalType":"string","encoding":"text"}}
        ],
        "rowCount":1,"truncated":false,"maxRows":1000,"totalRowCount":null,
        "rows":[[18446744073709551615,0.1000000000000000000000000001,null]]
    }}"#
    )
}

pub fn typed(text: &str, uri: &str) -> Value {
    let mut meta: Value = serde_json::from_str(text).unwrap();
    let rows = meta.as_object_mut().unwrap().remove("rows").unwrap();
    let mut nulls = Vec::new();
    for (r, row) in rows.as_array().unwrap().iter().enumerate() {
        for (c, value) in row.as_array().unwrap().iter().enumerate() {
            if value.is_null() {
                nulls.push(json!([r, c]));
            }
        }
    }
    meta["format"] = json!("json");
    meta["nulls"] = json!(nulls);
    json!({"type":"resource","resource":{"uri":uri,"mimeType":"application/json","text":text,
        "_meta":{"fabric.queryResult":meta}}})
}

pub fn success() -> Value {
    json!({"content":[{"type":"text","text":"Synthetic query result."},typed(&rowset(),"file:///fixture.json")]})
}

pub fn request() -> QueryRequest {
    QueryRequest::new(
        DataAgentTarget::new(AGENT, SOURCE, QueryStage::Production).unwrap(),
        "synthetic query".into(),
        TypedJson::Required,
    )
    .unwrap()
}

pub fn client(
    wire: Arc<Wire>,
    credential: Arc<Credential>,
    timer: Arc<Timer>,
    limits: Limits,
) -> AiHubQueryClient {
    AiHubQueryClient::new(
        AuthBinding::new(
            Url::parse(ENDPOINT).unwrap(),
            SCOPE.into(),
            "synthetic-tenant".into(),
            "synthetic-account".into(),
        )
        .unwrap(),
        credential,
        wire,
        timer,
        limits,
    )
    .unwrap()
}

pub fn deadline() -> NativeDeadline {
    NativeDeadline::new(Duration::from_secs(30)).unwrap()
}

pub fn messages(wire: &Wire) -> Vec<Value> {
    wire.requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice(Bytes::from(r.body()).as_ref()).unwrap())
        .collect()
}
