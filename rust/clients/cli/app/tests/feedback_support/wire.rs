// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    error::ErrorKind,
    http::{AsyncRawResponse, Body, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use std::{
    future::{Future, pending, poll_fn},
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};

const TOKEN: &str = "synthetic-opaque-feedback-token";
const PRIVATE: &str = "private-service-diagnostic-must-not-leak";

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
    fail: bool,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|s| (*s).into()).collect());
        if self.fail {
            return Err(azure_core::Error::with_message(
                ErrorKind::Credential,
                PRIVATE,
            ));
        }
        Ok(AccessToken::new(TOKEN, OffsetDateTime::UNIX_EPOCH))
    }
}

#[derive(Debug)]
struct Wire {
    requests: Mutex<Vec<Request>>,
    status: u16,
    transport_failure: bool,
    stall: bool,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        if self.stall {
            return pending().await;
        }
        if self.transport_failure {
            return Err(azure_core::Error::with_message(ErrorKind::Other, PRIVATE));
        }
        Ok(AsyncRawResponse::from_bytes(
            StatusCode::from(self.status),
            Headers::new(),
            PRIVATE.as_bytes().to_vec(),
        ))
    }
}

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(pending())
    }
}

fn harness(status: u16, transport_failure: bool, stall: bool) -> (Arc<Wire>, HttpExecutor) {
    let wire = Arc::new(Wire {
        requests: Mutex::default(),
        status,
        transport_failure,
        stall,
    });
    let executor =
        HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default()).unwrap();
    (wire, executor)
}

#[tokio::test]
async fn submission_uses_selected_exact_scope_and_single_authenticated_petrol_post() {
    for (environment, endpoint, scope) in [
        (
            Environment::Production,
            "https://petrol.office.microsoft.com/v1/feedback/authenticated",
            "https://petrol.office.microsoft.com/Feedback.ReadWrite",
        ),
        (
            Environment::Ppe,
            "https://petrol-int.office.microsoft.com/v1/feedback/authenticated",
            "https://petrol-int.office.microsoft.com/Feedback.ReadWrite",
        ),
    ] {
        let credential = Credential::default();
        let (wire, executor) = harness(200, false, false);
        let command = input(true);
        let result = fabric_cli::feedback::submit(
            &command,
            &config(environment),
            now(),
            &credential,
            &executor,
        )
        .await;
        assert!(result.accepted());
        assert_eq!(result.delivery, "accepted");
        assert_eq!(result.http_status, Some(200));
        assert_eq!(
            *credential.scopes.lock().unwrap(),
            vec![vec![scope.to_owned()]]
        );
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.url().as_str(), endpoint);
        assert_eq!(request.method(), Method::Post);
        assert_eq!(
            request.headers().get_str(&"authorization".into()).unwrap(),
            format!("Bearer {TOKEN}")
        );
        let Body::Bytes(body) = request.body() else {
            panic!("feedback body must be buffered")
        };
        let body: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(body, command.manifest(&config(environment), now()).unwrap());
        let receipt = serde_json::to_string(&result).unwrap();
        assert!(!receipt.contains(TOKEN));
        assert!(!receipt.contains(PRIVATE));
        assert!(!receipt.contains("Caller feedback"));
    }
}

#[tokio::test]
async fn status_classification_and_failures_never_replay_or_leak_provider_text() {
    for (status, delivery) in [
        (200, "accepted"),
        (202, "accepted"),
        (204, "accepted"),
        (302, "unknown"),
        (400, "rejected"),
        (401, "rejected"),
        (403, "rejected"),
        (408, "unknown"),
        (429, "rejected"),
        (500, "unknown"),
        (503, "unknown"),
    ] {
        let (wire, executor) = harness(status, false, false);
        let credential = Credential::default();
        let receipt = fabric_cli::feedback::submit(
            &input(true),
            &config(Environment::Ppe),
            now(),
            &credential,
            &executor,
        )
        .await;
        assert_eq!(receipt.delivery, delivery);
        assert_eq!(receipt.http_status, Some(status));
        assert_eq!(receipt.accepted(), delivery == "accepted");
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        assert!(!serde_json::to_string(&receipt).unwrap().contains(PRIVATE));
    }
    let (wire, executor) = harness(200, true, false);
    let receipt = fabric_cli::feedback::submit(
        &input(true),
        &config(Environment::Ppe),
        now(),
        &Credential::default(),
        &executor,
    )
    .await;
    assert_eq!(receipt.delivery, "unknown");
    assert_eq!(receipt.http_status, None);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
    assert!(!serde_json::to_string(&receipt).unwrap().contains(PRIVATE));
}

#[tokio::test]
async fn missing_send_invalid_manifest_or_auth_failure_produce_no_http_effect() {
    let (wire, executor) = harness(200, false, false);
    let credential = Credential::default();
    let result = fabric_cli::feedback::submit(
        &input(false),
        &config(Environment::Ppe),
        now(),
        &credential,
        &executor,
    )
    .await;
    assert_eq!(result.delivery, "not_sent");
    let mut invalid = input(true);
    invalid.feedback_id = None;
    let result = fabric_cli::feedback::submit(
        &invalid,
        &config(Environment::Ppe),
        now(),
        &credential,
        &executor,
    )
    .await;
    assert_eq!(result.delivery, "not_sent");
    assert!(result.client_feedback_id.is_none());
    invalid.feedback_id = Some("private-invalid-value".into());
    let result = fabric_cli::feedback::submit(
        &invalid,
        &config(Environment::Ppe),
        now(),
        &credential,
        &executor,
    )
    .await;
    assert_eq!(result.delivery, "not_sent");
    assert!(credential.scopes.lock().unwrap().is_empty());
    let credential = Credential {
        fail: true,
        ..Credential::default()
    };
    let result = fabric_cli::feedback::submit(
        &input(true),
        &config(Environment::Ppe),
        now(),
        &credential,
        &executor,
    )
    .await;
    assert_eq!(result.delivery, "not_sent");
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    assert!(wire.requests.lock().unwrap().is_empty());
    assert!(!serde_json::to_string(&result).unwrap().contains(PRIVATE));
}

#[tokio::test]
async fn dropping_a_pending_submission_stops_local_waiting_without_replay() {
    let (wire, executor) = harness(200, false, true);
    let credential = Credential::default();
    let command = input(true);
    let config = config(Environment::Ppe);
    let mut submission = Box::pin(fabric_cli::feedback::submit(
        &command,
        &config,
        now(),
        &credential,
        &executor,
    ));
    poll_fn(|cx| {
        assert!(submission.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
    drop(submission);
    tokio::task::yield_now().await;
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
    assert_eq!(credential.scopes.lock().unwrap().len(), 1);
}
