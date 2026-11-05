// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Final public-facade tests; the integration owner must export `fabric_sdk::livy`.
#![deny(unnameable_test_items)]

use fabric_sdk::{
    livy::*,
    runtime::{
        NativeEffect, NativeEndpoint, NativeError, NativeExecutor, NativeMethod, NativeRequest,
        NativeResponse,
    },
};
use serde_json::{Value, json};
use std::{collections::VecDeque, future::Future, pin::Pin, sync::Mutex};

struct Synthetic {
    replies: Mutex<VecDeque<Result<NativeResponse, NativeError>>>,
    calls: Mutex<Vec<(NativeRequest, usize)>>,
}

impl Synthetic {
    fn new(replies: Vec<Result<NativeResponse, NativeError>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn json(value: Value) -> Self {
        Self::new(vec![Ok(response(
            200,
            &serde_json::to_vec(&value).unwrap(),
            Some("application/json"),
        ))])
    }

    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl NativeExecutor for Synthetic {
    fn execute<'a>(
        &'a self,
        request: &'a NativeRequest,
        max_response_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<NativeResponse, NativeError>> + Send + 'a>> {
        self.calls
            .lock()
            .unwrap()
            .push((request.clone(), max_response_bytes));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected dispatch or replay");
        Box::pin(async move { reply })
    }
}

fn response(status: u16, bytes: &[u8], content_type: Option<&str>) -> NativeResponse {
    NativeResponse {
        status,
        body: bytes.to_vec(),
        content_type: content_type.map(str::to_owned),
        headers: Default::default(),
    }
}

fn endpoint() -> NativeEndpoint {
    NativeEndpoint::new("https://livy.example.test/proxy".parse().unwrap()).unwrap()
}

fn native() -> LivyService {
    LivyService::native(endpoint())
}

fn fabric() -> LivyService {
    LivyService::fabric(endpoint(), "workspace", "lakehouse").unwrap()
}

fn query<T: ResponseModel>(request: &LivyRequest<T>) -> Vec<(String, String)> {
    request
        .native()
        .url()
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

async fn exercise<T: ResponseModel>(
    request: LivyRequest<T>,
    method: NativeMethod,
    path: &str,
    effect: NativeEffect,
    seen: &mut Vec<RouteContract>,
) {
    assert_eq!(request.native().method(), method);
    assert_eq!(request.native().url().path(), path);
    assert_eq!(request.native().effect(), effect);
    assert_eq!(request.native().accept(), "application/json");
    assert_eq!(
        request
            .native()
            .headers()
            .get("accept-encoding")
            .map(String::as_str),
        Some("identity")
    );
    let requested_by = request
        .native()
        .headers()
        .get("x-requested-by")
        .map(String::as_str);
    if request.profile() == ServiceProfile::NativeWildfire {
        assert_eq!(requested_by, Some("fabric-sdk-rust-livy"));
    } else {
        assert!(requested_by.is_none());
    }
    if request.contract().encoding == RequestEncoding::Multipart {
        assert!(
            request
                .native()
                .content_type()
                .starts_with("multipart/form-data; boundary=")
        );
        assert!(request.native().bytes().is_some());
        assert!(request.native().body().is_none());
    } else {
        assert_eq!(request.native().content_type(), "application/json");
        assert!(request.native().bytes().is_none());
    }
    assert_eq!(
        request.contract().maturity(),
        Maturity::SourceModeledDeploymentUnverified
    );
    let executor = Synthetic::json(json!({"state": "idle", "opaque": {"preserved": true}}));
    let reply = request.execute(&executor, 4096).await.unwrap();
    assert_eq!(reply.status(), 200);
    assert_eq!(reply.disposition(), ResponseDisposition::Returned);
    assert!(reply.model().is_some());
    assert_eq!(
        reply.maturity(),
        Maturity::SourceModeledDeploymentUnverified
    );
    assert_eq!(executor.count(), 1);
    assert_eq!(executor.calls.lock().unwrap()[0].1, 4096);
    seen.push(*request.contract());
}

fn assert_matrix(seen: &[RouteContract], expected: &[RouteContract]) {
    assert_eq!(seen.len(), expected.len());
    for contract in expected {
        assert_eq!(
            seen.iter()
                .filter(|candidate| *candidate == contract)
                .count(),
            1,
            "{contract:?}"
        );
    }
}

#[tokio::test]
async fn all_49_native_routes_dispatch_json_or_explicit_multipart_bytes() {
    let service = native();
    let session = service.session("s").unwrap();
    let repl = session.repl("r").unwrap();
    let statements = session.statements();
    let statement = statements.statement(7).unwrap();
    let repl_statements = repl.statements();
    let repl_statement = repl_statements.statement(8).unwrap();
    let batch = service.batch("b").unwrap();
    let job = session.job(9).unwrap();
    let config = Configuration::default();
    let input = StatementInput::new("select 1").unwrap();
    let timeout = TimeoutInput::session(123).unwrap();
    let mut seen = Vec::new();
    macro_rules! call {
        ($request:expr, $method:ident, $path:literal, $effect:ident) => {
            exercise(
                $request.unwrap(),
                NativeMethod::$method,
                concat!("/proxy", $path),
                NativeEffect::$effect,
                &mut seen,
            )
            .await
        };
    }
    call!(service.version(), Get, "/version", Read);
    call!(
        service.list_sessions(ActivityPage::default()),
        Get,
        "/sessions",
        Read
    );
    call!(service.create_session(&config), Post, "/sessions", Create);
    call!(session.get(), Get, "/sessions/s", Read);
    call!(session.delete(), Delete, "/sessions/s", Delete);
    call!(session.get_state(), Get, "/sessions/s/state", Read);
    call!(
        session.logs(BytePage::default()),
        Get,
        "/sessions/s/log",
        Read
    );
    call!(
        session.reset_timeout(),
        Put,
        "/sessions/s/reset-timeout",
        Lifecycle
    );
    call!(session.stop(), Post, "/sessions/s/stop", Lifecycle);
    call!(
        session.interrupt(),
        Post,
        "/sessions/s/interrupt",
        Lifecycle
    );
    call!(session.connect(), Post, "/sessions/s/connect", Lifecycle);
    call!(session.get_timeout(), Get, "/sessions/s/timeout", Read);
    call!(
        session.set_timeout(&timeout),
        Put,
        "/sessions/s/timeout",
        Update
    );
    call!(
        session.completion(&CompletionInput::new("x", 1).unwrap()),
        Post,
        "/sessions/s/completion",
        Read
    );
    call!(
        session.inspect(&InspectionInput::default()),
        Post,
        "/sessions/s/inspect",
        Read
    );
    call!(
        session.debug(&DebugInput::new(json!({"content": {"command": "threads"}})).unwrap()),
        Post,
        "/sessions/s/debug",
        Execute
    );
    for (kind, add, upload, field) in [
        (ResourceKind::Jar, "/add-jar", "/upload-jar", "jar"),
        (
            ResourceKind::PyFile,
            "/add-pyfile",
            "/upload-pyfile",
            "file",
        ),
        (ResourceKind::File, "/add-file", "/upload-file", "file"),
    ] {
        exercise(
            session
                .add_resource(kind, &ResourceUri::new("hdfs:///resource").unwrap())
                .unwrap(),
            NativeMethod::Post,
            &format!("/proxy/sessions/s{add}"),
            NativeEffect::Update,
            &mut seen,
        )
        .await;
        let descriptor = session.upload_descriptor(kind, "a file.bin", 512).unwrap();
        assert_eq!(
            descriptor.url().path(),
            format!("/proxy/sessions/s{upload}")
        );
        assert_eq!(descriptor.contract().method, NativeMethod::Post);
        assert_eq!(descriptor.contract().encoding, RequestEncoding::Multipart);
        assert_eq!(
            descriptor.contract().maturity(),
            Maturity::SourceModeledDeploymentUnverified
        );
        assert_eq!(descriptor.part_name(), field);
        assert_eq!(descriptor.filename(), "a file.bin");
        assert_eq!(descriptor.content_length(), 512);
        assert_eq!(descriptor.content_type(), "application/octet-stream");
        assert_eq!(
            descriptor.clone().into_json_request().unwrap_err(),
            NativeError::UnsupportedCapability
        );
        exercise(
            descriptor
                .into_multipart_request(vec![b'x'; 512], "livy-test-boundary", 2048)
                .unwrap(),
            NativeMethod::Post,
            &format!("/proxy/sessions/s{upload}"),
            NativeEffect::Update,
            &mut seen,
        )
        .await;
    }
    let serialized = SerializedJob::new(b"\x00\xffopaque", "spark").unwrap();
    call!(
        session.submit_job(&serialized),
        Post,
        "/sessions/s/submit-job",
        Execute
    );
    call!(
        session.run_job(&serialized),
        Post,
        "/sessions/s/run-job",
        Execute
    );
    call!(job.get(), Get, "/sessions/s/jobs/9", Read);
    call!(job.cancel(), Post, "/sessions/s/jobs/9/cancel", Cancel);
    call!(
        statements.list(StatementListOptions::default()),
        Get,
        "/sessions/s/statements",
        Read
    );
    call!(
        statements.submit(&input),
        Post,
        "/sessions/s/statements",
        Execute
    );
    call!(
        statement.get(BytePage::default()),
        Get,
        "/sessions/s/statements/7",
        Read
    );
    call!(
        statement.cancel(),
        Post,
        "/sessions/s/statements/7/cancel",
        Cancel
    );
    call!(
        session.list_repls(&ReplListOptions::default()),
        Get,
        "/sessions/s/repls",
        Read
    );
    call!(repl.create(&config), Put, "/sessions/s/repls/r", Create);
    call!(repl.get(), Get, "/sessions/s/repls/r", Read);
    call!(repl.delete(), Delete, "/sessions/s/repls/r", Delete);
    call!(repl.stop(), Post, "/sessions/s/repls/r/stop", Lifecycle);
    call!(
        repl.reset_timeout(),
        Put,
        "/sessions/s/repls/r/reset-timeout",
        Lifecycle
    );
    call!(repl.get_timeout(), Get, "/sessions/s/repls/r/timeout", Read);
    call!(
        repl.set_timeout(&timeout),
        Put,
        "/sessions/s/repls/r/timeout",
        Update
    );
    call!(
        repl_statements.list(StatementListOptions::default()),
        Get,
        "/sessions/s/repls/r/statements",
        Read
    );
    call!(
        repl_statements.submit(&input),
        Post,
        "/sessions/s/repls/r/statements",
        Execute
    );
    call!(
        repl_statement.get(BytePage::default()),
        Get,
        "/sessions/s/repls/r/statements/8",
        Read
    );
    call!(
        repl_statement.cancel(),
        Post,
        "/sessions/s/repls/r/statements/8/cancel",
        Cancel
    );
    call!(
        service.list_batches(ActivityPage::default()),
        Get,
        "/batches",
        Read
    );
    call!(service.create_batch(&config), Post, "/batches", Execute);
    call!(batch.get(), Get, "/batches/b", Read);
    call!(batch.cancel(), Delete, "/batches/b", Cancel);
    call!(batch.get_state(), Get, "/batches/b/state", Read);
    call!(batch.logs(BytePage::default()), Get, "/batches/b/log", Read);
    call!(
        batch.reset_timeout(),
        Put,
        "/batches/b/reset-timeout",
        Lifecycle
    );
    assert_eq!(NATIVE_ROUTES.len(), 49);
    assert_matrix(&seen, NATIVE_ROUTES);
}

#[tokio::test]
async fn all_20_fabric_modeled_routes_preserve_management_and_execution_owners() {
    let service = fabric();
    let session = service.session("classic").unwrap();
    let batch = service.batch("b").unwrap();
    let acquisition = service.high_concurrency("acquisition").unwrap();
    let repl = acquisition.repl("backing", "r").unwrap();
    let statements = session.statements();
    let statement = statements.statement(7).unwrap();
    let hc_statements = repl.statements();
    let hc_statement = hc_statements.statement(8).unwrap();
    let input = StatementInput::new("select 1").unwrap();
    let config = Configuration::default();
    let mut seen = Vec::new();
    macro_rules! call {
        ($request:expr, $method:ident, $path:literal, $effect:ident) => {
            exercise(
                $request.unwrap(), NativeMethod::$method,
                concat!("/proxy/v1/workspaces/workspace/lakehouses/lakehouse/livyApi/versions/2024-04-01", $path),
                NativeEffect::$effect, &mut seen,
            ).await
        };
    }
    call!(
        service.list_batches(ActivityPage::default()),
        Get,
        "/batches",
        Read
    );
    call!(service.create_batch(&config), Post, "/batches", Execute);
    call!(batch.get(), Get, "/batches/b", Read);
    call!(batch.cancel(), Delete, "/batches/b", Cancel);
    call!(
        service.acquire_high_concurrency(&config),
        Post,
        "/highConcurrencySessions",
        Create
    );
    call!(
        acquisition.get(),
        Get,
        "/highConcurrencySessions/acquisition",
        Read
    );
    call!(
        acquisition.release(),
        Delete,
        "/highConcurrencySessions/acquisition",
        Delete
    );
    call!(
        hc_statements.list(StatementListOptions::default()),
        Get,
        "/highConcurrencySessions/backing/repls/r/statements",
        Read
    );
    call!(
        hc_statements.submit(&input),
        Post,
        "/highConcurrencySessions/backing/repls/r/statements",
        Execute
    );
    call!(
        hc_statement.get(BytePage::default()),
        Get,
        "/highConcurrencySessions/backing/repls/r/statements/8",
        Read
    );
    call!(
        hc_statement.cancel(),
        Post,
        "/highConcurrencySessions/backing/repls/r/statements/8/cancel",
        Cancel
    );
    call!(
        service.list_sessions(ActivityPage::default()),
        Get,
        "/sessions",
        Read
    );
    call!(service.create_session(&config), Post, "/sessions", Create);
    call!(session.get(), Get, "/sessions/classic", Read);
    call!(session.delete(), Delete, "/sessions/classic", Delete);
    call!(
        session.reset_timeout(),
        Post,
        "/sessions/classic/reset-timeout",
        Lifecycle
    );
    call!(
        statements.list(StatementListOptions::default()),
        Get,
        "/sessions/classic/statements",
        Read
    );
    call!(
        statements.submit(&input),
        Post,
        "/sessions/classic/statements",
        Execute
    );
    call!(
        statement.get(BytePage::default()),
        Get,
        "/sessions/classic/statements/7",
        Read
    );
    call!(
        statement.cancel(),
        Post,
        "/sessions/classic/statements/7/cancel",
        Cancel
    );
    assert_eq!(FABRIC_ROUTES.len(), 20);
    assert_matrix(&seen, FABRIC_ROUTES);
    assert_eq!(acquisition.acquisition_id(), "acquisition");
    assert_eq!(repl.acquisition_id(), Some("acquisition"));
    assert_eq!(repl.backing_session_id(), "backing");
    assert_eq!(repl.repl_id(), "r");
    assert_eq!(
        hc_statement.cancel().unwrap().acquisition_id(),
        Some("acquisition")
    );
}

#[test]
fn payload_defaults_and_exact_native_envelopes() {
    assert_eq!(
        StatementInput::new("select 1").unwrap().as_json(),
        &json!({"code": "select 1", "kind": "sql"})
    );
    assert_eq!(
        StatementInput::from_json(json!({"code": "x"}))
            .unwrap()
            .as_json(),
        &json!({"code": "x", "kind": "sql"})
    );
    assert_eq!(
        CompletionInput::new("😀x", 2).unwrap().as_json(),
        &json!({"kind": "spark", "code": "😀x", "cursor": 2})
    );
    assert_eq!(
        InspectionInput::default().as_json(),
        &json!({"kind": "spark", "vars": []})
    );
    assert_eq!(
        InspectionInput::new(&["a"])
            .unwrap()
            .with_source_id("r")
            .unwrap()
            .with_kind("ipyspark")
            .unwrap()
            .as_json(),
        &json!({"kind": "ipyspark", "sourceId": "r", "vars": ["a"]})
    );
    assert_eq!(
        CompletionInput::new("", 0)
            .unwrap()
            .with_source_id("r")
            .unwrap()
            .as_json(),
        &json!({"kind": "spark", "sourceId": "r", "code": "", "cursor": 0})
    );
    assert_eq!(
        TimeoutInput::session(100).unwrap().as_json(),
        &json!({"sessionTimeout": 100})
    );
    assert_eq!(
        TimeoutInput::repl(None, Some(200)).unwrap().as_json(),
        &json!({"replTimeout": 200})
    );
    assert_eq!(
        TimeoutInput::repl(Some(100), Some(200)).unwrap().as_json(),
        &json!({"sessionTimeout": 100, "replTimeout": 200})
    );
    assert_eq!(
        SerializedJob::new(b"\x00\xff", "pyspark")
            .unwrap()
            .as_json(),
        &json!({"job": "AP8=", "jobType": "pyspark"})
    );
    let debug =
        DebugInput::new(json!({"header": {}, "metadata": {}, "content": {"command": "threads"}}))
            .unwrap();
    assert_eq!(debug.as_json()["msg_type"], "debug_request");
    assert_eq!(
        serde_json::from_str::<Value>(debug.as_json()["content"].as_str().unwrap()).unwrap(),
        json!({"header": {}, "metadata": {}, "content": {"command": "threads"}})
    );
    assert_eq!(
        DebugInput::new(json!("opaque")).unwrap().as_json(),
        &json!({"msg_type": "debug_request", "content": "opaque"})
    );
    let value = json!({"custom": {"nested": [1, null]}, "conf": {"spark.setting": 4}});
    let config = Configuration::new(value.clone()).unwrap();
    assert_eq!(
        native().create_session(&config).unwrap().native().body(),
        Some(&value)
    );
    assert_eq!(
        native().create_batch(&config).unwrap().native().body(),
        Some(&value)
    );
    assert_eq!(
        native()
            .session("s")
            .unwrap()
            .repl("r")
            .unwrap()
            .create(&config)
            .unwrap()
            .native()
            .body(),
        Some(&value)
    );
    let n = native()
        .session("s")
        .unwrap()
        .statements()
        .statement(1)
        .unwrap()
        .cancel()
        .unwrap();
    let f = fabric()
        .session("s")
        .unwrap()
        .statements()
        .statement(1)
        .unwrap()
        .cancel()
        .unwrap();
    assert_eq!(n.native().body(), Some(&json!({})));
    assert_eq!(f.native().body(), Some(&json!({})));
}

#[test]
fn query_parameters_are_profile_exact_and_do_not_route_through_source_id() {
    let native = native();
    let fabric = fabric();
    let native_page = ActivityPage::Native {
        offset: Some(0),
        size: Some(10),
    };
    let fabric_page = ActivityPage::Fabric {
        top: Some(0),
        skip: Some(5),
        count: Some(false),
    };
    assert_eq!(
        query(&native.list_sessions(native_page).unwrap()),
        vec![("from".into(), "0".into()), ("size".into(), "10".into())]
    );
    assert_eq!(
        query(&fabric.list_batches(fabric_page).unwrap()),
        vec![
            ("$top".into(), "0".into()),
            ("$skip".into(), "5".into()),
            ("$count".into(), "false".into())
        ]
    );
    assert!(query(&native.list_batches(ActivityPage::default()).unwrap()).is_empty());
    assert!(native.list_sessions(fabric_page).is_err());
    assert!(fabric.list_sessions(native_page).is_err());
    let session = native.session("s").unwrap();
    let options = ReplListOptions::new(Some(0), Some(2), Some("state eq 'idle' & x=1")).unwrap();
    assert_eq!(
        query(&session.list_repls(&options).unwrap()),
        vec![
            ("from".into(), "0".into()),
            ("take".into(), "2".into()),
            ("filter".into(), "state eq 'idle' & x=1".into())
        ]
    );
    let list = StatementListOptions::new(Some(3), Some(4), Some(5)).unwrap();
    assert_eq!(
        query(&session.statements().list(list).unwrap()),
        vec![
            ("from".into(), "3".into()),
            ("size".into(), "4".into()),
            ("tail".into(), "5".into())
        ]
    );
    assert!(
        fabric
            .session("s")
            .unwrap()
            .statements()
            .list(list)
            .is_err()
    );
    let page = session
        .statements()
        .statement(1)
        .unwrap()
        .get(BytePage::new(Some(17), Some(128)).unwrap())
        .unwrap();
    assert_eq!(
        query(&page),
        vec![("from".into(), "17".into()), ("size".into(), "128".into())]
    );
    assert!(
        session
            .statements()
            .submit(&StatementInput::new("code").unwrap())
            .unwrap()
            .native()
            .body()
            .unwrap()
            .get("sourceId")
            .is_none()
    );
}

#[test]
fn native_extensions_are_rejected_on_fabric_before_any_executor_use() {
    let service = fabric();
    let session = service.session("s").unwrap();
    let repl = service
        .high_concurrency("a")
        .unwrap()
        .repl("backing", "r")
        .unwrap();
    let timeout = TimeoutInput::session(1).unwrap();
    let config = Configuration::default();
    let job = SerializedJob::new(b"bytes", "spark").unwrap();
    macro_rules! unsupported {
        ($expression:expr) => {
            assert_eq!($expression.unwrap_err(), NativeError::UnsupportedCapability);
        };
    }
    unsupported!(service.version());
    unsupported!(session.get_state());
    unsupported!(session.logs(BytePage::default()));
    unsupported!(session.stop());
    unsupported!(session.interrupt());
    unsupported!(session.connect());
    unsupported!(session.get_timeout());
    unsupported!(session.set_timeout(&timeout));
    unsupported!(session.completion(&CompletionInput::new("x", 1).unwrap()));
    unsupported!(session.inspect(&InspectionInput::default()));
    unsupported!(session.debug(&DebugInput::new(json!({})).unwrap()));
    unsupported!(session.list_repls(&ReplListOptions::default()));
    unsupported!(session.repl("r"));
    unsupported!(session.submit_job(&job));
    unsupported!(session.run_job(&job));
    unsupported!(session.job(1));
    for kind in [ResourceKind::Jar, ResourceKind::PyFile, ResourceKind::File] {
        unsupported!(session.add_resource(kind, &ResourceUri::new("hdfs:///x").unwrap()));
        unsupported!(session.upload_descriptor(kind, "x.bin", 1));
    }
    unsupported!(repl.create(&config));
    unsupported!(repl.get());
    unsupported!(repl.delete());
    unsupported!(repl.stop());
    unsupported!(repl.reset_timeout());
    unsupported!(repl.get_timeout());
    unsupported!(repl.set_timeout(&timeout));
    let batch = service.batch("b").unwrap();
    unsupported!(batch.get_state());
    unsupported!(batch.logs(BytePage::default()));
    unsupported!(batch.reset_timeout());
    unsupported!(native().acquire_high_concurrency(&config));
    unsupported!(native().high_concurrency("a"));
}

#[test]
fn identifiers_and_bad_bodies_are_rejected_without_echoing_code() {
    let service = native();
    for id in [
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "%2f",
        "a?b",
        "a#b",
        "x\n",
        "a b",
        "é",
        "https://evil",
    ] {
        assert!(service.session(id).is_err());
        assert!(service.batch(id).is_err());
        assert!(fabric().high_concurrency(id).is_err());
        assert!(service.session("s").unwrap().repl(id).is_err());
        assert!(
            fabric()
                .high_concurrency("a")
                .unwrap()
                .repl(id, "r")
                .is_err()
        );
        assert!(
            CompletionInput::new("", 0)
                .unwrap()
                .with_source_id(id)
                .is_err()
        );
    }
    for id in ["0", "uuid-123", "a_b.c~d"] {
        assert!(service.session(id).is_ok());
    }
    for value in [Value::Null, json!([]), json!("x"), json!(1)] {
        assert!(Configuration::new(value).is_err());
    }
    for value in [
        json!({"code": ""}),
        json!({"code": " "}),
        json!({"code": 1}),
        json!({"code": "secret", "kind": null}),
        json!({"code": "secret", "kind": ""}),
        json!({"code": "secret", "sourceId": "r"}),
        json!({"code": "secret", "parameters": {}}),
    ] {
        let error = StatementInput::from_json(value).unwrap_err();
        assert_eq!(error, NativeError::InvalidInput);
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
    assert!(CompletionInput::new("😀", 2).is_err());
    assert!(InspectionInput::new(&[""]).is_err());
    assert!(InspectionInput::default().with_kind(" ").is_err());
    assert!(TimeoutInput::session(0).is_err());
    assert!(TimeoutInput::session(u64::MAX).is_err());
    assert!(TimeoutInput::repl(None, None).is_err());
    assert!(
        service
            .session("s")
            .unwrap()
            .set_timeout(&TimeoutInput::repl(None, Some(1)).unwrap())
            .is_err()
    );
    assert!(SerializedJob::new(b"", "spark").is_err());
    assert!(SerializedJob::new(b"bytes", "").is_err());
    assert!(ResourceUri::new(" \n").is_err());
    assert!(DebugInput::new(json!([])).is_err());
    assert!(BytePage::new(Some(u64::MAX), None).is_err());
    assert!(BytePage::new(None, Some(u32::MAX)).is_err());
    assert!(ReplListOptions::new(Some(0), None, None).is_err());
    assert!(ReplListOptions::new(None, Some(2), None).is_err());
    assert!(ReplListOptions::new(None, None, Some("bad\nfilter")).is_err());
    assert!(service.session("s").unwrap().job(u64::MAX).is_err());
    assert!(
        service
            .session("s")
            .unwrap()
            .statements()
            .statement(u64::MAX)
            .is_err()
    );
    for filename in ["", ".", "..", "a/b", "a\\b", "bad\r\nname", "a\0b"] {
        assert!(
            service
                .session("s")
                .unwrap()
                .upload_descriptor(ResourceKind::File, filename, 1)
                .is_err()
        );
    }
    assert!(
        fabric()
            .acquire_high_concurrency(&Configuration::new(json!({"conf": []})).unwrap())
            .is_err()
    );
    assert!(
        fabric()
            .acquire_high_concurrency(&Configuration::new(json!({"sessionTag": 1})).unwrap())
            .is_err()
    );
    assert!(LivyService::fabric(endpoint(), "../x", "l").is_err());
    assert!(LivyService::fabric_versioned(endpoint(), "w", "l", "v1", "2024-1-01").is_err());
    assert!(
        LivyService::fabric_versioned(endpoint(), "w", "l", "v1", "2023-12-01-preview").is_ok()
    );
    let code = StatementInput::new("NEVER_ECHO_CODE").unwrap();
    let request = service
        .session("s")
        .unwrap()
        .statements()
        .submit(&code)
        .unwrap();
    assert!(!format!("{code:?} {request:?} {:?}", request.native()).contains("NEVER_ECHO_CODE"));
}

#[tokio::test]
async fn dispatch_once_errors_status_acceptance_and_wire_evidence_are_truthful() {
    let request = native()
        .session("s")
        .unwrap()
        .statements()
        .submit(&StatementInput::new("NEVER_ECHO").unwrap())
        .unwrap();
    for native_error in [
        NativeError::TimedOut,
        NativeError::Cancelled,
        NativeError::OutcomeUnknown,
    ] {
        let executor = Synthetic::new(vec![Err(native_error)]);
        let error = request.execute(&executor, 512).await.unwrap_err();
        assert_eq!(error.native_error(), native_error);
        assert_eq!(error.phase(), FailurePhase::Execution);
        assert!(error.outcome_unknown());
        assert_eq!(executor.count(), 1);
    }
    for status in [400, 404, 405, 408, 429, 500, 501] {
        let executor = Synthetic::new(vec![Ok(response(
            status,
            b"{\"message\":\"NEVER_ECHO\"}",
            Some("application/json"),
        ))]);
        let error = request.execute(&executor, 512).await.unwrap_err();
        assert_eq!(error.native_error(), NativeError::Http(status));
        assert_eq!(error.status(), Some(status));
        assert_eq!(error.phase(), FailurePhase::Http);
        assert!(error.outcome_unknown());
        assert_eq!(executor.count(), 1);
        assert!(!format!("{error:?} {error}").contains("NEVER_ECHO"));
        assert!(
            std::str::from_utf8(error.raw())
                .unwrap()
                .contains("NEVER_ECHO")
        );
    }
    for (bytes, media) in [
        (b"[]".as_slice(), Some("application/json")),
        (b"{}", Some("text/plain")),
        (b"not-json", Some("application/json")),
        (b"{\"state\":1}", Some("application/json")),
        (b"null", Some("application/json")),
    ] {
        let executor = Synthetic::new(vec![Ok(response(200, bytes, media))]);
        let error = request.execute(&executor, 512).await.unwrap_err();
        assert_eq!(error.native_error(), NativeError::InvalidResponse);
        assert_eq!(error.phase(), FailurePhase::Decode);
        assert!(error.outcome_unknown());
    }
    let no_dispatch = Synthetic::new(vec![]);
    let error = request.execute(&no_dispatch, 0).await.unwrap_err();
    assert_eq!(error.phase(), FailurePhase::Validation);
    assert!(!error.outcome_unknown());
    assert_eq!(no_dispatch.count(), 0);
    let too_large = Synthetic::new(vec![Ok(response(200, b"0123456789", None))]);
    let error = request.execute(&too_large, 4).await.unwrap_err();
    assert_eq!(error.native_error(), NativeError::Limit);
    assert_eq!(error.phase(), FailurePhase::ResponseLimit);
    assert!(error.raw().is_empty());
    assert_eq!(error.status(), None);
    assert_eq!(too_large.count(), 1);
    let accepted = Synthetic::new(vec![Ok(response(202, b"", None))]);
    let reply = request.execute(&accepted, 512).await.unwrap();
    assert_eq!(reply.disposition(), ResponseDisposition::Accepted);
    assert!(reply.model().is_none());
    let read = native().session("s").unwrap().get().unwrap();
    let executor = Synthetic::new(vec![Err(NativeError::TimedOut)]);
    assert!(
        !read
            .execute(&executor, 512)
            .await
            .unwrap_err()
            .outcome_unknown()
    );
}

#[tokio::test]
async fn raw_payload_is_immutable_and_state_errors_are_not_http_errors() {
    let statement = native()
        .session("s")
        .unwrap()
        .statements()
        .statement(1)
        .unwrap();
    let payload = json!({
        "id": 1, "state": "available", "code": "NEVER_ECHO",
        "output": {"status": "error", "ename": "Failure", "traceback": ["NEVER_ECHO"], "unknown": [1, null]},
        "extension": {"a": {"b": true}}
    });
    let bytes = serde_json::to_vec_pretty(&payload).unwrap();
    let executor = Synthetic::new(vec![Ok(response(
        200,
        &bytes,
        Some("Application/JSON; charset=utf-8"),
    ))]);
    let reply = statement
        .get(BytePage::default())
        .unwrap()
        .execute(&executor, 4096)
        .await
        .unwrap();
    assert_eq!(reply.raw(), bytes.as_slice());
    let model = reply.model().unwrap();
    assert_eq!(model.raw(), &payload);
    let failure = model.observe().unwrap_err();
    assert_eq!(failure.kind(), WorkloadFailureKind::StatementOutput);
    assert_eq!(failure.raw(), &payload);
    assert!(!format!("{model:?} {reply:?} {failure:?} {failure}").contains("NEVER_ECHO"));
    let mut copy = model.raw().clone();
    copy["extension"] = Value::Null;
    assert_eq!(model.raw(), &payload);
    let service = native();
    let session = service.session("s").unwrap();
    let failed_session = session
        .get()
        .unwrap()
        .execute(&Synthetic::json(json!({"state":"DEAD"})), 512)
        .await
        .unwrap();
    assert_eq!(
        failed_session
            .model()
            .unwrap()
            .observe()
            .unwrap_err()
            .kind(),
        WorkloadFailureKind::Session
    );
    let failed_batch = service
        .batch("b")
        .unwrap()
        .get()
        .unwrap()
        .execute(
            &Synthetic::json(json!({"state":"error", "result":"opaque"})),
            512,
        )
        .await
        .unwrap();
    assert_eq!(
        failed_batch.model().unwrap().observe().unwrap_err().kind(),
        WorkloadFailureKind::Batch
    );
    let failed_job = session
        .job(1)
        .unwrap()
        .get()
        .unwrap()
        .execute(
            &Synthetic::json(
                json!({"state":"SUCCEEDED", "error":"application failure", "result":"AP8="}),
            ),
            512,
        )
        .await
        .unwrap();
    assert_eq!(
        failed_job.model().unwrap().observe().unwrap_err().kind(),
        WorkloadFailureKind::Job
    );
    assert_eq!(failed_job.model().unwrap().result(), Some("AP8="));
    let unknown = statement
        .get(BytePage::default())
        .unwrap()
        .execute(&Synthetic::json(json!({"state":"available"})), 512)
        .await
        .unwrap();
    assert_eq!(
        unknown.model().unwrap().observe().unwrap(),
        Observation::Unknown
    );
    let cancelled = statement
        .get(BytePage::default())
        .unwrap()
        .execute(&Synthetic::json(json!({"state":"cancelled"})), 512)
        .await
        .unwrap();
    assert_eq!(
        cancelled.model().unwrap().observe().unwrap(),
        Observation::Cancelled
    );
    let idle = session
        .get()
        .unwrap()
        .execute(
            &Synthetic::json(
                json!({"state":"Idle", "id":"acquisition", "sessionId":"backing", "replId":"r"}),
            ),
            512,
        )
        .await
        .unwrap();
    assert_eq!(idle.model().unwrap().observe().unwrap(), Observation::Ready);
    assert_eq!(idle.model().unwrap().backing_session_id(), Some("backing"));
    assert_eq!(idle.model().unwrap().id(), Some(&json!("acquisition")));
    let starting = session
        .get()
        .unwrap()
        .execute(
            &Synthetic::json(json!({
                "state":"starting", "appId":null, "errorInfo":null
            })),
            512,
        )
        .await
        .unwrap();
    assert_eq!(
        starting.model().unwrap().observe().unwrap(),
        Observation::Pending
    );
    assert_eq!(starting.model().unwrap().raw()["appId"], Value::Null);
    let waiting = statement
        .get(BytePage::default())
        .unwrap()
        .execute(
            &Synthetic::json(json!({
                "state":"waiting", "output":null
            })),
            512,
        )
        .await
        .unwrap();
    assert_eq!(
        waiting.model().unwrap().observe().unwrap(),
        Observation::Pending
    );
}

#[tokio::test]
async fn statement_cancel_always_sends_empty_json_for_each_profile_and_owner() {
    let native_service = native();
    let fabric_service = fabric();
    let native_classic = native_service
        .session("native-session")
        .unwrap()
        .statements()
        .statement(7)
        .unwrap();
    let native_repl = native_service
        .session("native-session")
        .unwrap()
        .repl("native-repl")
        .unwrap()
        .statements()
        .statement(8)
        .unwrap();
    let fabric_classic = fabric_service
        .session("fabric-session")
        .unwrap()
        .statements()
        .statement(9)
        .unwrap();
    let fabric_repl = fabric_service
        .high_concurrency("acquisition")
        .unwrap()
        .repl("backing-session", "fabric-repl")
        .unwrap()
        .statements()
        .statement(10)
        .unwrap();
    let cases = [
        (
            native_classic,
            ServiceProfile::NativeWildfire,
            "/sessions/native-session/statements/7/cancel",
            None,
        ),
        (
            native_repl,
            ServiceProfile::NativeWildfire,
            "/sessions/native-session/repls/native-repl/statements/8/cancel",
            None,
        ),
        (
            fabric_classic,
            ServiceProfile::FabricJson,
            "/sessions/fabric-session/statements/9/cancel",
            None,
        ),
        (
            fabric_repl,
            ServiceProfile::FabricJson,
            "/highConcurrencySessions/backing-session/repls/fabric-repl/statements/10/cancel",
            Some("acquisition"),
        ),
    ];
    for (statement, profile, suffix, acquisition) in cases {
        let request = statement.cancel().unwrap();
        let prefix = match profile {
            ServiceProfile::NativeWildfire => "/proxy",
            ServiceProfile::FabricJson => {
                "/proxy/v1/workspaces/workspace/lakehouses/lakehouse/livyApi/versions/2024-04-01"
            }
        };
        assert_eq!(request.profile(), profile);
        assert_eq!(request.acquisition_id(), acquisition);
        assert_eq!(request.native().method(), NativeMethod::Post);
        assert_eq!(request.native().effect(), NativeEffect::Cancel);
        assert_eq!(request.native().url().path(), format!("{prefix}{suffix}"));
        assert!(request.native().url().query().is_none());
        assert_eq!(request.native().content_type(), "application/json");
        assert!(request.native().bytes().is_none());
        assert_eq!(request.native().body(), Some(&json!({})));
        assert_eq!(
            serde_json::to_vec(request.native().body().unwrap())
                .unwrap()
                .as_slice(),
            b"{}".as_slice()
        );
        let executor = Synthetic::json(json!({
            "msg": "cancel requested",
            "state": "cancelled"
        }));
        let reply = request.execute(&executor, 512).await.unwrap();
        assert_eq!(executor.count(), 1);
        assert_eq!(&executor.calls.lock().unwrap()[0].0, request.native());
        assert_eq!(reply.status(), 200);
        assert_eq!(reply.disposition(), ResponseDisposition::Returned);
        assert_eq!(reply.model().unwrap().message(), Some("cancel requested"));
        assert!(!reply.model().unwrap().proves_terminal());
    }
}

#[tokio::test]
async fn cancellation_acknowledgements_and_native_text_are_not_terminal_state() {
    let service = native();
    let session = service.session("s").unwrap();
    let statement = session.statements().statement(1).unwrap();
    let ack = statement
        .cancel()
        .unwrap()
        .execute(
            &Synthetic::json(json!({"msg":"cancel requested", "state":"cancelled"})),
            512,
        )
        .await
        .unwrap();
    assert!(!ack.model().unwrap().proves_terminal());
    let executor = Synthetic::new(vec![Ok(response(200, b"stopping", Some("text/plain")))]);
    let reply = session
        .stop()
        .unwrap()
        .execute(&executor, 512)
        .await
        .unwrap();
    assert_eq!(reply.raw(), b"stopping");
    assert!(reply.model().unwrap().payload().is_none());
    assert!(!reply.model().unwrap().proves_terminal());
    let empty = Synthetic::new(vec![Ok(response(204, b"", None))]);
    assert!(
        session
            .delete()
            .unwrap()
            .execute(&empty, 512)
            .await
            .unwrap()
            .model()
            .is_some()
    );
    let reset = Synthetic::new(vec![Ok(response(200, b"reset", Some("text/plain")))]);
    assert!(
        session
            .reset_timeout()
            .unwrap()
            .execute(&reset, 512)
            .await
            .unwrap()
            .model()
            .unwrap()
            .session()
            .is_none()
    );
    let reset = Synthetic::json(json!({"msg":"reset", "state":"idle"}));
    let reply = session
        .reset_timeout()
        .unwrap()
        .execute(&reset, 512)
        .await
        .unwrap();
    assert!(reply.model().unwrap().session().is_none());
    assert_eq!(
        reply.model().unwrap().acknowledgement(),
        Some(&json!({"msg":"reset", "state":"idle"}))
    );
    let reset = Synthetic::json(json!({"state":"idle"}));
    assert_eq!(
        fabric()
            .session("s")
            .unwrap()
            .reset_timeout()
            .unwrap()
            .execute(&reset, 512)
            .await
            .unwrap()
            .model()
            .unwrap()
            .session()
            .unwrap()
            .observe()
            .unwrap(),
        Observation::Ready
    );
    let timeout = session
        .get_timeout()
        .unwrap()
        .execute(
            &Synthetic::json(json!({"serverTimeout":100, "sessionTimeout":-1, "replTimeout":-1})),
            512,
        )
        .await
        .unwrap();
    assert_eq!(timeout.model().unwrap().session_timeout(), Some(-1));
    assert_eq!(timeout.model().unwrap().repl_timeout(), Some(-1));
}

#[tokio::test]
async fn typed_lists_logs_and_tooling_preserve_fields_without_empty_success_fallbacks() {
    let service = native();
    let session = service.session("s").unwrap();
    let list = service
        .list_sessions(ActivityPage::default())
        .unwrap()
        .execute(
            &Synthetic::json(json!({
                "sessions":[{"id":0,"state":"idle","appId":"app","sessionTag":"tag",
                    "creatorId":"creator","livyInstanceId":"instance","acquiredAtUtc":"timestamp",
                    "unknown":{"nested":true}}],
                "total":5,"pageSize":1
            })),
            2048,
        )
        .await
        .unwrap();
    let list = list.model().unwrap();
    assert_eq!(list.total(), Some(5));
    assert_eq!(list.page_size(), Some(1));
    let items = list.sessions().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].application_id(), Some("app"));
    assert_eq!(items[0].session_tag(), Some("tag"));
    assert_eq!(items[0].creator_id(), Some("creator"));
    assert_eq!(items[0].livy_instance_id(), Some("instance"));
    assert_eq!(items[0].acquired_at_utc(), Some("timestamp"));
    assert_eq!(items[0].raw()["unknown"]["nested"], json!(true));
    assert!(items[0].backing_session_id().is_none());
    let batches = service
        .list_batches(ActivityPage::default())
        .unwrap()
        .execute(
            &Synthetic::json(json!({"items":[{"id":"b","state":"success","result":"opaque"}]})),
            512,
        )
        .await
        .unwrap();
    assert_eq!(
        batches.model().unwrap().batches().unwrap()[0]
            .observe()
            .unwrap(),
        Observation::Succeeded
    );
    let statements = session
        .statements()
        .list(StatementListOptions::default())
        .unwrap()
        .execute(
            &Synthetic::json(json!({"statements":[{"id":1,"state":"waiting","output":null}]})),
            512,
        )
        .await
        .unwrap();
    assert_eq!(
        statements.model().unwrap().statements().unwrap()[0].statement_id(),
        Some(1)
    );
    let missing = service
        .list_sessions(ActivityPage::default())
        .unwrap()
        .execute(&Synthetic::json(json!({"total":0})), 512)
        .await
        .unwrap();
    assert_eq!(
        missing.model().unwrap().items().unwrap_err(),
        NativeError::InvalidResponse
    );

    let logs = session
        .logs(BytePage::default())
        .unwrap()
        .execute(
            &Synthetic::json(
                json!({"from":3,"total":8,"log":["line1","line2"],"nextExtension":true}),
            ),
            512,
        )
        .await
        .unwrap();
    let logs = logs.model().unwrap();
    assert_eq!(logs.lines().unwrap(), vec!["line1", "line2"]);
    assert_eq!(logs.offset(), Some(3));
    assert_eq!(logs.total(), Some(8));
    let malformed = session
        .logs(BytePage::default())
        .unwrap()
        .execute(&Synthetic::json(json!({"log":["line",1]})), 512)
        .await
        .unwrap_err();
    assert_eq!(malformed.phase(), FailurePhase::Decode);
    assert!(!malformed.outcome_unknown());

    let completion = session
        .completion(&CompletionInput::new("x", 1).unwrap())
        .unwrap()
        .execute(
            &Synthetic::json(json!({"candidates":["x1","x2"],"cursor_start":0,"cursor_end":1})),
            512,
        )
        .await
        .unwrap();
    assert_eq!(
        completion.model().unwrap().candidates().unwrap(),
        vec!["x1", "x2"]
    );
    let inspection = session
        .inspect(&InspectionInput::default())
        .unwrap()
        .execute(
            &Synthetic::json(json!({"vars":[{"name":"x","type":"integer","unknown":[null]}]})),
            512,
        )
        .await
        .unwrap();
    assert_eq!(
        inspection.model().unwrap().variables().unwrap()[0]["unknown"],
        json!([null])
    );
    let debug = session
        .debug(&DebugInput::new(json!({})).unwrap())
        .unwrap()
        .execute(
            &Synthetic::json(json!({"embedded":{"opaque":"protocol"}})),
            512,
        )
        .await
        .unwrap();
    assert_eq!(
        debug.model().unwrap().raw()["embedded"]["opaque"],
        json!("protocol")
    );
}

fn statement(service: &LivyService) -> Statement {
    service
        .session("s")
        .unwrap()
        .statements()
        .statement(1)
        .unwrap()
}

fn page_json(start: u64, next: Value) -> Value {
    json!({"state":"available", "output":{"status":"ok", "data":{"from":start, "next":next, "fragment":"😀"}}})
}

#[tokio::test]
async fn byte_cursors_are_opaque_bounded_and_owner_bound() {
    let service = native();
    let statement = statement(&service);
    let mut pages = StatementPages::new(statement.clone(), 0, 64, 3, 4096).unwrap();
    let request = pages.next_request().unwrap();
    assert_eq!(pages.next_request().unwrap_err(), CursorError::OutOfOrder);
    let reply = request
        .execute(&Synthetic::json(page_json(0, json!(17))), 512)
        .await
        .unwrap();
    assert_eq!(
        pages.advance(&reply).unwrap(),
        PageProgress::More { next_offset: 17 }
    );
    assert_eq!(pages.offset(), 17);
    assert_eq!(pages.received_bytes(), reply.raw().len());
    assert_eq!(pages.advance(&reply).unwrap_err(), CursorError::OutOfOrder);
    let request = pages.next_request().unwrap();
    assert_eq!(
        query(&request),
        vec![("from".into(), "17".into()), ("size".into(), "64".into())]
    );
    let reply = request
        .execute(
            &Synthetic::json(json!({
                "state":"available", "from":17, "next":-1,
                "output":{"status":"ok", "data":{"next":999, "schema":{"fields":[]},"fragment":"x"}}
            })),
            512,
        )
        .await
        .unwrap();
    assert!(reply.model().unwrap().schema().is_some());
    assert_eq!(pages.advance(&reply).unwrap(), PageProgress::Complete);
    assert_eq!(
        pages.next_request().unwrap_err(),
        CursorError::AlreadyComplete
    );
    assert_eq!(pages.requested_pages(), 2);
    assert_eq!(pages.remaining_bytes(), 4096 - pages.received_bytes());

    let mut pages = StatementPages::new(statement.clone(), 0, 64, 1, 4096).unwrap();
    let request = pages.next_request().unwrap();
    let reply = request
        .execute(&Synthetic::json(page_json(0, json!(1))), 512)
        .await
        .unwrap();
    pages.advance(&reply).unwrap();
    assert_eq!(pages.next_request().unwrap_err(), CursorError::Limit);

    let mut pages = StatementPages::new(statement.clone(), 0, 64, 1, 1).unwrap();
    let request = pages.next_request().unwrap();
    let reply = request
        .execute(&Synthetic::json(page_json(0, json!(-1))), 512)
        .await
        .unwrap();
    assert_eq!(pages.advance(&reply).unwrap_err(), CursorError::Limit);

    let mut pages = StatementPages::new(statement.clone(), 0, 64, 1, 4096).unwrap();
    pages.next_request().unwrap();
    for wrong in [
        statement
            .get(BytePage::new(Some(1), Some(64)).unwrap())
            .unwrap(),
        service
            .session("other")
            .unwrap()
            .statements()
            .statement(1)
            .unwrap()
            .get(BytePage::new(Some(0), Some(64)).unwrap())
            .unwrap(),
        service
            .session("s")
            .unwrap()
            .repl("r")
            .unwrap()
            .statements()
            .statement(1)
            .unwrap()
            .get(BytePage::new(Some(0), Some(64)).unwrap())
            .unwrap(),
        native()
            .session("s")
            .unwrap()
            .statements()
            .statement(1)
            .unwrap()
            .get(BytePage::new(Some(0), Some(64)).unwrap())
            .unwrap(),
    ] {
        let reply = wrong
            .execute(&Synthetic::json(page_json(0, json!(-1))), 512)
            .await
            .unwrap();
        assert_eq!(
            pages.advance(&reply).unwrap_err(),
            CursorError::OwnerMismatch
        );
    }
}

#[tokio::test]
async fn cursor_failures_never_advance_or_claim_complete_results() {
    let service = native();
    for (payload, expected) in [
        (page_json(0, json!(-2)), CursorError::RetrievalLimit),
        (page_json(0, json!(0)), CursorError::NonAdvancing),
        (page_json(0, json!(-3)), CursorError::InvalidMetadata),
        (page_json(0, json!(true)), CursorError::InvalidMetadata),
        (page_json(0, json!(1.5)), CursorError::InvalidMetadata),
        (page_json(0, json!(u64::MAX)), CursorError::InvalidMetadata),
        (page_json(1, json!(-1)), CursorError::InvalidMetadata),
        (
            json!({"state":"available","output":{"status":"ok","data":{}}}),
            CursorError::InvalidMetadata,
        ),
        (
            json!({"state":"available","output":{"status":"error","data":{"next":-1}}}),
            CursorError::NotSuccessful,
        ),
        (
            json!({"state":"running","output":{"status":"ok","data":{"next":-1}}}),
            CursorError::NotSuccessful,
        ),
    ] {
        let mut pages = StatementPages::new(statement(&service), 0, 64, 2, 4096).unwrap();
        let request = pages.next_request().unwrap();
        let reply = request
            .execute(&Synthetic::json(payload), 512)
            .await
            .unwrap();
        assert_eq!(pages.advance(&reply).unwrap_err(), expected);
        assert_eq!(pages.offset(), 0);
        assert_eq!(pages.received_bytes(), 0);
    }
    let mut pages = StatementPages::new(statement(&service), 0, 64, 2, 4096).unwrap();
    let request = pages.next_request().unwrap();
    let accepted = Synthetic::new(vec![Ok(response(202, b"", None))]);
    let reply = request.execute(&accepted, 512).await.unwrap();
    assert_eq!(
        pages.advance(&reply).unwrap_err(),
        CursorError::NotSuccessful
    );
    for (offset, size, count, bytes) in [
        (u64::MAX, 1, 1, 1),
        (0, 0, 1, 1),
        (0, 1, 0, 1),
        (0, 1, 1, 0),
    ] {
        assert!(StatementPages::new(statement(&service), offset, size, count, bytes).is_err());
    }
}

#[tokio::test]
async fn hc_acquisition_isolation_survives_identical_backing_and_statement_ids() {
    let service = fabric();
    let first = service.high_concurrency("acquisition-a").unwrap();
    let second = service.high_concurrency("acquisition-b").unwrap();
    let a = first
        .repl("shared-backing", "repl")
        .unwrap()
        .statements()
        .statement(1)
        .unwrap();
    let b = second
        .repl("shared-backing", "repl")
        .unwrap()
        .statements()
        .statement(1)
        .unwrap();
    let mut pages = StatementPages::new(a, 0, 64, 2, 4096).unwrap();
    let owned_request = pages.next_request().unwrap();
    let alien_request = b.get(BytePage::new(Some(0), Some(64)).unwrap()).unwrap();
    assert_eq!(owned_request.native(), alien_request.native());
    assert_ne!(
        owned_request.acquisition_id(),
        alien_request.acquisition_id()
    );
    let alien = alien_request
        .execute(&Synthetic::json(page_json(0, json!(-1))), 512)
        .await
        .unwrap();
    assert_eq!(
        pages.advance(&alien).unwrap_err(),
        CursorError::OwnerMismatch
    );
    let own = owned_request
        .execute(&Synthetic::json(page_json(0, json!(-1))), 512)
        .await
        .unwrap();
    assert_eq!(pages.advance(&own).unwrap(), PageProgress::Complete);
    assert!(
        first
            .release()
            .unwrap()
            .native()
            .url()
            .path()
            .ends_with("/highConcurrencySessions/acquisition-a")
    );
    assert!(
        !first
            .release()
            .unwrap()
            .native()
            .url()
            .path()
            .contains("shared-backing")
    );
    let other_workspace = LivyService::fabric(endpoint(), "other-workspace", "lakehouse").unwrap();
    assert_ne!(
        service.session("s").unwrap().get().unwrap().native().url(),
        other_workspace
            .session("s")
            .unwrap()
            .get()
            .unwrap()
            .native()
            .url()
    );
}

#[test]
fn buffered_multipart_bytes_headers_and_total_wire_limits_are_exact() {
    let service = native();
    let session = service.session("s").unwrap();
    for (kind, part) in [
        (ResourceKind::Jar, "jar"),
        (ResourceKind::PyFile, "file"),
        (ResourceKind::File, "file"),
    ] {
        let descriptor = session.upload_descriptor(kind, "a\"b.bin", 3).unwrap();
        let request = descriptor
            .clone()
            .into_multipart_request(vec![0, 255, 0], "boundary", 1024)
            .unwrap();
        let native = request.native();
        let mut expected = format!(
            "--boundary\r\nContent-Disposition: form-data; name=\"{part}\"; filename=\"a%22b.bin\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        ).into_bytes();
        expected.extend_from_slice(&[0, 255, 0]);
        expected.extend_from_slice(b"\r\n--boundary--\r\n");
        assert_eq!(native.bytes(), Some(expected.as_slice()));
        assert_eq!(
            native.content_type(),
            "multipart/form-data; boundary=boundary"
        );
        assert!(native.body().is_none());
        assert_eq!(
            native.headers().get("x-requested-by").map(String::as_str),
            Some("fabric-sdk-rust-livy")
        );
        assert_eq!(
            native.headers().get("accept-encoding").map(String::as_str),
            Some("identity")
        );
        assert!(!native.headers().contains_key("authorization"));
        assert!(!native.headers().contains_key("host"));
        assert!(
            descriptor
                .clone()
                .into_multipart_request(vec![0, 255, 0], "boundary", expected.len())
                .is_ok()
        );
        assert_eq!(
            descriptor
                .clone()
                .into_multipart_request(vec![0, 255, 0], "boundary", expected.len() - 1)
                .unwrap_err(),
            NativeError::Limit,
        );
        assert_eq!(
            descriptor
                .clone()
                .into_multipart_request(vec![0, 255], "boundary", 1024)
                .unwrap_err(),
            NativeError::InvalidInput,
        );
        assert_eq!(
            descriptor
                .clone()
                .into_multipart_request(vec![0, 255, 0], "boundary", 0)
                .unwrap_err(),
            NativeError::InvalidInput,
        );
        for boundary in [
            "",
            "bad\r\nboundary",
            "with spaces",
            "bad;header",
            "\"quoted\"",
        ] {
            assert_eq!(
                descriptor
                    .clone()
                    .into_multipart_request(vec![0, 255, 0], boundary, 1024)
                    .unwrap_err(),
                NativeError::InvalidInput,
            );
        }
        assert_eq!(
            descriptor
                .into_multipart_request(vec![0, 255, 0], &"x".repeat(71), 1024)
                .unwrap_err(),
            NativeError::InvalidInput,
        );
    }
    let content = b"before--boundary\r\nafter".to_vec();
    let descriptor = session
        .upload_descriptor(ResourceKind::File, "file.bin", content.len() as u64)
        .unwrap();
    assert_eq!(
        descriptor
            .into_multipart_request(content, "boundary", 1024)
            .unwrap_err(),
        NativeError::InvalidInput,
    );
    assert!(
        session
            .upload_descriptor(ResourceKind::File, "empty.bin", 0)
            .unwrap()
            .into_multipart_request(Vec::new(), "empty-boundary", 1024)
            .is_ok()
    );
}
