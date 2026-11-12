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

#[test]
fn expired_deadline_discards_buffered_success() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        deliver(
            Ok(Buffered {
                exit: 0,
                stdout: b"must-not-be-published\n".to_vec(),
                stderr: Vec::new(),
            }),
            Instant::now() - Duration::from_secs(1),
            &mut stdout,
            &mut stderr,
        )
        .unwrap(),
        1,
    );
    assert!(stdout.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(value["error"]["code"], "timed_out");
}

#[test]
fn partial_writer_failure_is_not_replayed_or_reported_as_success() {
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
        deliver(
            Ok(Buffered {
                exit: 0,
                stdout: b"schema\n".to_vec(),
                stderr: Vec::new()
            }),
            Instant::now() + Duration::from_secs(1),
            &mut Partial(false),
            &mut Vec::new(),
        )
        .is_err()
    );
}
