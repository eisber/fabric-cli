// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use super::*;
use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use std::{
    collections::VecDeque,
    future::{pending, ready},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

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
            "offline-native-schema",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Http(Mutex<VecDeque<Vec<u8>>>);

#[async_trait]
impl HttpClient for Http {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert!(matches!(request.method(), Method::Get | Method::Head));
        let body = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unplanned native HTTP");
        let mut headers = Headers::new();
        if request.method() == Method::Head {
            headers.insert("x-ms-resource-type", "directory");
        }
        Ok(AsyncRawResponse::from_bytes(StatusCode::Ok, headers, body))
    }
}

struct Timer;

impl fabric_sdk_http::ExecutionTimer for Timer {
    fn sleep(&self, _: Duration) -> fabric_sdk_http::ExecutionFuture<'_, ()> {
        Box::pin(pending())
    }
}

#[tokio::test]
async fn authenticated_native_entry_reaches_real_provider_without_navigation_or_config() {
    let http = Arc::new(Http(Mutex::new(VecDeque::from([
        br#"{"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}"#.to_vec(),
        br#"{"value":[{"id":"n","displayName":"Book","type":"Lakehouse","workspaceId":"w"}]}"#.to_vec(),
        Vec::new(),
        br#"{"paths":[{"name":"n/Tables/sales/_delta_log/1.json"}]}"#.to_vec(),
        br#"{"metaData":{"schemaString":"{\"fields\":[{\"name\":\"id\",\"type\":\"long\",\"nullable\":false}]}"}}"#.to_vec(),
    ]))));
    let sdk = fabric_sdk::Client::new(
        Arc::new(Credential),
        fabric_sdk::CredentialSelection::new("offline", "tenant", fabric_sdk::Audience::Fabric)
            .unwrap(),
    );
    let executor =
        fabric_sdk_http::HttpExecutor::new(http.clone(), Arc::new(Timer), Default::default())
            .unwrap();
    let output = execute_authenticated(
        &DataSchemaRequest::parse("/Demo.Workspace/Book.Lakehouse/Tables/sales").unwrap(),
        &sdk,
        &executor,
        fabric_cli_output::Timestamp::parse("2026-11-11T00:00:00Z").unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        output.exit,
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["columns"],
        serde_json::json!([{"name":"id","type":"long","nullable":false}])
    );
    assert_eq!(value["backend"], "delta-json-log");
    assert!(http.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_wins_before_host_work_and_after_synchronous_buffering() {
    let polled = AtomicBool::new(false);
    let result = controlled(
        async {
            polled.store(true, Ordering::SeqCst);
            Ok(())
        },
        ready(native::failure("cancelled", "cancelled", 1)),
    )
    .await;
    assert_eq!(result.unwrap_err().0, "cancelled");
    assert!(!polled.load(Ordering::SeqCst));
    let result = controlled(
        async {
            polled.store(true, Ordering::SeqCst);
            Ok(())
        },
        std::future::poll_fn(|_| {
            if polled.load(Ordering::SeqCst) {
                std::task::Poll::Ready(timed_out())
            } else {
                std::task::Poll::Pending
            }
        }),
    )
    .await;
    assert_eq!(result.unwrap_err().0, "timed_out");
}

#[tokio::test]
async fn deadline_drops_pending_host_work() {
    let result = controlled(pending::<Result<(), native::Failure>>(), async {
        tokio::time::sleep(Duration::from_millis(1)).await;
        timed_out()
    })
    .await;
    assert_eq!(result.unwrap_err().0, "timed_out");
}

#[tokio::test]
async fn expired_deadline_discards_buffered_success() {
    let mut stderr = Vec::new();
    let result = publish_stdout(
        Buffered {
            exit: 0,
            stdout: b"must-not-be-published\n".to_vec(),
            stderr: Vec::new(),
        },
        io::sink(),
        Instant::now() - Duration::from_secs(1),
        pending(),
    )
    .await;
    assert_eq!(deliver(result, &mut stderr).unwrap(), 1,);
    let value: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(value["error"]["code"], "timed_out");
}

#[tokio::test]
async fn partial_writer_failure_is_not_replayed_or_reported_as_success() {
    struct Partial(bool);
    impl Write for Partial {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            if self.0 {
                Err(io::ErrorKind::BrokenPipe.into())
            } else {
                self.0 = true;
                Ok(1)
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert!(
        publish_stdout(
            Buffered {
                exit: 0,
                stdout: b"schema\n".to_vec(),
                stderr: Vec::new()
            },
            Partial(false),
            Instant::now() + Duration::from_secs(1),
            pending(),
        )
        .await
        .is_err()
    );
}

struct BlockedSink {
    block_flush: bool,
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    release: std::sync::mpsc::Receiver<()>,
    finished: Option<std::sync::mpsc::Sender<()>>,
}

impl Write for BlockedSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.block_flush {
            return Ok(bytes.len());
        }
        if let Some(entered) = self.entered.take() {
            let _ = entered.send(());
            return Ok(bytes.len().min(1));
        }
        self.release
            .recv_timeout(Duration::from_secs(5))
            .map_err(io::Error::other)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.block_flush {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            self.release
                .recv_timeout(Duration::from_secs(5))
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
}

impl Drop for BlockedSink {
    fn drop(&mut self) {
        if let Some(finished) = self.finished.take() {
            let _ = finished.send(());
        }
    }
}

struct Release(Option<std::sync::mpsc::Sender<()>>);

impl Drop for Release {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}

async fn blocked_output(cancel: bool, block_flush: bool) {
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let release = Release(Some(release));
    let (finished, done) = std::sync::mpsc::channel();
    let deadline = Instant::now() + Duration::from_millis(if cancel { 5_000 } else { 100 });
    let start = Instant::now();
    let result = publish_stdout(
        Buffered {
            exit: 0,
            stdout: b"schema\n".to_vec(),
            stderr: Vec::new(),
        },
        BlockedSink {
            block_flush,
            entered: Some(entered),
            release: wait,
            finished: Some(finished),
        },
        deadline,
        async {
            started.await.unwrap();
            if cancel {
                native::failure("cancelled", "Synthetic output cancellation.", 1)
            } else {
                pending().await
            }
        },
    )
    .await;
    let elapsed = start.elapsed();
    // The host must return before releasing the blocked writer.
    let still_blocked = matches!(done.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    drop(release);
    done.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(still_blocked);
    assert!(elapsed < Duration::from_secs(2));
    let mut diagnostic = Vec::new();
    assert_eq!(deliver(result, &mut diagnostic).unwrap(), 1);
    let diagnostic: serde_json::Value = serde_json::from_slice(&diagnostic).unwrap();
    assert_eq!(
        diagnostic["error"]["code"],
        if cancel { "cancelled" } else { "timed_out" }
    );
    assert!(
        diagnostic["error"]["message"]
            .as_str()
            .unwrap()
            .contains("may be partial")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_stdout_deadline_stops_waiting_with_incomplete_output_evidence() {
    blocked_output(false, false).await;
    blocked_output(false, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_stdout_cancellation_stops_waiting_with_incomplete_output_evidence() {
    blocked_output(true, false).await;
    blocked_output(true, true).await;
}

#[tokio::test]
async fn ready_cancellation_never_starts_output_worker() {
    struct Forbidden;
    impl Write for Forbidden {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            panic!("cancelled publication must not write");
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("cancelled publication must not flush");
        }
    }
    let result = publish_stdout(
        Buffered {
            exit: 0,
            stdout: b"schema\n".to_vec(),
            stderr: Vec::new(),
        },
        Forbidden,
        Instant::now() + Duration::from_secs(5),
        ready(native::failure("cancelled", "Already cancelled.", 1)),
    )
    .await;
    assert_eq!(result.err().unwrap().0, "cancelled");
}

#[test]
fn shared_pipe_driver() {
    let Ok(mode) = std::env::var("FAB_SCHEMA_OUTPUT_TEST_DRIVER") else {
        return;
    };
    let cancel = mode == "cancel";
    let deadline = Instant::now() + Duration::from_millis(if cancel { 5_000 } else { 200 });
    let runtime = native::native_runtime().unwrap();
    let output = runtime.block_on(publish_stdout(
        Buffered {
            exit: 0,
            stdout: vec![b'x'; 4 * 1024 * 1024],
            stderr: Vec::new(),
        },
        owned_stream(OutputStream::Stdout).unwrap(),
        deadline,
        async {
            if cancel {
                tokio::time::sleep(Duration::from_millis(200)).await;
                native::failure("cancelled", "Synthetic shared-pipe cancellation.", 1)
            } else {
                pending().await
            }
        },
    ));
    let output = native::finish_runtime(runtime, output);
    let exit = deliver_native(output, deadline);
    std::process::exit(exit);
}

#[test]
fn shared_undrained_pipe_cannot_block_expiry_or_cancellation_diagnostics() {
    use std::process::{Command, Stdio};

    for mode in ["deadline", "cancel"] {
        let (reader, writer) = io::pipe().unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .env("FAB_SCHEMA_OUTPUT_TEST_DRIVER", mode)
            .args([
                "--exact",
                "schema::tests::shared_pipe_driver",
                "--nocapture",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::from(writer.try_clone().unwrap()))
            .stderr(Stdio::from(writer));
        for key in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let start = Instant::now();
        let mut child = command.spawn().unwrap();
        drop(command);
        // Keep the shared pipe open without draining either output stream.
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if start.elapsed() >= Duration::from_secs(4) {
                child.kill().unwrap();
                child.wait().unwrap();
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.and_then(|status| status.code()), Some(1), "{mode}");
        assert!(start.elapsed() < Duration::from_secs(4), "{mode}");
        drop(reader);
    }
}
