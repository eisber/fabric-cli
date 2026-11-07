// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline parser -> injected head -> actual native SDK -> single HTTP fake tests.

#![cfg(feature = "runtime-v2")]
#![deny(unnameable_test_items)]

use azure_core::http::Url;
use fabric_cli::runtime_v2::{
    OPERATIONS, OutputSink, OwnerKind, ParsedInvocation, ReadCoverage, ReadEffect, ReadError,
    ReadErrorKind, ReadOperation, ReadOptions, ReadReceipt, ReadStatus, Representation,
    ResolvedTarget, RuntimeHead, RuntimeResolver, TypedPath, WorkloadObservation, parse_args,
};
use fabric_sdk::{
    livy::LivyService,
    notebook_runtime::{NotebookRuntime, content::ContentLimits},
    runtime::{
        NativeEffect, NativeEndpoint, NativeError, NativeExecutor, NativeMethod, NativeRequest,
        NativeResponse,
    },
};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::BTreeMap,
    future::Future,
    io,
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll, Waker},
};

const N: &str = "/Team.Workspace/Analysis.Notebook";
const L: &str = "/Team.Workspace/Data.Lakehouse";
const V: &str = "/Team.Workspace/Native.LivyService";
const NOTEBOOK_BASE: &str = "https://notebook.example.invalid/webapi/capacities/cap/workloads/Notebook/Data/Automatic/api/workspaces/ws/artifacts/nb";

fn endpoint(value: &str) -> NativeEndpoint {
    NativeEndpoint::new(Url::parse(value).unwrap()).unwrap()
}

fn notebook() -> NotebookRuntime {
    NotebookRuntime::new(
        endpoint("https://notebook.example.invalid"),
        "cap",
        "ws",
        "nb",
        ContentLimits::default(),
        1024 * 1024,
        1000,
    )
    .unwrap()
}

struct Resolver {
    kind: OwnerKind,
    fabric: bool,
    calls: Cell<usize>,
}

impl Resolver {
    fn new(kind: OwnerKind) -> Self {
        Self {
            kind,
            fabric: false,
            calls: Cell::new(0),
        }
    }
}

impl RuntimeResolver for Resolver {
    fn resolve(&self, _: &TypedPath) -> Result<ResolvedTarget, NativeError> {
        self.calls.set(self.calls.get() + 1);
        let service = || {
            if self.fabric {
                LivyService::fabric(endpoint("https://fabric.example.invalid"), "ws", "lh").unwrap()
            } else {
                LivyService::native(endpoint("https://livy.example.invalid/base/"))
            }
        };
        Ok(match self.kind {
            OwnerKind::Notebook => ResolvedTarget::Notebook(notebook()),
            OwnerKind::Lakehouse => ResolvedTarget::Lakehouse(service()),
            OwnerKind::LivyService => ResolvedTarget::LivyService(service()),
        })
    }
}

struct HttpFake {
    status: u16,
    body: Vec<u8>,
    media: Option<String>,
    error: Option<NativeError>,
    requests: Mutex<Vec<NativeRequest>>,
    limits: Mutex<Vec<usize>>,
}

impl HttpFake {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            body: serde_json::to_vec(&value).unwrap(),
            media: Some("application/json".into()),
            error: None,
            requests: Mutex::new(Vec::new()),
            limits: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn request(&self) -> NativeRequest {
        self.requests.lock().unwrap()[0].clone()
    }
}

impl NativeExecutor for HttpFake {
    fn execute<'a>(
        &'a self,
        request: &'a NativeRequest,
        max_response_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<NativeResponse, NativeError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(request.method(), NativeMethod::Get);
            assert_eq!(request.effect(), NativeEffect::Read);
            assert!(request.body().is_none());
            assert!(request.bytes().is_none());
            self.requests.lock().unwrap().push(request.clone());
            self.limits.lock().unwrap().push(max_response_bytes);
            if let Some(error) = self.error {
                return Err(error);
            }
            // Deliberately allow an over-budget reply to test the consumer's bound.
            Ok(NativeResponse {
                status: self.status,
                body: self.body.clone(),
                content_type: self.media.clone(),
                headers: BTreeMap::new(),
            })
        })
    }
}

#[derive(Default)]
struct Sink {
    checks: usize,
    writes: usize,
    bytes: Vec<u8>,
    destination: String,
    unavailable: bool,
    fail_write: bool,
}

impl OutputSink for Sink {
    fn check(&mut self, _: &str) -> io::Result<()> {
        self.checks += 1;
        if self.unavailable {
            return Err(io::Error::other("private output path"));
        }
        Ok(())
    }

    fn write(&mut self, destination: &str, bytes: &[u8]) -> io::Result<()> {
        self.writes += 1;
        self.destination = destination.into();
        if self.fail_write {
            self.bytes.extend_from_slice(&bytes[..bytes.len().min(8)]);
            return Err(io::Error::other("private sink error"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

fn complete<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    for _ in 0..32 {
        if let Poll::Ready(value) = future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            return value;
        }
    }
    panic!("single-response offline fake did not complete")
}

fn input(command: &str, path: &str) -> ParsedInvocation {
    parse_args(["fab-cli-v2", command, path]).unwrap()
}

fn run(
    input: &ParsedInvocation,
    http: &HttpFake,
    resolver: &Resolver,
    sink: &mut Sink,
) -> Result<ReadReceipt, ReadError> {
    complete(
        RuntimeHead::inject(http, resolver, ReadOptions::default())
            .unwrap()
            .run(input, sink),
    )
}

fn document() -> Value {
    json!({
        "nbformat": 4, "nbformat_minor": 5,
        "metadata": {"future": [null, false, "你好"]},
        "future_document": {"opaque": 7},
        "cells": [{
            "cell_type": "code", "source": ["print('saved')\n"],
            "metadata": {}, "execution_count": 5,
            "future_cell": {"x": null},
            "outputs": [{
                "output_type": "display_data",
                "data": {"text/plain": ["saved"], "application/json": {"opaque": true}},
                "metadata": {"future_output": []}
            }]
        }]
    })
}

#[test]
fn notebook_content_preserves_unknown_fields_and_saved_outputs() {
    let value = document();
    let http = HttpFake::json(value.clone());
    let resolver = Resolver::new(OwnerKind::Notebook);
    let mut sink = Sink::default();
    let receipt = run(
        &input("get", &format!("{N}/src/content")),
        &http,
        &resolver,
        &mut sink,
    )
    .unwrap();
    assert_eq!(receipt.data, value);
    assert_eq!(receipt.meta.representation, Representation::NotebookSource);
    assert_eq!(receipt.meta.workload_observation, None);
    assert_eq!(receipt.status, ReadStatus::ReadComplete);
    assert_eq!(receipt.effect, ReadEffect::Read);
    assert_eq!(
        receipt.target.native_url,
        format!("{NOTEBOOK_BASE}/content")
    );
    assert_eq!(http.calls(), 1);
    assert_eq!(sink.writes, 1);
    assert_eq!(sink.bytes.last(), Some(&b'\n'));
    let encoded: Value = serde_json::from_slice(&sink.bytes).unwrap();
    assert_eq!(encoded["schemaVersion"], "0.1");
    assert_eq!(encoded["operationId"], "fabric.runtime.get");
    assert_eq!(encoded["status"], "read_complete");
    assert_eq!(
        serde_json::from_slice::<ReadReceipt>(&sink.bytes).unwrap(),
        receipt
    );
}

#[test]
fn canonical_and_notebook_facades_share_input_request_and_receipt() {
    for (command, suffix, value) in [
        ("get", "/src/content", document()),
        (
            "get",
            "/sessions/jupyter",
            json!({"id": "jupyter", "future": {"keep": null}}),
        ),
        ("list", "/sessions", json!([{"id": "jupyter"}])),
    ] {
        let path = format!("{N}{suffix}");
        let canonical = input(command, &path);
        let facade = parse_args(["fab-cli-v2", "artifact", "notebook", command, &path]).unwrap();
        assert_eq!(canonical, facade);
        let resolver = Resolver::new(OwnerKind::Notebook);
        let (http_a, http_b) = (HttpFake::json(value.clone()), HttpFake::json(value));
        let (mut sink_a, mut sink_b) = (Sink::default(), Sink::default());
        let a = run(&canonical, &http_a, &resolver, &mut sink_a).unwrap();
        let b = run(&facade, &http_b, &resolver, &mut sink_b).unwrap();
        assert_eq!(a, b);
        assert_eq!(http_a.request(), http_b.request());
        assert_eq!(sink_a.bytes, sink_b.bytes);
        assert_eq!(http_a.calls(), 1);
        assert_eq!(http_b.calls(), 1);
    }
}

#[test]
fn registry_exposes_only_two_shared_operations_and_json_input_schemas() {
    assert_eq!(OPERATIONS.len(), 2);
    for operation in OPERATIONS {
        let schema = operation.input_schema();
        assert_eq!(schema["$id"], operation.operation.id());
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["properties"]["operationId"]["const"],
            operation.operation.id()
        );
    }
}

#[test]
fn normalized_input_serde_and_explicit_alias_options_share_registered_contract() {
    let path = format!("{N}/sessions");
    let canonical = parse_args([
        "fab-cli-v2",
        "list",
        &path,
        "--format",
        "json",
        "--max-bytes",
        "4096",
        "--limit",
        "3",
        "--output",
        "owned-output.json",
    ])
    .unwrap();
    let facade = parse_args([
        "fab-cli-v2",
        "artifact",
        "notebook",
        "list",
        &path,
        "--format",
        "json",
        "--max-bytes",
        "4096",
        "--limit",
        "3",
        "--output",
        "owned-output.json",
    ])
    .unwrap();
    assert_eq!(canonical, facade);
    let encoded = serde_json::to_value(&canonical).unwrap();
    assert_eq!(encoded["operationId"], OPERATIONS[1].input_schema()["$id"]);
    assert!(encoded.get("operation").is_none());
    assert_eq!(encoded["path"]["owner"]["kind"], "notebook");
    assert_eq!(encoded["path"]["facet"]["kind"], "sessions");
    assert_eq!(encoded["maxBytes"], 4096);
    assert_eq!(encoded["limit"], 3);
    assert_eq!(
        serde_json::from_value::<ParsedInvocation>(encoded).unwrap(),
        canonical
    );
}

#[test]
fn notebook_session_list_is_array_not_public_history() {
    let http = HttpFake::json(json!({"value": [{"id": "j1", "future": "keep"}]}));
    let resolver = Resolver::new(OwnerKind::Notebook);
    let receipt = run(
        &input("list", &format!("{N}/sessions")),
        &http,
        &resolver,
        &mut Sink::default(),
    )
    .unwrap();
    assert_eq!(receipt.data, json!([{"id": "j1", "future": "keep"}]));
    assert_eq!(receipt.meta.coverage, ReadCoverage::ReturnedCollection);
    assert_eq!(
        http.request().url().as_str(),
        format!("{NOTEBOOK_BASE}/jupyterApi/versions/1/api/sessions")
    );
    assert!(parse_args(["fab-cli-v2", "list", &format!("{N}/sessions/livy-history")]).is_err());
}

#[test]
fn empty_and_singleton_lists_are_never_unwrapped() {
    for value in [json!([]), json!([{"id": "one"}])] {
        let http = HttpFake::json(value.clone());
        let resolver = Resolver::new(OwnerKind::Notebook);
        let receipt = run(
            &input("list", &format!("{N}/sessions")),
            &http,
            &resolver,
            &mut Sink::default(),
        )
        .unwrap();
        assert_eq!(receipt.data, value);
        assert_eq!(http.calls(), 1);
    }
}

#[test]
fn notebook_session_get_keeps_raw_unknown_or_failed_state() {
    let value = json!({"id": "j1", "state": "Failed", "kernel": {"id": "k1", "execution_state": "future"}, "error": {"x": 1}});
    let http = HttpFake::json(value.clone());
    let resolver = Resolver::new(OwnerKind::Notebook);
    let receipt = run(
        &input("get", &format!("{N}/sessions/j1")),
        &http,
        &resolver,
        &mut Sink::default(),
    )
    .unwrap();
    assert_eq!(receipt.data, value);
    assert_eq!(receipt.meta.workload_state, Some(json!("Failed")));
    assert_eq!(
        receipt.meta.workload_observation,
        Some(WorkloadObservation::Unknown)
    );
    assert_eq!(receipt.status, ReadStatus::ReadComplete);
    assert!(http.request().url().query().is_none());
}

#[test]
fn native_livy_list_maps_limit_to_size_and_keeps_provider_page_coverage() {
    let http =
        HttpFake::json(json!({"sessions": [{"id": 0, "state": "idle", "future": 1}], "total": 9}));
    let resolver = Resolver::new(OwnerKind::LivyService);
    let invocation = parse_args([
        "fab-cli-v2",
        "list",
        &format!("{V}/sessions"),
        "--limit",
        "3",
    ])
    .unwrap();
    let receipt = run(&invocation, &http, &resolver, &mut Sink::default()).unwrap();
    assert_eq!(
        receipt.data,
        json!([{"id": 0, "state": "idle", "future": 1}])
    );
    assert_eq!(receipt.meta.provider_total, Some(9));
    assert_eq!(receipt.meta.coverage, ReadCoverage::ProviderPage);
    assert_eq!(receipt.meta.continuation, None);
    assert_eq!(
        http.request().url().as_str(),
        "https://livy.example.invalid/base/sessions?size=3"
    );
    assert_eq!(
        http.request()
            .headers()
            .get("x-requested-by")
            .map(String::as_str),
        Some("fabric-sdk-rust-livy")
    );
}

#[test]
fn fabric_livy_list_uses_top_not_native_size() {
    let http = HttpFake::json(json!({"items": [{"id": "0", "state": "starting"}]}));
    let resolver = Resolver {
        fabric: true,
        ..Resolver::new(OwnerKind::Lakehouse)
    };
    let invocation = parse_args([
        "fab-cli-v2",
        "list",
        &format!("{L}/sessions"),
        "--limit",
        "2",
    ])
    .unwrap();
    run(&invocation, &http, &resolver, &mut Sink::default()).unwrap();
    let request = http.request();
    assert_eq!(
        request.url().path(),
        "/v1/workspaces/ws/lakehouses/lh/livyApi/versions/2024-04-01/sessions"
    );
    assert_eq!(request.url().query_pairs().count(), 1);
    assert_eq!(
        request
            .url()
            .query_pairs()
            .find(|(name, _)| name == "$top")
            .map(|(_, value)| value.into_owned()),
        Some("2".into())
    );
    assert!(!request.headers().contains_key("x-requested-by"));
}

#[test]
fn failed_livy_session_is_a_completed_read_not_a_failed_command() {
    let value =
        json!({"id": 0, "state": "error", "errorInfo": [{"message": "synthetic"}], "future": null});
    let http = HttpFake::json(value.clone());
    let resolver = Resolver::new(OwnerKind::Lakehouse);
    let receipt = run(
        &input("get", &format!("{L}/sessions/0")),
        &http,
        &resolver,
        &mut Sink::default(),
    )
    .unwrap();
    assert_eq!(receipt.data, value);
    assert_eq!(receipt.status, ReadStatus::ReadComplete);
    assert_eq!(receipt.meta.workload_state, Some(json!("error")));
    assert_eq!(
        receipt.meta.workload_observation,
        Some(WorkloadObservation::Failed)
    );
}

#[test]
fn statement_result_is_existing_json_and_preserves_output_error_and_byte_cursor() {
    let value = json!({"id": 2, "state": "available", "next": -2, "output": {
        "status": "error", "ename": "synthetic", "traceback": ["private"], "data": {"future": [1]}
    }});
    let http = HttpFake::json(value.clone());
    let resolver = Resolver::new(OwnerKind::Lakehouse);
    let receipt = run(
        &input("get", &format!("{L}/sessions/0/statements/2/result")),
        &http,
        &resolver,
        &mut Sink::default(),
    )
    .unwrap();
    assert_eq!(receipt.data, value);
    assert_eq!(receipt.meta.representation, Representation::LivyStatement);
    assert_eq!(
        receipt.meta.workload_observation,
        Some(WorkloadObservation::Failed)
    );
    assert_eq!(receipt.meta.workload_state, Some(json!("available")));
    assert_eq!(receipt.meta.coverage, ReadCoverage::Resource);
    assert_eq!(
        http.request().url().as_str(),
        "https://livy.example.invalid/base/sessions/0/statements/2"
    );
    assert_eq!(http.calls(), 1);
}

#[test]
fn unknown_livy_state_is_retained_without_claiming_success() {
    let http = HttpFake::json(json!({"id": "s1", "state": "future-state", "output": null}));
    let resolver = Resolver::new(OwnerKind::LivyService);
    let receipt = run(
        &input("get", &format!("{V}/sessions/s1")),
        &http,
        &resolver,
        &mut Sink::default(),
    )
    .unwrap();
    assert_eq!(receipt.meta.workload_state, Some(json!("future-state")));
    assert_eq!(
        receipt.meta.workload_observation,
        Some(WorkloadObservation::Unknown)
    );
}

#[test]
fn accepted_get_is_not_read_complete_or_workload_success() {
    let mut http = HttpFake::json(Value::Null);
    http.status = 202;
    http.body.clear();
    let resolver = Resolver::new(OwnerKind::Lakehouse);
    let receipt = run(
        &input("get", &format!("{L}/sessions/0")),
        &http,
        &resolver,
        &mut Sink::default(),
    )
    .unwrap();
    assert_eq!(receipt.status, ReadStatus::Accepted);
    assert_eq!(receipt.meta.http_status, 202);
    assert_eq!(receipt.data, Value::Null);
    assert_eq!(receipt.meta.coverage, ReadCoverage::Accepted);
    assert_eq!(receipt.meta.workload_observation, None);
    assert_eq!(http.calls(), 1);
}

#[test]
fn root_metadata_explicitly_fails_without_content_or_resolver_io() {
    let http = HttpFake::json(document());
    let resolver = Resolver::new(OwnerKind::Notebook);
    let mut sink = Sink::default();
    let error = run(&input("get", N), &http, &resolver, &mut sink).unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::UnsupportedCapability);
    assert_eq!(resolver.calls.get(), 0);
    assert_eq!(http.calls(), 0);
    assert_eq!(sink.checks, 0);
}

#[test]
fn wrong_owner_facade_and_wrong_resolver_binding_fail_before_http() {
    let error = parse_args([
        "fab-cli-v2",
        "artifact",
        "notebook",
        "get",
        &format!("{L}/sessions/0"),
    ])
    .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::WrongOwner);
    let http = HttpFake::json(json!({"id": "j1"}));
    let resolver = Resolver::new(OwnerKind::Lakehouse);
    let mut sink = Sink::default();
    let error = run(
        &input("get", &format!("{N}/sessions/j1")),
        &http,
        &resolver,
        &mut sink,
    )
    .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::WrongOwner);
    assert_eq!(http.calls(), 0);
    assert_eq!(sink.checks, 0);
}

#[test]
fn native_service_path_cannot_bind_fabric_profile() {
    let http = HttpFake::json(json!({"id": 0}));
    let resolver = Resolver {
        fabric: true,
        ..Resolver::new(OwnerKind::LivyService)
    };
    let error = run(
        &input("get", &format!("{V}/sessions/0")),
        &http,
        &resolver,
        &mut Sink::default(),
    )
    .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::WrongOwner);
    assert_eq!(http.calls(), 0);
}

#[test]
fn native_sdk_rejects_profile_specific_bad_id_before_output_or_http() {
    let http = HttpFake::json(json!({"id": "bad id"}));
    let resolver = Resolver::new(OwnerKind::LivyService);
    let mut sink = Sink::default();
    let invocation = input("get", &format!("{V}/sessions/bad id"));
    let error = run(&invocation, &http, &resolver, &mut sink).unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::InvalidInput);
    assert_eq!(resolver.calls.get(), 1);
    assert_eq!(http.calls(), 0);
    assert_eq!(sink.checks, 0);
}

#[test]
fn bad_paths_and_unadvertised_facets_are_rejected_without_io() {
    for path in [
        "",
        "relative",
        "/",
        "/x",
        "/x.Workspace/.Notebook/src/content",
        "/x.Workspace/n.Notebook//src/content",
        "/x.Workspace/../src/content",
        "/x.Workspace/n.Notebook/src/%63ontent",
        "/x.Workspace/n.Notebook/sessions/a?b",
        "/x.Workspace/n.Notebook/sessions/a#b",
        "/x.Workspace/n.Notebook/sessions/a\\b",
        "/x.Workspace/n.Notebook/sessions/\n",
        "/x.Workspace/n.Notebook/src/content/",
        "/x.Workspace/n.Notebook/src/definition",
        "/x.Workspace/n.Notebook/docs",
        "/x.Workspace/n.Notebook/sessions/active",
        "/x.Workspace/n.Notebook/sessions/livy-history",
        "/x.Workspace/l.Lakehouse/sessions/0/statements/-1",
        "/x.Workspace/l.Lakehouse/sessions/0/statements/9223372036854775808",
        "/x.Workspace/n.Notebook/sessions/0/statements/2",
    ] {
        assert!(parse_args(["fab-cli-v2", "get", path]).is_err(), "{path:?}");
    }
}

#[test]
fn unsupported_flags_fail_before_resolver_or_executor() {
    for flag in [
        vec!["--wait"],
        vec!["--recursive"],
        vec!["--depth", "1"],
        vec!["--timeout", "30s"],
        vec!["--profile", "profile"],
        vec!["--project", "x"],
        vec!["--format", "raw"],
        vec!["--format", "markdown"],
    ] {
        let path = format!("{N}/src/content");
        let mut argv = vec!["fab-cli-v2", "get", &path];
        argv.extend(flag);
        assert_eq!(
            parse_args(argv).unwrap_err().kind,
            ReadErrorKind::UnsupportedCapability
        );
    }
    assert_eq!(
        parse_args([
            "fab-cli-v2",
            "list",
            &format!("{L}/sessions"),
            "--continuation",
            "opaque"
        ])
        .unwrap_err()
        .kind,
        ReadErrorKind::UnsupportedCapability
    );
    for value in ["0", "-1", "1.5", "184467440737095516160"] {
        assert!(
            parse_args([
                "fab-cli-v2",
                "get",
                &format!("{N}/src/content"),
                "--max-bytes",
                value
            ])
            .is_err()
        );
    }
}

#[test]
fn typed_deserialized_input_cannot_bypass_path_or_operation_validation() {
    let mut invocation = input("get", &format!("{N}/sessions/j1"));
    invocation.path.owner.kind = OwnerKind::Lakehouse;
    let http = HttpFake::json(json!({"id": "j1"}));
    let resolver = Resolver::new(OwnerKind::Notebook);
    assert_eq!(
        run(&invocation, &http, &resolver, &mut Sink::default())
            .unwrap_err()
            .kind,
        ReadErrorKind::InvalidInput
    );
    assert_eq!(resolver.calls.get(), 0);
    invocation = input("get", &format!("{N}/sessions/j1"));
    invocation.operation = ReadOperation::List;
    assert!(run(&invocation, &http, &resolver, &mut Sink::default()).is_err());
    assert_eq!(http.calls(), 0);
}

#[test]
fn wrong_response_id_is_not_observation_of_requested_session_or_statement() {
    for (path, value) in [
        (format!("{N}/sessions/j1"), json!({"id": "j2"})),
        (format!("{L}/sessions/0"), json!({"id": 1, "state": "idle"})),
        (
            format!("{L}/sessions/0/statements/2"),
            json!({"id": 3, "state": "available"}),
        ),
    ] {
        let http = HttpFake::json(value);
        let resolver = Resolver::new(if path.contains(".Notebook") {
            OwnerKind::Notebook
        } else {
            OwnerKind::Lakehouse
        });
        let mut sink = Sink::default();
        let error = run(&input("get", &path), &http, &resolver, &mut sink).unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::InvalidResponse);
        assert_eq!(http.calls(), 1);
        assert_eq!(sink.writes, 0);
    }
}

#[test]
fn http_and_executor_failures_keep_real_status_without_success_output_or_replay() {
    for native_error in [
        None,
        Some(NativeError::TimedOut),
        Some(NativeError::Cancelled),
    ] {
        let mut http = HttpFake::json(json!({"provider_secret": "not a diagnostic"}));
        http.status = 403;
        http.error = native_error;
        let resolver = Resolver::new(OwnerKind::Notebook);
        let mut sink = Sink::default();
        let error = run(
            &input("get", &format!("{N}/src/content")),
            &http,
            &resolver,
            &mut sink,
        )
        .unwrap_err();
        assert_eq!(
            error.kind,
            match native_error {
                None => ReadErrorKind::Http,
                Some(NativeError::TimedOut) => ReadErrorKind::Timeout,
                _ => ReadErrorKind::Cancelled,
            }
        );
        assert_eq!(
            error.http_status,
            if native_error.is_none() {
                Some(403)
            } else {
                None
            }
        );
        assert!(!format!("{error:?} {error}").contains("provider_secret"));
        assert!(error.receipt.is_none());
        assert_eq!(http.calls(), 1);
        assert_eq!(sink.writes, 0);
    }
}

#[test]
fn malformed_models_and_paged_notebook_collections_are_not_empty_successes() {
    for (path, command, value, kind) in [
        (
            format!("{N}/src/content"),
            "get",
            json!({"cells": []}),
            ReadErrorKind::InvalidResponse,
        ),
        (
            format!("{N}/sessions"),
            "list",
            json!({"value": [], "continuationToken": "more"}),
            ReadErrorKind::UnsupportedCapability,
        ),
        (
            format!("{N}/sessions"),
            "list",
            json!([{"id": false}]),
            ReadErrorKind::InvalidResponse,
        ),
        (
            format!("{L}/sessions"),
            "list",
            json!({"sessions": [{} , {"id": 1, "state": 1}]}),
            ReadErrorKind::InvalidResponse,
        ),
    ] {
        let resolver = Resolver::new(if path.contains(".Notebook") {
            OwnerKind::Notebook
        } else {
            OwnerKind::Lakehouse
        });
        let http = HttpFake::json(value);
        let mut sink = Sink::default();
        assert_eq!(
            run(&input(command, &path), &http, &resolver, &mut sink)
                .unwrap_err()
                .kind,
            kind
        );
        assert_eq!(http.calls(), 1);
        assert_eq!(sink.writes, 0);
    }
}

#[test]
fn serialized_data_budget_is_exact_and_never_clips_source() {
    let value = document();
    let bytes = serde_json::to_vec(&value).unwrap().len();
    for (max, succeeds) in [(bytes, true), (bytes - 1, false)] {
        let http = HttpFake::json(value.clone());
        let resolver = Resolver::new(OwnerKind::Notebook);
        let mut invocation = input("get", &format!("{N}/src/content"));
        invocation.max_bytes = max;
        let mut sink = Sink::default();
        let result = run(&invocation, &http, &resolver, &mut sink);
        if succeeds {
            assert_eq!(result.unwrap().data, value);
        } else {
            assert_eq!(result.unwrap_err().kind, ReadErrorKind::BudgetExceeded);
            assert_eq!(sink.writes, 0);
        }
        assert_eq!(http.calls(), 1);
    }
}

#[test]
fn response_and_receipt_budgets_are_independent_and_finite() {
    for options in [
        ReadOptions {
            max_response_bytes: 8,
            ..ReadOptions::default()
        },
        ReadOptions {
            max_receipt_bytes: 8,
            ..ReadOptions::default()
        },
    ] {
        let http = HttpFake::json(document());
        let resolver = Resolver::new(OwnerKind::Notebook);
        let mut sink = Sink::default();
        let invocation = input("get", &format!("{N}/src/content"));
        let error = complete(
            RuntimeHead::inject(&http, &resolver, options)
                .unwrap()
                .run(&invocation, &mut sink),
        )
        .unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::BudgetExceeded);
        assert_eq!(http.calls(), 1);
        assert_eq!(sink.writes, 0);
        assert!(http.limits.lock().unwrap()[0] <= options.max_response_bytes);
    }
}

#[test]
fn record_limit_is_all_or_error_without_skipping_rows_or_inventing_resume() {
    let http = HttpFake::json(json!([{"id": "j1"}, {"id": "j2"}]));
    let resolver = Resolver::new(OwnerKind::Notebook);
    let invocation = parse_args([
        "fab-cli-v2",
        "list",
        &format!("{N}/sessions"),
        "--limit",
        "1",
    ])
    .unwrap();
    let mut sink = Sink::default();
    let error = run(&invocation, &http, &resolver, &mut sink).unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::BudgetExceeded);
    assert_eq!(http.calls(), 1);
    assert_eq!(sink.writes, 0);
}

#[test]
fn host_ceiling_and_invalid_options_fail_without_io() {
    let http = HttpFake::json(document());
    let resolver = Resolver::new(OwnerKind::Notebook);
    let mut invocation = input("get", &format!("{N}/src/content"));
    invocation.max_bytes = usize::MAX;
    assert_eq!(
        run(&invocation, &http, &resolver, &mut Sink::default())
            .unwrap_err()
            .kind,
        ReadErrorKind::BudgetExceeded
    );
    assert!(
        RuntimeHead::inject(
            &http,
            &resolver,
            ReadOptions {
                max_response_bytes: 0,
                ..ReadOptions::default()
            }
        )
        .is_err()
    );
    assert_eq!(resolver.calls.get(), 0);
    assert_eq!(http.calls(), 0);
}

#[test]
fn explicit_file_destination_is_delivered_only_through_the_callers_sink() {
    let http = HttpFake::json(document());
    let resolver = Resolver::new(OwnerKind::Notebook);
    let invocation = parse_args([
        "fab-cli-v2",
        "get",
        &format!("{N}/src/content"),
        "--format",
        "json",
        "-o",
        "owned-output.json",
    ])
    .unwrap();
    let mut sink = Sink::default();
    run(&invocation, &http, &resolver, &mut sink).unwrap();
    assert_eq!(sink.destination, "owned-output.json");
    assert_eq!(sink.checks, 1);
    assert_eq!(sink.writes, 1);
    assert_eq!(http.calls(), 1);
}

#[test]
fn unavailable_output_preflight_prevents_remote_dispatch() {
    let http = HttpFake::json(document());
    let resolver = Resolver::new(OwnerKind::Notebook);
    let mut sink = Sink {
        unavailable: true,
        ..Sink::default()
    };
    let error = run(
        &input("get", &format!("{N}/src/content")),
        &http,
        &resolver,
        &mut sink,
    )
    .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::OutputUnavailable);
    assert_eq!(http.calls(), 0);
    assert_eq!(sink.writes, 0);
}

#[test]
fn delivery_failure_retains_completed_read_and_never_replays_http_or_sink() {
    let value = document();
    let http = HttpFake::json(value.clone());
    let resolver = Resolver::new(OwnerKind::Notebook);
    let mut sink = Sink {
        fail_write: true,
        ..Sink::default()
    };
    let error = run(
        &input("get", &format!("{N}/src/content")),
        &http,
        &resolver,
        &mut sink,
    )
    .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::Delivery);
    assert!(!format!("{error:?} {error}").contains("private"));
    let receipt = error.receipt.unwrap();
    assert_eq!(receipt.status, ReadStatus::ReadComplete);
    assert_eq!(receipt.data, value);
    assert_eq!(receipt.meta.http_status, 200);
    assert_eq!(http.calls(), 1);
    assert_eq!(sink.writes, 1);
    assert!(!sink.bytes.is_empty());
}
