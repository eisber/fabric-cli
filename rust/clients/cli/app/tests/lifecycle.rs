// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline subprocess coverage of the production runtime, signals, streams, and exit mapping.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::credentials::{AccessToken, TokenCredential, TokenRequestOptions};
use fabric_cli::Host;
use fabric_cli_command_contracts::CommandError;
use fabric_core::{ApiRequest, ApiResponse, Clock, CoreError, HttpTransport, OperationContext};
use fabric_identity::Binding;
use serde_json::Value;
use std::{
    collections::VecDeque,
    ffi::OsString,
    io::{BufRead, BufReader, Read},
    process::{Child, Command, ExitCode, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct FixtureClock {
    now: Mutex<Instant>,
    interrupt: bool,
    sleeps: AtomicUsize,
}

#[async_trait]
impl Clock for FixtureClock {
    fn now(&self) -> Instant {
        *self.now.lock().unwrap()
    }

    fn wall_now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(946684800)
    }

    async fn sleep(&self, delay: Duration, context: &OperationContext) -> Result<(), CoreError> {
        self.sleeps.fetch_add(1, Ordering::SeqCst);
        if self.interrupt {
            // The production lifecycle registers its signal listener before running commands.
            eprintln!("OFFLINE_SIGNAL_READY");
            std::future::pending::<()>().await;
        }
        let mut now = self.now.lock().unwrap();
        *now += delay;
        context.remaining_at(*now)?;
        Ok(())
    }
}

#[derive(Debug)]
struct FixtureCredential {
    unauthorized: bool,
}

#[async_trait]
impl TokenCredential for FixtureCredential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        if self.unauthorized {
            return Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Credential,
                "OFFLINE_SECRET_CANARY",
            ));
        }
        Ok(AccessToken::new(
            "OFFLINE_SECRET_CANARY",
            azure_core::time::OffsetDateTime::from_unix_timestamp(4102444800).unwrap(),
        ))
    }
}

struct FixtureTransport {
    responses: Mutex<VecDeque<ApiResponse>>,
    methods: Mutex<Vec<String>>,
}

#[async_trait]
impl HttpTransport for FixtureTransport {
    async fn send(
        &self,
        request: ApiRequest,
        _: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        self.methods.lock().unwrap().push(request.method);
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP attempt"))
    }
}

struct FixtureHost {
    clock: Arc<FixtureClock>,
    transport: Arc<FixtureTransport>,
    unauthorized: bool,
}

#[async_trait]
impl Host for FixtureHost {
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
        panic!("API must not persist a selection")
    }

    fn clear_selection(&self) -> Result<(), CommandError> {
        panic!("API must not clear a selection")
    }

    async fn login(&self, _: &OperationContext) -> Result<Binding, CommandError> {
        panic!("API must not invoke login")
    }

    fn credential(&self, _: Binding) -> Result<Arc<dyn TokenCredential>, CommandError> {
        Ok(Arc::new(FixtureCredential {
            unauthorized: self.unauthorized,
        }))
    }

    fn transport(&self) -> Result<Arc<dyn HttpTransport>, CommandError> {
        Ok(self.transport.clone())
    }
}

fn response(status: u16, body: &str, operation: bool) -> ApiResponse {
    ApiResponse {
        status,
        headers: if operation {
            [("x-ms-operation-id".into(), "offline-operation".into())].into()
        } else {
            Default::default()
        },
        body: body.as_bytes().to_vec(),
    }
}

fn fixture(scenario: &str) -> u8 {
    let responses = match scenario {
        "async" | "cancel" => vec![response(202, r#"{"accepted":true}"#, true)],
        "poll" => vec![
            response(202, r#"{"accepted":true}"#, true),
            response(200, r#"{"status":"Succeeded"}"#, false),
        ],
        "failed" => vec![
            response(202, "{}", true),
            response(200, r#"{"status":"Failed"}"#, false),
        ],
        "service" => vec![response(403, r#"{"error":"offline-denied"}"#, false)],
        "unauthorized" => vec![],
        _ => panic!("unknown offline lifecycle scenario"),
    };
    let transport = Arc::new(FixtureTransport {
        responses: Mutex::new(responses.into()),
        methods: Mutex::new(Vec::new()),
    });
    let host = FixtureHost {
        clock: Arc::new(FixtureClock {
            now: Mutex::new(Instant::now()),
            interrupt: scenario == "cancel",
            sleeps: AtomicUsize::new(0),
        }),
        transport: transport.clone(),
        unauthorized: scenario == "unauthorized",
    };
    let mut args: Vec<OsString> = ["fab-rs", "api", "items", "--method", "post"]
        .into_iter()
        .map(Into::into)
        .collect();
    if scenario == "async" {
        args.extend([OsString::from("--async"), OsString::from("--show-headers")]);
    }
    let code = fabric_cli::lifecycle::run_process(args, &host);
    let expected: &[&str] = match scenario {
        "unauthorized" => &[],
        "poll" | "failed" => &["POST", "GET"],
        _ => &["POST"],
    };
    assert_eq!(
        transport.methods.lock().unwrap().as_slice(),
        expected,
        "mutation replay or unexpected polling"
    );
    if scenario == "async" {
        assert_eq!(host.clock.sleeps.load(Ordering::SeqCst), 0);
    }
    code
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn subprocess(scenario: &str) -> Output {
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(["--offline-lifecycle-fixture", scenario])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let stderr = child.0.stderr.take().unwrap();
    let stdout = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        BufReader::new(stdout).read_to_end(&mut bytes).unwrap();
        bytes
    });
    let (ready, receive_ready) = mpsc::channel();
    let stderr = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut reader = BufReader::new(stderr);
        loop {
            let mut line = Vec::new();
            if reader.read_until(b'\n', &mut line).unwrap() == 0 {
                break;
            }
            if line == b"OFFLINE_SIGNAL_READY\n" || line == b"OFFLINE_SIGNAL_READY\r\n" {
                let _ = ready.send(());
            }
            bytes.extend(line);
        }
        bytes
    });
    if scenario == "cancel" {
        receive_ready.recv_timeout(Duration::from_secs(5)).unwrap();
        #[cfg(unix)]
        assert!(
            Command::new("kill")
                .args(["-INT", &child.0.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        #[cfg(not(unix))]
        panic!("this signal fixture must only be invoked on Unix");
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "offline lifecycle subprocess hung"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

fn assert_lifecycle(scenario: &str) {
    let output = subprocess(scenario);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("OFFLINE_SECRET_CANARY"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("OFFLINE_SECRET_CANARY"));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.stdout.ends_with(b"\n"));
    if scenario == "cancel" {
        assert!(
            output.stderr == b"OFFLINE_SIGNAL_READY\n"
                || output.stderr == b"OFFLINE_SIGNAL_READY\r\n",
            "{:?}",
            output.stderr
        );
    } else {
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    }
    match scenario {
        "unauthorized" => {
            assert_eq!(output.status.code(), Some(4));
            assert_eq!(envelope["result"]["error_code"], "Unauthorized");
        }
        "failed" | "cancel" => {
            assert_eq!(output.status.code(), Some(1));
            assert_eq!(envelope["status"], "Failure");
            if scenario == "cancel" {
                let stdout = String::from_utf8(output.stdout).unwrap();
                assert!(stdout.contains("Cancelled"));
                assert!(stdout.contains("offline-operation"));
            }
        }
        _ => {
            assert!(output.status.success());
            assert_eq!(envelope["status"], "Success");
            let result = &envelope["result"]["data"][0];
            assert_eq!(
                result["status_code"],
                match scenario {
                    "async" => 202,
                    "service" => 403,
                    _ => 200,
                }
            );
            if scenario == "async" {
                assert_eq!(result["text"]["accepted"], true);
                assert_eq!(result["headers"]["x-ms-operation-id"], "offline-operation");
            } else if scenario == "poll" {
                assert_eq!(result["text"]["status"], "Succeeded");
            }
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--offline-lifecycle-fixture") {
        return ExitCode::from(fixture(args.get(2).expect("fixture scenario")));
    }
    for scenario in ["async", "poll", "unauthorized", "failed", "service"] {
        assert_lifecycle(scenario);
    }
    #[cfg(unix)]
    assert_lifecycle("cancel");
    ExitCode::SUCCESS
}
