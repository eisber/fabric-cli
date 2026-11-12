// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real parser, host, engine execution, and output with offline injected effects.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use fabric_cli::{Host, SelectionFile};
use fabric_cli_command_contracts::CommandError;
use fabric_core::{
    AccessToken, ApiRequest, ApiResponse, CancellationToken, Clock, CoreError, ErrorKind,
    HttpTransport, OperationContext, TokenCredential,
};
use fabric_identity::Binding;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    ffi::OsString,
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const TENANT: &str = "00000000-0000-0000-0000-000000000001";
const PRINCIPAL: &str = "00000000-0000-0000-0000-000000000002";
static FILE_ID: AtomicUsize = AtomicUsize::new(0);

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
}
#[async_trait]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self.now.lock().unwrap()
    }
    fn wall_now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(946684800)
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

struct Credential {
    calls: AtomicUsize,
    error: Option<ErrorKind>,
}
#[async_trait]
impl TokenCredential for Credential {
    async fn token(&self, _: &str, context: &OperationContext) -> Result<AccessToken, CoreError> {
        context.remaining()?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.error {
            return Err(CoreError::new(error, "Safe synthetic credential failure"));
        }
        Ok(AccessToken {
            token: "synthetic-token".into(),
            expires_on: UNIX_EPOCH + Duration::from_secs(4102444800),
        })
    }
}

struct Transport {
    requests: Mutex<Vec<ApiRequest>>,
    responses: Mutex<VecDeque<ApiResponse>>,
}
#[async_trait]
impl HttpTransport for Transport {
    async fn send(
        &self,
        request: ApiRequest,
        _: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        self.requests.lock().unwrap().push(request);
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP attempt"))
    }
}

struct FakeHost {
    binding: Mutex<Option<Binding>>,
    hooks: AtomicUsize,
    credential: Arc<Credential>,
    transport: Arc<Transport>,
    clock: Arc<TestClock>,
}
impl FakeHost {
    fn new(responses: Vec<ApiResponse>) -> Self {
        Self {
            binding: Mutex::new(Some(binding())),
            hooks: AtomicUsize::new(0),
            credential: Arc::new(Credential {
                calls: AtomicUsize::new(0),
                error: None,
            }),
            transport: Arc::new(Transport {
                requests: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into()),
            }),
            clock: Arc::new(TestClock {
                now: Mutex::new(Instant::now()),
                sleeps: AtomicUsize::new(0),
                diagnostics: Mutex::new(Vec::new()),
            }),
        }
    }
    fn hook(&self) {
        self.hooks.fetch_add(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl Host for FakeHost {
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

async fn run(args: &[&str], host: &FakeHost) -> (u8, String, String) {
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
async fn root_and_subhelp_are_completely_effect_free() {
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["api", "--help"],
        vec!["auth", "--help"],
        vec!["auth", "login", "--help"],
        vec!["auth", "status", "--help"],
        vec!["auth", "logout", "--help"],
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
                    "x-fixture=a=b,cookie=synthetic",
                    "--async",
                ],
                &host,
            )
            .await;
            assert_eq!(code, 0);
            let requests = host.transport.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method, method.to_uppercase());
            assert_eq!(requests[0].url.query(), Some("token=a%2Fb%3D%3D&n=2"));
            assert_eq!(requests[0].headers["x-fixture"], "a=b");
        }
    }
}

#[tokio::test]
async fn input_and_query_validation_precede_selection_and_credentials() {
    for args in [
        vec!["api", "https://evil.invalid/path"],
        vec!["api", "../unsafe"],
        vec!["api", "workspaces#fragment"],
        vec!["api", "workspaces", "--headers", "authorization=CANARY"],
        vec!["api", "workspaces", "--headers", "host=evil.invalid"],
        vec![
            "api",
            "workspaces",
            "--headers",
            "x-fixture=value\r\nInjected=yes",
        ],
        vec!["api", "workspaces", "--query", "["],
        vec![
            "api",
            "workspaces",
            "--input",
            "/nonexistent-fab-rs-fixture.json",
        ],
        vec!["api", "workspaces", "--input", "{invalid}"],
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
    assert_eq!(data(&stdout)[0]["text"], "(Empty)");
    assert!(data(&stdout)[0].get("headers").is_none());
    let host = FakeHost::new(vec![response(200, "not-json CANARY", &[])]);
    let (code, stdout, _) = run(&["api", "items"], &host).await;
    assert_eq!(code, 1);
    assert!(!stdout.contains("CANARY"));
    assert!(stdout.contains("InvalidJson"));
}

#[tokio::test]
async fn python_query_runs_on_full_payload_preserves_falsey_and_keeps_lists_flat() {
    for (query, expected) in [
        ("text.items", json!([{"id":1},{"id":2}])),
        ("status_code", json!(["200"])),
        ("headers.\"x-fixture\" ", json!(["yes"])),
        ("text.flag", json!(["False"])),
        ("text.zero", json!(["0"])),
        ("text.missing", json!(["None"])),
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
    for query in ["text.items", "text.empty", "text.object"] {
        let host = FakeHost::new(vec![response(
            200,
            r#"{"items":[],"empty":"","object":{}}"#,
            &[],
        )]);
        let (_, stdout, _) = run(&["api", "items", "--query", query], &host).await;
        assert_eq!(data(&stdout)[0]["status_code"], 200);
    }
    let host = FakeHost::new(vec![response(
        200,
        r#"{"items":[],"empty":"","object":{}}"#,
        &[],
    )]);
    let (code, stdout, _) = run(&["api", "items", "--query", ""], &host).await;
    assert_eq!(code, 0);
    assert_eq!(
        data(&stdout),
        json!(["status_code", "text.items", "text.empty"])
    );
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
async fn auth_failure_and_cancellation_remain_structured_and_do_not_send() {
    for kind in [
        ErrorKind::Unauthorized,
        ErrorKind::Cancelled,
        ErrorKind::Timeout,
    ] {
        let mut host = FakeHost::new(vec![]);
        host.credential = Arc::new(Credential {
            calls: AtomicUsize::new(0),
            error: Some(kind),
        });
        let (code, stdout, stderr) = run(&["api", "items"], &host).await;
        assert_eq!(
            code,
            if kind == ErrorKind::Unauthorized {
                4
            } else {
                1
            }
        );
        assert_eq!(
            serde_json::from_str::<Value>(&stdout).unwrap()["status"],
            "Failure"
        );
        assert!(stderr.is_empty());
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

#[test]
fn python_output_result_lists_stay_flat_and_falsey_nonlists_omit_data() {
    for (input, expected) in [
        (
            json!([{"id":1},{"id":2}]),
            json!({"data":[{"id":1},{"id":2}]}),
        ),
        (json!([]), json!({"data":[]})),
        (json!({}), json!({})),
        (json!(false), json!({})),
        (json!(0), json!({})),
        (json!(""), json!({})),
    ] {
        let output = fabric_cli_output::success("2000-01-01T00:00:00Z", "api", input);
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap()["result"],
            expected
        );
        assert!(output.ends_with('\n'));
    }
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
                &["api", "items", "--method", "post", "--input", input],
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
fn actual_fab_rs_metadata_runs_with_empty_path_and_no_configuration() {
    let directory = TempDirectory::new();
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["api", "--help"],
        vec!["auth", "login", "--help"],
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
fn actual_fab_rs_absent_binding_is_exit_four_without_azure_cli() {
    let directory = TempDirectory::new();
    let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args(["api", "items"])
        .env("PATH", "")
        .env("HOME", &directory.0)
        .env("APPDATA", &directory.0)
        .env("XDG_CONFIG_HOME", &directory.0)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["result"]["error_code"], "Unauthorized");
    assert!(output.stderr.is_empty());
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
        assert_eq!(result["stderr"], "");
        let data = data(result["stdout"].as_str().unwrap());
        assert_eq!(
            data[0]["status_code"],
            if scenario == "async" { 202 } else { 200 }
        );
    }
}
