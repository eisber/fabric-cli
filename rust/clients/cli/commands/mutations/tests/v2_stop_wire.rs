// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Stop argv through the preserved adapter, SDK and shared HTTP execution stack.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_mutations::{FabricMutationBackend, v2_stop};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_operations::{Audience, OperationLimits};
use fabric_sdk_workspaces::CredentialSelection;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending, ready},
    sync::{Arc, Mutex},
    time::Duration,
};

const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
const ITEM: &str = "22222222-2222-4222-8222-222222222222";
const TARGET: &str = "/11111111-1111-4111-8111-111111111111.Workspace/22222222-2222-4222-8222-222222222222.MirroredDatabase/runtime/mirroring";
const TIMEOUT: Duration = Duration::from_secs(240);

#[derive(Debug)]
struct Credential;

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        Ok(AccessToken::new(
            "synthetic-offline-stop",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Network {
    replies: Mutex<VecDeque<AsyncRawResponse>>,
    actions: Mutex<Vec<String>>,
}

#[async_trait]
impl HttpClient for Network {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.method(), Method::Post);
        assert!(Bytes::from(request.body()).is_empty());
        let prefix = format!(
            "https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/mirroredDatabases/{ITEM}/"
        );
        self.actions.lock().unwrap().push(
            request
                .url()
                .as_str()
                .strip_prefix(&prefix)
                .expect("exact Fabric mirroring route")
                .to_owned(),
        );
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("no polling, replay or compensation"))
    }
}

struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        assert_eq!(delay, TIMEOUT, "no polling or retry delay");
        Box::pin(pending())
    }
}

fn response(status: u16, body: Value) -> AsyncRawResponse {
    AsyncRawResponse::from_bytes(
        StatusCode::from(status),
        Headers::new(),
        if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&body).unwrap()
        },
    )
}

async fn invoke(
    approved: bool,
    replies: Vec<AsyncRawResponse>,
    cancelled: impl Future<Output = ()> + Send,
) -> (u8, Value, Vec<String>) {
    let command = v2_stop::parse_args(
        ["stop", TARGET]
            .into_iter()
            .chain(approved.then_some("--yes")),
    )
    .unwrap();
    let network = Arc::new(Network {
        replies: Mutex::new(replies.into()),
        actions: Mutex::new(Vec::new()),
    });
    let http = HttpExecutor::new(
        network.clone(),
        Arc::new(Timer),
        ExecutionOptions {
            timeout: TIMEOUT,
            max_delay: Duration::from_secs(3),
        },
    )
    .unwrap();
    let mut backend = FabricMutationBackend::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "synthetic-tenant", Audience::Fabric).unwrap(),
        &http,
        OperationLimits::default(),
        cancelled,
    );
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let run = tokio::time::timeout(
        Duration::from_secs(2),
        v2_stop::run(&command, &mut backend, &mut stdout, &mut stderr),
    )
    .await
    .expect("the bounded adapter must not follow a monitor");
    let document = if run.result.is_ok() {
        assert!(stderr.is_empty());
        serde_json::from_slice(&stdout).unwrap()
    } else {
        assert!(stdout.is_empty());
        serde_json::from_slice(&stderr).unwrap()
    };
    let actions = network.actions.lock().unwrap().clone();
    (run.exit_code(), document, actions)
}

#[tokio::test]
async fn no_op_and_in_progress_preconditions_use_only_the_status_request() {
    for (state, outcome, confirmed) in [
        ("Stopped", "noOp", true),
        ("Stopping", "alreadyStopping", false),
    ] {
        let (exit, value, actions) = invoke(
            false,
            vec![response(200, json!({"status": state}))],
            pending(),
        )
        .await;
        assert_eq!(exit, 0);
        assert_eq!(value["outcome"], outcome);
        assert_eq!(value["stoppedConfirmed"], confirmed);
        assert_eq!(actions, ["getMirroringStatus"]);
    }
}

#[tokio::test]
async fn running_without_intent_and_invalid_states_cannot_reach_a_stop_write() {
    for (state, code) in [
        ("Running", "ApprovalRequired"),
        ("Paused", "InvalidState"),
        ("FutureState", "InvalidResponse"),
    ] {
        let (exit, value, actions) = invoke(
            false,
            vec![response(200, json!({"status": state}))],
            pending(),
        )
        .await;
        assert_eq!(exit, 1);
        assert_eq!(value["error"]["code"], code);
        assert_eq!(actions, ["getMirroringStatus"]);
    }
}

#[tokio::test]
async fn acceptance_and_completed_request_are_not_stopped_confirmation() {
    for (submission, readback, outcome, confirmed) in [
        (202, None, "accepted", false),
        (200, Some("Running"), "requestCompleted", false),
        (200, Some("Stopped"), "stopped", true),
    ] {
        let mut replies = vec![
            response(200, json!({"status": "Running"})),
            response(submission, Value::Null),
        ];
        if let Some(state) = readback {
            replies.push(response(200, json!({"status": state})));
        }
        let (exit, value, actions) = invoke(true, replies, pending()).await;
        assert_eq!(exit, 0);
        assert_eq!(value["outcome"], outcome);
        assert_eq!(value["stoppedConfirmed"], confirmed);
        let mut expected = vec!["getMirroringStatus", "stopMirroring"];
        if readback.is_some() {
            expected.push("getMirroringStatus");
        }
        assert_eq!(actions, expected);
    }
}

#[tokio::test]
async fn already_cancelled_caller_budget_never_reaches_http_or_remote_cancel() {
    let (exit, value, actions) = invoke(true, Vec::new(), ready(())).await;
    assert_eq!(exit, 1);
    assert_eq!(value["error"]["code"], "PreconditionFailed");
    assert_eq!(value["mutationMayHaveOccurred"], false);
    assert!(actions.is_empty());
}
