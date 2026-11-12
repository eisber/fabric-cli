// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline shared parser -> Notebook list leaf -> RuntimeHead -> actual SDK tests.
//! Scripted native execution is not executable-host or live authorization proof.

#![cfg(feature = "runtime-v2")]
#![deny(unnameable_test_items)]

use azure_core::http::Url;
use fabric_cli::runtime_v2::{
    OutputSink, ParsedInvocation, ReadCoverage, ReadError, ReadErrorKind, ReadOperation,
    ReadOptions, ReadReceipt, ReadStatus, Representation, ResolvedTarget, RuntimeHead,
    RuntimeResolver, TypedPath, native_runtime_notebook_list as leaf, parse_args,
};
use fabric_sdk::{
    livy::LivyService,
    notebook_runtime::{NotebookRuntime, content::ContentLimits},
    runtime::{
        NativeEffect, NativeEndpoint, NativeError, NativeExecutor, NativeFuture, NativeMethod,
        NativeRequest, NativeResponse,
    },
};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::BTreeMap,
    future::Future,
    io,
    sync::Mutex,
    task::{Context, Poll, Waker},
};

const PATH: &str = "/Team.Workspace/Analysis.Notebook/sessions";
const ROUTE: &str = "https://notebook.example.invalid/webapi/capacities/cap/workloads/Notebook/Data/Automatic/api/workspaces/ws/artifacts/nb/jupyterApi/versions/1/api/sessions";

fn endpoint(origin: &str) -> NativeEndpoint {
    NativeEndpoint::new(Url::parse(origin).unwrap()).unwrap()
}

#[derive(Default)]
struct Resolver {
    calls: Cell<usize>,
    unavailable: bool,
    wrong_owner: bool,
}

impl RuntimeResolver for Resolver {
    fn resolve(&self, path: &TypedPath) -> Result<ResolvedTarget, NativeError> {
        self.calls.set(self.calls.get() + 1);
        assert_eq!(path, &TypedPath::parse(PATH).unwrap());
        if self.unavailable {
            return Err(NativeError::UnsupportedCapability);
        }
        if self.wrong_owner {
            return Ok(ResolvedTarget::Lakehouse(LivyService::native(endpoint(
                "https://livy.example.invalid",
            ))));
        }
        Ok(ResolvedTarget::Notebook(
            NotebookRuntime::new(
                endpoint("https://notebook.example.invalid"),
                "cap",
                "ws",
                "nb",
                ContentLimits::default(),
                4096,
                1000,
            )
            .unwrap(),
        ))
    }
}

struct Script {
    response: Mutex<Option<Result<NativeResponse, NativeError>>>,
    requests: Mutex<Vec<NativeRequest>>,
    limits: Mutex<Vec<usize>>,
}

impl Script {
    fn reply(status: u16, body: &[u8]) -> Self {
        Self {
            response: Mutex::new(Some(Ok(NativeResponse {
                status,
                body: body.into(),
                content_type: Some("application/json".into()),
                headers: BTreeMap::new(),
            }))),
            requests: Mutex::new(Vec::new()),
            limits: Mutex::new(Vec::new()),
        }
    }

    fn json(value: &Value) -> Self {
        Self::reply(200, &serde_json::to_vec(value).unwrap())
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl NativeExecutor for Script {
    fn execute<'a>(
        &'a self,
        request: &'a NativeRequest,
        max_response_bytes: usize,
    ) -> NativeFuture<'a, Result<NativeResponse, NativeError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            self.limits.lock().unwrap().push(max_response_bytes);
            self.response
                .lock()
                .unwrap()
                .take()
                .expect("A Notebook list must not retry or replay HTTP")
        })
    }
}

#[derive(Default)]
struct Sink {
    checks: usize,
    writes: usize,
    bytes: Vec<u8>,
    unavailable: bool,
    partial_failure: bool,
}

impl OutputSink for Sink {
    fn check(&mut self, _: &str) -> io::Result<()> {
        self.checks += 1;
        if self.unavailable {
            return Err(io::Error::other("private output destination"));
        }
        Ok(())
    }

    fn write(&mut self, _: &str, bytes: &[u8]) -> io::Result<()> {
        self.writes += 1;
        if self.partial_failure {
            self.bytes.extend_from_slice(&bytes[..bytes.len().min(7)]);
            return Err(io::Error::other("private output failure"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

fn complete<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("The one-response offline script must complete immediately"),
    }
}

fn invocation(extra: &[&str]) -> ParsedInvocation {
    parse_args(
        ["fab-cli-v2", "list", PATH]
            .into_iter()
            .chain(extra.iter().copied()),
    )
    .unwrap()
}

fn run(
    input: &ParsedInvocation,
    script: &Script,
    resolver: &Resolver,
    sink: &mut Sink,
    options: ReadOptions,
) -> Result<ReadReceipt, ReadError> {
    complete(leaf::run(
        &RuntimeHead::inject(script, resolver, options).unwrap(),
        input,
        sink,
    ))
}

#[test]
fn claims_only_notebook_session_collection_list() {
    assert!(leaf::claims(&invocation(&[])));
    let script = Script::json(&json!([]));
    let resolver = Resolver::default();
    let mut sink = Sink::default();
    for (command, path) in [
        ("get", "/Team.Workspace/Analysis.Notebook"),
        ("get", "/Team.Workspace/Analysis.Notebook/src/content"),
        ("get", "/Team.Workspace/Analysis.Notebook/sessions/exact"),
        ("list", "/Team.Workspace/Data.Lakehouse/sessions"),
        ("list", "/Team.Workspace/Native.LivyService/sessions"),
        ("get", "/Team.Workspace/Data.Lakehouse/sessions/1"),
        (
            "get",
            "/Team.Workspace/Native.LivyService/sessions/1/statements/2/result",
        ),
    ] {
        let input = parse_args(["fab-cli-v2", command, path]).unwrap();
        assert!(!leaf::claims(&input));
        assert_eq!(
            run(
                &input,
                &script,
                &resolver,
                &mut sink,
                ReadOptions::default()
            )
            .unwrap_err()
            .kind,
            ReadErrorKind::UnsupportedCapability
        );
    }
    let mut forged = invocation(&[]);
    forged.operation = ReadOperation::Get;
    assert!(!leaf::claims(&forged));
    assert_eq!(resolver.calls.get(), 0);
    assert_eq!(script.calls(), 0);
    assert_eq!((sink.checks, sink.writes), (0, 0));
}

#[test]
fn canonical_and_facade_deliver_identical_lossless_collection_and_exact_get() {
    let rows = json!([
        {"id":"failed", "state":"Failed", "error":{"message":"provider data"},
         "kernel":{"id":"kernel-1", "execution_state":"future-state", "future":null}},
        {"id":"unknown", "state":{"future":true}, "kernel":null, "unknown":[null,[],{}]}
    ]);
    let common = invocation(&["--limit", "2"]);
    let facade = parse_args([
        "fab-cli-v2",
        "artifact",
        "notebook",
        "list",
        PATH,
        "--limit",
        "2",
    ])
    .unwrap();
    assert_eq!(common, facade);
    let mut receipts = Vec::new();
    let mut deliveries = Vec::new();
    for input in [&common, &facade] {
        let script = Script::json(&rows);
        let resolver = Resolver::default();
        let mut sink = Sink::default();
        let receipt = run(input, &script, &resolver, &mut sink, ReadOptions::default()).unwrap();
        assert_eq!(receipt.data, rows);
        assert_eq!(receipt.status, ReadStatus::ReadComplete);
        assert_eq!(receipt.meta.representation, Representation::NotebookSession);
        assert_eq!(receipt.meta.coverage, ReadCoverage::ReturnedCollection);
        assert_eq!(receipt.meta.continuation, None);
        assert_eq!(receipt.meta.workload_state, None);
        assert_eq!(receipt.meta.workload_observation, None);
        assert_eq!(receipt.target.native_url, ROUTE);
        assert_eq!(receipt.meta.http_status, 200);
        let requests = script.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), NativeMethod::Get);
        assert_eq!(requests[0].effect(), NativeEffect::Read);
        assert_eq!(requests[0].url().as_str(), ROUTE);
        assert_eq!(requests[0].url().query(), None);
        assert!(requests[0].body().is_none());
        assert!(requests[0].bytes().is_none());
        assert_eq!((sink.checks, sink.writes), (1, 1));
        assert_eq!(sink.bytes.last(), Some(&b'\n'));
        assert_eq!(
            serde_json::from_slice::<ReadReceipt>(&sink.bytes).unwrap(),
            receipt
        );
        receipts.push(receipt);
        deliveries.push(sink.bytes);
    }
    assert_eq!(receipts[0], receipts[1]);
    assert_eq!(deliveries[0], deliveries[1]);
}

#[test]
fn empty_and_singleton_collections_remain_arrays() {
    for rows in [json!([]), json!([{"id":"one","unknown":null}])] {
        for body in [rows.clone(), json!({"value":rows,"nextLink":null})] {
            let script = Script::json(&body);
            let receipt = run(
                &invocation(&[]),
                &script,
                &Resolver::default(),
                &mut Sink::default(),
                ReadOptions::default(),
            )
            .unwrap();
            assert_eq!(receipt.data, rows);
            assert_eq!(script.calls(), 1);
        }
    }
}

#[test]
fn malformed_accepted_denied_partial_and_continued_are_not_empty_successes() {
    let mut cases = vec![
        (200, b"not-json".to_vec(), ReadErrorKind::InvalidResponse),
        (200, b"null".to_vec(), ReadErrorKind::InvalidResponse),
        (200, b"{}".to_vec(), ReadErrorKind::InvalidResponse),
        (
            200,
            br#"[{"id":false}]"#.to_vec(),
            ReadErrorKind::InvalidResponse,
        ),
        (
            200,
            br#"[{"id":"one"},{"kernel":null}]"#.to_vec(),
            ReadErrorKind::InvalidResponse,
        ),
        (
            200,
            br#"[{"id":"one","kernel":{}}]"#.to_vec(),
            ReadErrorKind::InvalidResponse,
        ),
        (202, b"[]".to_vec(), ReadErrorKind::InvalidResponse),
        (206, b"[]".to_vec(), ReadErrorKind::InvalidResponse),
        (
            401,
            b"private provider response".to_vec(),
            ReadErrorKind::Http,
        ),
        (
            403,
            b"private provider response".to_vec(),
            ReadErrorKind::Http,
        ),
        (
            503,
            b"private provider response".to_vec(),
            ReadErrorKind::Http,
        ),
    ];
    for field in ["continuationToken", "continuationUri", "nextLink"] {
        for continuation in [json!("more"), json!(false), json!(0), json!({})] {
            let mut body = json!({"value":[]});
            body[field] = continuation;
            cases.push((
                200,
                serde_json::to_vec(&body).unwrap(),
                ReadErrorKind::UnsupportedCapability,
            ));
        }
    }
    for (status, body, expected) in cases {
        let script = Script::reply(status, &body);
        let mut sink = Sink::default();
        let error = run(
            &invocation(&[]),
            &script,
            &Resolver::default(),
            &mut sink,
            ReadOptions::default(),
        )
        .unwrap_err();
        assert_eq!(error.kind, expected);
        assert_eq!(error.http_status, Some(status));
        assert!(!format!("{error} {error:?}").contains("private provider response"));
        assert!(error.receipt.is_none());
        assert_eq!(script.calls(), 1);
        assert_eq!(sink.writes, 0);
        assert!(sink.bytes.is_empty());
    }
}

#[test]
fn unsupported_options_and_facets_fail_before_any_injected_io() {
    for options in [
        vec!["--continuation", "opaque"],
        vec!["--timeout", "1"],
        vec!["--profile", "private"],
        vec!["--project", "id"],
        vec!["--format", "text"],
        vec!["--all"],
        vec!["--limit", "0"],
        vec!["--limit", "2147483648"],
        vec!["--max-bytes", "0"],
        vec!["-o", "file"],
    ] {
        assert!(parse_args(["fab-cli-v2", "list", PATH].into_iter().chain(options)).is_err());
    }
    for path in [
        "/Team.Workspace/Analysis.Notebook/sessions/active",
        "/Team.Workspace/Analysis.Notebook/sessions/livy-history",
        "/Team.Workspace/Analysis.Notebook/src/content",
        "/Team.Workspace/Analysis.Notebook/sessions/exact",
    ] {
        assert!(parse_args(["fab-cli-v2", "list", path]).is_err());
    }
}

#[test]
fn wrong_owner_missing_binding_invalid_input_and_preflight_are_zero_send() {
    for resolver in [
        Resolver {
            unavailable: true,
            ..Resolver::default()
        },
        Resolver {
            wrong_owner: true,
            ..Resolver::default()
        },
    ] {
        let script = Script::json(&json!([]));
        let mut sink = Sink::default();
        let error = run(
            &invocation(&[]),
            &script,
            &resolver,
            &mut sink,
            ReadOptions::default(),
        )
        .unwrap_err();
        assert_eq!(
            error.kind,
            if resolver.wrong_owner {
                ReadErrorKind::WrongOwner
            } else {
                ReadErrorKind::UnsupportedCapability
            }
        );
        assert_eq!(script.calls(), 0);
        assert_eq!((sink.checks, sink.writes), (0, 0));
    }
    for change in 0..4 {
        let script = Script::json(&json!([]));
        let resolver = Resolver::default();
        let mut sink = Sink {
            unavailable: change == 3,
            ..Sink::default()
        };
        let mut input = invocation(&[]);
        match change {
            0 => input.limit = Some(1001),
            1 => input.max_bytes = usize::MAX,
            2 => input.path.owner.artifact = "Different.Lakehouse".into(),
            _ => {}
        }
        let error = run(
            &input,
            &script,
            &resolver,
            &mut sink,
            ReadOptions::default(),
        )
        .unwrap_err();
        assert_eq!(
            error.kind,
            match change {
                0 | 1 => ReadErrorKind::BudgetExceeded,
                2 => ReadErrorKind::InvalidInput,
                _ => ReadErrorKind::OutputUnavailable,
            }
        );
        assert_eq!(resolver.calls.get(), usize::from(change == 3));
        assert_eq!(script.calls(), 0);
        assert_eq!(sink.writes, 0);
    }
}

#[test]
fn whole_page_record_and_exact_data_byte_bounds_never_clip() {
    let rows = json!([{"id":"one","unknown":[null, "value"]},{"id":"two","state":"Failed"}]);
    let bytes = serde_json::to_vec(&rows).unwrap().len();
    for (records, data_bytes, succeeds) in
        [(2, bytes, true), (1, bytes, false), (2, bytes - 1, false)]
    {
        let script = Script::json(&rows);
        let mut input = invocation(&[]);
        input.limit = Some(records);
        input.max_bytes = data_bytes;
        let mut sink = Sink::default();
        let result = run(
            &input,
            &script,
            &Resolver::default(),
            &mut sink,
            ReadOptions::default(),
        );
        if succeeds {
            assert_eq!(result.unwrap().data, rows);
            assert_eq!(sink.writes, 1);
        } else {
            assert_eq!(result.unwrap_err().kind, ReadErrorKind::BudgetExceeded);
            assert_eq!(sink.writes, 0);
            assert!(sink.bytes.is_empty());
        }
        assert_eq!(script.calls(), 1);
        assert_eq!(script.requests.lock().unwrap()[0].url().query(), None);
    }
}

#[test]
fn independent_response_and_receipt_caps_reject_whole_collection() {
    let rows = json!([{"id":"one"}]);
    for options in [
        ReadOptions {
            max_response_bytes: 2,
            ..ReadOptions::default()
        },
        ReadOptions {
            max_receipt_bytes: 2,
            ..ReadOptions::default()
        },
    ] {
        let script = Script::json(&rows);
        let mut sink = Sink::default();
        assert_eq!(
            run(
                &invocation(&[]),
                &script,
                &Resolver::default(),
                &mut sink,
                options,
            )
            .unwrap_err()
            .kind,
            ReadErrorKind::BudgetExceeded
        );
        assert_eq!(script.calls(), 1);
        assert!(script.limits.lock().unwrap()[0] <= options.max_response_bytes);
        assert_eq!(sink.writes, 0);
    }
}

#[test]
fn native_cancel_timeout_and_transport_failure_do_not_deliver_or_replay() {
    for (native, expected) in [
        (NativeError::Cancelled, ReadErrorKind::Cancelled),
        (NativeError::TimedOut, ReadErrorKind::Timeout),
        (NativeError::Transport, ReadErrorKind::Execution),
        (NativeError::Limit, ReadErrorKind::BudgetExceeded),
    ] {
        let script = Script::json(&json!([]));
        *script.response.lock().unwrap() = Some(Err(native));
        let mut sink = Sink::default();
        let error = run(
            &invocation(&[]),
            &script,
            &Resolver::default(),
            &mut sink,
            ReadOptions::default(),
        )
        .unwrap_err();
        assert_eq!(error.kind, expected);
        assert_eq!(error.http_status, None);
        assert_eq!(script.calls(), 1);
        assert_eq!(sink.writes, 0);
    }
}

#[test]
fn partial_delivery_retains_remote_evidence_without_replay() {
    let rows = json!([{"id":"one","state":"Failed"}]);
    let script = Script::json(&rows);
    let mut sink = Sink {
        partial_failure: true,
        ..Sink::default()
    };
    let error = run(
        &invocation(&[]),
        &script,
        &Resolver::default(),
        &mut sink,
        ReadOptions::default(),
    )
    .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::Delivery);
    let receipt = error.receipt.unwrap();
    assert_eq!(receipt.target.native_url, ROUTE);
    assert_eq!(receipt.data, rows);
    assert_eq!(receipt.status, ReadStatus::ReadComplete);
    assert_eq!(script.calls(), 1);
    assert_eq!(sink.writes, 1);
    assert_eq!(sink.bytes.len(), 7);
}
