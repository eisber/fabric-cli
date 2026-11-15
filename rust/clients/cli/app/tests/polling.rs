// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline live stderr progress through the real CLI composition.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::credentials::{AccessToken, TokenCredential, TokenRequestOptions};
use fabric_cli::Host;
use fabric_cli_command_contracts::CommandError;
use fabric_core::{
    ApiRequest, ApiResponse, CancellationToken, Clock, CoreError, HttpTransport, OperationContext,
};
use fabric_identity::Binding;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    ffi::OsString,
    io::{self, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

const JOB: &str = "/v1/workspaces/workspace/items/item/jobs/instances/job?secret=QUERY-CANARY";

#[derive(Default)]
struct Streams {
    stdout: Mutex<Vec<u8>>,
    stderr: Mutex<Vec<u8>>,
    flushes: Mutex<usize>,
}

struct Writer {
    streams: Arc<Streams>,
    stderr: bool,
    fail: bool,
    cancel: Option<CancellationToken>,
}
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail {
            return Err(io::Error::other("IO-CANARY"));
        }
        let stream = if self.stderr {
            &self.streams.stderr
        } else {
            &self.streams.stdout
        };
        stream.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        *self.streams.flushes.lock().unwrap() += 1;
        if let Some(cancellation) = &self.cancel {
            cancellation.cancel();
        }
        Ok(())
    }
}

struct TestClock(Mutex<Instant>, Mutex<Vec<&'static str>>);
#[async_trait]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
    fn diagnostic(&self, message: &'static str) {
        self.1.lock().unwrap().push(message);
    }
    async fn sleep(&self, delay: Duration, context: &OperationContext) -> Result<(), CoreError> {
        *self.0.lock().unwrap() += delay;
        context.remaining_at(self.now())?;
        Ok(())
    }
}

#[derive(Debug)]
struct Credential;
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        Ok(AccessToken::new(
            "TOKEN-CANARY",
            azure_core::time::OffsetDateTime::from_unix_timestamp(4102444800).unwrap(),
        ))
    }
}

struct Transport {
    replies: Mutex<VecDeque<ApiResponse>>,
    calls: Mutex<usize>,
    streams: Arc<Streams>,
    before_third_request: String,
    urls: Vec<String>,
}
#[async_trait]
impl HttpTransport for Transport {
    async fn send(
        &self,
        request: ApiRequest,
        _: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        let mut calls = self.calls.lock().unwrap();
        if *calls == 0 {
            assert_eq!(request.method, "POST");
        } else {
            assert_eq!(request.method, "GET");
            assert!(request.body.is_empty());
            assert_eq!(request.headers.len(), 1, "fresh authorization only");
        }
        if !self.urls.is_empty() {
            assert_eq!(request.url.as_str(), self.urls[*calls]);
        }
        if *calls == 2 {
            assert_eq!(
                &*self.streams.stderr.lock().unwrap(),
                self.before_third_request.as_bytes()
            );
            assert_eq!(*self.streams.flushes.lock().unwrap(), 1);
            assert!(
                self.streams.stdout.lock().unwrap().is_empty(),
                "no JSON until completion"
            );
        }
        *calls += 1;
        Ok(self.replies.lock().unwrap().pop_front().unwrap())
    }
}

struct FakeHost {
    clock: Arc<TestClock>,
    transport: Arc<Transport>,
}
#[async_trait]
impl Host for FakeHost {
    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }
    fn selection(&self) -> Result<Option<Binding>, CommandError> {
        Ok(Some(Binding {
            tenant: "00000000-0000-0000-0000-000000000001".into(),
            principal: "00000000-0000-0000-0000-000000000002".into(),
        }))
    }
    fn save_selection(&self, _: &Binding) -> Result<(), CommandError> {
        unreachable!()
    }
    fn clear_selection(&self) -> Result<(), CommandError> {
        unreachable!()
    }
    async fn login(&self, _: &OperationContext) -> Result<Binding, CommandError> {
        unreachable!()
    }
    fn credential(&self, _: Binding) -> Result<Arc<dyn TokenCredential>, CommandError> {
        Ok(Arc::new(Credential))
    }
    fn transport(&self) -> Result<Arc<dyn HttpTransport>, CommandError> {
        Ok(self.transport.clone())
    }
    fn take_diagnostics(&self) -> Vec<&'static str> {
        std::mem::take(&mut *self.clock.1.lock().unwrap())
    }
}

fn response(status: u16, body: Value, headers: &[(&str, &str)]) -> ApiResponse {
    ApiResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

async fn run(
    args: &[&str],
    replies: Vec<ApiResponse>,
    fail_stderr: bool,
    cancel_on_progress: bool,
) -> (u8, String, String, usize) {
    run_script(
        args,
        replies,
        fail_stderr,
        cancel_on_progress,
        "Job instance status: InProgress\n",
        &[],
    )
    .await
}

async fn run_script(
    args: &[&str],
    replies: Vec<ApiResponse>,
    fail_stderr: bool,
    cancel_on_progress: bool,
    before_third_request: &str,
    urls: &[&str],
) -> (u8, String, String, usize) {
    let streams = Arc::new(Streams::default());
    let host = FakeHost {
        clock: Arc::new(TestClock(
            Mutex::new(Instant::now()),
            Mutex::new(Vec::new()),
        )),
        transport: Arc::new(Transport {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(0),
            streams: streams.clone(),
            before_third_request: before_third_request.into(),
            urls: urls.iter().map(|url| (*url).into()).collect(),
        }),
    };
    let cancellation = CancellationToken::new();
    let mut stdout = Writer {
        streams: streams.clone(),
        stderr: false,
        fail: false,
        cancel: None,
    };
    let mut stderr = Writer {
        streams: streams.clone(),
        stderr: true,
        fail: fail_stderr,
        cancel: cancel_on_progress.then(|| cancellation.clone()),
    };
    let code = fabric_cli::run(
        std::iter::once("fab-rs")
            .chain(args.iter().copied())
            .map(OsString::from),
        &host,
        cancellation,
        &mut stdout,
        &mut stderr,
    )
    .await;
    let stdout = String::from_utf8(streams.stdout.lock().unwrap().clone()).unwrap();
    let stderr = String::from_utf8(streams.stderr.lock().unwrap().clone()).unwrap();
    let calls = *host.transport.calls.lock().unwrap();
    (code, stdout, stderr, calls)
}

fn accepted(location: &str) -> ApiResponse {
    response(
        202,
        json!({}),
        &[("location", location), ("x-fixture", "HEADER-CANARY")],
    )
}

#[tokio::test]
async fn statuses_are_flushed_before_next_poll_and_stdout_is_one_projected_envelope() {
    let (code, stdout, stderr, calls) = run(
        &[
            "api",
            "items",
            "--method",
            "post",
            "--query",
            "text.value",
            "--show-headers",
        ],
        vec![
            accepted(JOB),
            response(
                200,
                json!({"status":"InProgress","percentageComplete":"PERCENT-CANARY"}),
                &[],
            ),
            response(
                200,
                json!({"status":"Completed","value":[],"secret":"BODY-CANARY"}),
                &[("x-fixture", "HEADER-CANARY")],
            ),
        ],
        false,
        false,
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(calls, 3);
    assert_eq!(
        stderr,
        "Job instance status: InProgress\nJob instance status: Completed\nInvalid percentageComplete; omitting polling percentage\n"
    );
    let envelope: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(envelope["status"], "Success");
    assert_eq!(envelope["result"]["data"], json!([]));
    assert!(!stdout.contains("status:"));
    assert!(!stdout.contains("CANARY"));
    assert!(!stderr.contains("CANARY"));
}

#[tokio::test]
async fn async_raw_help_and_version_never_report_poll_progress() {
    for location in [
        JOB,
        "https://wabi-west-europe-redirect.analysis.windows.net/v1/operations/op-1",
        "/v1/operations/op-1#FRAGMENT-CANARY",
    ] {
        let (code, stdout, stderr, calls) = run(
            &["api", "items", "--method", "post", "--async"],
            vec![accepted(location)],
            false,
            false,
        )
        .await;
        assert_eq!(code, 0);
        assert_eq!(calls, 1);
        assert!(stderr.is_empty());
        assert_eq!(
            serde_json::from_str::<Value>(&stdout).unwrap()["result"]["data"][0]["status_code"],
            202
        );
    }
    for args in [vec!["--help"], vec!["--version"], vec!["api", "--help"]] {
        let (code, _, stderr, calls) = run(&args, vec![], false, false).await;
        assert_eq!(code, 0);
        assert_eq!(calls, 0);
        assert!(stderr.is_empty());
    }
    let (code, _, stderr, calls) = run(
        &["api", "items", "--method", "post"],
        vec![response(200, json!({"status":"InProgress"}), &[])],
        false,
        false,
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(calls, 1);
    assert!(stderr.is_empty());
}

#[tokio::test]
async fn failure_and_unknown_statuses_are_safe_and_output_failure_stops_polling() {
    for status in ["Failed", "Cancelled", "BODY-CANARY\nINJECTION"] {
        let (code, stdout, stderr, calls) = run(
            &["api", "items", "--method", "post", "--show-headers"],
            vec![
                accepted(JOB),
                response(200, json!({"status":status,"error":"ERROR-CANARY"}), &[]),
            ],
            false,
            false,
        )
        .await;
        assert_eq!(code, 1);
        assert_eq!(calls, 2);
        assert!(!stdout.contains("CANARY"));
        assert!(!stderr.contains("CANARY"));
        assert_eq!(
            stderr,
            if status.contains("CANARY") {
                String::new()
            } else {
                format!("Job instance status: {status}\n")
            }
        );
    }
    let (code, stdout, stderr, calls) = run(
        &["api", "items", "--method", "post"],
        vec![
            accepted(JOB),
            response(200, json!({"status":"InProgress"}), &[]),
        ],
        true,
        false,
    )
    .await;
    assert_eq!(code, 1);
    assert_eq!(calls, 2);
    assert!(stderr.is_empty());
    assert!(stdout.contains("Cannot write polling progress"));
    assert!(stdout.contains("outcome_uncertain"));
    assert!(!stdout.contains("CANARY"));
}

#[tokio::test]
async fn local_cancellation_and_deadline_do_not_report_future_polls() {
    let (code, stdout, stderr, calls) = run(
        &["api", "items", "--method", "post"],
        vec![
            accepted(JOB),
            response(200, json!({"status":"InProgress"}), &[]),
        ],
        false,
        true,
    )
    .await;
    assert_eq!(code, 1);
    assert_eq!(calls, 2);
    assert!(stdout.contains("Cancelled"));
    assert_eq!(stderr, "Job instance status: InProgress\n");
    let (code, stdout, stderr, calls) = run(
        &[
            "api",
            "items",
            "--method",
            "post",
            "--polling-config",
            r#"{"timeout":1}"#,
        ],
        vec![accepted(JOB)],
        false,
        false,
    )
    .await;
    assert_eq!(code, 1);
    assert_eq!(calls, 1);
    assert!(stdout.contains("Timeout"));
    assert!(stderr.is_empty());
}

#[tokio::test]
async fn regional_lro_polls_with_live_percentages_and_fetches_its_relative_result() {
    const STATUS: &str = "https://wabi-us-central-b-primary-redirect.analysis.windows.net/v1/operations/op-1?secret=QUERY-CANARY";
    const RESULT: &str = "https://wabi-us-central-b-primary-redirect.analysis.windows.net/v1/operations/op-1/result?secret=QUERY-CANARY";
    let (code, stdout, stderr, calls) = run_script(
        &[
            "api",
            "workspaces/workspace/items/item/jobs/execute/instances",
            "--method",
            "post",
            "--input",
            "{}",
            "--headers",
            "x-secret=MUTATION-CANARY",
            "--show-headers",
        ],
        vec![
            accepted(STATUS),
            response(200, json!({"status":"Running","percentageComplete":0}), &[]),
            response(200, json!({"status":"Running","percentageComplete":12.5}), &[]),
            response(
                200,
                json!({"status":"Succeeded","percentageComplete":100}),
                &[("location", "/v1/operations/op-1/result?secret=QUERY-CANARY")],
            ),
            response(200, json!({"value":"completed result"}), &[("x-result", "ok")]),
        ],
        false,
        false,
        "Operation status: Running (0%)\n",
        &[
            "https://api.fabric.microsoft.com/v1/workspaces/workspace/items/item/jobs/execute/instances",
            STATUS,
            STATUS,
            STATUS,
            RESULT,
        ],
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(calls, 5, "one POST, three status GETs and one result GET");
    assert_eq!(
        stderr,
        "Operation status: Running (0%)\nOperation status: Running (12.5%)\nOperation status: Succeeded (100%)\n"
    );
    let envelope: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        envelope["result"]["data"][0]["text"],
        json!({"value":"completed result"})
    );
    assert_eq!(envelope["result"]["data"][0]["headers"]["x-result"], "ok");
    assert!(!stdout.contains("CANARY"));
    assert!(!stderr.contains("CANARY"));
}

#[tokio::test]
async fn fabric_origin_hints_use_canonical_gets_and_flush_progress_before_completion() {
    const STATUS: &str =
        "https://wabi-west-europe-redirect.analysis.windows.net/v1/operations/op-1";
    const RESULT: &str =
        "https://wabi-west-europe-redirect.analysis.windows.net/v1/operations/op-1/result";
    let (code, stdout, stderr, calls) = run_script(
        &[
            "api",
            "workspaces/workspace/items/item/jobs/execute/instances",
            "--method",
            "post",
            "--input",
            "{}",
            "--headers",
            "x-secret=MUTATION-CANARY",
            "--show-headers",
        ],
        vec![
            response(
                202,
                json!({}),
                &[
                    ("x-ms-operation-id", "op-1"),
                    (
                        "Location",
                        "https://wabi-west-europe-redirect.analysis.windows.net/v1/workspaces/workspace/items/item?secret=QUERY-CANARY",
                    ),
                ],
            ),
            response(200, json!({"status":"Running","percentageComplete":12.5}), &[]),
            response(
                200,
                json!({"status":"Succeeded","percentageComplete":100}),
                &[("Location", "/v1/workspaces/workspace/items/item?secret=RESULT-CANARY")],
            ),
            response(200, json!({"value":"completed result"}), &[("x-result", "ok")]),
        ],
        false,
        false,
        "Operation status: Running (12.5%)\n",
        &[
            "https://api.fabric.microsoft.com/v1/workspaces/workspace/items/item/jobs/execute/instances",
            STATUS,
            STATUS,
            RESULT,
        ],
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(calls, 4, "one POST, two status GETs and one result GET");
    assert_eq!(
        stderr,
        "Operation status: Running (12.5%)\nOperation status: Succeeded (100%)\n"
    );
    let envelope: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        envelope["result"]["data"][0]["text"],
        json!({"value":"completed result"})
    );
    assert_eq!(envelope["result"]["data"][0]["headers"]["x-result"], "ok");
    assert!(!stdout.contains("CANARY"));
    assert!(!stderr.contains("CANARY"));
}

#[tokio::test]
async fn unsafe_fabric_origin_hints_do_not_authorize_status_gets() {
    for location in [
        "https://evil.invalid/v1/items/item?secret=CANARY",
        "https://user:CANARY@wabi-west-europe-redirect.analysis.windows.net/v1/items/item",
        "/v1/items/%2e%2e/item?secret=CANARY",
    ] {
        let (code, stdout, stderr, calls) = run(
            &["api", "items", "--method", "post"],
            vec![response(
                202,
                json!({}),
                &[("Location", location), ("x-ms-operation-id", "op-1")],
            )],
            false,
            false,
        )
        .await;
        assert_eq!(code, 1);
        assert_eq!(calls, 1, "no status GET and no mutation replay");
        assert!(stdout.contains("Protocol"));
        assert!(!stdout.contains("CANARY"));
        assert!(stderr.is_empty());
    }
}

#[tokio::test]
async fn msit_job_polls_its_location_and_reports_progress_without_following_home_cluster() {
    const MSIT_JOB: &str = "https://msitapi.fabric.microsoft.com/v1/workspaces/workspace/items/item/jobs/instances/job";
    let mut initial = response(
        202,
        json!({}),
        &[
            ("location", MSIT_JOB),
            ("x-ms-job-id", "job"),
            ("retry-after", "60"),
            ("content-type", "application/octet-stream"),
            (
                "home-cluster-uri",
                "https://df-msit-scus-redirect.analysis.windows.net/",
            ),
        ],
    );
    initial.body.clear();
    let (code, stdout, stderr, calls) = run_script(
        &[
            "api",
            "workspaces/workspace/items/item/jobs/execute/instances",
            "--method",
            "post",
            "--input",
            "{}",
            "--show-headers",
        ],
        vec![
            initial,
            response(200, json!({"status":"InProgress"}), &[("retry-after", "60")]),
            response(200, json!({"status":"Completed"}), &[]),
        ],
        false,
        false,
        "Job instance status: InProgress\n",
        &[
            "https://api.fabric.microsoft.com/v1/workspaces/workspace/items/item/jobs/execute/instances",
            MSIT_JOB,
            MSIT_JOB,
        ],
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(calls, 3, "one POST and two job GETs");
    assert_eq!(
        stderr,
        "Job instance status: InProgress\nJob instance status: Completed\n"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap()["result"]["data"][0]["text"]["status"],
        "Completed"
    );
}

#[tokio::test]
async fn regional_job_reports_terminal_success_or_failure_without_replaying_post() {
    const REGIONAL_JOB: &str = "https://wabi-west-europe-redirect.analysis.windows.net/v1/workspaces/workspace/items/item/jobs/instances/job?secret=QUERY-CANARY";
    for terminal in ["Completed", "Failed"] {
        let (code, stdout, stderr, calls) = run_script(
            &["api", "items", "--method", "post"],
            vec![
                accepted(REGIONAL_JOB),
                response(
                    200,
                    json!({"status":"InProgress","percentageComplete":25}),
                    &[],
                ),
                response(200, json!({"status":terminal}), &[]),
            ],
            false,
            false,
            "Job instance status: InProgress (25%)\n",
            &[
                "https://api.fabric.microsoft.com/v1/items",
                REGIONAL_JOB,
                REGIONAL_JOB,
            ],
        )
        .await;
        assert_eq!(calls, 3);
        assert_eq!(code, u8::from(terminal == "Failed"), "{stdout}");
        assert_eq!(
            stderr,
            format!("Job instance status: InProgress (25%)\nJob instance status: {terminal}\n")
        );
        assert!(!stdout.contains("QUERY-CANARY"));
        if terminal == "Failed" {
            assert!(stdout.contains("outcome_uncertain"));
        } else {
            assert_eq!(
                serde_json::from_str::<Value>(&stdout).unwrap()["result"]["data"][0]["text"]["status"],
                "Completed"
            );
        }
    }
}

#[tokio::test]
async fn invalid_percentages_keep_safe_status_progress_and_report_a_static_warning() {
    for percentage in [
        json!("PERCENT-CANARY\nINJECTION"),
        json!(-1),
        json!(101),
        json!({}),
    ] {
        let (code, stdout, stderr, calls) = run(
            &["api", "items", "--method", "post", "--query", "text.status"],
            vec![
                accepted(JOB),
                response(
                    200,
                    json!({"status":"Completed","percentageComplete":percentage}),
                    &[],
                ),
            ],
            false,
            false,
        )
        .await;
        assert_eq!(code, 0, "{stdout}");
        assert_eq!(calls, 2);
        assert_eq!(
            stderr,
            "Job instance status: Completed\nInvalid percentageComplete; omitting polling percentage\n"
        );
        assert!(!stdout.contains("CANARY"));
        assert!(!stderr.contains("CANARY"));
    }
    let (code, stdout, stderr, calls) = run(
        &["api", "items", "--method", "post", "--query", "text.status"],
        vec![
            accepted(JOB),
            response(
                200,
                json!({"status":"InProgress","percentageComplete":-1}),
                &[],
            ),
            response(
                200,
                json!({"status":"InProgress","percentageComplete":101}),
                &[],
            ),
            response(200, json!({"status":"Completed"}), &[]),
        ],
        false,
        false,
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(calls, 4);
    assert_eq!(
        stderr,
        "Job instance status: InProgress\nJob instance status: InProgress\nJob instance status: Completed\nInvalid percentageComplete; omitting polling percentage\n"
    );
}
