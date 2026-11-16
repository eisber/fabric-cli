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
    fail_after_flushes: Option<usize>,
    fail_once_after_bytes: Option<usize>,
    cancel: Option<CancellationToken>,
    cancel_after_flushes: usize,
}
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail
            || self
                .fail_after_flushes
                .is_some_and(|count| *self.streams.flushes.lock().unwrap() >= count)
        {
            return Err(io::Error::other("IO-CANARY"));
        }
        let stream = if self.stderr {
            &self.streams.stderr
        } else {
            &self.streams.stdout
        };
        if let Some(limit) = self.fail_once_after_bytes {
            let mut stream = stream.lock().unwrap();
            if stream.len() >= limit {
                self.fail_once_after_bytes = None;
                return Err(io::Error::other("IO-CANARY"));
            }
            let count = bytes.len().min(limit - stream.len());
            stream.extend_from_slice(&bytes[..count]);
            return Ok(count);
        }
        stream.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        *self.streams.flushes.lock().unwrap() += 1;
        if let Some(cancellation) = &self.cancel
            && *self.streams.flushes.lock().unwrap() >= self.cancel_after_flushes
        {
            cancellation.cancel();
        }
        Ok(())
    }
}

struct TestClock {
    now: Mutex<Instant>,
    diagnostics: Mutex<Vec<&'static str>>,
    streams: Arc<Streams>,
    first_wait: Mutex<bool>,
    real_time: bool,
    verbose: bool,
}
#[async_trait]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        if self.real_time {
            tokio::time::Instant::now().into_std()
        } else {
            *self.now.lock().unwrap()
        }
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
    fn diagnostic(&self, message: &'static str) {
        self.diagnostics.lock().unwrap().push(message);
    }
    async fn sleep(&self, delay: Duration, context: &OperationContext) -> Result<(), CoreError> {
        if std::mem::take(&mut *self.first_wait.lock().unwrap()) {
            let stderr = self.streams.stderr.lock().unwrap();
            if self.verbose {
                let text = String::from_utf8_lossy(&stderr);
                assert!(text.contains("HTTP #1 received 202"));
                assert!(text.contains("accepted. Polling for completion..."));
                assert!(*self.streams.flushes.lock().unwrap() > 1);
            } else {
                assert!(
                    stderr.starts_with(b"Job accepted. Polling for completion...\n")
                        || stderr.starts_with(b"Operation accepted. Polling for completion...\n")
                        || stderr.starts_with(
                            b"\x1b[36mJob accepted. Polling for completion...\x1b[0m\n"
                        ),
                    "start notification must precede the first wait: {stderr:?}"
                );
                assert_eq!(*self.streams.flushes.lock().unwrap(), 2);
            }
            assert!(self.streams.stdout.lock().unwrap().is_empty());
        }
        if self.real_time {
            tokio::time::sleep(delay).await;
        } else {
            *self.now.lock().unwrap() += delay;
        }
        context.remaining_at(self.now())?;
        Ok(())
    }
}

#[derive(Debug)]
struct Credential {
    delay: Duration,
    expected_scope: Option<&'static str>,
}
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        if let Some(expected) = self.expected_scope {
            assert_eq!(scopes, &[expected]);
        }
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
    before_third_request: Option<String>,
    urls: Vec<String>,
    response_delays: Mutex<VecDeque<Duration>>,
}
#[async_trait]
impl HttpTransport for Transport {
    async fn send(
        &self,
        request: ApiRequest,
        _: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        {
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
            if *calls == 2
                && let Some(expected) = &self.before_third_request
            {
                assert_eq!(&*self.streams.stderr.lock().unwrap(), expected.as_bytes());
                assert_eq!(*self.streams.flushes.lock().unwrap(), 6);
                assert!(
                    self.streams.stdout.lock().unwrap().is_empty(),
                    "no JSON until completion"
                );
            }
            *calls += 1;
        }
        let delay = self.response_delays.lock().unwrap().pop_front();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        Ok(self.replies.lock().unwrap().pop_front().unwrap())
    }
}

struct FakeHost {
    clock: Arc<TestClock>,
    transport: Arc<Transport>,
    color: bool,
    credential_delay: Duration,
    terminal: bool,
    expected_scope: Option<&'static str>,
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
        Ok(Arc::new(Credential {
            delay: self.credential_delay,
            expected_scope: self.expected_scope,
        }))
    }
    fn transport(&self) -> Result<Arc<dyn HttpTransport>, CommandError> {
        Ok(self.transport.clone())
    }
    fn take_diagnostics(&self) -> Vec<&'static str> {
        std::mem::take(&mut *self.clock.diagnostics.lock().unwrap())
    }
    fn progress_color(&self) -> bool {
        self.color
    }
    fn progress_terminal(&self) -> bool {
        self.terminal
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
        "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: InProgress\nWaiting for job completion; next status check in 7s...\nChecking job status...\n",
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
    run_options(
        args,
        replies,
        fail_stderr,
        cancel_on_progress,
        RunOptions {
            before_third_request: Some(before_third_request),
            urls,
            ..Default::default()
        },
    )
    .await
}

#[derive(Default)]
struct RunOptions<'a> {
    before_third_request: Option<&'a str>,
    urls: &'a [&'a str],
    real_time: bool,
    response_delays: Vec<Duration>,
    color: bool,
    terminal: bool,
    fail_after_flushes: Option<usize>,
    fail_once_after_bytes: Option<usize>,
    cancel_after_flushes: Option<usize>,
    credential_delay: Duration,
    expected_scope: Option<&'static str>,
}

async fn run_options(
    args: &[&str],
    replies: Vec<ApiResponse>,
    fail_stderr: bool,
    cancel_on_progress: bool,
    options: RunOptions<'_>,
) -> (u8, String, String, usize) {
    let streams = Arc::new(Streams::default());
    let host = FakeHost {
        clock: Arc::new(TestClock {
            now: Mutex::new(Instant::now()),
            diagnostics: Mutex::new(Vec::new()),
            streams: streams.clone(),
            first_wait: Mutex::new(true),
            real_time: options.real_time,
            verbose: args.contains(&"--verbose"),
        }),
        transport: Arc::new(Transport {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(0),
            streams: streams.clone(),
            before_third_request: options.before_third_request.map(Into::into),
            urls: options.urls.iter().map(|url| (*url).into()).collect(),
            response_delays: Mutex::new(options.response_delays.into()),
        }),
        color: options.color,
        credential_delay: options.credential_delay,
        terminal: options.terminal,
        expected_scope: options.expected_scope,
    };
    let cancellation = CancellationToken::new();
    let mut stdout = Writer {
        streams: streams.clone(),
        stderr: false,
        fail: false,
        fail_after_flushes: None,
        fail_once_after_bytes: None,
        cancel: None,
        cancel_after_flushes: 1,
    };
    let mut stderr = Writer {
        streams: streams.clone(),
        stderr: true,
        fail: fail_stderr,
        fail_after_flushes: options.fail_after_flushes,
        fail_once_after_bytes: options.fail_once_after_bytes,
        cancel: (cancel_on_progress || options.cancel_after_flushes.is_some())
            .then(|| cancellation.clone()),
        cancel_after_flushes: options.cancel_after_flushes.unwrap_or(1),
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

#[tokio::test(start_paused = true)]
async fn terminal_prints_fixed_wait_once_without_refreshing_or_elapsed_time() {
    for color in [false, true] {
        let start = tokio::time::Instant::now();
        let (code, stdout, stderr, calls) = run_options(
            &["api", "items", "--method", "post"],
            vec![
                response(202, json!({}), &[("location", JOB), ("retry-after", "3")]),
                response(200, json!({"status":"Completed"}), &[]),
            ],
            false,
            false,
            RunOptions {
                real_time: true,
                terminal: true,
                color,
                response_delays: vec![Duration::ZERO, Duration::from_secs(3)],
                ..Default::default()
            },
        )
        .await;
        assert_eq!(code, 0, "{stdout}");
        assert_eq!(calls, 2);
        assert_eq!(start.elapsed(), Duration::from_secs(5));
        let permanent = |message: &str, tone: &str| {
            if color {
                format!("\x1b[{tone}m{message}\x1b[0m\n")
            } else {
                format!("{message}\n")
            }
        };
        let mut expected = permanent("Job accepted. Polling for completion...", "36");
        expected += &permanent(
            "Waiting for job completion; next status check in 2s...",
            "90",
        );
        expected += &permanent("Checking job status...", "36");
        expected += &permanent("Job instance status: Completed", "32");
        expected += &permanent("Job completed.", "32");
        assert_eq!(stderr, expected);
        if !color {
            assert!(!stderr.contains("\x1b[90m"));
            assert!(!stderr.contains("\x1b[0m"));
        }
        assert!(!stdout.contains('\x1b'));
        assert!(!stdout.contains("Waiting"));
    }
}

#[tokio::test(start_paused = true)]
async fn terminal_cancellation_timeout_and_http_failure_leave_no_live_line() {
    for scenario in ["cancel", "timeout", "http"] {
        let (code, stdout, stderr, calls) = run_options(
            &[
                "api",
                "items",
                "--method",
                "post",
                "--polling-config",
                r#"{"timeout":3,"poll_interval":1}"#,
            ],
            vec![
                response(
                    202,
                    json!({}),
                    &[
                        ("location", JOB),
                        ("retry-after", if scenario == "http" { "1" } else { "60" }),
                    ],
                ),
                response(
                    403,
                    json!({"error":{"code":"Forbidden","message":"BODY-CANARY"}}),
                    &[],
                ),
            ],
            false,
            false,
            RunOptions {
                real_time: true,
                terminal: true,
                cancel_after_flushes: (scenario == "cancel").then_some(2),
                response_delays: vec![Duration::ZERO, Duration::from_millis(1500)],
                ..Default::default()
            },
        )
        .await;
        assert_eq!(code, 1, "{stdout}");
        assert_eq!(calls, if scenario == "http" { 2 } else { 1 });
        assert!(stderr.ends_with('\n'), "{stderr:?}");
        assert!(!stderr.contains('\r'));
        assert!(!stderr.contains("completed."));
        assert!(!stderr.contains("CANARY"));
        assert!(!stdout.contains("outcome_uncertain"));
        assert!(stdout.contains("Check status_url before retrying."));
        let error = if scenario == "cancel" {
            "Cancelled"
        } else if scenario == "timeout" {
            "Timeout"
        } else {
            "Forbidden"
        };
        assert!(stdout.contains(error), "{stdout}");
    }
}

#[tokio::test(start_paused = true)]
async fn waiting_line_write_failure_stops_before_sleep_with_safe_recovery() {
    let start = tokio::time::Instant::now();
    let (code, stdout, stderr, calls) = run_options(
        &["api", "items", "--method", "post"],
        vec![response(
            202,
            json!({}),
            &[("location", JOB), ("retry-after", "60")],
        )],
        false,
        false,
        RunOptions {
            real_time: true,
            terminal: true,
            fail_after_flushes: Some(1),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(code, 1);
    assert_eq!(calls, 1);
    assert_eq!(start.elapsed(), Duration::ZERO);
    assert!(stdout.contains("Cannot write polling progress"));
    assert!(!stdout.contains("outcome_uncertain"));
    assert!(stdout.contains("Check status_url before retrying."));
    assert!(!stdout.contains("CANARY"));
    assert!(!stderr.contains("Checking"));
}

#[tokio::test]
async fn partial_colored_wait_failure_resets_terminal_style() {
    let accepted_line = "\x1b[36mJob accepted. Polling for completion...\x1b[0m\n";
    let partial = "\x1b[90mWaiting ";
    let (code, stdout, stderr, calls) = run_options(
        &["api", "items", "--method", "post"],
        vec![accepted(JOB)],
        false,
        false,
        RunOptions {
            terminal: true,
            color: true,
            fail_once_after_bytes: Some(accepted_line.len() + partial.len()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(code, 1, "{stdout}");
    assert_eq!(calls, 1);
    assert_eq!(stderr, format!("{accepted_line}{partial}\x1b[0m"));
    assert!(stdout.contains("Cannot write polling progress"));
    assert!(!stdout.contains("outcome_uncertain"));
    assert!(stdout.contains("Check status_url before retrying."));
    assert!(!stdout.contains("CANARY"));
}

#[tokio::test]
async fn deduplicated_job_is_not_mislabeled_as_completed() {
    let (code, stdout, stderr, calls) = run_options(
        &["api", "items", "--method", "post"],
        vec![
            accepted(JOB),
            response(200, json!({"status":"Deduped"}), &[]),
        ],
        false,
        false,
        RunOptions::default(),
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(calls, 2);
    assert_eq!(
        stderr,
        "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: Deduped\nJob finished with status: Deduped.\n"
    );
    assert!(!stderr.contains("Job completed."));
}

#[tokio::test]
async fn completion_output_failure_preserves_recovery_instead_of_reporting_success() {
    let (code, stdout, stderr, calls) = run_options(
        &["api", "items", "--method", "post"],
        vec![
            accepted(JOB),
            response(200, json!({"status":"Completed"}), &[]),
        ],
        false,
        false,
        RunOptions {
            fail_after_flushes: Some(4),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(code, 1);
    assert_eq!(calls, 2);
    assert!(stderr.ends_with("Job instance status: Completed\n"));
    assert!(stdout.contains("Cannot write polling progress"));
    assert!(!stdout.contains("outcome_uncertain"));
    assert!(!stdout.contains("Check status_url before retrying."));
}

#[tokio::test]
async fn service_error_code_reaches_json_for_status_and_result_failures() {
    for (replies, expected, calls) in [
        (
            vec![
                accepted("/v1/operations/op-1"),
                response(
                    200,
                    json!({"status":"Failed","error":{"errorCode":"DuplicateDatasetNamesInWorkspace","message":"SECRET-CANARY"}}),
                    &[],
                ),
            ],
            "DuplicateDatasetNamesInWorkspace",
            2,
        ),
        (
            vec![
                accepted("/v1/operations/op-1"),
                response(
                    200,
                    json!({"status":"Succeeded"}),
                    &[("location", "/v1/operations/op-1/result")],
                ),
                response(
                    400,
                    json!({"errorCode":"ResultFailed","message":"SECRET-CANARY"}),
                    &[],
                ),
            ],
            "ResultFailed",
            3,
        ),
        (
            vec![
                accepted("/v1/operations/op-1"),
                response(
                    200,
                    json!({"status":"Failed","error":{"errorCode":"Unauthorized"}}),
                    &[],
                ),
            ],
            "Unauthorized",
            2,
        ),
    ] {
        let (exit, stdout, stderr, actual_calls) = run_options(
            &["api", "items", "--method", "post"],
            replies,
            false,
            false,
            RunOptions::default(),
        )
        .await;
        assert_eq!(exit, 1, "{stdout}");
        assert_eq!(actual_calls, calls);
        let envelope: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(envelope["status"], "Failure");
        assert_eq!(envelope["result"]["error_code"], expected);
        assert_eq!(envelope["result"]["data"][0]["operation_id"], "op-1");
        assert!(!stdout.contains("outcome_uncertain"));
        assert!(!stdout.contains("Check status_url before retrying."));
        assert!(!stdout.contains("SECRET-CANARY"));
        assert!(!stderr.contains("SECRET-CANARY"));
    }
}

#[tokio::test(start_paused = true)]
async fn fixed_wait_lines_follow_fabric_schedule_and_preserve_every_real_status() {
    for terminal in [false, true] {
        for (location, label, status_label, pending, completed) in [
            (JOB, "Job", "Job instance status", "InProgress", "Completed"),
            (
                "/v1/operations/op-1",
                "Operation",
                "Operation status",
                "Running",
                "Succeeded",
            ),
        ] {
            let start = tokio::time::Instant::now();
            let (code, stdout, stderr, calls) = run_options(
                &["api", "items", "--method", "post"],
                vec![
                    response(
                        202,
                        json!({}),
                        &[("location", location), ("retry-after", "20")],
                    ),
                    response(
                        200,
                        json!({"status":pending,"percentageComplete":25}),
                        &[("retry-after", "20")],
                    ),
                    response(
                        200,
                        json!({"status":pending,"percentageComplete":25}),
                        &[("retry-after", "20")],
                    ),
                    response(200, json!({"status":completed}), &[]),
                ],
                false,
                false,
                RunOptions {
                    real_time: true,
                    terminal,
                    ..Default::default()
                },
            )
            .await;
            assert_eq!(code, 0, "{stdout}");
            assert_eq!(calls, 4);
            assert_eq!(start.elapsed(), Duration::from_secs(29));
            let subject = label.to_ascii_lowercase();
            let mut expected = format!("{label} accepted. Polling for completion...\n");
            for (delay, status) in [
                (2, format!("{pending} (25%)")),
                (7, format!("{pending} (25%)")),
                (20, completed.into()),
            ] {
                expected += &format!(
                    "Waiting for {subject} completion; next status check in {delay}s...\nChecking {subject} status...\n{status_label}: {status}\n"
                );
            }
            expected += &format!("{label} completed.\n");
            assert_eq!(stderr, expected);
            assert!(!stderr.contains("CANARY"));
            let envelope: Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(envelope["result"]["data"][0]["text"]["status"], completed);
            assert!(!stdout.contains("elapsed"));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn verbose_events_during_credentials_do_not_postpone_single_wait_notice() {
    let replies = || {
        vec![
            response(202, json!({}), &[("location", JOB), ("retry-after", "1")]),
            response(200, json!({"status":"Completed"}), &[]),
        ]
    };
    let options = || RunOptions {
        real_time: true,
        credential_delay: Duration::from_secs(7),
        response_delays: vec![Duration::ZERO, Duration::from_secs(25)],
        ..Default::default()
    };
    let plain = run_options(
        &["api", "items", "--method", "post"],
        replies(),
        false,
        false,
        options(),
    )
    .await;
    let verbose = run_options(
        &["--verbose", "api", "items", "--method", "post"],
        replies(),
        false,
        false,
        options(),
    )
    .await;
    assert_eq!(plain.0, 0, "{}", plain.1);
    assert_eq!(verbose.0, 0, "{}", verbose.1);
    assert_eq!(verbose.1, plain.1);
    assert_eq!(verbose.3, plain.3);
    assert_eq!(verbose.3, 2);
    let progress = verbose
        .2
        .lines()
        .filter(|line| !line.starts_with("[verbose] [") && !line.starts_with("    "))
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    assert_eq!(progress, plain.2);
    assert_eq!(
        progress
            .matches("Waiting for job status response...")
            .count(),
        1
    );
    assert!(!progress.contains("elapsed"));
    assert!(
        verbose
            .2
            .find("Waiting for job status response...")
            .unwrap()
            < verbose.2.rfind("Credentials: completed (7000 ms)").unwrap()
    );
    assert!(verbose.2.contains("Credentials: completed (7000 ms)"));
    assert!(!verbose.2.contains("CANARY"));
}

#[tokio::test]
async fn verbose_color_uses_only_renderer_sequences_and_preserves_stdout() {
    let replies = || {
        vec![
            accepted(JOB),
            response(200, json!({"status":"Completed"}), &[]),
        ]
    };
    let mut outputs = Vec::new();
    for color in [false, true] {
        let (code, stdout, stderr, calls) = run_options(
            &["--verbose", "api", "items", "--method", "post"],
            replies(),
            false,
            false,
            RunOptions {
                color,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(code, 0, "{stdout}");
        assert_eq!(calls, 2);
        assert!(!stdout.contains('\x1b'));
        assert!(!stderr.contains("CANARY"));
        assert_eq!(
            stderr.contains("\x1b[2;36m[verbose] [1970-01-01T00:00:00.000Z]"),
            color
        );
        if !color {
            assert!(!stderr.contains('\x1b'));
        }
        outputs.push(stdout);
    }
    assert_eq!(outputs[0], outputs[1]);
}

#[tokio::test]
async fn verbose_groups_preserve_fixed_wait_lines_without_terminal_refresh() {
    let (code, stdout, stderr, calls) = run_options(
        &["--verbose", "api", "items", "--method", "post"],
        vec![
            accepted(JOB),
            response(200, json!({"status":"Completed"}), &[]),
        ],
        false,
        false,
        RunOptions {
            terminal: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(calls, 2);
    assert!(stderr.contains("next status check in "));
    assert_eq!(stderr.matches("next status check in 2s...").count(), 1);
    assert!(!stderr.contains('\r'));
    assert!(!stderr.contains('\x1b'));
    assert!(stderr.contains("Job completed.\n"));
    assert!(!stderr.contains("CANARY"));
    assert!(!stdout.contains('\x1b'));
}

#[tokio::test(start_paused = true)]
async fn slow_status_request_reports_waiting_once_without_refresh() {
    let start = tokio::time::Instant::now();
    let (code, stdout, stderr, calls) = run_options(
        &["api", "items", "--method", "post"],
        vec![
            response(202, json!({}), &[("location", JOB), ("retry-after", "1")]),
            response(200, json!({"status":"Completed"}), &[]),
        ],
        false,
        false,
        RunOptions {
            real_time: true,
            response_delays: vec![Duration::ZERO, Duration::from_secs(25)],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(calls, 2);
    assert_eq!(start.elapsed(), Duration::from_secs(27));
    assert_eq!(
        stderr,
        concat!(
            "Job accepted. Polling for completion...\n",
            "Waiting for job completion; next status check in 2s...\n",
            "Checking job status...\n",
            "Waiting for job status response...\n",
            "Job instance status: Completed\n",
            "Job completed.\n",
        )
    );
}

#[tokio::test(start_paused = true)]
async fn completed_service_status_is_not_reported_as_running_while_fetching_a_result() {
    let (code, stdout, stderr, calls) = run_options(
        &["api", "items", "--method", "post"],
        vec![
            accepted("/v1/operations/op-1"),
            response(
                200,
                json!({"status":"Succeeded"}),
                &[("location", "/v1/operations/op-1/result")],
            ),
            response(200, json!({"value":"done"}), &[]),
        ],
        false,
        false,
        RunOptions {
            real_time: true,
            response_delays: vec![Duration::ZERO, Duration::ZERO, Duration::from_secs(25)],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(calls, 3);
    assert_eq!(
        stderr,
        concat!(
            "Operation accepted. Polling for completion...\n",
            "Waiting for operation completion; next status check in 2s...\n",
            "Checking operation status...\n",
            "Operation status: Succeeded\n",
            "Waiting for operation result; Last reported status: Succeeded...\n",
            "Operation completed.\n",
        )
    );
}

#[tokio::test(start_paused = true)]
async fn waiting_output_failure_and_cancellation_stop_waits_and_inflight_requests() {
    for inflight in [false, true] {
        for fail in [false, true] {
            let initial_delay = if inflight { "1" } else { "60" };
            let completed_flushes = if inflight { 3 } else { 1 };
            let start = tokio::time::Instant::now();
            let (code, stdout, stderr, calls) = run_options(
                &["api", "items", "--method", "post"],
                vec![
                    response(
                        202,
                        json!({}),
                        &[("location", JOB), ("retry-after", initial_delay)],
                    ),
                    response(200, json!({"status":"Completed"}), &[]),
                ],
                false,
                false,
                RunOptions {
                    real_time: true,
                    response_delays: vec![Duration::ZERO, Duration::from_secs(60)],
                    fail_after_flushes: fail.then_some(completed_flushes),
                    cancel_after_flushes: (!fail).then_some(completed_flushes + 1),
                    ..Default::default()
                },
            )
            .await;
            assert_eq!(code, 1, "{stdout}");
            assert_eq!(calls, if inflight { 2 } else { 1 });
            assert_eq!(
                start.elapsed(),
                Duration::from_secs(if inflight { 7 } else { 0 })
            );
            let envelope: Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(
                envelope["result"]["error_code"],
                if fail { "Transport" } else { "Cancelled" }
            );
            assert!(!stdout.contains("outcome_uncertain"));
            assert!(stdout.contains("Check status_url before retrying."));
            if fail {
                assert!(stdout.contains("Cannot write polling progress"));
                assert!(!stderr.contains("elapsed"));
            }
            assert!(!stderr.contains("elapsed"));
            assert!(!stderr.contains("Completed"));
            assert!(!stdout.contains("CANARY"));
            assert!(!stderr.contains("CANARY"));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn deadline_stops_configured_wait_without_refreshing_or_sending_a_status_request() {
    let start = tokio::time::Instant::now();
    let (code, stdout, stderr, calls) = run_options(
        &[
            "api",
            "items",
            "--method",
            "post",
            "--polling-config",
            r#"{"timeout":15,"poll_interval":60}"#,
        ],
        vec![response(
            202,
            json!({}),
            &[("location", JOB), ("retry-after", "60")],
        )],
        false,
        false,
        RunOptions {
            real_time: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(code, 1, "{stdout}");
    assert_eq!(calls, 1);
    assert_eq!(start.elapsed(), Duration::from_secs(15));
    assert!(stdout.contains("Timeout"));
    assert_eq!(
        stderr,
        "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 60s...\n"
    );
}

#[tokio::test(start_paused = true)]
async fn color_is_opt_in_for_injected_streams_and_never_reaches_json() {
    for (terminal, tone) in [("Completed", "32"), ("Failed", "31")] {
        let (code, stdout, stderr, calls) = run_options(
            &["api", "items", "--method", "post"],
            vec![
                response(202, json!({}), &[("location", JOB), ("retry-after", "15")]),
                response(200, json!({"status":terminal}), &[]),
            ],
            false,
            false,
            RunOptions {
                real_time: true,
                color: true,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(code, u8::from(terminal == "Failed"), "{stdout}");
        assert_eq!(calls, 2);
        assert_eq!(
            stderr,
            format!(
                "\x1b[36mJob accepted. Polling for completion...\x1b[0m\n\
                 \x1b[90mWaiting for job completion; next status check in 2s...\x1b[0m\n\
                 \x1b[36mChecking job status...\x1b[0m\n\
                 \x1b[{tone}mJob instance status: {terminal}\x1b[0m\n{}",
                if terminal == "Completed" {
                    "\x1b[32mJob completed.\x1b[0m\n"
                } else {
                    ""
                },
            )
        );
        assert!(!stdout.contains('\x1b'));
        serde_json::from_str::<Value>(&stdout).unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn slow_initial_raw_and_async_requests_do_not_start_a_heartbeat() {
    for args in [
        vec!["api", "items", "--method", "post"],
        vec!["api", "items", "--method", "post", "--async"],
    ] {
        let asynchronous = args.contains(&"--async");
        let (code, stdout, stderr, calls) = run_options(
            &args,
            vec![if asynchronous {
                accepted(JOB)
            } else {
                response(200, json!({}), &[])
            }],
            false,
            false,
            RunOptions {
                response_delays: vec![Duration::from_secs(25)],
                ..Default::default()
            },
        )
        .await;
        assert_eq!(code, 0, "{stdout}");
        assert_eq!(calls, 1);
        assert!(stderr.is_empty());
    }
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
        "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: InProgress\nWaiting for job completion; next status check in 7s...\nChecking job status...\nJob instance status: Completed\nJob completed.\nInvalid percentageComplete; omitting polling percentage\n"
    );
    let envelope: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(envelope["status"], "Success");
    assert_eq!(envelope["result"]["data"], json!([]));
    assert!(!stdout.contains("status:"));
    assert!(!stdout.contains("CANARY"));
    assert!(!stderr.contains("CANARY"));
}

#[tokio::test]
async fn start_is_flushed_before_a_long_retry_after_for_operations_and_jobs() {
    for (location, terminal, expected) in [
        (
            "/v1/operations/op-1",
            "Succeeded",
            "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 2s...\nChecking operation status...\nOperation status: Succeeded\nOperation completed.\n",
        ),
        (
            JOB,
            "Completed",
            "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: Completed\nJob completed.\n",
        ),
    ] {
        let (code, stdout, stderr, calls) = run(
            &[
                "api",
                "items",
                "--method",
                "post",
                "--input",
                "{}",
                "--show-headers",
            ],
            vec![
                response(
                    202,
                    json!({}),
                    &[("location", location), ("retry-after", "600")],
                ),
                response(200, json!({"status":terminal}), &[]),
            ],
            false,
            false,
        )
        .await;
        assert_eq!(code, 0, "{stdout}");
        assert_eq!(calls, 2, "one mutation and one status GET");
        assert_eq!(stderr, expected);
        assert_eq!(
            serde_json::from_str::<Value>(&stdout).unwrap()["result"]["data"][0]["text"]["status"],
            terminal
        );
    }
}

#[tokio::test]
async fn storage_uses_azure_polling_protocols_and_progress_unless_async() {
    for header in ["azure-asyncoperation", "location"] {
        for asynchronous in [false, true] {
            let mut args = vec![
                "api",
                "items",
                "--method",
                "post",
                "--audience",
                "storage",
                "--show-headers",
            ];
            if asynchronous {
                args.push("--async");
            }
            let initial = response(
                202,
                json!({"original":"resource"}),
                &[(header, "/status?secret=QUERY-CANARY")],
            );
            let (code, stdout, stderr, calls) = run_options(
                &args,
                vec![initial, response(200, json!({"status":"Succeeded"}), &[])],
                false,
                false,
                RunOptions {
                    urls: &[
                        "https://onelake.dfs.fabric.microsoft.com/items",
                        "https://onelake.dfs.fabric.microsoft.com/status?secret=QUERY-CANARY",
                    ],
                    expected_scope: Some("https://storage.azure.com/.default"),
                    ..Default::default()
                },
            )
            .await;
            assert_eq!(code, 0, "{stdout}");
            assert_eq!(calls, if asynchronous { 1 } else { 2 });
            let output: Value = serde_json::from_str(&stdout).unwrap();
            let result = &output["result"]["data"][0];
            assert_eq!(result["status_code"], if asynchronous { 202 } else { 200 });
            assert_eq!(result["text"], json!({"original":"resource"}));
            if asynchronous {
                assert!(stderr.is_empty());
            } else {
                assert_eq!(
                    stderr,
                    "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 10s...\nChecking operation status...\nOperation status: Succeeded\nOperation completed.\n"
                );
            }
            assert!(!stderr.contains("CANARY"));
        }
    }
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
async fn failure_and_unknown_statuses_are_safe_and_start_output_failure_stops_polling() {
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
                "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\n"
                    .into()
            } else {
                format!(
                    "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: {status}\n"
                )
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
    assert_eq!(
        calls, 1,
        "failed start output must prevent the first status GET"
    );
    assert!(stderr.is_empty());
    assert!(stdout.contains("Cannot write polling progress"));
    assert!(!stdout.contains("outcome_uncertain"));
    assert!(stdout.contains("Check status_url before retrying."));
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
    assert_eq!(
        calls, 1,
        "cancellation after start must prevent the first status GET"
    );
    assert!(stdout.contains("Cancelled"));
    assert_eq!(stderr, "Job accepted. Polling for completion...\n");
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
    assert_eq!(
        stderr,
        "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\n"
    );
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
        "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 2s...\nChecking operation status...\nOperation status: Running (0%)\nWaiting for operation completion; next status check in 7s...\nChecking operation status...\n",
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
        "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 2s...\nChecking operation status...\nOperation status: Running (0%)\nWaiting for operation completion; next status check in 7s...\nChecking operation status...\nOperation status: Running (12.5%)\nWaiting for operation completion; next status check in 20s...\nChecking operation status...\nOperation status: Succeeded (100%)\nOperation completed.\n"
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
async fn semantic_model_creation_trusts_advertised_status_and_result_hosts() {
    const RESULT: &str =
        "https://df-msit-scus-redirect.analysis.windows.net/v1/operations/op-1/result";
    for (status, result) in [
        (
            "https://api.fabric.microsoft.com/v1/operations/op-1",
            RESULT,
        ),
        (
            "https://df-msit-scus-redirect.analysis.windows.net/v1/operations/op-1",
            RESULT,
        ),
        (
            "https://service-advertised.example.test/v1/operations/op-1",
            "https://result-service.example.test/v1/operations/op-1/result",
        ),
    ] {
        let progress = "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 2s...\nChecking operation status...\nOperation status: Succeeded\nOperation completed.\n";
        let (code, stdout, stderr, calls) = run_options(
            &[
                "api",
                "workspaces/workspace/items",
                "--method",
                "post",
                "--input",
                r#"{"displayName":"test","type":"SemanticModel"}"#,
            ],
            vec![
                accepted(status),
                response(
                    200,
                    json!({"status":"Succeeded","percentComplete":100,"error":null}),
                    &[("location", result), ("x-ms-operation-id", "op-1")],
                ),
                response(
                    201,
                    json!({"id":"item","displayName":"test","type":"SemanticModel"}),
                    &[],
                ),
            ],
            false,
            false,
            RunOptions {
                urls: &[
                    "https://api.fabric.microsoft.com/v1/workspaces/workspace/items",
                    status,
                    result,
                ],
                ..Default::default()
            },
        )
        .await;
        assert_eq!(code, 0, "{stdout}");
        assert_eq!(calls, 3, "one POST, one status GET and one result GET");
        assert_eq!(stderr, progress);
        let envelope: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(envelope["result"]["data"][0]["status_code"], 201);
        assert_eq!(
            envelope["result"]["data"][0]["text"],
            json!({"id":"item","displayName":"test","type":"SemanticModel"})
        );
        assert!(!stdout.contains("CANARY"));
    }
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
        "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 2s...\nChecking operation status...\nOperation status: Running (12.5%)\nWaiting for operation completion; next status check in 7s...\nChecking operation status...\n",
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
        "Operation accepted. Polling for completion...\nWaiting for operation completion; next status check in 2s...\nChecking operation status...\nOperation status: Running (12.5%)\nWaiting for operation completion; next status check in 7s...\nChecking operation status...\nOperation status: Succeeded (100%)\nOperation completed.\n"
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
        "http://evil.invalid/v1/items/item?secret=CANARY",
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
        "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: InProgress\nWaiting for job completion; next status check in 7s...\nChecking job status...\n",
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
        "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: InProgress\nWaiting for job completion; next status check in 7s...\nChecking job status...\nJob instance status: Completed\nJob completed.\n"
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
            "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: InProgress (25%)\nWaiting for job completion; next status check in 7s...\nChecking job status...\n",
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
            format!(
                "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: InProgress (25%)\nWaiting for job completion; next status check in 7s...\nChecking job status...\nJob instance status: {terminal}\n{}",
                if terminal == "Completed" {
                    "Job completed.\n"
                } else {
                    ""
                },
            )
        );
        assert!(!stdout.contains("QUERY-CANARY"));
        if terminal == "Failed" {
            assert!(!stdout.contains("outcome_uncertain"));
            assert!(!stdout.contains("Check status_url before retrying."));
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
            "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: Completed\nJob completed.\nInvalid percentageComplete; omitting polling percentage\n"
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
        "Job accepted. Polling for completion...\nWaiting for job completion; next status check in 2s...\nChecking job status...\nJob instance status: InProgress\nWaiting for job completion; next status check in 7s...\nChecking job status...\nJob instance status: InProgress\nWaiting for job completion; next status check in 20s...\nChecking job status...\nJob instance status: Completed\nJob completed.\nInvalid percentageComplete; omitting polling percentage\n"
    );
}
