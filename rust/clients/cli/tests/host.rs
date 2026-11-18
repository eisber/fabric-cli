// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real parser, host, engine execution, and output with offline injected effects.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::credentials::{AccessToken, TokenCredential, TokenRequestOptions};
use fabric_cli::{Host, SelectionFile};
use fabric_cli_command_contracts::CommandError;
use fabric_core::{
    ApiRequest, ApiResponse, CancellationToken, Clock, CoreError, ErrorKind, HttpTransport,
    OperationContext, ResponseHeadCapture,
};
use fabric_identity::Binding;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const TENANT: &str = "00000000-0000-0000-0000-000000000001";
const PRINCIPAL: &str = "00000000-0000-0000-0000-000000000002";
static FILE_ID: AtomicUsize = AtomicUsize::new(0);
const WELCOME: &str = "Welcome to fab-rs.\n\n\n\
                      Get started:\n  fab-rs api workspaces   List workspaces\n  fab-rs auth             Authentication commands\n  fab-rs --help           All commands\n";

fn binding() -> Binding {
    Binding {
        tenant: TENANT.into(),
        principal: PRINCIPAL.into(),
    }
}

struct TestClock {
    now: Mutex<Instant>,
    sleeps: AtomicUsize,
    diagnostics: Mutex<Vec<&'static str>>,
    save_gate: Mutex<Option<SaveGate>>,
}

struct SaveGate {
    directory: PathBuf,
    runtime_thread: std::thread::ThreadId,
    started: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}
#[async_trait]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        let gate = {
            let mut gate = self.save_gate.lock().unwrap();
            if gate.as_ref().is_some_and(|gate| {
                std::thread::current().id() != gate.runtime_thread
                    && std::fs::read_dir(&gate.directory).is_ok_and(|mut entries| {
                        entries.any(|entry| {
                            entry.ok().is_some_and(|entry| {
                                entry.file_name().to_string_lossy().ends_with(".tmp")
                            })
                        })
                    })
            }) {
                gate.take()
            } else {
                None
            }
        };
        if let Some(gate) = gate {
            gate.started.send(()).unwrap();
            gate.release
                .recv_timeout(Duration::from_secs(5))
                .expect("export cancellation did not release the worker");
        }
        *self.now.lock().unwrap()
    }
    fn wall_now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(946684800)
    }
    async fn wait_until(&self, deadline: Instant, _: &OperationContext) -> Result<(), CoreError> {
        tokio::time::sleep(deadline.saturating_duration_since(self.now())).await;
        Ok(())
    }
    fn diagnostic(&self, message: &'static str) {
        self.diagnostics.lock().unwrap().push(message);
    }
    async fn sleep(&self, delay: Duration, context: &OperationContext) -> Result<(), CoreError> {
        self.sleeps.fetch_add(1, Ordering::SeqCst);
        let mut now = self.now.lock().unwrap();
        *now += delay;
        context.remaining_at(*now)?;
        Ok(())
    }
}

#[derive(Debug)]
struct Credential {
    calls: AtomicUsize,
    error: bool,
    hang: bool,
    dropped: AtomicBool,
}

struct CredentialFutureGuard<'a>(&'a AtomicBool);
impl Drop for CredentialFutureGuard<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes.len(), 1);
        assert!(options.is_some());
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _guard = CredentialFutureGuard(&self.dropped);
        if self.hang {
            std::future::pending::<()>().await;
        }
        if self.error {
            return Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Credential,
                "SDK-ERROR-CANARY",
            ));
        }
        Ok(AccessToken::new(
            "synthetic-token",
            azure_core::time::OffsetDateTime::from_unix_timestamp(4102444800).unwrap(),
        ))
    }
}

struct Transport {
    requests: Mutex<Vec<ApiRequest>>,
    responses: Mutex<VecDeque<ApiResponse>>,
    error: Option<CoreError>,
    error_head: Option<ApiResponse>,
}
#[async_trait]
impl HttpTransport for Transport {
    async fn send(
        &self,
        request: ApiRequest,
        _: &OperationContext,
        _: &dyn Clock,
    ) -> Result<ApiResponse, CoreError> {
        self.requests.lock().unwrap().push(request);
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP attempt"))
    }

    async fn send_with_observer(
        &self,
        request: ApiRequest,
        context: &OperationContext,
        clock: &dyn Clock,
        head: &ResponseHeadCapture,
    ) -> Result<ApiResponse, CoreError> {
        let result = self.send(request, context, clock).await;
        if let Some(response) = result.as_ref().ok().or(self.error_head.as_ref()) {
            head.record(response.status, &response.headers)?;
        }
        result
    }
}

struct FakeHost {
    environment: std::collections::BTreeMap<String, OsString>,
    environment_reads: AtomicUsize,
    binding: Mutex<Option<Binding>>,
    hooks: AtomicUsize,
    credential: Arc<Credential>,
    transport: Arc<Transport>,
    clock: Arc<TestClock>,
    confirmations: Mutex<VecDeque<Result<bool, CommandError>>>,
    confirmation_calls: AtomicUsize,
}
impl FakeHost {
    fn new(responses: Vec<ApiResponse>) -> Self {
        Self {
            environment: Default::default(),
            environment_reads: AtomicUsize::new(0),
            binding: Mutex::new(Some(binding())),
            hooks: AtomicUsize::new(0),
            credential: Arc::new(Credential {
                calls: AtomicUsize::new(0),
                error: false,
                hang: false,
                dropped: AtomicBool::new(false),
            }),
            transport: Arc::new(Transport {
                requests: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into()),
                error: None,
                error_head: None,
            }),
            clock: Arc::new(TestClock {
                now: Mutex::new(Instant::now()),
                sleeps: AtomicUsize::new(0),
                diagnostics: Mutex::new(Vec::new()),
                save_gate: Mutex::new(None),
            }),
            confirmations: Mutex::new(VecDeque::new()),
            confirmation_calls: AtomicUsize::new(0),
        }
    }
    fn hook(&self) {
        self.hooks.fetch_add(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl Host for FakeHost {
    fn environment(&self, name: &str) -> Option<OsString> {
        self.environment_reads.fetch_add(1, Ordering::SeqCst);
        self.environment.get(name).cloned()
    }
    fn clock(&self) -> Arc<dyn Clock> {
        self.hook();
        self.clock.clone()
    }
    fn selection(&self) -> Result<Option<Binding>, CommandError> {
        self.hook();
        Ok(self.binding.lock().unwrap().clone())
    }
    fn save_selection(&self, binding: &Binding) -> Result<(), CommandError> {
        self.hook();
        *self.binding.lock().unwrap() = Some(binding.clone());
        Ok(())
    }
    fn clear_selection(&self) -> Result<(), CommandError> {
        self.hook();
        *self.binding.lock().unwrap() = None;
        Ok(())
    }
    async fn login(&self, _: &OperationContext) -> Result<Binding, CommandError> {
        self.hook();
        Ok(binding())
    }
    fn credential(&self, _: Binding) -> Result<Arc<dyn TokenCredential>, CommandError> {
        self.hook();
        Ok(self.credential.clone())
    }
    fn transport(&self) -> Result<Arc<dyn HttpTransport>, CommandError> {
        self.hook();
        Ok(self.transport.clone())
    }
    async fn confirm(&self, _: CancellationToken) -> Result<bool, CommandError> {
        self.confirmation_calls.fetch_add(1, Ordering::SeqCst);
        self.confirmations
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                Err(CommandError::new(
                    "InvalidInput",
                    "Confirmation unavailable",
                ))
            })
    }
    fn take_diagnostics(&self) -> Vec<&'static str> {
        std::mem::take(&mut *self.clock.diagnostics.lock().unwrap())
    }
}

fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> ApiResponse {
    ApiResponse {
        status,
        body: body.as_bytes().to_vec(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

async fn run(args: &[&str], host: &dyn Host) -> (u8, String, String) {
    let args = std::iter::once("fab-rs")
        .chain(args.iter().copied())
        .map(OsString::from);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = fabric_cli::run(
        args,
        host,
        CancellationToken::new(),
        &mut stdout,
        &mut stderr,
    )
    .await;
    (
        code,
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

fn data(stdout: &str) -> Value {
    serde_json::from_str::<Value>(stdout).unwrap()["result"]["data"].clone()
}

#[tokio::test]
async fn resources_validate_locally_before_credentials_or_http() {
    let guid = "00000000-0000-0000-0000-000000000001";
    let cases: Vec<Vec<&str>> = vec![
        vec!["workspace", "list", "--workspace", guid],
        vec!["item", "list"],
        vec!["item", "create", "new", "--payload", r#"{"type":"X"}"#],
        vec!["item", "get", guid],
        vec![
            "item",
            "update",
            guid,
            "--payload",
            r#"{"displayName":"new"}"#,
        ],
        vec!["item", "delete", guid, "--force"],
        vec!["workspace", "create", guid],
        vec!["item", "create", guid, "--workspace", guid],
        vec!["workspace", "create"],
        vec![
            "workspace",
            "create",
            "one",
            "--payload",
            r#"{"displayName":"two"}"#,
        ],
        vec!["item", "create", "one", "--workspace", guid],
        vec![
            "item",
            "create",
            "one",
            "--workspace",
            guid,
            "--payload",
            r#"{"type":"X","workspaceId":"x"}"#,
        ],
        vec![
            "item",
            "update",
            guid,
            "--workspace",
            guid,
            "--payload",
            "[]",
        ],
        vec![
            "item",
            "create",
            "one",
            "--workspace",
            guid,
            "--payload",
            r#"{"type":"X","definition":{},"creationPayload":{}}"#,
        ],
        vec![
            "item",
            "create",
            "one",
            "--workspace",
            guid,
            "--payload",
            r#"{"type":"X","options":{}}"#,
        ],
        vec!["workspace", "get", guid, "--type", "x"],
        vec!["workspace", "delete", guid, "--workspace", guid],
        vec!["workspace", "create", "Admin monitoring"],
        vec![
            "workspace",
            "create",
            "one",
            "--payload",
            r#"{"capacityId":"invalid"}"#,
        ],
        vec![
            "item",
            "create",
            "one",
            "--workspace",
            guid,
            "--payload",
            r#"{"type":"X","folderId":42}"#,
        ],
        vec!["workspace", "update", guid, "--payload", "{}", "--force"],
        vec![
            "workspace",
            "update",
            guid,
            "--payload",
            "{}",
            "--allow-pairing-by-name",
        ],
        vec![
            "item",
            "update",
            guid,
            "--workspace",
            guid,
            "--payload",
            "{}",
            "--force",
        ],
        vec![
            "item",
            "update",
            guid,
            "--workspace",
            guid,
            "--payload",
            "{}",
            "--format",
            "x",
        ],
        vec![
            "item",
            "create",
            "one",
            "--workspace",
            guid,
            "--payload",
            r#"{"type":"X"}"#,
            "--format",
            "x",
        ],
        vec![
            "item",
            "create",
            "one",
            "--workspace",
            guid,
            "--payload",
            r#"{"type":"X"}"#,
            "--type",
            "X",
        ],
    ];
    for args in cases {
        let host = FakeHost::new(vec![]);
        let (code, _, _) = run(&args, &host).await;
        assert_eq!(code, 1, "{args:?}");
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
        assert!(host.transport.requests.lock().unwrap().is_empty());
    }
    let name = "a".repeat(257);
    let host = FakeHost::new(vec![]);
    let (code, _, _) = run(&["workspace", "create", &name], &host).await;
    assert_eq!(code, 1);
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
    assert!(host.transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn resources_use_direct_ids_and_native_output() {
    let workspace = "00000000-0000-0000-0000-000000000001";
    let item = "00000000-0000-0000-0000-000000000002";
    let host = FakeHost::new(vec![response(
        200,
        r#"{"id":"answer","displayName":"result"}"#,
        &[],
    )]);
    let (code, stdout, _) = run(
        &[
            "item",
            "get",
            item,
            "--workspace",
            workspace,
            "--query",
            "displayName",
        ],
        &host,
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(data(&stdout), json!("result"));
    let requests = host.transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(
        requests[0].url.path(),
        format!("/v1/workspaces/{workspace}/items/{item}")
    );
}

#[tokio::test]
async fn resource_list_field_projection_runs_after_name_resolution_and_all_pages() {
    let workspace = "00000000-0000-0000-0000-000000000001";
    for kind in ["workspace", "item"] {
        let mut responses = Vec::new();
        if kind == "item" {
            responses.push(response(
                200,
                &format!(r#"{{"value":[{{"id":"{workspace}","displayName":"testrustcrud"}}]}}"#),
                &[],
            ));
        }
        responses.extend([
            response(
                200,
                r#"{"value":[{"id":"b2fe34d2-2197-4f2f-933f-8352fd0318fb","hidden":"CANARY"}],"continuationToken":"next"}"#,
                &[],
            ),
            response(200, r#"{"value":[{"id":"second","hidden":"CANARY"}]}"#, &[]),
        ]);
        let host = FakeHost::new(responses);
        let mut args = vec![kind, "list", "--query", "id"];
        if kind == "item" {
            args.extend(["--workspace", "testrustcrud"]);
        }
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 0, "{stdout} {stderr}");
        assert_eq!(
            data(&stdout),
            json!([{"id":"b2fe34d2-2197-4f2f-933f-8352fd0318fb"}, {"id":"second"}])
        );
        assert!(!stdout.contains("CANARY"));
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), if kind == "item" { 3 } else { 2 });
        let last = requests.last().unwrap();
        assert_eq!(last.url.query(), Some("continuationToken=next"));
        assert_eq!(
            last.url.path(),
            if kind == "item" {
                format!("/v1/workspaces/{workspace}/items")
            } else {
                "/v1/workspaces".to_owned()
            }
        );
    }
}

#[tokio::test]
async fn resource_list_field_projection_keeps_json_fidelity_and_empty_results() {
    for (body, query, expected) in [
        (r#"{"value":[]}"#, "id", "[]"),
        (
            r#"{"value":[{"v":false},{"v":0},{"v":""},{"v":null},{}]}"#,
            "v",
            r#"[{"v":false},{"v":0},{"v":""},{"v":null},{"v":null}]"#,
        ),
        (
            r#"{"value":[{"v":184467440737095516170},{"v":-0},{"v":[null,false]},{"v":{"$serde_json::private::Number":"123","é":1.0}}]}"#,
            "v",
            r#"[{"v":184467440737095516170},{"v":0},{"v":[null,false]},{"v":{"$serde_json::private::Number":"123","é":1.0}}]"#,
        ),
        (
            r#"{"value":[{"$serde_json::private::Number":123,"hidden":"CANARY"},{}]}"#,
            r#""$serde_json::private::Number""#,
            r#"[{"$serde_json::private::Number":123},{"$serde_json::private::Number":null}]"#,
        ),
    ] {
        let host = FakeHost::new(vec![response(200, body, &[])]);
        let (code, stdout, stderr) = run(&["workspace", "list", "--query", query], &host).await;
        assert_eq!(code, 0, "{stdout} {stderr}");
        let result = fabric_core_json::decode(stdout.as_bytes()).unwrap();
        assert_eq!(
            result["result"]["data"],
            fabric_core_json::decode(expected.as_bytes()).unwrap(),
            "{query}"
        );
        assert!(!stdout.contains("CANARY"));
    }
}

#[tokio::test]
async fn resource_list_queries_preserve_explicit_jmespath_and_identity() {
    let body = r#"{"value":[{"id":"b","n":2},{"id":"a","n":1}]}"#;
    for (query, expected) in [
        ("[].id", json!(["b", "a"])),
        ("[0].id", json!("b")),
        ("[?n > `1`].id", json!(["b"])),
        ("[].{key: id}", json!([{"key":"b"}, {"key":"a"}])),
        ("sort_by(@, &n)[].id", json!(["a", "b"])),
        ("length(@)", json!("2")),
        ("@ | id", json!("None")),
        ("[?n > `9`]", json!([])),
        ("", json!([{"id":"b","n":2}, {"id":"a","n":1}])),
        (".", json!([{"id":"b","n":2}, {"id":"a","n":1}])),
        ("@", json!([{"id":"b","n":2}, {"id":"a","n":1}])),
    ] {
        let host = FakeHost::new(vec![response(200, body, &[])]);
        let (code, stdout, stderr) = run(&["workspace", "list", "--query", query], &host).await;
        assert_eq!(code, 0, "{query}: {stdout} {stderr}");
        assert_eq!(data(&stdout), expected, "{query}");
    }
}

#[tokio::test]
async fn resource_field_shortcut_does_not_change_single_resources_or_raw_api() {
    let guid = "00000000-0000-0000-0000-000000000001";
    for kind in ["workspace", "item"] {
        for (query, expected) in [
            ("id", json!("answer")),
            ("n", json!("0")),
            ("missing", json!("None")),
            ("{key: id}", json!({"key":"answer"})),
            ("v", json!([])),
        ] {
            let host = FakeHost::new(vec![response(200, r#"{"id":"answer","n":0,"v":[]}"#, &[])]);
            let mut args = vec![kind, "get", guid, "--query", query];
            if kind == "item" {
                args.extend(["--workspace", guid]);
            }
            let (code, stdout, stderr) = run(&args, &host).await;
            assert_eq!(code, 0, "{stdout} {stderr}");
            assert_eq!(data(&stdout), expected, "{kind} {query}");
        }
    }
    for (query, expected) in [
        ("id", json!(["None"])),
        ("text.value[].id", json!(["answer"])),
        ("text.value[].{id: id}", json!([{"id":"answer"}])),
    ] {
        let host = FakeHost::new(vec![response(
            200,
            r#"{"value":[{"id":"answer","hidden":"CANARY"}],"continuationToken":"next"}"#,
            &[],
        )]);
        let (code, stdout, stderr) = run(&["api", "workspaces", "--query", query], &host).await;
        assert_eq!(code, 0, "{stdout} {stderr}");
        assert_eq!(data(&stdout), expected, "{query}");
        assert!(!stdout.contains("CANARY"));
        assert_eq!(host.transport.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn resource_projection_does_not_publish_partial_lists_or_bypass_preflight() {
    for second in [
        response(403, r#"{"error":"CANARY"}"#, &[]),
        response(200, r#"{"value":[null]}"#, &[]),
    ] {
        let host = FakeHost::new(vec![
            response(
                200,
                r#"{"value":[{"id":"PARTIAL-CANARY"}],"continuationToken":"next"}"#,
                &[],
            ),
            second,
        ]);
        let (code, stdout, _) = run(&["workspace", "list", "--query", "id"], &host).await;
        assert_ne!(code, 0);
        assert!(!stdout.contains("PARTIAL-CANARY"));
        assert_eq!(host.transport.requests.lock().unwrap().len(), 2);
    }
    for query in ["[?", "[?id='CANARY']", "length()", "[::0]"] {
        let host = FakeHost::new(vec![]);
        let (code, _, _) = run(&["workspace", "list", "--query", query], &host).await;
        assert_eq!(code, 1, "{query}");
        assert_eq!(host.hooks.load(Ordering::SeqCst), 1);
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
        assert!(host.transport.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn item_selector_combinations_only_fetch_required_names() {
    let workspace = "00000000-0000-0000-0000-000000000001";
    let item = "00000000-0000-0000-0000-000000000002";
    for (workspace_name, item_name) in [(false, false), (true, false), (false, true), (true, true)]
    {
        let mut replies = Vec::new();
        if workspace_name {
            replies.push(response(
                200,
                &format!(r#"{{"value":[{{"id":"{workspace}","displayName":"workspace"}}]}}"#),
                &[],
            ));
        }
        if item_name {
            replies.push(response(
                200,
                &format!(r#"{{"value":[{{"id":"{item}","displayName":"item"}}]}}"#),
                &[],
            ));
        }
        replies.push(response(200, "{}", &[]));
        let host = FakeHost::new(replies);
        let target = if item_name { "item" } else { item };
        let scope = if workspace_name {
            "workspace"
        } else {
            workspace
        };
        let (code, _, _) = run(&["item", "get", target, "--workspace", scope], &host).await;
        assert_eq!(code, 0);
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            1 + usize::from(workspace_name) + usize::from(item_name)
        );
        assert_eq!(
            requests.last().unwrap().url.path(),
            format!("/v1/workspaces/{workspace}/items/{item}")
        );
    }
}

#[tokio::test]
async fn resource_page_errors_and_missing_names_do_not_mutate() {
    let workspace = "00000000-0000-0000-0000-000000000001";
    let cases = [
        response(200, r#"{"value":[]}"#, &[]),
        response(200, r#"{"value":[],"continuationToken":42}"#, &[]),
        response(503, r#"{"private":"not-for-output"}"#, &[]),
        response(200, "{", &[]),
    ];
    for reply in cases {
        let host = FakeHost::new(vec![reply]);
        let (code, stdout, _) = run(
            &[
                "item",
                "delete",
                "absent",
                "--workspace",
                workspace,
                "--force",
            ],
            &host,
        )
        .await;
        assert_eq!(code, 1);
        assert!(!stdout.contains("not-for-output"));
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
    }
}

#[tokio::test]
async fn resource_creation_accepts_payload_name_and_matching_names() {
    let id = "00000000-0000-0000-0000-000000000001";
    for args in [
        vec![
            "workspace",
            "create",
            "--payload",
            r#"{"displayName":"00000000-0000-0000-0000-000000000001"}"#,
        ],
        vec![
            "workspace",
            "create",
            "ws",
            "--payload",
            r#"{"displayName":"ws"}"#,
        ],
        vec!["workspace", "create", "ws"],
        vec![
            "item",
            "create",
            "--workspace",
            id,
            "--payload",
            r#"{"displayName":"item","type":"UnknownType"}"#,
        ],
    ] {
        let host = FakeHost::new(vec![response(201, "{}", &[])]);
        let (code, _, _) = run(&args, &host).await;
        assert_eq!(code, 0, "{args:?}");
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
    }
}

#[tokio::test]
async fn resource_resolution_reads_all_pages_and_rejects_duplicate_names() {
    let id = "00000000-0000-0000-0000-000000000001";
    let first = response(200, r#"{"value":[],"continuationToken":"a+/="}"#, &[]);
    let second = response(
        200,
        &format!(r#"{{"value":[{{"id":"{id}","displayName":"ws"}}]}}"#),
        &[],
    );
    let item = "00000000-0000-0000-0000-000000000002";
    let host = FakeHost::new(vec![
        first.clone(),
        second.clone(),
        response(200, "{}", &[]),
    ]);
    let (code, _, _) = run(&["item", "get", item, "--workspace", "ws"], &host).await;
    assert_eq!(code, 0);
    {
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[1].url.query(),
            Some("continuationToken=a%2B%2F%3D")
        );
    }

    let host = FakeHost::new(vec![
        response(
            200,
            r#"{"value":[],"continuationToken":"ABCsMTAwMDAwLDA%3D&x=y"}"#,
            &[],
        ),
        response(200, r#"{"value":[]}"#, &[]),
    ]);
    let (code, _, _) = run(&["workspace", "list"], &host).await;
    assert_eq!(code, 0);
    assert_eq!(
        host.transport.requests.lock().unwrap()[1].url.query(),
        Some("continuationToken=ABCsMTAwMDAwLDA%3D%26x%3Dy")
    );

    let duplicate = response(
        200,
        &format!(r#"{{"value":[{{"id":"{id}","displayName":"ws"}}],"continuationToken":"next"}}"#),
        &[],
    );
    let host = FakeHost::new(vec![duplicate, second]);
    let (code, stdout, _) = run(&["workspace", "get", "ws"], &host).await;
    assert_eq!(code, 1);
    assert!(stdout.contains("Ambiguous"));
    assert_eq!(host.transport.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn resource_creation_and_patch_keep_native_payloads() {
    let workspace = "00000000-0000-0000-0000-000000000001";
    let host = FakeHost::new(vec![response(201, r#"{"id":"created"}"#, &[])]);
    let input = r#"{"type":"FutureType","other":{"$serde_json::private::Number":"x","big":123456789012345678901234567890}}"#;
    let (code, stdout, _) = run(
        &[
            "item",
            "create",
            "new",
            "--workspace",
            workspace,
            "--payload",
            input,
        ],
        &host,
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(data(&stdout)["id"], "created");
    {
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(requests[0].method, "POST");
        let body = fabric_core_json::decode(&requests[0].body).unwrap();
        assert_eq!(body["displayName"], "new");
        assert_eq!(body["type"], "FutureType");
        assert_eq!(body["other"]["$serde_json::private::Number"], "x");
    }

    let host = FakeHost::new(vec![response(200, r#"{"displayName":"new"}"#, &[])]);
    let (code, _, _) = run(
        &[
            "workspace",
            "update",
            workspace,
            "--payload",
            r#"{"displayName":"new"}"#,
        ],
        &host,
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(host.transport.requests.lock().unwrap()[0].method, "PATCH");
}

#[tokio::test]
async fn deletion_confirms_after_resolution_and_force_never_reads() {
    let id = "00000000-0000-0000-0000-000000000001";
    let lookup = response(
        200,
        &format!(r#"{{"value":[{{"id":"{id}","displayName":"ws"}}]}}"#),
        &[],
    );
    let host = FakeHost::new(vec![lookup]);
    host.confirmations.lock().unwrap().push_back(Ok(false));
    let (code, stdout, stderr) = run(&["workspace", "delete", "ws"], &host).await;
    assert_eq!(code, 1);
    assert!(stdout.contains("Cancelled"));
    assert!(stderr.contains(id));
    assert_eq!(host.transport.requests.lock().unwrap().len(), 1);
    let host = FakeHost::new(vec![response(204, "", &[])]);
    let (code, _, stderr) = run(&["workspace", "delete", id, "--force"], &host).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(host.confirmation_calls.load(Ordering::SeqCst), 0);
    assert_eq!(host.transport.requests.lock().unwrap()[0].method, "DELETE");
}

#[tokio::test]
async fn resource_requests_and_polling_use_the_configured_fabric_endpoint() {
    let id = "00000000-0000-0000-0000-000000000001";
    let mut host = FakeHost::new(vec![
        response(202, "", &[("x-ms-operation-id", "operation-1")]),
        response(200, r#"{"status":"Succeeded"}"#, &[]),
    ]);
    host.environment.insert(
        "FABRIC_API_ENDPOINT".into(),
        OsString::from("msitapi.fabric.microsoft.com"),
    );
    let (code, stdout, _) = run(&["workspace", "delete", id, "--force"], &host).await;
    assert_eq!(code, 0, "{stdout}");
    let requests = host.transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert_eq!(request.url.host_str(), Some("msitapi.fabric.microsoft.com"));
    }
}

#[tokio::test]
async fn invalid_resource_endpoint_is_rejected_before_credentials_or_http() {
    let mut host = FakeHost::new(vec![]);
    host.environment.insert(
        "FABRIC_API_ENDPOINT".into(),
        OsString::from("api.fabric.microsoft.com.evil.example"),
    );
    let (code, stdout, _) = run(&["workspace", "list"], &host).await;
    assert_eq!(code, 1);
    assert!(stdout.contains("FABRIC_API_ENDPOINT"));
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
    assert!(host.transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn resource_empty_delete_and_accepted_mutation_are_distinct() {
    let id = "00000000-0000-0000-0000-000000000001";
    for status in [200, 204] {
        for args in [
            vec!["workspace", "delete", id, "--force"],
            vec!["item", "delete", id, "--workspace", id, "--force"],
        ] {
            let host = FakeHost::new(vec![response(status, "", &[])]);
            let (code, stdout, _) = run(&args, &host).await;
            assert_eq!(code, 0, "{args:?} {status}: {stdout}");
            assert_eq!(data(&stdout), Value::Null);
        }
    }

    for args in [
        vec!["workspace", "get", id],
        vec!["item", "get", id, "--workspace", id],
    ] {
        let host = FakeHost::new(vec![response(200, "", &[])]);
        let (code, stdout, _) = run(&args, &host).await;
        assert_eq!(code, 1);
        assert!(stdout.contains("Empty resource response"));
    }

    let host = FakeHost::new(vec![response(202, "", &[("x-ms-operation-id", id)])]);
    let (code, stdout, _) = run(&["workspace", "delete", id, "--force", "--async"], &host).await;
    assert_eq!(code, 0);
    assert_eq!(data(&stdout)["status_code"], 202);
    assert_eq!(data(&stdout)["status"], "Accepted");
    assert_eq!(data(&stdout)["recovery"]["operation_id"], id);
    assert_eq!(host.transport.requests.lock().unwrap().len(), 1);

    let host = FakeHost::new(vec![response(202, "", &[])]);
    let (code, stdout, _) = run(&["workspace", "delete", id, "--force", "--async"], &host).await;
    assert_eq!(code, 0);
    assert_eq!(data(&stdout)["status"], "Accepted");
    assert!(data(&stdout).get("recovery").is_none());

    let host = FakeHost::new(vec![response(
        202,
        "",
        &[
            ("location", "http://evil.invalid/operation?secret=synthetic"),
            ("x-ms-operation-id", id),
        ],
    )]);
    let (code, stdout, _) = run(&["workspace", "delete", id, "--force", "--async"], &host).await;
    assert_eq!(code, 0);
    assert_eq!(data(&stdout)["status"], "Accepted");
    assert_eq!(data(&stdout)["recovery"]["operation_id"], id);
    assert!(!stdout.contains("evil.invalid"));
    assert!(!stdout.contains("synthetic"));
}

#[tokio::test]
async fn resource_service_errors_preserve_code_without_leaking_diagnostics() {
    let body = json!({
        "errorCode": "Diagnostic",
        "message": "Missing item https://example.invalid/path?token=SECRET",
        "requestId": "00000000-0000-0000-0000-000000000001"
    })
    .to_string();
    let host = FakeHost::new(vec![response(404, &body, &[])]);
    let (code, stdout, stderr) = run(&["--verbose", "workspace", "get", WS], &host).await;
    assert_eq!(code, 1, "{stdout}");
    let result = &serde_json::from_str::<Value>(&stdout).unwrap()["result"];
    assert_eq!(result["error_code"], "Diagnostic");
    assert!(result["message"].as_str().unwrap().contains("Missing item"));
    assert!(!stdout.contains("SECRET"));
    assert!(stderr.contains("Command: failed (service failure)"));
    assert!(!stderr.contains("Missing item"));
    assert!(!stderr.contains("SECRET"));
}

#[tokio::test]
async fn resource_mutations_complete_or_preserve_operation_recovery() {
    let id = "00000000-0000-0000-0000-000000000001";
    for (status, success) in [("Succeeded", true), ("Failed", false)] {
        let host = FakeHost::new(vec![
            response(202, "", &[("x-ms-operation-id", "operation-1")]),
            response(200, &format!(r#"{{"status":"{status}"}}"#), &[]),
        ]);
        let (code, stdout, _) = run(&["workspace", "delete", id, "--force"], &host).await;
        assert_eq!(code == 0, success);
        if !success {
            assert!(stdout.contains("operation-1"));
        }
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "DELETE");
        assert_eq!(requests[1].method, "GET");
    }
}

#[tokio::test]
async fn resource_payload_files_are_bounded_and_read_before_auth() {
    let directory = TempDirectory::new();
    let path = directory.0.join("payload.json");
    std::fs::write(&path, br#"{"displayName":"file"}"#).unwrap();
    let argument = format!("@{}", path.display());
    let host = FakeHost::new(vec![response(201, "{}", &[])]);
    let (code, _, _) = run(&["workspace", "create", "--payload", &argument], &host).await;
    assert_eq!(code, 0);

    std::fs::write(&path, vec![b' '; fabric_core::MAX_REQUEST_BYTES + 1]).unwrap();
    let host = FakeHost::new(vec![]);
    let (code, _, _) = run(&["workspace", "create", "--payload", &argument], &host).await;
    assert_eq!(code, 1);
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
    assert!(host.transport.requests.lock().unwrap().is_empty());
}

const WS: &str = "00000000-0000-0000-0000-00000000000a";
const ITEM: &str = "00000000-0000-0000-0000-00000000000b";

fn item_export_replies(path: &str) -> Vec<ApiResponse> {
    vec![
        response(200, r#"{"displayName":"nb","type":"Notebook"}"#, &[]),
        response(
            200,
            &format!(
                r#"{{"definition":{{"parts":[{{"path":"{path}","payload":"aGVsbG8=","payloadType":"InlineBase64"}}]}}}}"#
            ),
            &[],
        ),
    ]
}

#[tokio::test]
async fn item_get_destination_writes_decoded_parts_and_refuses_overwrite() {
    let dir = TempDirectory::new();
    let destination = dir.0.to_str().unwrap();
    let args = [
        "item",
        "get",
        ITEM,
        "--workspace",
        WS,
        "--export-to",
        destination,
    ];
    let host = FakeHost::new(item_export_replies("src/a.py"));
    let (code, stdout, stderr) = run(&args, &host).await;
    assert_eq!(code, 0, "{stderr}");
    let file = dir.0.join("nb.Notebook").join("src").join("a.py");
    assert_eq!(std::fs::read(&file).unwrap(), b"hello");
    assert_eq!(data(&stdout)["saved"], true);
    {
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(requests[1].method, "POST");
        assert_eq!(
            requests[1].url.path(),
            format!("/v1/workspaces/{WS}/items/{ITEM}/getDefinition")
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    std::fs::write(&file, b"keep").unwrap();
    let host = FakeHost::new(item_export_replies("src/a.py"));
    let (code, _, _) = run(&args, &host).await;
    assert_eq!(code, 1);
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");
    let mut overwrite = args.to_vec();
    overwrite.push("--overwrite");
    let host = FakeHost::new(item_export_replies("src/a.py"));
    let (code, _, stderr) = run(&overwrite, &host).await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(std::fs::read(&file).unwrap(), b"hello");
}

#[tokio::test]
async fn workspace_export_refuses_stale_files_before_overwriting() {
    let dir = TempDirectory::new();
    let folder = dir.0.join("nb.Notebook");
    std::fs::create_dir(&folder).unwrap();
    let current = folder.join("a.py");
    let stale = folder.join("old.py");
    std::fs::write(&current, b"existing").unwrap();
    std::fs::write(&stale, b"stale").unwrap();
    let body = json!({"definitionParts":[{
        "path":"/nb.Notebook/a.py",
        "payload":"aGVsbG8=",
        "payloadType":"InlineBase64"
    }]});
    let host = FakeHost::new(vec![response(200, &body.to_string(), &[])]);
    let (code, stdout, _) = run(
        &[
            "workspace",
            "get",
            WS,
            "--export-to",
            dir.0.to_str().unwrap(),
            "--overwrite",
        ],
        &host,
    )
    .await;
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("stale file"));
    assert_eq!(std::fs::read(&current).unwrap(), b"existing");
    assert_eq!(std::fs::read(&stale).unwrap(), b"stale");
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2);
}

#[tokio::test]
async fn item_export_checks_only_the_selected_item_folder() {
    let dir = TempDirectory::new();
    let folder = dir.0.join("nb.Notebook");
    std::fs::create_dir(&folder).unwrap();
    let stale = folder.join("old.py");
    std::fs::write(&stale, b"stale").unwrap();
    let unrelated = dir.0.join("unrelated.txt");
    std::fs::write(&unrelated, b"unrelated").unwrap();
    let args = [
        "item",
        "get",
        ITEM,
        "--workspace",
        WS,
        "--export-to",
        dir.0.to_str().unwrap(),
    ];
    let host = FakeHost::new(item_export_replies("a.py"));
    let (code, stdout, _) = run(&args, &host).await;
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("stale file"));
    assert!(!folder.join("a.py").exists());
    assert_eq!(std::fs::read(&stale).unwrap(), b"stale");

    std::fs::remove_file(stale).unwrap();
    let host = FakeHost::new(item_export_replies("a.py"));
    let (code, _, stderr) = run(&args, &host).await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(std::fs::read(folder.join("a.py")).unwrap(), b"hello");
    assert_eq!(std::fs::read(unrelated).unwrap(), b"unrelated");
}

#[tokio::test]
async fn cancellation_during_export_stops_before_publishing_the_file() {
    let dir = TempDirectory::new();
    let folder = dir.0.join("nb.Notebook");
    std::fs::create_dir(&folder).unwrap();
    let file = folder.join("a.py");
    std::fs::write(&file, b"existing").unwrap();
    let host = FakeHost::new(item_export_replies("a.py"));
    let (started, reached) = tokio::sync::oneshot::channel();
    let (release, resumed) = std::sync::mpsc::channel();
    *host.clock.save_gate.lock().unwrap() = Some(SaveGate {
        directory: folder.clone(),
        runtime_thread: std::thread::current().id(),
        started,
        release: resumed,
    });
    let cancellation = CancellationToken::new();
    let action_cancellation = cancellation.clone();
    let invocation = async {
        let args = [
            "fab-rs",
            "item",
            "get",
            ITEM,
            "--workspace",
            WS,
            "--export-to",
            dir.0.to_str().unwrap(),
            "--overwrite",
        ]
        .into_iter()
        .map(OsString::from);
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code =
            fabric_cli::run(args, &host, action_cancellation, &mut stdout, &mut stderr).await;
        (code, String::from_utf8(stdout).unwrap())
    };
    let cancel = async {
        tokio::time::timeout(Duration::from_secs(3), reached)
            .await
            .expect("export blocked the current-thread runtime")
            .expect("export worker did not reach publication");
        cancellation.cancel();
        release.send(()).unwrap();
    };
    let ((code, stdout), ()) = tokio::join!(invocation, cancel);
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("Cancelled"));
    assert_eq!(std::fs::read(file).unwrap(), b"existing");
    assert_eq!(std::fs::read_dir(folder).unwrap().count(), 1);
}

#[tokio::test]
async fn item_get_destination_rejects_traversal_parts() {
    let dir = TempDirectory::new();
    let host = FakeHost::new(item_export_replies("../escape"));
    let destination = dir.0.join("out");
    std::fs::create_dir(&destination).unwrap();
    let (code, _, _) = run(
        &[
            "item",
            "get",
            ITEM,
            "--workspace",
            WS,
            "--export-to",
            destination.to_str().unwrap(),
        ],
        &host,
    )
    .await;
    assert_eq!(code, 1);
    assert_eq!(host.transport.requests.lock().unwrap().len(), 2);
    assert!(!dir.0.join("escape").exists());
    assert!(!destination.join("escape").exists());
}

#[tokio::test]
async fn item_get_destination_rejects_traversal_in_service_display_name() {
    let dir = TempDirectory::new();
    let host = FakeHost::new(vec![
        response(200, r#"{"displayName":"../escape","type":"Notebook"}"#, &[]),
        response(
            200,
            r#"{"definition":{"parts":[{"path":"file","payload":"aA==","payloadType":"InlineBase64"}]}}"#,
            &[],
        ),
    ]);
    let (code, _, _) = run(
        &[
            "item",
            "get",
            ITEM,
            "--workspace",
            WS,
            "--export-to",
            dir.0.to_str().unwrap(),
        ],
        &host,
    )
    .await;
    assert_eq!(code, 1);
    assert_eq!(host.transport.requests.lock().unwrap().len(), 1);
    assert!(!dir.0.join("escape.Notebook").exists());
}

#[tokio::test]
async fn async_export_returns_accepted_without_writing() {
    let dir = TempDirectory::new();
    let host = FakeHost::new(vec![
        response(200, r#"{"displayName":"nb","type":"Notebook"}"#, &[]),
        response(202, "", &[("x-ms-operation-id", "op-1")]),
    ]);
    let (code, stdout, stderr) = run(
        &[
            "item",
            "get",
            ITEM,
            "--workspace",
            WS,
            "--export-to",
            dir.0.to_str().unwrap(),
            "--async",
        ],
        &host,
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(data(&stdout)["saved"], false);
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
}

#[tokio::test]
async fn old_definition_path_flags_are_rejected() {
    let dir = TempDirectory::new();
    let path = dir.0.to_str().unwrap();
    let cases = [
        vec![
            "item",
            "get",
            ITEM,
            "--workspace",
            WS,
            "--destination",
            path,
        ],
        vec!["item", "update", ITEM, "--workspace", WS, "--source", path],
        vec!["workspace", "get", WS, "--destination", path],
        vec!["workspace", "update", WS, "--source", path],
    ];
    for args in cases {
        let host = FakeHost::new(vec![]);
        let (code, _, _) = run(&args, &host).await;
        assert_ne!(code, 0, "{args:?}");
        assert!(host.transport.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn item_create_source_infers_type_from_platform() {
    let dir = TempDirectory::new();
    std::fs::write(
        dir.0.join(".platform"),
        br#"{"metadata":{"type":"Notebook"}}"#,
    )
    .unwrap();
    std::fs::write(dir.0.join("a.py"), b"hello").unwrap();
    let host = FakeHost::new(vec![response(201, r#"{"id":"x"}"#, &[])]);
    let (code, _, stderr) = run(
        &[
            "item",
            "create",
            "nb",
            "--workspace",
            WS,
            "--import-from",
            dir.0.to_str().unwrap(),
        ],
        &host,
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    let requests = host.transport.requests.lock().unwrap();
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].url.path(), format!("/v1/workspaces/{WS}/items"));
    let body = fabric_core_json::decode(&requests[0].body).unwrap();
    assert_eq!(body["displayName"], "nb");
    assert_eq!(body["type"], "Notebook");
    let parts = body["definition"]["parts"].as_array().unwrap();
    assert!(parts.iter().any(|part| part["path"] == "a.py"
        && part["payload"] == "aGVsbG8="
        && part["payloadType"] == "InlineBase64"));
    assert!(parts.iter().any(|part| part["path"] == ".platform"
        && part["payload"] == "eyJtZXRhZGF0YSI6eyJ0eXBlIjoiTm90ZWJvb2sifX0="));
}

#[tokio::test]
async fn item_create_source_rejects_type_mismatch_before_effects() {
    let dir = TempDirectory::new();
    std::fs::write(
        dir.0.join(".platform"),
        br#"{"metadata":{"type":"Notebook"}}"#,
    )
    .unwrap();
    let host = FakeHost::new(vec![]);
    let (code, _, _) = run(
        &[
            "item",
            "create",
            "nb",
            "--workspace",
            WS,
            "--type",
            "Report",
            "--import-from",
            dir.0.to_str().unwrap(),
        ],
        &host,
    )
    .await;
    assert_eq!(code, 1);
    assert!(host.transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn item_update_source_confirms_unless_forced() {
    let dir = TempDirectory::new();
    std::fs::write(dir.0.join("a.py"), b"hello").unwrap();
    let source = dir.0.to_str().unwrap();
    let args = [
        "item",
        "update",
        ITEM,
        "--workspace",
        WS,
        "--import-from",
        source,
    ];
    let host = FakeHost::new(vec![]);
    host.confirmations.lock().unwrap().push_back(Ok(false));
    let (code, _, _) = run(&args, &host).await;
    assert_eq!(code, 1);
    assert!(host.transport.requests.lock().unwrap().is_empty());
    let mut forced = args.to_vec();
    forced.push("--force");
    let host = FakeHost::new(vec![response(200, "", &[])]);
    let (code, _, stderr) = run(&forced, &host).await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(host.confirmation_calls.load(Ordering::SeqCst), 0);
    let requests = host.transport.requests.lock().unwrap();
    assert_eq!(
        requests[0].url.path(),
        format!("/v1/workspaces/{WS}/items/{ITEM}/updateDefinition")
    );
    let body = fabric_core_json::decode(&requests[0].body).unwrap();
    assert_eq!(body["definition"]["parts"][0]["path"], "a.py");
}

#[tokio::test]
async fn workspace_update_source_reports_failed_item_imports() {
    let dir = TempDirectory::new();
    std::fs::create_dir(dir.0.join("nb.Notebook")).unwrap();
    std::fs::write(dir.0.join("nb.Notebook").join("a.py"), b"hello").unwrap();
    let args = [
        "workspace",
        "update",
        WS,
        "--import-from",
        dir.0.to_str().unwrap(),
        "--force",
    ];
    for status in ["Failed", "SucceededDespiteFailures"] {
        let body = json!({"importItemDefinitionsDetails":[{
            "itemDisplayName":"report \u{1b} https://example.invalid/a?token=SECRET",
            "operationStatus":status
        }]});
        let host = FakeHost::new(vec![response(200, &body.to_string(), &[])]);
        let (code, stdout, _) = run(&args, &host).await;
        assert_eq!(code, 1);
        let result = &serde_json::from_str::<Value>(&stdout).unwrap()["result"];
        assert_eq!(result["error_code"], "Service");
        let message = result["message"].as_str().unwrap();
        assert!(message.contains("report"));
        assert!(message.contains(status));
        assert!(message.contains("remote changes may have been applied"));
        assert!(!message.contains('\u{1b}'));
        assert!(!message.contains("SECRET"));
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(
            requests[0].url.path(),
            format!("/v1/workspaces/{WS}/items/bulkImportDefinitions")
        );
        let body = fabric_core_json::decode(&requests[0].body).unwrap();
        assert_eq!(body["definitionParts"][0]["path"], "/nb.Notebook/a.py");
        assert!(body.get("options").is_none());
    }
    for body in [
        "",
        r#"{}"#,
        r#"{"importItemDefinitionsDetails":[]}"#,
        r#"{"importItemDefinitionsDetails":[{}]}"#,
        r#"{"importItemDefinitionsDetails":[{"operationStatus":"Unknown"}]}"#,
    ] {
        let host = FakeHost::new(vec![response(200, body, &[])]);
        let (code, stdout, _) = run(&args, &host).await;
        assert_eq!(code, 1, "{body}");
        assert!(stdout.contains("remote changes may have been applied"));
    }
}

#[tokio::test]
async fn workspace_import_accepts_by_path_and_only_pairs_by_name_when_requested() {
    let dir = TempDirectory::new();
    let report = dir.0.join("report.Report");
    std::fs::create_dir(&report).unwrap();
    std::fs::write(
        report.join("definition.pbir"),
        br#"{"datasetReference":{"byPath":{"path":"../model.SemanticModel"}}}"#,
    )
    .unwrap();
    for pairing in [false, true] {
        let mut args = vec![
            "workspace",
            "update",
            WS,
            "--import-from",
            dir.0.to_str().unwrap(),
            "--force",
        ];
        if pairing {
            args.push("--allow-pairing-by-name");
        }
        let host = FakeHost::new(vec![response(
            200,
            r#"{"importItemDefinitionsDetails":[{"operationStatus":"Succeeded"}]}"#,
            &[],
        )]);
        let (code, stdout, _) = run(&args, &host).await;
        assert_eq!(code, 0, "{stdout}");
        let requests = host.transport.requests.lock().unwrap();
        let body = fabric_core_json::decode(&requests[0].body).unwrap();
        assert_eq!(
            body["definitionParts"][0]["path"],
            "/report.Report/definition.pbir"
        );
        if pairing {
            assert_eq!(body["options"]["allowPairingByName"], true);
        } else {
            assert!(body.get("options").is_none());
        }
    }
    let host = FakeHost::new(vec![]);
    let (code, stdout, _) = run(
        &[
            "item",
            "update",
            ITEM,
            "--workspace",
            WS,
            "--import-from",
            report.to_str().unwrap(),
            "--force",
        ],
        &host,
    )
    .await;
    assert_eq!(code, 1);
    assert!(stdout.contains("byPath"));
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
    assert!(host.transport.requests.lock().unwrap().is_empty());

    let host = FakeHost::new(vec![]);
    let (code, _, _) = run(
        &[
            "workspace",
            "update",
            WS,
            "--payload",
            "{}",
            "--allow-pairing-by-name",
        ],
        &host,
    )
    .await;
    assert_eq!(code, 1);
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn root_and_subhelp_are_completely_effect_free() {
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["api", "--help"],
        vec!["auth"],
        vec!["auth", "--help"],
        vec!["auth", "login", "--help"],
        vec!["auth", "status", "--help"],
        vec!["auth", "logout", "--help"],
        vec!["--verbose", "--help"],
        vec!["--verbose", "--version"],
        vec!["--verbose", "api", "--help"],
        vec!["api", "--verbose", "--help"],
        vec!["--verbose", "auth"],
        vec!["auth", "--verbose"],
        vec!["auth", "--verbose", "login", "--help"],
        vec!["auth", "status", "--verbose", "--help"],
    ] {
        let host = FakeHost::new(vec![]);
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 0, "{args:?}");
        assert!(stdout.contains("fab-rs"));
        assert!(stderr.is_empty());
        assert_eq!(host.hooks.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn verbose_only_welcomes_without_host_effects() {
    let host = FakeHost::new(vec![]);
    let (code, stdout, stderr) = run(&["--verbose"], &host).await;
    assert_eq!(code, 0);
    assert_eq!(stdout, WELCOME);
    assert!(stderr.is_empty());
    assert_eq!(host.hooks.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn verbose_is_global_without_changing_stdout_or_option_values() {
    for args in [
        vec!["--verbose", "api", "workspaces"],
        vec!["api", "--verbose", "workspaces"],
        vec!["api", "workspaces", "--verbose"],
    ] {
        let host = FakeHost::new(vec![
            response(200, r#"{"value":[]}"#, &[]),
            response(200, r#"{"value":[]}"#, &[]),
        ]);
        let (code, stdout, stderr) = run(&args, &host).await;
        let plain = run(&["api", "workspaces"], &host).await;
        assert_eq!(code, 0, "{stdout}");
        assert_eq!(stdout, plain.1);
        let prefix = "[verbose] [2000-01-01T00:00:00.000Z] ";
        assert!(stderr.starts_with(prefix), "{stderr}");
        assert!(
            stderr
                .lines()
                .filter(|line| !line.starts_with("    "))
                .all(|line| line.starts_with(prefix)),
            "{stderr}"
        );
        assert!(!stderr.contains('\x1b'));
        assert!(
            plain.2.is_empty(),
            "verbosity must not leak between invocations"
        );
        assert_eq!(host.transport.requests.lock().unwrap().len(), 2);
    }
    for args in [
        vec!["--verbose", "auth", "status"],
        vec!["auth", "--verbose", "status"],
        vec!["auth", "status", "--verbose"],
    ] {
        let host = FakeHost::new(vec![]);
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 0, "{stdout}");
        assert_eq!(
            serde_json::from_str::<Value>(&stdout).unwrap()["command"],
            "auth status"
        );
        assert!(stderr.starts_with("[verbose] [2000-01-01T00:00:00.000Z] "));
        for secret in [TENANT, PRINCIPAL, "synthetic-token"] {
            assert!(!stderr.contains(secret));
        }
    }
    for args in [
        vec!["api", "--", "--verbose"],
        vec!["api", "items", "--query", "['--verbose']"],
    ] {
        let host = FakeHost::new(vec![response(200, "{}", &[])]);
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 0, "{stdout}");
        assert!(
            stderr.is_empty(),
            "an option value or endpoint is not a global flag"
        );
    }
    let host = FakeHost::new(vec![]);
    let (code, stdout, stderr) =
        run(&["--verbose", "api", "items", "--method", "POST"], &host).await;
    assert_eq!(code, 1);
    assert!(stdout.contains("use post instead of POST"));
    assert!(stderr.is_empty());
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn hidden_skill_is_global_without_confusing_command_names_or_metadata_paths() {
    for (args, skill) in [
        (vec!["--skill", "api", "api", "items"], "api"),
        (vec!["api", "--skill", "auth", "items"], "auth"),
        (vec!["api", "items", "--skill=status"], "status"),
        (vec!["--skill=--verbose", "api", "items"], "--verbose"),
        (vec!["api", "items", "--skill", ""], ""),
        (vec!["api", "items", "--skill", "   "], ""),
    ] {
        let host = FakeHost::new(vec![response(200, "{}", &[])]);
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 0, "{stdout}");
        assert!(stderr.is_empty(), "a skill value is not a verbose flag");
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(
            requests[0]
                .headers
                .get(fabric_core::FABRIC_SKILL_HEADER)
                .map(String::as_str),
            (!skill.is_empty()).then_some(skill),
        );
    }
    for args in [
        vec!["--help"],
        vec!["api", "--help"],
        vec!["auth", "--help"],
        vec!["auth", "login", "--help"],
        vec!["--skill", "api", "--version"],
        vec!["--skill", "auth"],
        vec!["auth", "--skill", "api"],
        vec!["--skill=api", "--verbose", "auth"],
    ] {
        let host = FakeHost::new(vec![]);
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 0, "{stdout}");
        assert!(!stdout.contains("--skill"));
        assert!(stderr.is_empty());
        assert_eq!(host.hooks.load(Ordering::SeqCst), 0);
        assert_eq!(host.environment_reads.load(Ordering::SeqCst), 0);
    }
    for args in [
        vec!["api", "items", "--skill", "SKILL-CANARY\r\nInjected: true"],
        vec!["api", "items", "--payload", "PAYLOAD-CANARY"],
        vec!["api", "items", "--headers", "Accept=HEADER-CANARY"],
    ] {
        let host = FakeHost::new(vec![]);
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 1, "{stdout}");
        assert!(!stdout.contains("CANARY"));
        assert!(!stderr.contains("CANARY"));
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
        assert!(host.transport.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn agent_hints_resolve_fixed_names_and_only_unambiguous_complete_guids() {
    const ID: &str = "aabbccdd-0000-4000-8000-000000000001";
    const UPPER: &str = "AABBCCDD-0000-4000-8000-000000000001";
    const OTHER: &str = "aabbccdd-0000-4000-8000-000000000002";
    type AgentCase<'a> = (&'a [(&'a str, &'a str)], Option<&'a str>, Option<&'a str>);
    let cases: &[AgentCase<'_>] = &[
        (&[], None, None),
        (&[("COPILOT_CLI", "1")], Some("copilot"), None),
        (
            &[("COPILOT_AGENT_SESSION_ID", ID)],
            Some("copilot"),
            Some(ID),
        ),
        (&[("COPILOT_AGENT_JOB_ID", ID)], Some("copilot"), None),
        (
            &[("CODEX_THREAD_ID", ID), ("CODEX_SESSION_ID", UPPER)],
            Some("codex"),
            Some(ID),
        ),
        (
            &[("CODEX_THREAD_ID", ID), ("CODEX_SESSION_ID", OTHER)],
            Some("codex"),
            None,
        ),
        (
            &[("CODEX_THREAD_ID", ID), ("CLAUDE_CODE_SESSION_ID", ID)],
            None,
            Some(ID),
        ),
        (
            &[("CLAUDE_CODE_SESSION_ID", ID), ("CLAUDECODE", "1")],
            Some("claude-code"),
            Some(ID),
        ),
        (
            &[("CLAUDE_CODE_CHILD_SESSION", "1")],
            Some("claude-code"),
            None,
        ),
        (
            &[("CURSOR_AGENT", "1"), ("CURSOR_SANDBOX", "1")],
            Some("cursor"),
            None,
        ),
        (&[("GEMINI_SESSION_ID", ID)], Some("gemini-cli"), Some(ID)),
        (&[("OPENCODE_PID", "123")], Some("opencode"), None),
        (
            &[(
                "AMP_CURRENT_THREAD_ID",
                "T-aabbccdd-0000-4000-8000-000000000001",
            )],
            Some("amp"),
            None,
        ),
        (
            &[(
                "KIRO_SESSION_ID",
                "sess_aabbccdd-0000-4000-8000-000000000001",
            )],
            Some("kiro"),
            None,
        ),
        (&[("KIRO_SESSION_ID", ID)], Some("kiro"), Some(ID)),
        (&[("COPILOT_CLI", "0"), ("GEMINI_CLI", "false")], None, None),
        (&[("COPILOT_CLI", ""), ("GEMINI_CLI", " ")], None, None),
        (&[("FAB_AGENT_SESSION_ID", ID)], None, None),
    ];
    for &(signals, kind, expected_id) in cases {
        let mut host = FakeHost::new(vec![response(200, "{}", &[])]);
        host.environment.extend(
            signals
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into())),
        );
        let (code, stdout, stderr) = run(
            &["--verbose", "--skill", "SKILL-CANARY", "api", "items"],
            &host,
        )
        .await;
        assert_eq!(code, 0, "{stdout}");
        for secret in [ID, UPPER, OTHER, "SKILL-CANARY"] {
            assert!(!stdout.contains(secret));
            assert!(!stderr.contains(secret));
        }
        let requests = host.transport.requests.lock().unwrap();
        let headers = &requests[0].headers;
        assert_eq!(
            headers
                .get(fabric_core::CLIENT_REQUEST_ID_HEADER)
                .map(String::as_str),
            expected_id
        );
        let suffix = kind.map(|kind| format!("; {kind}")).unwrap_or_default();
        assert_eq!(
            headers[fabric_core::USER_AGENT_HEADER],
            format!(
                "fabric-agent-cli/{} (api; {}; {}{suffix})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH,
            )
        );
    }
    for invalid_id in [
        " aabbccdd-0000-4000-8000-000000000001",
        "aabbccdd-0000-4000-8000-000000000001 ",
        "{aabbccdd-0000-4000-8000-000000000001}",
        "urn:uuid:aabbccdd-0000-4000-8000-000000000001",
        "aabbccdd000040008000000000000001",
        "aabbccdd-0000-4000-8000-00000000000z",
        "1",
    ] {
        let mut host = FakeHost::new(vec![response(200, "{}", &[])]);
        host.environment
            .insert("CODEX_THREAD_ID".into(), invalid_id.into());
        assert_eq!(run(&["api", "items"], &host).await.0, 0);
        assert!(
            !host.transport.requests.lock().unwrap()[0]
                .headers
                .contains_key(fabric_core::CLIENT_REQUEST_ID_HEADER)
        );
    }
}

#[tokio::test]
async fn cli_metadata_scopes_skill_to_fabric_not_powerbi_with_the_same_token_scope() {
    for audience in ["fabric", "azure", "storage", "powerbi"] {
        let mut host = FakeHost::new(vec![response(200, "{}", &[])]);
        let id = "aabbccdd-0000-4000-8000-000000000001";
        host.environment.insert("CODEX_THREAD_ID".into(), id.into());
        let (code, stdout, _) = run(
            &[
                "api",
                "items",
                "--audience",
                audience,
                "--skill",
                "SKILL-CANARY",
            ],
            &host,
        )
        .await;
        assert_eq!(code, 0, "{stdout}");
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(
            requests[0]
                .headers
                .contains_key(fabric_core::FABRIC_SKILL_HEADER),
            audience == "fabric"
        );
        assert_eq!(
            requests[0].headers[fabric_core::CLIENT_REQUEST_ID_HEADER],
            id
        );
    }
}

#[tokio::test]
async fn protected_response_headers_stay_hidden_with_and_without_verbose() {
    for verbose in [false, true] {
        let host = FakeHost::new(vec![response(
            200,
            "{}",
            &[
                ("aUtHoRiZaTiOn", "RESPONSE-AUTH-CANARY"),
                ("pRoXy-AuThOrIzAtIoN", "RESPONSE-PROXY-CANARY"),
                ("x-visible", "visible"),
            ],
        )]);
        let mut args = vec![
            "api",
            "items",
            "--show-headers",
            "--query",
            "values(headers)",
        ];
        if verbose {
            args.push("--verbose");
        }
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 0, "{stdout}");
        assert_eq!(data(&stdout), json!(["visible"]));
        for secret in ["RESPONSE-AUTH-CANARY", "RESPONSE-PROXY-CANARY"] {
            assert!(!stdout.contains(secret));
            assert!(!stderr.contains(secret));
        }
        assert!(!stderr.to_ascii_lowercase().contains("authorization"));
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0]
                .headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("authorization")
                    && value == "Bearer synthetic-token")
        );
    }
}

struct DiagnosticWriter {
    bytes: Vec<u8>,
    fail_on: &'static str,
    fail_flush: bool,
    partial: bool,
    broken: bool,
    failed_flush_pending: bool,
    writes_after_failure: usize,
}

impl DiagnosticWriter {
    fn new(fail_on: &'static str, fail_flush: bool) -> Self {
        Self {
            bytes: Vec::new(),
            fail_on,
            fail_flush,
            partial: !fail_flush,
            broken: false,
            failed_flush_pending: false,
            writes_after_failure: 0,
        }
    }
}

impl Write for DiagnosticWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.broken {
            self.writes_after_failure += 1;
            return Err(io::Error::other("PRIVATE-IO-CANARY"));
        }
        if String::from_utf8_lossy(bytes).contains(self.fail_on) {
            if self.fail_flush {
                self.failed_flush_pending = true;
            } else {
                self.broken = true;
                if self.partial {
                    let length = bytes.len().min(12);
                    self.bytes.extend_from_slice(&bytes[..length]);
                    return Ok(length);
                }
                return Err(io::Error::other("PRIVATE-IO-CANARY"));
            }
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.failed_flush_pending {
            self.broken = true;
            return Err(io::Error::other("PRIVATE-FLUSH-CANARY"));
        }
        Ok(())
    }
}

#[tokio::test]
async fn verbose_output_failures_stop_without_replay_and_preserve_known_outcomes() {
    for (fail_on, expected_calls, accepted, terminal) in [
        ("Command: started", 0, false, false),
        ("HTTP #1 sending", 0, false, false),
        ("HTTP #1 received 202", 1, true, false),
        ("HTTP #2 sending", 1, true, false),
        ("Command: completed", 2, true, true),
    ] {
        for fail_flush in [false, true] {
            for asynchronous in [false, true] {
                if asynchronous && (expected_calls == 2 || fail_on == "HTTP #2 sending") {
                    continue;
                }
                let host = FakeHost::new(vec![
                    response(202, "{}", &[("x-ms-operation-id", "RECOVERY-CANARY")]),
                    response(200, r#"{"status":"Succeeded"}"#, &[]),
                ]);
                let mut stderr = DiagnosticWriter::new(fail_on, fail_flush);
                let mut stdout = Vec::new();
                let mut args = vec!["fab-rs", "--verbose", "api", "items", "--method", "post"];
                if asynchronous {
                    args.push("--async");
                }
                let code = tokio::time::timeout(
                    Duration::from_secs(2),
                    fabric_cli::run(
                        args.into_iter().map(OsString::from),
                        &host,
                        CancellationToken::new(),
                        &mut stdout,
                        &mut stderr,
                    ),
                )
                .await
                .expect("diagnostic acknowledgment must not deadlock");
                let output = String::from_utf8(stdout).unwrap();
                let logs = String::from_utf8(stderr.bytes).unwrap();
                assert!(stderr.broken, "failure trigger missed: {fail_on}: {logs}");
                assert_eq!(code, 1, "{fail_on}: {output}");
                assert_eq!(
                    host.transport.requests.lock().unwrap().len(),
                    expected_calls,
                    "{fail_on}"
                );
                assert!(output.contains("\"Diagnostic\""), "{fail_on}: {output}");
                assert_eq!(output.contains("RECOVERY-CANARY"), accepted, "{output}");
                assert!(!logs.contains("RECOVERY-CANARY"));
                assert!(!logs.contains("PRIVATE-"));
                assert!(!output.contains("PRIVATE-"));
                assert!(stderr.writes_after_failure <= usize::from(!fail_flush));
                if expected_calls == 0 {
                    assert!(output.contains("no request was sent"), "{output}");
                }
                if terminal {
                    assert!(output.contains("completed"), "{output}");
                }
            }
        }
    }
}

#[tokio::test]
async fn verbose_failures_preserve_raw_response_and_uncertain_send_outcomes() {
    for (fail_on, transport_failure, status, asynchronous) in [
        ("HTTP #1 received 200", false, 200, false),
        ("HTTP #1 received 200", false, 200, true),
        ("HTTP #1 failed", true, 200, false),
        ("HTTP #1 failed", true, 200, true),
        ("Command: completed", false, 202, true),
    ] {
        for fail_flush in [false, true] {
            let mut host = FakeHost::new(vec![response(
                status,
                "{}",
                &[("x-ms-operation-id", "RECOVERY-CANARY")],
            )]);
            if transport_failure {
                Arc::get_mut(&mut host.transport).unwrap().error =
                    Some(CoreError::new(ErrorKind::Transport, "TRANSPORT-CANARY"));
            }
            let mut stderr = DiagnosticWriter::new(fail_on, fail_flush);
            let mut stdout = Vec::new();
            let mut args = vec!["fab-rs", "--verbose", "api", "items", "--method", "post"];
            if asynchronous {
                args.push("--async");
            }
            let code = tokio::time::timeout(
                Duration::from_secs(2),
                fabric_cli::run(
                    args.into_iter().map(OsString::from),
                    &host,
                    CancellationToken::new(),
                    &mut stdout,
                    &mut stderr,
                ),
            )
            .await
            .unwrap();
            let output = String::from_utf8(stdout).unwrap();
            let logs = String::from_utf8(stderr.bytes).unwrap();
            assert_eq!(code, 1, "{output}");
            assert_eq!(host.transport.requests.lock().unwrap().len(), 1);
            assert!(output.contains("\"Diagnostic\""), "{output}");
            assert!(stderr.broken, "missed {fail_on}");
            assert!(stderr.writes_after_failure <= usize::from(!fail_flush));
            if transport_failure {
                assert!(output.contains("uncertain"), "{output}");
            } else {
                assert!(
                    output.contains(&format!("HTTP status {status}")),
                    "{output}"
                );
            }
            assert!(!output.contains("no request was sent"));
            for secret in ["TRANSPORT-CANARY", "PRIVATE-"] {
                assert!(!output.contains(secret));
                assert!(!logs.contains(secret));
            }
            if status == 202 {
                assert!(output.contains("RECOVERY-CANARY"));
                assert!(output.contains("may continue"));
            }
            assert!(!logs.contains("RECOVERY-CANARY"));
        }
    }
}

#[tokio::test]
async fn service_codes_and_recovery_never_become_diagnostic_categories() {
    for service_code in ["ERROR-CODE-CANARY", "Unauthorized", "Diagnostic"] {
        let body = json!({
            "status": "Failed",
            "error": { "errorCode": service_code, "message": "ERROR-MESSAGE-CANARY" },
        })
        .to_string();
        let host = FakeHost::new(vec![
            response(202, "{}", &[("x-ms-operation-id", "RECOVERY-CANARY")]),
            response(200, &body, &[]),
        ]);
        let (code, stdout, stderr) =
            run(&["--verbose", "api", "items", "--method", "post"], &host).await;
        assert_eq!(code, 1, "{stdout}");
        let envelope: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(envelope["result"]["error_code"], service_code);
        assert!(stdout.contains("RECOVERY-CANARY"));
        assert!(
            stderr.contains("Command: failed (service failure)"),
            "{stderr}"
        );
        assert!(!stderr.contains("CANARY"));
        assert_eq!(host.transport.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn final_diagnostic_failure_keeps_response_status_after_body_read_failure() {
    for (status, asynchronous) in [(200, false), (202, true)] {
        for fail_flush in [false, true] {
            let mut host = FakeHost::new(vec![]);
            let transport = Arc::get_mut(&mut host.transport).unwrap();
            transport.error = Some(CoreError::new(ErrorKind::Transport, "BODY-READ-CANARY"));
            transport.error_head = Some(response(
                status,
                "",
                &[("x-ms-operation-id", "RECOVERY-CANARY")],
            ));
            let mut stderr = DiagnosticWriter::new("Command: failed", fail_flush);
            let mut stdout = Vec::new();
            let mut args = vec!["fab-rs", "--verbose", "api", "items", "--method", "post"];
            if asynchronous {
                args.push("--async");
            }
            let code = tokio::time::timeout(
                Duration::from_secs(2),
                fabric_cli::run(
                    args.into_iter().map(OsString::from),
                    &host,
                    CancellationToken::new(),
                    &mut stdout,
                    &mut stderr,
                ),
            )
            .await
            .unwrap();
            let output = String::from_utf8(stdout).unwrap();
            let logs = String::from_utf8(stderr.bytes).unwrap();
            assert_eq!(code, 1, "{output}");
            assert!(stderr.broken, "final-event trigger missed: {logs}");
            assert!(stderr.writes_after_failure <= usize::from(!fail_flush));
            assert_eq!(host.transport.requests.lock().unwrap().len(), 1);
            assert!(output.contains("\"Diagnostic\""), "{output}");
            assert!(
                output.contains(&format!("HTTP status {status}")),
                "{output}"
            );
            assert!(!output.contains("no request was sent"), "{output}");
            assert!(!output.contains("BODY-READ-CANARY"));
            assert!(!output.contains("PRIVATE-"));
            assert!(!logs.contains("CANARY"));
            assert!(!logs.contains("PRIVATE-"));
            if status == 202 {
                assert!(output.contains("RECOVERY-CANARY"), "{output}");
            }
        }
    }
}

#[tokio::test]
async fn both_output_streams_failing_do_not_retry_diagnostics_or_start_requests() {
    let host = FakeHost::new(vec![]);
    let mut stdout = DiagnosticWriter::new("", false);
    let mut stderr = DiagnosticWriter::new("Command: started", true);
    let code = fabric_cli::run(
        ["fab-rs", "--verbose", "api", "items"]
            .into_iter()
            .map(OsString::from),
        &host,
        CancellationToken::new(),
        &mut stdout,
        &mut stderr,
    )
    .await;
    assert_eq!(code, 1);
    assert!(stdout.broken && stderr.broken);
    assert_eq!(stderr.writes_after_failure, 0);
    assert!(host.transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn verbose_sanitizes_details_before_grouped_output_without_redacting_stdout() {
    const REQUEST_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
    let host = FakeHost::new(vec![response(
        202,
        r#"{"body":"BODY-CANARY"}"#,
        &[
            ("ReQuEsTiD", REQUEST_ID),
            (
                "Location",
                "/v1/operations/OPERATION-CANARY?token=QUERY-CANARY",
            ),
            ("Set-Cookie", "COOKIE-CANARY"),
            ("x-unknown", "UNKNOWN-CANARY"),
            ("Content-Type", "application/json; boundary=BOUNDARY-CANARY"),
        ],
    )]);
    let (code, stdout, stderr) = run(
        &[
            "--verbose",
            "api",
            "workspaces/WORKSPACE-CANARY/items?secret=INPUT-CANARY",
            "--async",
        ],
        &host,
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("BODY-CANARY"),
        "ordinary stdout is not a debug sink"
    );
    assert!(stderr.contains(&format!("    Request ID: {REQUEST_ID}\n")));
    assert!(
        stderr.contains(
            "    Location: https://api.fabric.microsoft.com/v1/operations/{operationId}\n"
        )
    );
    assert!(stderr.contains("https://api.fabric.microsoft.com/v1/workspaces/{workspaceId}/items"));
    for canary in [
        "OPERATION-CANARY",
        "QUERY-CANARY",
        "COOKIE-CANARY",
        "UNKNOWN-CANARY",
        "BOUNDARY-CANARY",
        "BODY-CANARY",
        "WORKSPACE-CANARY",
        "INPUT-CANARY",
        "synthetic-token",
        TENANT,
        PRINCIPAL,
    ] {
        assert!(!stderr.contains(canary), "{canary}: {stderr}");
    }
    assert!(!stderr.to_ascii_lowercase().contains("authorization"));
    assert!(!stderr.contains('\x1b'));
}

#[tokio::test]
async fn bare_auth_matches_explicit_help_without_host_effects() {
    let host = FakeHost::new(vec![]);
    *host.binding.lock().unwrap() = None;
    let bare = run(&["auth"], &host).await;
    let explicit = run(&["auth", "--help"], &host).await;
    assert_eq!(bare, explicit);
    assert_eq!(bare.0, 0);
    assert!(bare.2.is_empty());
    for text in [
        "Bind fab-rs to an existing Azure CLI identity.",
        "Usage: fab-rs auth [OPTIONS] <COMMAND>",
        "  login",
        "  status",
        "  logout",
        "Validate and bind the current Azure CLI tenant/principal",
        "Validate the selected Azure CLI identity",
        "Clear Rust selection only; leave Azure CLI signed in",
        "fab-rs does not sign in to or sign out of Azure CLI; run az login separately.",
    ] {
        assert!(bare.1.contains(text), "missing help text: {text}");
    }
    assert_eq!(host.hooks.load(Ordering::SeqCst), 0);
    assert!(host.binding.lock().unwrap().is_none());
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
    assert!(host.transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn bare_root_welcomes_without_host_effects() {
    for selected in [false, true] {
        let host = FakeHost::new(vec![]);
        let selection = selected.then(binding);
        *host.binding.lock().unwrap() = selection.clone();
        let (code, stdout, stderr) = run(&[], &host).await;
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        assert_eq!(stdout, WELCOME);
        assert_eq!(host.hooks.load(Ordering::SeqCst), 0);
        assert_eq!(*host.binding.lock().unwrap(), selection);
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
        assert!(host.transport.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn bare_root_ignores_selection_errors_without_modification() {
    let directory = TempDirectory::new();
    let path = directory.0.join("selection.json");
    let file = SelectionFile::new(path.clone());
    file.write(&binding()).unwrap();
    let contents = b"INVALID-SELECTION-SECRET-CANARY";
    std::fs::write(&path, contents).unwrap();
    let host = fabric_cli::NativeHost::with_selection_path(path.clone());
    let (code, stdout, stderr) = run(&[], &host).await;
    assert_eq!(code, 0);
    assert_eq!(stdout, WELCOME);
    assert!(stderr.is_empty());
    assert_eq!(std::fs::read(path).unwrap(), contents);
    let (_, help, help_stderr) = run(&["--help"], &host).await;
    assert!(help.contains("Usage:"));
    assert!(help_stderr.is_empty());
}

#[tokio::test]
async fn genuine_argument_errors_remain_sanitized_failures_without_auth_effects() {
    for args in [
        vec!["UNKNOWN-INPUT-CANARY"],
        vec!["auth", "UNKNOWN-INPUT-CANARY"],
        vec!["auth", "--UNKNOWN-INPUT-CANARY"],
        vec!["auth", "login", "--tenant", "UNKNOWN-INPUT-CANARY"],
        vec!["auth", "logout", "UNKNOWN-INPUT-CANARY"],
        vec!["api"],
        vec!["api", "--method", "get"],
        vec!["api", "items", "--method"],
        vec!["api", "items", "--UNKNOWN-INPUT-CANARY"],
        vec!["api", "items", "--method", "UNKNOWN-INPUT-CANARY"],
        vec!["api", "items", "--audience", "UNKNOWN-INPUT-CANARY"],
    ] {
        let host = FakeHost::new(vec![]);
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 1, "{args:?}");
        let envelope: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(envelope["status"], "Failure");
        assert_eq!(envelope["result"]["error_code"], "InvalidInput");
        assert!(!stdout.contains("UNKNOWN-INPUT-CANARY"));
        assert!(stderr.is_empty());
        // Failure timestamps are the only permitted host effect.
        assert_eq!(host.hooks.load(Ordering::SeqCst), 1);
        assert_eq!(*host.binding.lock().unwrap(), Some(binding()));
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
        assert!(host.transport.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn invalid_global_option_cannot_dispatch_any_command_family() {
    for args in [
        ["--verbose=BAD-VERBOSE-CANARY", "auth", "logout"],
        ["--verbose=BAD-VERBOSE-CANARY", "api", "workspaces"],
    ] {
        let host = FakeHost::new(vec![]);
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 1, "{args:?}");
        let envelope: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(envelope["result"]["error_code"], "InvalidInput");
        assert!(!stdout.contains("BAD-VERBOSE-CANARY"));
        assert!(stderr.is_empty());
        // Only the failure envelope reads the clock.
        assert_eq!(host.hooks.load(Ordering::SeqCst), 1);
        assert_eq!(*host.binding.lock().unwrap(), Some(binding()));
    }
}

#[tokio::test]
async fn rejected_headers_precede_payload_file_access() {
    let host = FakeHost::new(vec![]);
    let (code, stdout, stderr) = run(
        &[
            "api",
            "items",
            "--headers",
            r#"{"Authorization":"HEADER-CANARY"}"#,
            "--payload",
            "/MISSING-PAYLOAD-CANARY.json",
        ],
        &host,
    )
    .await;
    assert_eq!(code, 1, "{stdout}");
    let envelope: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(envelope["result"]["error_code"], "InvalidInput");
    assert!(
        envelope["result"]["message"]
            .as_str()
            .unwrap()
            .contains("--headers")
    );
    assert!(!stdout.contains("CANARY"));
    assert!(stderr.is_empty());
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
    assert!(host.transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn missing_item_workspace_is_actionable_in_the_existing_failure_envelope() {
    let host = FakeHost::new(vec![]);
    let (code, stdout, stderr) = run(&["item", "list"], &host).await;
    assert_eq!(code, 1);
    let envelope: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(envelope["status"], "Failure");
    assert_eq!(envelope["command"], "item list");
    assert_eq!(envelope["result"]["error_code"], "InvalidInput");
    let message = envelope["result"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("Missing required argument:")
            || message.starts_with("A required argument is missing."),
        "{message}"
    );
    assert!(message.ends_with("Run fab-rs item list --help."));
    assert!(stderr.is_empty());
    assert_eq!(host.hooks.load(Ordering::SeqCst), 1);
    assert_eq!(*host.binding.lock().unwrap(), Some(binding()));
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
    assert!(host.transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn parser_diagnostics_identify_schema_arguments_without_echoing_values() {
    for (args, details, help) in [
        (vec!["item", "update"], vec![], "fab-rs item update --help"),
        (
            vec!["workspace", "get"],
            vec![],
            "fab-rs workspace get --help",
        ),
        (
            vec!["api", "--headers", "authorization=SECRET-CANARY"],
            vec![],
            "fab-rs api --help",
        ),
        (
            vec!["item", "list", "--workspace"],
            vec![],
            "fab-rs item list --help",
        ),
        (
            vec!["workspace", "create", "SECRET-CANARY", "--payload"],
            vec![],
            "fab-rs workspace create --help",
        ),
        (
            vec!["api", "SECRET-CANARY", "--headers"],
            vec![],
            "fab-rs api --help",
        ),
        (
            vec!["api", "SECRET-CANARY", "--audience=SECRET-CANARY"],
            vec!["Invalid value for --audience <audience>."],
            "fab-rs api --help",
        ),
        (
            vec![
                "item",
                "list",
                "--workspace",
                "SECRET-CANARY",
                "--query",
                "FIRST-SECRET-CANARY",
                "--query",
                "SECOND-SECRET-CANARY",
            ],
            vec![],
            "fab-rs item list --help",
        ),
        (
            vec!["item"],
            vec!["A subcommand is required."],
            "fab-rs item --help",
        ),
        (
            vec!["item", "SECRET-CANARY"],
            vec!["Unrecognized command."],
            "fab-rs item --help",
        ),
        (
            vec![
                "item",
                "list",
                "--workspace",
                "SECRET-CANARY",
                "--SECRET-CANARY=SECRET-CANARY",
            ],
            vec!["Unrecognized argument or option."],
            "fab-rs item list --help",
        ),
    ] {
        let host = FakeHost::new(vec![]);
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 1, "{args:?}");
        let envelope: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(envelope["status"], "Failure");
        assert_eq!(envelope["result"]["error_code"], "InvalidInput");
        let message = envelope["result"]["message"].as_str().unwrap();
        for detail in details {
            assert!(message.contains(detail), "{args:?}: {message}");
        }
        assert!(message.ends_with(&format!("Run {help}.")), "{message}");
        assert!(!stdout.contains("CANARY"), "{stdout}");
        assert!(stderr.is_empty());
        assert_eq!(host.hooks.load(Ordering::SeqCst), 1);
        assert_eq!(*host.binding.lock().unwrap(), Some(binding()));
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
        assert!(host.transport.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn invalid_methods_explain_lowercase_values_without_disclosure_or_auth_effects() {
    for method in [
        "POST",
        "GET",
        "PoSt",
        "head",
        "OPTIONS",
        "",
        "METHOD-CANARY",
    ] {
        for inline in [false, true] {
            let host = FakeHost::new(vec![]);
            let inline_method = format!("--method={method}");
            let mut args = vec!["api", "workspaces/ENDPOINT-CANARY/jobs/execute/instances"];
            if inline {
                args.push(&inline_method);
            } else {
                args.extend(["--method", method]);
            }
            args.extend([
                "--headers",
                "authorization=HEADER-CANARY",
                "--payload",
                r#"{"token":"BODY-CANARY"}"#,
                "--query",
                "QUERY-CANARY",
            ]);
            let (code, stdout, stderr) = run(&args, &host).await;
            assert_eq!(code, 1, "{method:?}, inline={inline}");
            assert_eq!(
                serde_json::from_str::<Value>(&stdout).unwrap(),
                json!({
                    "timestamp": "2000-01-01T00:00:00Z",
                    "status": "Failure",
                    "command": "api",
                    "result": {
                        "error_code": "InvalidInput",
                        "message": "Invalid HTTP method for --method. Supported values are lowercase: get, post, delete, put, patch. For example, use post instead of POST."
                    }
                })
            );
            assert!(!stdout.contains("CANARY"));
            assert!(stderr.is_empty());
            // Failure timestamps are the only permitted host effect.
            assert_eq!(host.hooks.load(Ordering::SeqCst), 1);
            assert_eq!(*host.binding.lock().unwrap(), Some(binding()));
            assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
            assert!(host.transport.requests.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn lowercase_methods_still_reach_identity_validation_without_sending_requests() {
    for method in ["get", "post", "delete", "put", "patch"] {
        let host = FakeHost::new(vec![]);
        *host.binding.lock().unwrap() = None;
        let (code, stdout, stderr) = run(
            &[
                "api",
                "workspaces/items/jobs/execute/instances",
                "--method",
                method,
            ],
            &host,
        )
        .await;
        assert_eq!(code, 4, "{method}");
        let envelope: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(envelope["result"]["error_code"], "Unauthorized");
        assert_eq!(
            envelope["result"]["message"],
            "No Rust identity selected. Run fab-rs auth login."
        );
        assert!(stderr.is_empty());
        assert_eq!(host.hooks.load(Ordering::SeqCst), 2);
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
        assert!(host.transport.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn unrelated_parser_errors_do_not_receive_method_diagnostics() {
    for (args, expected) in [
        (
            vec!["api", "items", "--method", "post", "--audience", "POST"],
            "Invalid value for --audience <audience>. Supported values: fabric, storage, azure, powerbi. Run fab-rs api --help.",
        ),
        (
            vec!["api", "items", "--UNKNOWN-CANARY", "--method", "POST"],
            "Unrecognized argument or option. Run fab-rs api --help.",
        ),
        (
            vec!["auth", "login", "--method", "POST"],
            "Unrecognized argument or option. Run fab-rs auth login --help.",
        ),
    ] {
        let host = FakeHost::new(vec![]);
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 1);
        let envelope: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(envelope["result"]["error_code"], "InvalidInput");
        assert_eq!(envelope["result"]["message"], expected);
        assert!(!stdout.contains("CANARY"));
        assert!(stderr.is_empty());
        assert_eq!(host.hooks.load(Ordering::SeqCst), 1);
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
        assert!(host.transport.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn independent_python_envelope_bytes_and_unauthorized_exit_four() {
    let host = FakeHost::new(vec![response(200, r#"{"ok":true,"label":"café"}"#, &[])]);
    let (code, stdout, stderr) = run(&["api", "workspaces"], &host).await;
    assert_eq!(code, 0);
    assert_eq!(stdout, include_str!("fixtures/python-object.json"));
    assert!(stderr.is_empty());
    *host.binding.lock().unwrap() = None;
    let (code, stdout, stderr) = run(&["api", "workspaces"], &host).await;
    assert_eq!(code, 4);
    assert_eq!(stdout, include_str!("fixtures/python-failure.json"));
    assert!(stderr.is_empty());
    assert_eq!(host.transport.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn every_approved_method_and_audience_preserves_endpoint_query_and_header_equals() {
    for audience in ["fabric", "storage", "azure", "powerbi"] {
        for method in ["get", "post", "delete", "put", "patch"] {
            let host = FakeHost::new(vec![response(200, "{}", &[])]);
            let (code, _, _) = run(
                &[
                    "api",
                    "items?token=a%2Fb%3D%3D&n=2",
                    "--method",
                    method,
                    "--audience",
                    audience,
                    "--headers",
                    r#"{"If-Match":"a=b,c"}"#,
                    "--async",
                ],
                &host,
            )
            .await;
            assert_eq!(code, 0);
            let requests = host.transport.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method, method.to_uppercase());
            assert!(requests[0].body.is_empty());
            assert_eq!(requests[0].url.query(), Some("token=a%2Fb%3D%3D&n=2"));
            assert_eq!(requests[0].headers["if-match"], "a=b,c");
        }
    }
}

#[tokio::test]
async fn input_and_query_validation_precede_selection_and_credentials() {
    for args in [
        vec!["api", "https://evil.invalid/path"],
        vec!["api", "../unsafe"],
        vec!["api", "workspaces#fragment"],
        vec![
            "api",
            "workspaces",
            "--headers",
            r#"{"Authorization":"CANARY"}"#,
        ],
        vec![
            "api",
            "workspaces",
            "--headers",
            r#"{"Host":"evil.invalid"}"#,
        ],
        vec![
            "api",
            "workspaces",
            "--headers",
            r#"{"Accept":"value\r\nInjected=CANARY"}"#,
        ],
        vec!["api", "workspaces", "--query", "["],
        vec!["api", "workspaces", "--query", "[?displayName='CANARY']"],
        vec![
            "api",
            "workspaces",
            "--payload",
            "/nonexistent-fab-rs-fixture.json",
        ],
        vec!["api", "workspaces", "--payload", "{invalid}"],
    ] {
        let host = FakeHost::new(vec![]);
        let (code, stdout, _) = run(&args, &host).await;
        assert_eq!(code, 1, "{args:?}");
        assert!(!stdout.contains("CANARY"));
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
        assert!(host.transport.requests.lock().unwrap().is_empty());
        // Only timestamp rendering may call clock(); selection/provider/transport never run.
        assert_eq!(host.hooks.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn delete_rejects_all_supplied_inputs_before_loading_identity_or_sending() {
    let directory = TempDirectory::new();
    let json_file = directory.0.join("INPUT-CANARY.json");
    let null_file = directory.0.join("NULL-CANARY.json");
    let upload = directory.0.join("UPLOAD-CANARY.txt");
    let missing_file = directory.0.join("MISSING-CANARY.json");
    std::fs::write(&json_file, br#"{"description":"BODY-CANARY"}"#).unwrap();
    std::fs::write(&null_file, b"null").unwrap();
    std::fs::write(&upload, b"FILE-CANARY").unwrap();
    for input in [
        r#"{"description":"BODY-CANARY"}"#,
        json_file.to_str().unwrap(),
        null_file.to_str().unwrap(),
        upload.to_str().unwrap(),
        missing_file.to_str().unwrap(),
        "{INVALID-CANARY}",
        "",
        " ",
    ] {
        for audience in ["fabric", "storage", "azure", "powerbi"] {
            for asynchronous in [false, true] {
                let host = FakeHost::new(vec![]);
                if asynchronous {
                    *host.binding.lock().unwrap() = None;
                }
                let mut args = vec![
                    "api",
                    "workspaces/ENDPOINT-CANARY/items/ITEM-CANARY",
                    "--method",
                    "delete",
                    "--payload",
                    input,
                    "--audience",
                    audience,
                ];
                if asynchronous {
                    args.push("--async");
                }
                let (code, stdout, stderr) = run(&args, &host).await;
                assert_eq!(code, 1, "{input:?}, {audience}, async={asynchronous}");
                assert_eq!(
                    serde_json::from_str::<Value>(&stdout).unwrap(),
                    json!({
                        "timestamp": "2000-01-01T00:00:00Z",
                        "status": "Failure",
                        "command": "api",
                        "result": {
                            "error_code": "InvalidInput",
                            "message": "--payload cannot be combined with --method delete"
                        }
                    })
                );
                assert!(!stdout.contains("CANARY"));
                assert!(stderr.is_empty());
                assert_eq!(host.hooks.load(Ordering::SeqCst), 1);
                assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
                assert!(host.transport.requests.lock().unwrap().is_empty());
            }
        }
    }
}

#[tokio::test]
async fn invalid_static_query_functions_cannot_reach_auth_or_mutating_http() {
    for query in [
        "not_a_function(@)",
        "length()",
        "length(@, @)",
        "length(not_a_function(@))",
        "`true` || not_a_function(@)",
        "map(&length(), text.items)",
        "map(&not_a_function(@), text.items)",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(std::iter::once(vec!["a"; 129].join(".")))
    {
        let host = FakeHost::new(vec![]);
        *host.binding.lock().unwrap() = None;
        let (code, stdout, _) = run(
            &[
                "api",
                "items",
                "--method",
                "post",
                "--payload",
                "{}",
                "--query",
                query.as_str(),
            ],
            &host,
        )
        .await;
        assert_eq!(code, 1, "{stdout}");
        assert!(stdout.contains("InvalidInput"));
        assert!(!stdout.contains("Unauthorized"));
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
        assert!(host.transport.requests.lock().unwrap().is_empty());
        // Timestamp rendering only: no selection read, provider or transport setup.
        assert_eq!(host.hooks.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn zero_slice_queries_fail_before_selection_provider_or_mutation() {
    for query in ["[::0]", "map(&[::0], text.items)"] {
        let host = FakeHost::new(vec![]);
        *host.binding.lock().unwrap() = None;
        let (code, stdout, stderr) = run(
            &[
                "api",
                "items",
                "--method",
                "post",
                "--payload",
                "{}",
                "--query",
                query,
            ],
            &host,
        )
        .await;
        assert_eq!(code, 1, "{query}: {stdout}");
        assert!(stdout.contains("InvalidInput"));
        assert!(!stdout.contains("Unauthorized"));
        assert!(stderr.is_empty());
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
        assert!(host.transport.requests.lock().unwrap().is_empty());
        // Timestamp rendering only: no selection read, provider or transport setup.
        assert_eq!(host.hooks.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn unsupported_aliases_params_and_polling_knobs_fail_without_acquisition() {
    for option in [
        "-A",
        "-X",
        "-H",
        "-i",
        "-q",
        "--params",
        "--show_headers",
        "--wait",
        "--no-wait",
        "--lro-config",
    ] {
        let host = FakeHost::new(vec![]);
        let (code, stdout, _) = run(&["api", "workspaces", option], &host).await;
        assert_eq!(code, 1, "{option}");
        assert!(!stdout.contains("synthetic-token"));
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
    }
    for action in ["login", "status", "logout"] {
        let host = FakeHost::new(vec![]);
        assert_eq!(
            run(&["auth", action, "--tenant", "something"], &host)
                .await
                .0,
            1
        );
    }
}

#[tokio::test]
async fn raw_failures_empty_and_invalid_json_and_headers_match_source_contract() {
    for status in [400, 401, 403, 404, 429, 500, 503] {
        let host = FakeHost::new(vec![response(
            status,
            r#"{"error":{"code":"synthetic"}}"#,
            &[("x-fixture", "yes")],
        )]);
        let (code, stdout, _) = run(&["api", "items", "--show-headers"], &host).await;
        assert_eq!(code, 0);
        assert_eq!(data(&stdout)[0]["status_code"], status);
        assert_eq!(data(&stdout)[0]["headers"]["x-fixture"], "yes");
        assert_eq!(host.transport.requests.lock().unwrap().len(), 1);
    }
    let host = FakeHost::new(vec![response(204, " \n\t", &[])]);
    let (_, stdout, _) = run(&["api", "items"], &host).await;
    assert_eq!(data(&stdout)[0]["text"], "");
    assert!(data(&stdout)[0].get("headers").is_none());
    let host = FakeHost::new(vec![response(200, "not-json CANARY", &[])]);
    let (code, stdout, _) = run(&["api", "items"], &host).await;
    assert_eq!(code, 1);
    assert!(!stdout.contains("CANARY"));
    assert!(stdout.contains("InvalidJson"));
}

#[tokio::test]
async fn api_query_runs_on_response_object_preserves_scalars_and_keeps_lists_flat() {
    for (query, expected) in [
        ("text.items", json!([{"id":1},{"id":2}])),
        ("text.flag", json!(["False"])),
        ("text.zero", json!(["0"])),
        ("text.missing", json!(["None"])),
        ("items", json!(["None"])),
        ("status_code", json!(["200"])),
        ("headers.\"x-fixture\" ", json!(["yes"])),
    ] {
        let host = FakeHost::new(vec![response(
            200,
            r#"{"items":[{"id":1},{"id":2}],"flag":false,"zero":0}"#,
            &[("x-fixture", "yes")],
        )]);
        let (code, stdout, _) =
            run(&["api", "items", "--show-headers", "--query", query], &host).await;
        assert_eq!(code, 0);
        assert_eq!(data(&stdout), expected, "{query}");
    }
    for query in ["", ".", "@"] {
        let host = FakeHost::new(vec![response(
            200,
            r#"{"items":[],"empty":"","object":{}}"#,
            &[],
        )]);
        let (code, stdout, _) = run(&["api", "items", "--query", query], &host).await;
        assert_eq!(code, 0);
        assert_eq!(
            data(&stdout),
            json!([{"status_code":200,"text":{"items":[],"empty":"","object":{}}}]),
            "{query}"
        );
    }
}

#[tokio::test]
async fn explicit_query_selects_body_fields_and_rows_without_pagination_or_disclosure() {
    for (body, query, expected) in [
        (
            r#"{"value":[{"id":"a","displayName":"Sales"},{"id":"b","displayName":"Engineering"},{"id":"c","secret":"BODY-SECRET-CANARY"}],"continuationToken":"BODY-SECRET-CANARY"}"#,
            "text.value[].displayName",
            json!(["Sales", "Engineering"]),
        ),
        (
            r#"[{"displayName":"Sales"},{"displayName":"Engineering"},{"displayName":null}]"#,
            "text[].displayName",
            json!(["Sales", "Engineering"]),
        ),
        (
            r#"{"displayName":"Sales"}"#,
            "text.displayName",
            json!(["Sales"]),
        ),
        (
            r#"{"displayName":false}"#,
            "text.displayName",
            json!(["False"]),
        ),
        (r#"{"displayName":0}"#, "text.displayName", json!(["0"])),
        (r#"{"displayName":""}"#, "text.displayName", json!([""])),
        (r#"{"displayName":{}}"#, "text.displayName", json!([{}])),
        (
            r#"{"displayName":{"label":"Sales"}}"#,
            "text.displayName",
            json!([{"label":"Sales"}]),
        ),
        (
            r#"{"displayName":["Sales","Engineering"]}"#,
            "text.displayName",
            json!(["Sales", "Engineering"]),
        ),
        (r#"{"displayName":[]}"#, "text.displayName", json!([])),
        (
            r#"{"value":[],"secret":"BODY-SECRET-CANARY"}"#,
            "text.value[].displayName",
            json!([]),
        ),
        (
            r#"{"value":[{"secret":"BODY-SECRET-CANARY"}]}"#,
            "text.value[].displayName",
            json!([]),
        ),
        (
            r#"{"secret":"BODY-SECRET-CANARY"}"#,
            "text.displayName",
            json!(["None"]),
        ),
        (
            r#"{"displayName":null}"#,
            "text.displayName",
            json!(["None"]),
        ),
        (
            r#""BODY-SECRET-CANARY""#,
            "text.displayName",
            json!(["None"]),
        ),
        ("null", "text.displayName", json!(["None"])),
        ("42", "text.displayName", json!(["None"])),
        ("[]", "text[].displayName", json!([])),
        ("", "text.displayName", json!(["None"])),
    ] {
        for show_headers in [false, true] {
            let host = FakeHost::new(vec![response(
                200,
                body,
                &[("x-fixture", "HEADER-SECRET-CANARY")],
            )]);
            let mut args = vec!["api", "workspaces", "--query", query];
            if show_headers {
                args.push("--show-headers");
            }
            let (code, stdout, stderr) = run(&args, &host).await;
            assert_eq!(code, 0, "{body}: {stdout} {stderr}");
            assert_eq!(data(&stdout), expected, "{body}");
            assert!(stderr.is_empty());
            for canary in ["BODY-SECRET-CANARY", "HEADER-SECRET-CANARY"] {
                assert!(!stdout.contains(canary), "{body}: {stdout}");
            }
            assert_eq!(host.transport.requests.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn api_query_uses_explicit_wrapper_paths_without_implicit_unwrapping() {
    for (query, expected) in [
        (" displayName ", json!(["None"])),
        ("[].displayName", json!(["None"])),
        ("[?displayName=='Sales']", json!(["None"])),
        (
            "text.value[?displayName=='Sales']",
            json!([{"displayName":"Sales"}]),
        ),
        ("text.value[?displayName=='missing']", json!([])),
        ("length(text.value)", json!(["1"])),
        ("text.value[].displayName", json!(["Sales"])),
        ("result.data", json!(["None"])),
    ] {
        let host = FakeHost::new(vec![response(
            200,
            r#"{"displayName":"BODY-SECRET-CANARY","value":[{"displayName":"Sales"}]}"#,
            &[("x-fixture", "HEADER-SECRET-CANARY")],
        )]);
        let (code, stdout, stderr) = run(
            &["api", "workspaces", "--show-headers", "--query", query],
            &host,
        )
        .await;
        assert_eq!(code, 0, "{query}: {stdout} {stderr}");
        assert_eq!(data(&stdout), expected, "{query}");
        assert!(stderr.is_empty());
        assert!(!stdout.contains("BODY-SECRET-CANARY"), "{query}: {stdout}");
        assert!(
            !stdout.contains("HEADER-SECRET-CANARY"),
            "{query}: {stdout}"
        );
    }
}

#[tokio::test]
async fn workspace_filter_uses_explicit_response_body_paths_without_pagination() {
    for (body, query) in [
        (
            r#"{"value":[{"id":"a","displayName":"rustcli"},{"id":"b","displayName":"other","secret":"BODY-SECRET-CANARY"}],"continuationToken":"BODY-SECRET-CANARY"}"#,
            "text.value[?displayName=='rustcli']",
        ),
        (
            r#"[{"id":"a","displayName":"rustcli"},{"id":"b","displayName":"other","secret":"BODY-SECRET-CANARY"}]"#,
            "text[?displayName=='rustcli']",
        ),
        (
            r#"{"items":[{"id":"a","displayName":"rustcli"},{"id":"b","displayName":"other","secret":"BODY-SECRET-CANARY"}]}"#,
            "text.items[?displayName=='rustcli']",
        ),
    ] {
        let host = FakeHost::new(vec![response(
            200,
            body,
            &[("x-fixture", "HEADER-SECRET-CANARY")],
        )]);
        let (code, stdout, stderr) = run(
            &["api", "workspaces", "--show-headers", "--query", query],
            &host,
        )
        .await;
        assert_eq!(code, 0, "{query}: {stdout} {stderr}");
        assert_eq!(data(&stdout), json!([{"id":"a","displayName":"rustcli"}]));
        assert!(stderr.is_empty());
        assert!(!stdout.contains("BODY-SECRET-CANARY"));
        assert!(!stdout.contains("HEADER-SECRET-CANARY"));
        assert_eq!(host.transport.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn api_query_preserves_explicit_empty_null_and_scalar_body_projections() {
    for (body, query, expected) in [
        (
            r#"{"value":[],"secret":"BODY-SECRET-CANARY"}"#,
            "text.value",
            json!([]),
        ),
        (
            r#"{"value":{},"secret":"BODY-SECRET-CANARY"}"#,
            "text.value",
            json!([{}]),
        ),
        (
            r#"{"value":"","secret":"BODY-SECRET-CANARY"}"#,
            "text.value",
            json!([""]),
        ),
        (
            r#"{"value":null,"secret":"BODY-SECRET-CANARY"}"#,
            "text.value",
            json!(["None"]),
        ),
        (
            r#"{"value":false,"secret":"BODY-SECRET-CANARY"}"#,
            "text.value",
            json!(["False"]),
        ),
        (
            r#"{"value":0,"secret":"BODY-SECRET-CANARY"}"#,
            "text.value",
            json!(["0"]),
        ),
        (
            r#"{"value":"Sales","secret":"BODY-SECRET-CANARY"}"#,
            "text.value",
            json!(["Sales"]),
        ),
        (
            r#"{"value":{"displayName":"Sales"},"displayName":"BODY-SECRET-CANARY"}"#,
            "text.value.displayName",
            json!(["Sales"]),
        ),
        (
            r#"{"value":{"value":"Sales"},"secret":"BODY-SECRET-CANARY"}"#,
            "text.value.value",
            json!(["Sales"]),
        ),
        (
            r#"{"displayName":"Sales"}"#,
            "text.displayName",
            json!(["Sales"]),
        ),
        (r#"{"displayName":"Sales"}"#, "text.value", json!(["None"])),
        (r#""Sales""#, "text", json!(["Sales"])),
        ("", "text", json!([""])),
        ("null", "text", json!(["None"])),
        ("false", "text", json!(["False"])),
        ("0", "text", json!(["0"])),
        (
            r#"{"text":"Sales","status_code":"body","headers":{"x-fixture":"body"}}"#,
            "text.text",
            json!(["Sales"]),
        ),
        (
            r#"{"text":"Sales","status_code":"body","headers":{"x-fixture":"body"}}"#,
            "text.status_code",
            json!(["body"]),
        ),
        (
            r#"{"text":"Sales","status_code":"body","headers":{"x-fixture":"body"}}"#,
            "text.headers",
            json!([{"x-fixture":"body"}]),
        ),
    ] {
        let host = FakeHost::new(vec![response(
            200,
            body,
            &[("x-fixture", "HEADER-SECRET-CANARY")],
        )]);
        let (code, stdout, stderr) =
            run(&["api", "items", "--show-headers", "--query", query], &host).await;
        assert_eq!(code, 0, "{body} {query}: {stdout} {stderr}");
        assert_eq!(data(&stdout), expected, "{body} {query}");
        assert!(stderr.is_empty());
        assert!(!stdout.contains("BODY-SECRET-CANARY"), "{body}: {stdout}");
        assert!(!stdout.contains("HEADER-SECRET-CANARY"), "{body}: {stdout}");
    }
}

#[tokio::test]
async fn empty_api_query_results_do_not_disclose_unselected_response_data() {
    for (query, expected) in [
        ("text.items", json!([])),
        ("text.empty", json!([""])),
        ("text.object", json!([{}])),
        ("text.keys[?keyName == 'primary'].keyName", json!([])),
    ] {
        for show_headers in [false, true] {
            let host = FakeHost::new(vec![response(
                200,
                r#"{"items":[],"empty":"","object":{},"keys":[{"keyName":"key1","value":"BODY-SECRET-CANARY"}]}"#,
                &[("x-fixture", "HEADER-SECRET-CANARY")],
            )]);
            let mut args = vec!["api", "items", "--query", query];
            if show_headers {
                args.push("--show-headers");
            }
            let (code, stdout, stderr) = run(&args, &host).await;
            assert_eq!(code, 0, "{query}: {stdout} {stderr}");
            assert_eq!(data(&stdout), expected, "{query}");
            for canary in ["BODY-SECRET-CANARY", "HEADER-SECRET-CANARY"] {
                assert!(!stdout.contains(canary), "{query}: {stdout}");
                assert!(!stderr.contains(canary), "{query}: {stderr}");
            }
        }
    }
}

#[tokio::test]
async fn cli_empty_query_bypasses_projection_even_though_library_empty_query_inventories() {
    let payload = json!({"status_code":200,"text":{"items":[],"empty":"","object":{}}});
    assert_eq!(
        fabric_cli_query::search(&payload, "").unwrap(),
        json!(["status_code", "text.items", "text.empty"]),
    );
    for query in ["", "''", "\"\""] {
        let host = FakeHost::new(vec![response(
            200,
            r#"{"items":[],"empty":"","object":{}}"#,
            &[],
        )]);
        let (code, stdout, _) = run(&["api", "items", "--query", query], &host).await;
        assert_eq!(code, 0);
        assert_eq!(data(&stdout), json!([payload.clone()]));
    }
}

#[tokio::test]
async fn async_is_one_send_no_sleep_default_passes_actual_initial_to_polling() {
    for method in ["post", "put", "patch", "delete"] {
        let host = FakeHost::new(vec![response(
            202,
            r#"{"accepted":true}"#,
            &[("x-ms-operation-id", "synthetic")],
        )]);
        let (code, stdout, _) = run(
            &[
                "api",
                "items",
                "--method",
                method,
                "--async",
                "--show-headers",
            ],
            &host,
        )
        .await;
        assert_eq!(code, 0);
        assert_eq!(data(&stdout)[0]["status_code"], 202);
        assert_eq!(host.transport.requests.lock().unwrap().len(), 1);
        assert_eq!(host.clock.sleeps.load(Ordering::SeqCst), 0);
        let host = FakeHost::new(vec![
            response(
                202,
                "{}",
                &[(
                    "location",
                    "https://api.fabric.microsoft.com/v1/operations/synthetic",
                )],
            ),
            response(200, r#"{"status":"Succeeded"}"#, &[]),
        ]);
        let (code, stdout, _) = run(&["api", "items", "--method", method], &host).await;
        assert_eq!(code, 0, "{stdout}");
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, method.to_uppercase());
        assert_eq!(requests[1].method, "GET");
        assert!(requests[1].body.is_empty());
        assert_eq!(host.clock.sleeps.load(Ordering::SeqCst), 1);
        assert_eq!(data(&stdout)[0]["text"]["status"], "Succeeded");
    }
}

#[tokio::test]
async fn configured_fabric_origin_is_used_for_initial_status_and_result_requests() {
    let mut host = FakeHost::new(vec![
        response(202, "{}", &[("x-ms-operation-id", "synthetic")]),
        response(
            200,
            r#"{"status":"Succeeded"}"#,
            &[("location", "/v1/operations/synthetic/result")],
        ),
        response(200, r#"{"from":"result"}"#, &[]),
    ]);
    host.environment.insert(
        "FABRIC_API_ENDPOINT".into(),
        "msitapi.fabric.microsoft.com".into(),
    );
    host.environment
        .insert("AZURE_API_ENDPOINT".into(), "invalid.example.test".into());

    let (code, stdout, _) = run(&["api", "items", "--method", "post"], &host).await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(data(&stdout)[0]["text"]["from"], "result");
    let requests = host.transport.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|request| (request.method.as_str(), request.url.as_str()))
            .collect::<Vec<_>>(),
        [
            ("POST", "https://msitapi.fabric.microsoft.com/v1/items"),
            (
                "GET",
                "https://msitapi.fabric.microsoft.com/v1/operations/synthetic",
            ),
            (
                "GET",
                "https://msitapi.fabric.microsoft.com/v1/operations/synthetic/result",
            ),
        ],
    );
}

#[tokio::test]
async fn malformed_initial_json_fails_before_polling_with_safe_recovery() {
    for asynchronous in [false, true] {
        for body in [b"initial".as_slice(), b"\xffBODY-CANARY".as_slice()] {
            let mut initial = response(
                202,
                "",
                &[
                    ("x-ms-operation-id", "op-1"),
                    ("x-fixture", "HEADER-CANARY"),
                ],
            );
            initial.body = body.to_vec();
            let host = FakeHost::new(vec![
                initial,
                response(200, r#"{"status":"Succeeded"}"#, &[]),
            ]);
            let mut args = vec!["api", "items", "--method", "post", "--show-headers"];
            if asynchronous {
                args.push("--async");
            }
            let (code, stdout, stderr) = run(&args, &host).await;
            assert_eq!(code, 1, "{stdout}");
            let envelope: Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(envelope["status"], "Failure");
            assert_eq!(envelope["result"]["error_code"], "InvalidJson");
            assert_eq!(
                data(&stdout),
                json!([{
                    "operation_id": "op-1",
                    "status_url": "https://api.fabric.microsoft.com/v1/operations/op-1"
                }]),
            );
            assert!(!stdout.contains("CANARY"));
            assert!(!stdout.contains("synthetic-token"));
            assert!(stderr.is_empty());
            let requests = host.transport.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method, "POST");
            assert_eq!(host.clock.sleeps.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn empty_initial_body_remains_valid_with_default_polling_and_async() {
    for body in ["", " \t\r\n", "\u{1c}\u{1f}", "\u{2003}"] {
        for asynchronous in [false, true] {
            let host = FakeHost::new(vec![
                response(202, body, &[("x-ms-operation-id", "op-1")]),
                response(200, r#"{"status":"Succeeded"}"#, &[]),
            ]);
            let mut args = vec!["api", "items", "--method", "post"];
            if asynchronous {
                args.push("--async");
            }
            let (code, stdout, stderr) = run(&args, &host).await;
            assert_eq!(code, 0, "{stdout}");
            assert_eq!(
                stderr,
                if asynchronous {
                    ""
                } else {
                    "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 2s...\nChecking operation status...\nOperation status: Succeeded\nOperation completed.\n"
                }
            );
            let requests = host.transport.requests.lock().unwrap();
            assert_eq!(requests.len(), if asynchronous { 1 } else { 2 });
            assert_eq!(requests[0].method, "POST");
            if asynchronous {
                assert_eq!(data(&stdout)[0]["text"], "");
                assert_eq!(host.clock.sleeps.load(Ordering::SeqCst), 0);
            } else {
                assert_eq!(requests[1].method, "GET");
                assert_eq!(data(&stdout)[0]["text"]["status"], "Succeeded");
            }
        }
    }
}

#[tokio::test]
async fn arm_creation_reports_acceptance_without_the_http_code() {
    let host = FakeHost::new(vec![
        response(201, "{}", &[("azure-asyncoperation", "/status")]),
        response(200, r#"{"status":"Succeeded"}"#, &[]),
    ]);
    let (code, stdout, stderr) = run(
        &["api", "items", "--method", "post", "--audience", "azure"],
        &host,
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(
        stderr,
        "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 10s...\nChecking operation status...\nOperation status: Succeeded\nOperation completed.\n"
    );
    assert_eq!(host.transport.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn job_post_polls_status_by_default_and_async_returns_empty_initial_body() {
    for body in ["", " \t\r\n"] {
        for asynchronous in [false, true] {
            let location = "https://api.fabric.microsoft.com/v1/workspaces/workspace/items/item/jobs/instances/job";
            let terminal = r#"{"status":"Completed","id":"job"}"#;
            let host = FakeHost::new(vec![
                response(202, body, &[("Location", location), ("Retry-After", "3")]),
                response(200, r#"{"status":"InProgress"}"#, &[("Retry-After", "2")]),
                response(200, terminal, &[("x-terminal", "retained")]),
            ]);
            let mut args = vec![
                "api",
                "workspaces/workspace/items/item/jobs/execute/instances",
                "--method",
                "post",
                "--payload",
                "{}",
                "--headers",
                r#"{"If-Match":"mutation-only"}"#,
                "--show-headers",
            ];
            if asynchronous {
                args.push("--async");
            }
            let (code, stdout, stderr) = run(&args, &host).await;
            assert_eq!(code, 0, "{stdout}");
            assert_eq!(
                stderr,
                if asynchronous {
                    ""
                } else {
                    "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: InProgress\nWaiting for job completion; next status check in 7s...\nChecking job status...\nJob instance status: Completed\nJob completed.\n"
                }
            );
            let requests = host.transport.requests.lock().unwrap();
            assert_eq!(requests.len(), if asynchronous { 1 } else { 3 });
            assert_eq!(requests[0].method, "POST");
            assert_eq!(requests[0].body, b"{}");
            if asynchronous {
                assert_eq!(
                    data(&stdout),
                    json!([{
                        "status_code": 202,
                        "text": "",
                        "headers": {"Location": location, "Retry-After": "3"}
                    }])
                );
                assert_eq!(host.clock.sleeps.load(Ordering::SeqCst), 0);
            } else {
                assert_eq!(
                    data(&stdout),
                    json!([{
                        "status_code": 200,
                        "text": {"status": "Completed", "id": "job"},
                        "headers": {"x-terminal": "retained"}
                    }])
                );
                for request in &requests[1..] {
                    assert_eq!(request.method, "GET");
                    assert_eq!(request.url.as_str(), location);
                    assert!(request.body.is_empty());
                    assert!(!request.headers.contains_key("if-match"));
                }
                assert_eq!(host.clock.sleeps.load(Ordering::SeqCst), 2);
            }
        }
    }
}

#[tokio::test]
async fn job_polling_failure_retains_safe_handle_without_replaying_mutation() {
    let location = "/v1/workspaces/workspace/items/item/jobs/instances/job?secret=CANARY";
    for terminal in [
        r#"{"status":"Failed","failureReason":{"message":"BODY-CANARY"}}"#,
        r#"{"status":"Cancelled"}"#,
        r#"{"status":"Unknown"}"#,
    ] {
        let host = FakeHost::new(vec![
            response(202, "", &[("location", location)]),
            response(200, terminal, &[]),
        ]);
        let (code, stdout, stderr) = run(
            &[
                "api",
                "workspaces/workspace/items/item/jobs/execute/instances",
                "--method",
                "post",
                "--payload",
                "{}",
            ],
            &host,
        )
        .await;
        assert_eq!(code, 1, "{stdout}");
        assert_eq!(
            stderr,
            match terminal {
                r#"{"status":"Failed","failureReason":{"message":"BODY-CANARY"}}"# => {
                    "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: Failed\n"
                }
                r#"{"status":"Cancelled"}"# => {
                    "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: Cancelled\n"
                }
                _ =>
                    "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\n",
            }
        );
        assert!(!stdout.contains("secret=CANARY"));
        assert!(!stderr.contains("CANARY"));
        let envelope: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            envelope["result"]["error_code"],
            if terminal.contains("Unknown") {
                "Protocol"
            } else {
                "Service"
            }
        );
        if terminal.contains("Failed") {
            assert_eq!(envelope["result"]["message"], "Backend job failed");
            assert_eq!(
                envelope["result"]["failure_reason"],
                json!({"message":"BODY-CANARY"})
            );
        } else {
            assert!(envelope["result"].get("failure_reason").is_none());
            if terminal.contains("Cancelled") {
                assert_eq!(envelope["result"]["message"], "Backend job was cancelled");
            }
        }
        assert_eq!(
            data(&stdout),
            json!([{
                "operation_id": null,
                "status_url": "https://api.fabric.microsoft.com/v1/workspaces/workspace/items/item/jobs/instances/job"
            }])
        );
        assert_eq!(
            stdout.contains("Check status_url before retrying."),
            terminal.contains("Unknown")
        );
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[1].method, "GET");
        assert_eq!(requests[1].url.query(), Some("secret=CANARY"));
    }
}

#[tokio::test]
async fn job_failure_message_is_json_only_even_with_verbose_logging() {
    for verbose in [false, true] {
        let host = FakeHost::new(vec![
            response(
                202,
                "",
                &[(
                    "location",
                    "/v1/workspaces/workspace/items/item/jobs/instances/job?sig=QUERY-CANARY",
                )],
            ),
            response(
                200,
                r#"{"status":"Failed","failureReason":{"errorCode":"RefreshFailed","message":"DETAIL-CANARY\n\u001b[31m","details":[{"message":"OTHER-CANARY"}]},"unrelated":"BODY-CANARY"}"#,
                &[],
            ),
        ]);
        let mut args = vec!["api", "items", "--method", "post", "--payload", "{}"];
        if verbose {
            args.push("--verbose");
        }
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 1, "{stdout}");
        let envelope: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(envelope["status"], "Failure");
        assert_eq!(envelope["result"]["error_code"], "RefreshFailed");
        assert_eq!(envelope["result"]["message"], "Backend job failed");
        assert_eq!(
            envelope["result"]["failure_reason"],
            json!({"message":"DETAIL-CANARY\n\u{001b}[31m"})
        );
        assert!(!stdout.contains("outcome_uncertain"));
        assert!(!stdout.contains("Check status_url before retrying."));
        assert!(!stdout.contains("QUERY-CANARY"));
        assert!(!stdout.contains("OTHER-CANARY"));
        assert!(!stdout.contains("BODY-CANARY"));
        assert!(!stdout.contains('\x1b'));
        assert!(!stderr.contains("CANARY"));
        assert!(!stderr.contains("RefreshFailed"));
        assert!(stderr.contains("Job instance status: Failed\n"));
        let requests = host.transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[1].method, "GET");
    }
}

#[tokio::test]
async fn job_terminal_query_does_not_disclose_unselected_payload() {
    let host = FakeHost::new(vec![
        response(
            202,
            "",
            &[(
                "location",
                "/v1/workspaces/workspace/items/item/jobs/instances/job",
            )],
        ),
        response(
            200,
            r#"{"status":"Completed","secret":"CANARY","values":[]}"#,
            &[],
        ),
    ]);
    let (code, stdout, stderr) = run(
        &[
            "api",
            "workspaces/workspace/items/item/jobs/execute/instances",
            "--method",
            "post",
            "--payload",
            "{}",
            "--query",
            "text.values",
        ],
        &host,
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(
        stderr,
        "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: Completed\nJob completed.\n"
    );
    assert_eq!(data(&stdout), json!([]));
    assert!(!stdout.contains("CANARY"));
    assert_eq!(host.transport.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn accepted_response_conversion_failures_retain_safe_initial_recovery() {
    for (asynchronous, verbose) in [(true, false), (true, true), (false, false), (false, true)] {
        for (body, query, error_code) in [
            (b"{BODY-CANARY".as_slice(), None, "InvalidJson"),
            (b"\xffBODY-CANARY".as_slice(), None, "InvalidJson"),
            (
                br#"{"value":42,"secret":"BODY-CANARY"}"#.as_slice(),
                Some("length(text.value)"),
                "InvalidInput",
            ),
        ] {
            let mut converted = response(
                if asynchronous { 202 } else { 200 },
                "",
                &[("x-fixture", "HEADER-CANARY")],
            );
            converted.body = body.to_vec();
            let initial_headers = [("location", "/v1/operations/op-1?secret=CANARY")];
            let responses = if asynchronous {
                converted.headers.extend(
                    initial_headers
                        .iter()
                        .map(|(name, value)| (name.to_string(), value.to_string())),
                );
                vec![converted]
            } else {
                vec![
                    response(202, "{}", &initial_headers),
                    response(
                        200,
                        r#"{"status":"Succeeded"}"#,
                        &[("location", "/v1/workspaces/result?secret=RESULT-CANARY")],
                    ),
                    converted,
                ]
            };
            let host = FakeHost::new(responses);
            let mut args = vec![
                "api",
                "items",
                "--method",
                "post",
                "--payload",
                "{}",
                "--headers",
                r#"{"If-Match":"REQUEST-CANARY"}"#,
                "--show-headers",
            ];
            if asynchronous {
                args.push("--async");
            }
            if verbose {
                args.push("--verbose");
            }
            if let Some(query) = query {
                args.extend(["--query", query]);
            }
            let (code, stdout, stderr) = run(&args, &host).await;
            assert_eq!(code, 1, "{stdout}");
            let envelope: Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(envelope["status"], "Failure");
            assert_eq!(envelope["result"]["error_code"], error_code);
            assert_eq!(
                data(&stdout),
                json!([{
                    "operation_id": "op-1",
                    "status_url": "https://api.fabric.microsoft.com/v1/operations/op-1"
                }]),
            );
            assert_eq!(
                stdout.contains("Check status_url before retrying."),
                asynchronous
            );
            assert!(!stdout.contains("CANARY"));
            assert!(!stdout.contains("synthetic-token"));
            if !verbose {
                assert_eq!(
                    stderr,
                    if asynchronous {
                        ""
                    } else {
                        "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 2s...\nChecking operation status...\nOperation status: Succeeded\n"
                    }
                );
            } else {
                assert_eq!(
                    stderr.contains("Operation status: Succeeded\n"),
                    !asynchronous
                );
                assert!(!stderr.contains("CANARY"));
            }
            let requests = host.transport.requests.lock().unwrap();
            assert_eq!(requests.len(), if asynchronous { 1 } else { 3 });
            assert_eq!(requests[0].method, "POST");
            assert!(requests[1..].iter().all(|request| request.method == "GET"));
            if asynchronous {
                assert_eq!(host.clock.sleeps.load(Ordering::SeqCst), 0);
            }
        }
    }
}

#[tokio::test]
async fn async_invalid_handles_remain_raw_and_conversion_only_retains_safe_evidence() {
    for known_id in [false, true] {
        for body in ["{}", "{BODY-CANARY"] {
            let mut headers = vec![(
                "location",
                "http://evil.invalid/operations/UNSAFE-CANARY?secret=CANARY",
            )];
            if known_id {
                headers.push(("x-ms-operation-id", "op-1"));
            }
            let host = FakeHost::new(vec![response(202, body, &headers)]);
            let (code, stdout, stderr) =
                run(&["api", "items", "--method", "post", "--async"], &host).await;
            let envelope: Value = serde_json::from_str(&stdout).unwrap();
            if body == "{}" {
                assert_eq!(code, 0, "{stdout}");
                assert_eq!(data(&stdout)[0]["status_code"], 202);
            } else {
                assert_eq!(code, 1, "{stdout}");
                assert_eq!(envelope["result"]["error_code"], "InvalidJson");
                if known_id {
                    assert_eq!(
                        data(&stdout),
                        json!([{
                            "operation_id": "op-1",
                            "status_url": "https://api.fabric.microsoft.com/v1/operations/op-1"
                        }]),
                    );
                } else {
                    assert!(envelope["result"].get("data").is_none());
                }
            }
            assert!(!stdout.contains("CANARY"));
            assert!(!stdout.contains("synthetic-token"));
            assert!(stderr.is_empty());
            let requests = host.transport.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method, "POST");
            assert_eq!(host.clock.sleeps.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn login_status_logout_selection_required_and_no_network() {
    let host = FakeHost::new(vec![]);
    *host.binding.lock().unwrap() = None;
    assert_eq!(run(&["auth", "status"], &host).await.0, 4);
    assert_eq!(run(&["auth", "login"], &host).await.0, 0);
    assert_eq!(run(&["auth", "status"], &host).await.0, 0);
    assert_eq!(run(&["auth", "logout"], &host).await.0, 0);
    assert_eq!(run(&["auth", "logout"], &host).await.0, 0);
    assert_eq!(run(&["api", "items"], &host).await.0, 4);
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 1);
    assert!(host.transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn explicit_status_reports_only_validated_binding_metadata() {
    let host = FakeHost::new(vec![]);
    let status = run(&["auth", "status"], &host).await;
    assert_eq!(status.0, 0);
    assert_eq!(
        data(&status.1),
        json!([{
            "logged_in": true,
            "authentication_mode": "Azure CLI",
            "tenant_id": TENANT,
            "principal_id": PRINCIPAL,
        }])
    );
    assert!(!status.1.contains("synthetic-token"));
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 1);
    assert!(host.transport.requests.lock().unwrap().is_empty());

    *host.binding.lock().unwrap() = None;
    let status = run(&["auth", "status"], &host).await;
    assert_eq!(status.0, 4);
    assert!(
        status
            .1
            .contains("No Rust identity selected. Run fab-rs auth login.")
    );
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 1);
    assert!(host.binding.lock().unwrap().is_none());
}

#[tokio::test]
async fn login_reports_the_validated_binding_without_claiming_service_access() {
    let host = FakeHost::new(vec![]);
    *host.binding.lock().unwrap() = None;
    let login = run(&["auth", "login"], &host).await;
    let status = run(&["auth", "status"], &host).await;
    assert_eq!(login.0, 0);
    assert_eq!(status.0, 0);
    assert_eq!(
        data(&login.1),
        json!([{
            "selected": true,
            "logged_in": true,
            "authentication_mode": "Azure CLI",
            "tenant_id": TENANT,
            "principal_id": PRINCIPAL,
            "mfa_evidence": "not_checked",
            "service_access_verified": false,
        }])
    );
    assert_eq!(*host.binding.lock().unwrap(), Some(binding()));
    assert!(!login.1.contains("synthetic-token"));
    assert!(host.transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn sdk_auth_failure_is_sanitized_unauthorized_and_never_sends() {
    for args in [
        vec!["api", "items"],
        vec!["auth", "status"],
        vec!["--verbose", "api", "items"],
        vec!["auth", "status", "--verbose"],
    ] {
        let mut host = FakeHost::new(vec![]);
        host.credential = Arc::new(Credential {
            calls: AtomicUsize::new(0),
            error: true,
            hang: false,
            dropped: AtomicBool::new(false),
        });
        let (code, stdout, stderr) = run(&args, &host).await;
        assert_eq!(code, 4);
        assert!(stdout.contains("Unauthorized"));
        assert!(!stdout.contains("SDK-ERROR-CANARY"));
        assert_eq!(
            serde_json::from_str::<Value>(&stdout).unwrap()["status"],
            "Failure"
        );
        if args.contains(&"--verbose") {
            assert!(stderr.contains("Command: failed (unauthorized)"));
            assert!(!stderr.contains("SDK-ERROR-CANARY"));
            assert!(!stderr.contains("synthetic-token"));
        } else {
            assert!(stderr.is_empty());
        }
        assert!(host.transport.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn api_and_auth_status_cancel_sdk_futures_without_sending() {
    for args in [
        vec!["fab-rs", "api", "items"],
        vec!["fab-rs", "auth", "status"],
    ] {
        let mut host = FakeHost::new(vec![]);
        host.credential = Arc::new(Credential {
            calls: AtomicUsize::new(0),
            error: false,
            hang: true,
            dropped: AtomicBool::new(false),
        });
        let cancellation = CancellationToken::new();
        let signal = cancellation.clone();
        let cancel = async {
            tokio::task::yield_now().await;
            signal.cancel();
        };
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let (code, ()) = tokio::join!(
            fabric_cli::run(
                args.into_iter().map(OsString::from),
                &host,
                cancellation,
                &mut stdout,
                &mut stderr
            ),
            cancel,
        );
        assert_eq!(code, 1);
        assert!(String::from_utf8(stdout).unwrap().contains("Cancelled"));
        assert!(host.credential.dropped.load(Ordering::SeqCst));
        assert_eq!(host.credential.calls.load(Ordering::SeqCst), 1);
        assert!(host.transport.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn api_and_auth_status_bound_hanging_sdk_credentials_by_deadline() {
    for args in [vec!["api", "items"], vec!["auth", "status"]] {
        let mut host = FakeHost::new(vec![]);
        host.credential = Arc::new(Credential {
            calls: AtomicUsize::new(0),
            error: false,
            hang: true,
            dropped: AtomicBool::new(false),
        });
        let (code, stdout, _) = run(&args, &host).await;
        assert_eq!(code, 1);
        assert!(stdout.contains("Timeout"));
        assert!(host.credential.dropped.load(Ordering::SeqCst));
        assert!(host.transport.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn invalid_retry_after_is_a_safe_stderr_warning_not_json_progress_or_header_dump() {
    let host = FakeHost::new(vec![
        response(
            202,
            "{}",
            &[
                ("x-ms-operation-id", "synthetic"),
                ("retry-after", "HEADER-CANARY"),
            ],
        ),
        response(200, r#"{"status":"Succeeded"}"#, &[]),
    ]);
    let (code, stdout, stderr) = run(&["api", "items", "--method", "post"], &host).await;
    assert_eq!(code, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["status"],
        "Success"
    );
    assert!(stderr.contains("Invalid Retry-After"));
    assert!(!stderr.contains("HEADER-CANARY"));
    assert!(!stdout.contains("HEADER-CANARY"));
}

struct TempDirectory(PathBuf);
impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "fab-rs-offline-{}-{}",
            std::process::id(),
            FILE_ID.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn bodyless_methods_and_null_files_keep_python_default_or_custom_content_type() {
    let directory = TempDirectory::new();
    let null_file = directory.0.join("null.json");
    std::fs::write(&null_file, b"null").unwrap();
    for method in ["get", "post", "put", "patch", "delete"] {
        for input in [None, Some(null_file.to_str().unwrap())] {
            if method == "delete" && input.is_some() {
                continue;
            }
            for custom in [false, true] {
                let mut args = vec!["api", "items", "--method", method];
                if let Some(input) = input {
                    args.extend(["--payload", input]);
                }
                if custom {
                    args.extend(["--headers", r#"{"cOnTeNt-TyPe":"application/custom"}"#]);
                }
                let host = FakeHost::new(vec![response(200, "{}", &[])]);
                let (code, output, _) = run(&args, &host).await;
                assert_eq!(code, 0, "{output}");
                let requests = host.transport.requests.lock().unwrap();
                assert_eq!(requests.len(), 1);
                let request = &requests[0];
                assert_eq!(request.method, method.to_uppercase());
                assert_eq!(
                    request.url.as_str(),
                    "https://api.fabric.microsoft.com/v1/items"
                );
                assert_eq!(request.body.as_slice(), b"");
                assert_eq!(request.headers["authorization"], "Bearer synthetic-token");
                assert_eq!(
                    request.headers["content-type"],
                    if custom {
                        "application/custom"
                    } else {
                        "application/json"
                    },
                );
            }
        }
    }
}

#[tokio::test]
async fn arbitrary_integer_request_and_response_bytes_match_independent_python_numbers() {
    let source = r#"{"positive":18446744073709551617,"negative":-9223372036854775809,"huge":123456789012345678901234567890123456789012345678901234567890,"small_negative":-1,"zero":0}"#;
    let expected_request = r#"{"positive": 18446744073709551617, "negative": -9223372036854775809, "huge": 123456789012345678901234567890123456789012345678901234567890, "small_negative": -1, "zero": 0}"#;
    let directory = TempDirectory::new();
    let input_file = directory.0.join("integers.json");
    std::fs::write(&input_file, source).unwrap();
    for input in [source, input_file.to_str().unwrap()] {
        let host = FakeHost::new(vec![response(200, source, &[])]);
        let (code, stdout, _) = run(
            &["api", "items", "--method", "post", "--payload", input],
            &host,
        )
        .await;
        assert_eq!(code, 0);
        assert_eq!(
            host.transport.requests.lock().unwrap()[0].body,
            expected_request.as_bytes()
        );
        for exact in [
            "\"positive\": 18446744073709551617",
            "\"negative\": -9223372036854775809",
            "\"huge\": 123456789012345678901234567890123456789012345678901234567890",
            "\"small_negative\": -1",
            "\"zero\": 0",
        ] {
            assert!(stdout.contains(exact), "{stdout}");
        }
    }
    for (query, expected) in [
        ("text.positive", "18446744073709551617"),
        ("text.negative", "-9223372036854775809"),
        (
            "text.huge",
            "123456789012345678901234567890123456789012345678901234567890",
        ),
        ("to_string(text)", source),
    ] {
        let host = FakeHost::new(vec![response(200, source, &[])]);
        let (code, stdout, _) = run(&["api", "items", "--query", query], &host).await;
        assert_eq!(code, 0, "{stdout}");
        assert_eq!(data(&stdout), json!([expected]));
    }
    let host = FakeHost::new(vec![response(
        200,
        "[18446744073709551617,-9223372036854775809]",
        &[],
    )]);
    let (code, stdout, _) = run(&["api", "items", "--query", "text"], &host).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("18446744073709551617"));
    assert!(stdout.contains("-9223372036854775809"));
    assert_eq!(data(&stdout).as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn integer_negative_zero_matches_python_in_actual_api_input_output_and_query() {
    let source = r#"{"n":-0,"f":-0.0}"#;
    let directory = TempDirectory::new();
    let file = directory.0.join("zero.json");
    std::fs::write(&file, source).unwrap();
    for input in [source, file.to_str().unwrap()] {
        let host = FakeHost::new(vec![response(200, source, &[])]);
        let (code, stdout, stderr) = run(
            &["api", "items", "--method", "post", "--payload", input],
            &host,
        )
        .await;
        assert_eq!(code, 0, "{stdout}");
        assert!(stderr.is_empty(), "{stderr}");
        assert_eq!(
            host.transport.requests.lock().unwrap()[0].body,
            br#"{"n": 0, "f": -0.0}"#
        );
        let payload: RawJsonObject<'_> =
            serde_json::from_str(first_raw_data(&stdout).get()).unwrap();
        let text: RawJsonObject<'_> = serde_json::from_str(payload["text"].get()).unwrap();
        assert_eq!(text["n"].get(), "0");
        assert_eq!(text["f"].get(), "-0.0");
    }
    for (query, expected) in [
        ("text.n", "0"),
        ("text.f", "-0.0"),
        ("to_string(text)", r#"{"n":0,"f":-0.0}"#),
    ] {
        let host = FakeHost::new(vec![response(200, source, &[])]);
        let (code, stdout, stderr) = run(&["api", "items", "--query", query], &host).await;
        assert_eq!(code, 0, "{stdout}");
        assert!(stderr.is_empty(), "{stderr}");
        assert_eq!(data(&stdout), json!([expected]), "{query}");
    }
}

type RawJsonObject<'a> = std::collections::BTreeMap<String, &'a serde_json::value::RawValue>;

// Inspect output independently of ValueVisitor and of the shared decoder under
// test; either one could otherwise hide the reserved-key corruption in assertions.
fn first_raw_data(stdout: &str) -> &serde_json::value::RawValue {
    let envelope: RawJsonObject<'_> = serde_json::from_str(stdout).unwrap();
    let result: RawJsonObject<'_> = serde_json::from_str(envelope["result"].get()).unwrap();
    let data: Vec<&serde_json::value::RawValue> =
        serde_json::from_str(result["data"].get()).unwrap();
    data[0]
}

fn assert_reserved_object(raw: &serde_json::value::RawValue, nested: bool, expected_marker: &str) {
    let object: RawJsonObject<'_> = serde_json::from_str(raw.get()).unwrap();
    let marker = if nested {
        let metadata: RawJsonObject<'_> = serde_json::from_str(object["metadata"].get()).unwrap();
        metadata["$serde_json::private::Number"]
    } else {
        object["$serde_json::private::Number"]
    };
    assert_eq!(
        serde_json::from_str::<String>(marker.get()).unwrap(),
        expected_marker,
    );
    if let Some(wide) = object.get("wide") {
        assert_eq!(wide.get(), "18446744073709551617");
        assert_eq!(object["negative"].get(), "-9223372036854775809");
    }
}

#[tokio::test]
async fn reserved_keys_survive_actual_api_input_response_and_projection() {
    for (source, expected_body, expected_compact, nested, marker) in [
        (
            r#"{"$serde_json::private::Number":"123"}"#,
            r#"{"$serde_json::private::Number": "123"}"#,
            r#"{"$serde_json::private::Number":"123"}"#,
            false,
            "123",
        ),
        (
            r#"{"$serde_json::private::Number":"not a number"}"#,
            r#"{"$serde_json::private::Number": "not a number"}"#,
            r#"{"$serde_json::private::Number":"not a number"}"#,
            false,
            "not a number",
        ),
        (
            r#"{"\u0024serde_json::private::Number":"123"}"#,
            r#"{"$serde_json::private::Number": "123"}"#,
            r#"{"$serde_json::private::Number":"123"}"#,
            false,
            "123",
        ),
        (
            r#"{"$serde_json::private::\u004eumber":"not a number"}"#,
            r#"{"$serde_json::private::Number": "not a number"}"#,
            r#"{"$serde_json::private::Number":"not a number"}"#,
            false,
            "not a number",
        ),
        (
            r#"{"metadata":{"\u0024serde_json::private::Number":"123"},"wide":18446744073709551617,"negative":-9223372036854775809}"#,
            r#"{"metadata": {"$serde_json::private::Number": "123"}, "wide": 18446744073709551617, "negative": -9223372036854775809}"#,
            r#"{"metadata":{"$serde_json::private::Number":"123"},"wide":18446744073709551617,"negative":-9223372036854775809}"#,
            true,
            "123",
        ),
        (
            r#"{"metadata":{"$serde_json::private::\u004eumber":"not a number"}}"#,
            r#"{"metadata": {"$serde_json::private::Number": "not a number"}}"#,
            r#"{"metadata":{"$serde_json::private::Number":"not a number"}}"#,
            true,
            "not a number",
        ),
    ] {
        let directory = TempDirectory::new();
        let file = directory.0.join("reserved.json");
        std::fs::write(&file, source).unwrap();
        for input in [source, file.to_str().unwrap()] {
            let host = FakeHost::new(vec![response(200, source, &[])]);
            let (code, stdout, _) = run(
                &["api", "items", "--method", "post", "--payload", input],
                &host,
            )
            .await;
            assert_eq!(code, 0, "{stdout}");
            assert_eq!(
                host.transport.requests.lock().unwrap()[0].body,
                expected_body.as_bytes()
            );
            let payload: RawJsonObject<'_> =
                serde_json::from_str(first_raw_data(&stdout).get()).unwrap();
            assert_reserved_object(payload["text"], nested, marker);
        }
        for query in ["text", "to_string(text)"] {
            let host = FakeHost::new(vec![response(200, source, &[])]);
            let (code, stdout, _) = run(&["api", "items", "--query", query], &host).await;
            assert_eq!(code, 0, "{stdout}");
            let projected = first_raw_data(&stdout);
            if query == "text" {
                assert_reserved_object(projected, nested, marker);
            } else {
                assert_eq!(
                    serde_json::from_str::<String>(projected.get()).unwrap(),
                    expected_compact,
                );
            }
        }
    }
}

#[tokio::test]
async fn reserved_key_number_values_do_not_replace_actual_api_objects() {
    for (source, expected_body, nested, digits) in [
        (
            r#"{"$serde_json::private::Number":18446744073709551617}"#,
            r#"{"$serde_json::private::Number": 18446744073709551617}"#,
            false,
            "18446744073709551617",
        ),
        (
            r#"{"\u0024serde_json::private::Number":-9223372036854775809}"#,
            r#"{"$serde_json::private::Number": -9223372036854775809}"#,
            false,
            "-9223372036854775809",
        ),
        (
            r#"{"metadata":{"$serde_json::private::\u004eumber":18446744073709551617}}"#,
            r#"{"metadata": {"$serde_json::private::Number": 18446744073709551617}}"#,
            true,
            "18446744073709551617",
        ),
    ] {
        let host = FakeHost::new(vec![response(200, source, &[])]);
        let (code, stdout, _) = run(
            &["api", "items", "--method", "post", "--payload", source],
            &host,
        )
        .await;
        assert_eq!(code, 0, "{stdout}");
        assert_eq!(
            host.transport.requests.lock().unwrap()[0].body,
            expected_body.as_bytes()
        );
        let payload: RawJsonObject<'_> =
            serde_json::from_str(first_raw_data(&stdout).get()).unwrap();
        let object: RawJsonObject<'_> = serde_json::from_str(payload["text"].get()).unwrap();
        let marker = if nested {
            let metadata: RawJsonObject<'_> =
                serde_json::from_str(object["metadata"].get()).unwrap();
            metadata["$serde_json::private::Number"]
        } else {
            object["$serde_json::private::Number"]
        };
        assert_eq!(marker.get(), digits);
    }
}

#[tokio::test]
async fn actual_api_query_literals_preserve_numeric_and_nonnumeric_reserved_key_objects() {
    for (query, expected) in [
        (
            r#"to_string(`{"$serde_json::private::Number":"123"}`)"#,
            r#"{"$serde_json::private::Number":"123"}"#,
        ),
        (
            r#"to_string(`{"metadata":{"\u0024serde_json::private::Number":"not a number"}}`)"#,
            r#"{"metadata":{"$serde_json::private::Number":"not a number"}}"#,
        ),
    ] {
        let host = FakeHost::new(vec![response(200, "{}", &[])]);
        let (code, stdout, _) = run(
            &[
                "api",
                "items",
                "--method",
                "post",
                "--payload",
                "{}",
                "--query",
                query,
            ],
            &host,
        )
        .await;
        assert_eq!(code, 0, "{stdout}");
        assert_eq!(
            serde_json::from_str::<String>(first_raw_data(&stdout).get()).unwrap(),
            expected,
        );
    }
}

#[tokio::test]
async fn unsupported_float_overflow_is_rejected_without_rounding_or_preauth_effects() {
    let host = FakeHost::new(vec![]);
    let (code, stdout, _) = run(&["api", "items", "--payload", r#"{"value":1e9999}"#], &host).await;
    assert_eq!(code, 1);
    assert!(stdout.contains("InvalidJson"));
    assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
    assert!(host.transport.requests.lock().unwrap().is_empty());
    let host = FakeHost::new(vec![response(200, r#"{"value":-1e9999}"#, &[])]);
    let (code, stdout, _) = run(&["api", "items"], &host).await;
    assert_eq!(code, 1);
    assert!(stdout.contains("InvalidJson"));
}

#[tokio::test]
async fn json_file_inline_and_non_json_multipart_use_bounded_real_files() {
    let directory = TempDirectory::new();
    let json_file = directory.0.join("body.json");
    let upload = directory.0.join("payload.bin");
    std::fs::write(&json_file, br#"{"ok":true}"#).unwrap();
    std::fs::write(&upload, b"binary\0fab-rs-multipart").unwrap();
    for input in [
        r#"{"ok":true}"#,
        json_file.to_str().unwrap(),
        upload.to_str().unwrap(),
    ] {
        let host = FakeHost::new(vec![response(200, "{}", &[])]);
        assert_eq!(
            run(
                &["api", "items", "--method", "post", "--payload", input],
                &host
            )
            .await
            .0,
            0
        );
        let requests = host.transport.requests.lock().unwrap();
        let request = &requests[0];
        if input == upload.to_str().unwrap() {
            let body = String::from_utf8_lossy(&request.body);
            assert!(body.contains("filename=\"payload.bin\""));
            assert!(body.contains("name=\"file\""));
            assert!(request.headers["content-type"].starts_with("multipart/form-data; boundary="));
            assert!(!body.contains(directory.0.to_str().unwrap()));
        } else {
            assert_eq!(
                serde_json::from_slice::<Value>(&request.body).unwrap(),
                json!({"ok":true})
            );
        }
    }
}

#[test]
fn selection_persists_only_binding_handles_malformed_and_logout_is_idempotent() {
    let directory = TempDirectory::new();
    let path = directory.0.join("rust").join("selection.json");
    let file = SelectionFile::new(path.clone());
    assert!(file.read().unwrap().is_none());
    file.clear().unwrap();
    file.write(&binding()).unwrap();
    file.write(&binding()).unwrap();
    assert_eq!(file.read().unwrap(), Some(binding()));
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored.as_object().unwrap().len(), 2);
    assert!(stored.get("token").is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::write(&path, b"{bad").unwrap();
    assert!(file.read().is_err());
    file.clear().unwrap();
    file.clear().unwrap();
    assert!(file.read().unwrap().is_none());
}

#[test]
fn endpoint_environment_validation_precedes_identity_selection() {
    let directory = TempDirectory::new();
    for (audience, variable, valid) in [
        (
            "fabric",
            "FABRIC_API_ENDPOINT",
            "msitapi.fabric.microsoft.com",
        ),
        (
            "storage",
            "ONELAKE_API_ENDPOINT",
            "onelake.dfs.fabric.microsoft.com",
        ),
        ("azure", "AZURE_API_ENDPOINT", "management.azure.com"),
        ("powerbi", "POWER_BI_API_ENDPOINT", "api.powerbi.com"),
    ] {
        let invoke = |hostname: &str| {
            Command::new(env!("CARGO_BIN_EXE_fab-rs"))
                .args(["api", "items", "--audience", audience])
                .env("PATH", "")
                .env("HOME", &directory.0)
                .env("APPDATA", &directory.0)
                .env("XDG_CONFIG_HOME", &directory.0)
                .env(variable, hostname)
                .output()
                .unwrap()
        };
        let invalid = invoke("invalid.example.test");
        let output = String::from_utf8(invalid.stdout).unwrap();
        assert!(output.contains(variable));
        assert!(!output.contains("invalid.example.test"));
        assert!(!output.contains("No Rust identity selected"));
        let accepted = invoke(valid);
        assert!(
            String::from_utf8(accepted.stdout)
                .unwrap()
                .contains("No Rust identity selected")
        );
    }
}

#[test]
fn actual_fab_rs_metadata_runs_with_empty_path_and_no_configuration() {
    let directory = TempDirectory::new();
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["api", "--help"],
        vec!["auth"],
        vec!["auth", "--help"],
        vec!["auth", "login", "--help"],
        vec!["--verbose"],
        vec!["--verbose", "--help"],
        vec!["--verbose", "--version"],
        vec!["api", "--verbose", "--help"],
        vec!["auth", "--verbose"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
            .args(args)
            .env("PATH", "")
            .env("HOME", &directory.0)
            .env("APPDATA", &directory.0)
            .env("XDG_CONFIG_HOME", &directory.0)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout).unwrap().contains("fab-rs"));
        assert!(output.stderr.is_empty());
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 0);
    }
}

#[test]
fn actual_bare_root_welcomes_without_azure_cli_and_preserves_selection() {
    let directory = TempDirectory::new();
    let path = directory.0.join("fab-rs").join("selection.json");
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_fab-rs"))
            .env("PATH", "")
            .env("HOME", &directory.0)
            .env("APPDATA", &directory.0)
            .env("XDG_CONFIG_HOME", &directory.0)
            .output()
            .unwrap()
    };
    let absent = invoke();
    assert!(absent.status.success());
    assert!(absent.stderr.is_empty());
    assert_eq!(String::from_utf8(absent.stdout).unwrap(), WELCOME);
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 0);
    SelectionFile::new(path.clone()).write(&binding()).unwrap();
    let contents = std::fs::read(&path).unwrap();
    let selected = invoke();
    assert!(selected.status.success());
    assert!(selected.stderr.is_empty());
    assert_eq!(String::from_utf8(selected.stdout).unwrap(), WELCOME);
    assert_eq!(std::fs::read(&path).unwrap(), contents);
    std::fs::write(&path, b"INVALID-SELECTION-SECRET-CANARY").unwrap();
    let malformed = invoke();
    assert!(malformed.status.success());
    assert_eq!(String::from_utf8(malformed.stdout).unwrap(), WELCOME);
    assert!(malformed.stderr.is_empty());
    assert_eq!(
        std::fs::read(path).unwrap(),
        b"INVALID-SELECTION-SECRET-CANARY"
    );
}

#[test]
fn actual_bare_root_welcomes_without_configuration_environment() {
    let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .env("PATH", "")
        .env_remove("HOME")
        .env_remove("APPDATA")
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), WELCOME);
    assert!(output.stderr.is_empty());
}

#[test]
fn actual_bare_auth_matches_help_without_configuration_environment() {
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_fab-rs"))
            .args(args)
            .env("PATH", "")
            .env_remove("HOME")
            .env_remove("APPDATA")
            .env_remove("XDG_CONFIG_HOME")
            .output()
            .unwrap()
    };
    let bare = invoke(&["auth"]);
    let explicit = invoke(&["auth", "--help"]);
    assert!(bare.status.success());
    assert!(explicit.status.success());
    assert_eq!(bare.stdout, explicit.stdout);
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_fab-rs"));
    let executable_name = executable.file_name().unwrap().to_str().unwrap();
    let help = String::from_utf8(bare.stdout).unwrap();
    assert!(
        help.contains(&format!("{executable_name} auth [OPTIONS] <COMMAND>")),
        "{help}"
    );
    assert!(bare.stderr.is_empty());
    assert!(explicit.stderr.is_empty());
}

#[test]
fn actual_fab_rs_absent_binding_is_exit_four_without_azure_cli() {
    let directory = TempDirectory::new();
    for args in [vec!["api", "items"], vec!["auth", "status"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
            .args(&args)
            .env("PATH", "")
            .env("HOME", &directory.0)
            .env("APPDATA", &directory.0)
            .env("XDG_CONFIG_HOME", &directory.0)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(4), "{args:?}");
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["result"]["error_code"], "Unauthorized");
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn offline_fixture_process_entrypoint() {
    let Ok(scenario) = std::env::var("FAB_RS_OFFLINE_FIXTURE_SCENARIO") else {
        return;
    };
    let (responses, arguments) = match scenario.as_str() {
        "async" => (
            vec![response(
                202,
                r#"{"accepted":true}"#,
                &[("x-ms-operation-id", "synthetic")],
            )],
            vec![
                "api",
                "items",
                "--method",
                "post",
                "--async",
                "--show-headers",
            ],
        ),
        "poll" => (
            vec![
                response(202, "{}", &[("x-ms-operation-id", "synthetic")]),
                response(200, r#"{"status":"Succeeded"}"#, &[]),
            ],
            vec!["api", "items", "--method", "post"],
        ),
        _ => panic!("unknown synthetic fixture"),
    };
    let host = FakeHost::new(responses);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (code, stdout, stderr) = runtime.block_on(run(&arguments, &host));
    if scenario == "async" {
        assert_eq!(host.transport.requests.lock().unwrap().len(), 1);
        assert_eq!(host.clock.sleeps.load(Ordering::SeqCst), 0);
    } else {
        assert_eq!(host.transport.requests.lock().unwrap().len(), 2);
        assert_eq!(host.clock.sleeps.load(Ordering::SeqCst), 1);
    }
    // Machine-readable marker avoids depending on platform test-harness decorations.
    println!(
        "FAB_RS_OFFLINE_RESULT={}",
        json!({"exit":code,"stdout":stdout,"stderr":stderr})
    );
}

#[test]
fn blackbox_process_uses_real_parser_host_engine_and_output_with_injected_effects() {
    for scenario in ["async", "poll"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "offline_fixture_process_entrypoint",
                "--nocapture",
            ])
            .env("FAB_RS_OFFLINE_FIXTURE_SCENARIO", scenario)
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let marker = stdout
            .split("FAB_RS_OFFLINE_RESULT=")
            .nth(1)
            .unwrap()
            .lines()
            .next()
            .unwrap();
        let result: Value = serde_json::from_str(marker).unwrap();
        assert_eq!(result["exit"], 0);
        assert_eq!(
            result["stderr"],
            if scenario == "async" {
                ""
            } else {
                "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 2s...\nChecking operation status...\nOperation status: Succeeded\nOperation completed.\n"
            }
        );
        let data = data(result["stdout"].as_str().unwrap());
        assert_eq!(
            data[0]["status_code"],
            if scenario == "async" { 202 } else { 200 }
        );
    }
}

mod polling_configuration {
    use super::*;

    const ASYNC_CONFIG_ERROR: &str = "--async cannot be combined with --polling-config: async mode returns only the initial response and does not support polling";

    #[tokio::test]
    async fn invalid_config_is_rejected_before_identity_or_mutation() {
        for (config, expected) in [
            (
                "invalid",
                "--polling-config is not valid JSON: syntax error at line 1, column 1",
            ),
            ("[]", "Polling configuration must be a JSON object"),
            ("null", "Polling configuration must be a JSON object"),
            (
                r#"{"poll_interval":0}"#,
                "Invalid poll_interval: expected a positive JSON number of seconds",
            ),
            (
                r#"{"poll_interval":-1}"#,
                "Invalid poll_interval: expected a positive JSON number of seconds",
            ),
            (
                r#"{"poll_interval":"-1"}"#,
                "Invalid poll_interval: expected a positive JSON number of seconds",
            ),
            (
                r#"{"poll_interval":true}"#,
                "Invalid poll_interval: expected a positive JSON number of seconds",
            ),
            (
                r#"{"timeout":"10"}"#,
                "Invalid timeout: expected a positive JSON number of seconds",
            ),
            (
                r#"{"timeout":0}"#,
                "Invalid timeout: expected a positive JSON number of seconds",
            ),
            (
                r#"{"timeout":-1}"#,
                "Invalid timeout: expected a positive JSON number of seconds",
            ),
            (
                r#"{"timeout":1e100}"#,
                "Invalid timeout: duration exceeds supported range",
            ),
            (
                r#"{"poll_interval":1e-100}"#,
                "Invalid poll_interval: must be positive",
            ),
            (r#"{"timeout":1e-100}"#, "Invalid timeout: must be positive"),
            (
                r#"{"polling_interval":"-1"}"#,
                "Unknown polling configuration option 'polling_interval'; use 'poll_interval'",
            ),
            (
                r#"{"max_attempts":60}"#,
                "Unknown polling configuration option; supported keys are poll_interval and timeout",
            ),
            (
                r#"{"max_wait_seconds":600}"#,
                "Unknown polling configuration option; supported keys are poll_interval and timeout",
            ),
            (
                r#"{"$serde_json::private::Number":"CANARY"}"#,
                "Unknown polling configuration option; supported keys are poll_interval and timeout",
            ),
        ] {
            for asynchronous in [false, true] {
                let host = FakeHost::new(vec![]);
                let mut args = vec![
                    "api",
                    "items",
                    "--method",
                    "post",
                    "--polling-config",
                    config,
                ];
                if asynchronous {
                    args.push("--async");
                }
                let (code, stdout, stderr) = run(&args, &host).await;
                assert_eq!(code, 1, "{config}: {stdout}");
                assert!(stdout.contains("InvalidInput"));
                let envelope: Value = serde_json::from_str(&stdout).unwrap();
                assert_eq!(
                    envelope["result"]["message"],
                    if asynchronous {
                        ASYNC_CONFIG_ERROR
                    } else {
                        expected
                    }
                );
                assert!(!stdout.contains("CANARY"));
                assert!(stderr.is_empty());
                assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
                assert!(host.transport.requests.lock().unwrap().is_empty());
                assert_eq!(host.hooks.load(Ordering::SeqCst), 1);
            }
        }
    }

    #[tokio::test]
    async fn async_rejects_valid_or_empty_config_before_input_identity_or_mutation() {
        for config in [
            "{}",
            r#"{"poll_interval":null,"timeout":null}"#,
            r#"{"poll_interval":5}"#,
            r#"{"timeout":10}"#,
        ] {
            let host = FakeHost::new(vec![]);
            let (code, stdout, stderr) = run(
                &[
                    "api",
                    "items",
                    "--method",
                    "post",
                    "--async",
                    "--polling-config",
                    config,
                    "--payload",
                    "MISSING-INPUT-CANARY",
                ],
                &host,
            )
            .await;
            assert_eq!(code, 1, "{stdout}");
            let envelope: Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(envelope["result"]["error_code"], "InvalidInput");
            assert_eq!(envelope["result"]["message"], ASYNC_CONFIG_ERROR);
            assert!(!stdout.contains("CANARY"));
            assert!(stderr.is_empty());
            assert_eq!(host.credential.calls.load(Ordering::SeqCst), 0);
            assert!(host.transport.requests.lock().unwrap().is_empty());
            assert_eq!(host.clock.sleeps.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn config_wiring_controls_waits_with_optional_timeout() {
        let location = "/v1/workspaces/workspace/items/item/jobs/instances/job";
        for (audience, job) in [("fabric", false), ("fabric", true), ("azure", false)] {
            for config in [
                r#"{"poll_interval":0.5}"#,
                r#"{"poll_interval":0.5,"timeout":1}"#,
            ] {
                let headers = if job {
                    vec![("location", location)]
                } else if audience == "azure" {
                    vec![("azure-asyncoperation", "/status")]
                } else {
                    vec![("x-ms-operation-id", "op-1")]
                };
                let host = FakeHost::new(vec![
                    response(202, "", &headers),
                    response(
                        200,
                        if job {
                            r#"{"status":"Completed"}"#
                        } else {
                            r#"{"status":"Succeeded"}"#
                        },
                        &[],
                    ),
                ]);
                let start = host.clock.now();
                let args = [
                    "api",
                    "items",
                    "--method",
                    "post",
                    "--audience",
                    audience,
                    "--payload",
                    "{}",
                    "--polling-config",
                    config,
                ];
                let (code, stdout, stderr) = run(&args, &host).await;
                assert_eq!(code, 0, "{stdout}");
                assert_eq!(
                    stderr,
                    if job {
                        "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 0.5s...\nChecking job status...\nJob instance status: Completed\nJob completed.\n"
                    } else {
                        "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 0.5s...\nChecking operation status...\nOperation status: Succeeded\nOperation completed.\n"
                    }
                );
                assert_eq!(
                    host.clock.now().duration_since(start),
                    Duration::from_millis(500)
                );
                let requests = host.transport.requests.lock().unwrap();
                assert_eq!(requests.len(), 2);
                assert_eq!(requests[0].method, "POST");
                assert_eq!(requests[1].method, "GET");
                assert_eq!(data(&stdout)[0]["status_code"], 200);
            }
        }
    }

    #[tokio::test]
    async fn async_without_config_remains_initial_only_and_silent() {
        for headers in [
            vec![("x-ms-operation-id", "op-1")],
            vec![(
                "location",
                "/v1/workspaces/workspace/items/item/jobs/instances/job",
            )],
            vec![("azure-asyncoperation", "/status")],
        ] {
            let audience = if headers[0].0 == "azure-asyncoperation" {
                "azure"
            } else {
                "fabric"
            };
            let host = FakeHost::new(vec![response(202, "", &headers)]);
            let (code, stdout, stderr) = run(
                &[
                    "api",
                    "items",
                    "--method",
                    "post",
                    "--audience",
                    audience,
                    "--async",
                ],
                &host,
            )
            .await;
            assert_eq!(code, 0, "{stdout}");
            assert!(stderr.is_empty());
            assert_eq!(data(&stdout)[0]["status_code"], 202);
            assert_eq!(data(&stdout)[0]["text"], "");
            assert_eq!(host.clock.sleeps.load(Ordering::SeqCst), 0);
            let requests = host.transport.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method, "POST");
        }
    }

    #[tokio::test]
    async fn timeout_stops_local_wait_and_default_fabric_delay_ignores_initial_hint() {
        for (config, timed) in [
            (r#"{"timeout":0.5}"#, true),
            (r#"{"timeout":701}"#, false),
            ("{}", false),
            (r#"{"timeout":null,"poll_interval":null}"#, false),
        ] {
            let host = FakeHost::new(vec![
                response(
                    202,
                    "",
                    &[("x-ms-operation-id", "op-1"), ("retry-after", "700")],
                ),
                response(200, r#"{"status":"Succeeded"}"#, &[]),
            ]);
            let start = host.clock.now();
            let (code, stdout, stderr) = run(
                &[
                    "api",
                    "items",
                    "--method",
                    "post",
                    "--polling-config",
                    config,
                ],
                &host,
            )
            .await;
            assert_eq!(code, if timed { 1 } else { 0 }, "{stdout}");
            assert_eq!(
                stderr,
                if timed {
                    "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 2s...\n"
                } else {
                    "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 2s...\nChecking operation status...\nOperation status: Succeeded\nOperation completed.\n"
                }
            );
            let requests = host.transport.requests.lock().unwrap();
            assert_eq!(requests.len(), if timed { 1 } else { 2 });
            assert_eq!(requests[0].method, "POST");
            if timed {
                assert!(stdout.contains("Timeout"));
                assert_eq!(data(&stdout)[0]["operation_id"], "op-1");
                assert_eq!(
                    host.clock.now().duration_since(start),
                    Duration::from_millis(500)
                );
            } else {
                assert_eq!(
                    host.clock.now().duration_since(start),
                    Duration::from_secs(2)
                );
            }
        }
    }

    #[test]
    fn help_documents_seconds_and_optional_timeout() {
        let help = fabric_cli::parser()
            .find_subcommand_mut("api")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(help.contains("--polling-config"));
        assert!(!help.contains("--lro-config"));
        assert!(help.contains("poll_interval"));
        assert!(help.contains("seconds"));
        assert!(help.contains("no default"));
        assert!(help.contains("both keys are optional"));
        assert!(help.contains("Cannot be combined with --async"));
        assert!(!help.contains("600 seconds"));
    }
}
