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

struct TestClock(Mutex<Instant>);
#[async_trait]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
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
        }
        if *calls == 2 {
            assert_eq!(
                &*self.streams.stderr.lock().unwrap(),
                b"Job instance status: InProgress\n"
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
    let streams = Arc::new(Streams::default());
    let host = FakeHost {
        clock: Arc::new(TestClock(Mutex::new(Instant::now()))),
        transport: Arc::new(Transport {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(0),
            streams: streams.clone(),
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
        "Job instance status: InProgress\nJob instance status: Completed\n"
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
    for location in [JOB, "/v1/operations/op-1#FRAGMENT-CANARY"] {
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
