// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native headless selection -> actual SDK/shared HTTP -> retained cancellation.
#![deny(unnameable_test_items)]

use super::*;
use async_trait::async_trait;
use azure_core::http::{
    AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers,
};
use std::{
    collections::VecDeque,
    ffi::OsString,
    sync::{Arc, Mutex},
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const INSTANCE: &str = "33333333-3333-3333-3333-333333333333";
const PATH: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook/runs/33333333-3333-3333-3333-333333333333";
const TOKEN: &str =
    r#"{"access_token":"synthetic-native-cancel-token","token_type":"Bearer","expires_in":3600}"#;

#[derive(Debug)]
enum Reply {
    Response(u16, &'static str),
    Pending,
    Lost,
}

#[derive(Debug)]
struct Wire {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Request>>,
    fabric_sent: tokio::sync::Notify,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        if request.url().host_str() == Some("dailyapi.fabric.microsoft.com") {
            self.fabric_sent.notify_one();
        }
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP");
        match reply {
            Reply::Response(status, body) => Ok(AsyncRawResponse::from_bytes(
                StatusCode::from(status),
                Headers::new(),
                body.as_bytes().to_vec(),
            )),
            Reply::Pending => std::future::pending().await,
            Reply::Lost => Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::Io,
                "synthetic-transport-secret",
            )),
        }
    }
}

fn wire(replies: impl IntoIterator<Item = Reply>) -> Arc<Wire> {
    Arc::new(Wire {
        replies: Mutex::new(replies.into_iter().collect()),
        requests: Mutex::new(Vec::new()),
        fabric_sent: tokio::sync::Notify::new(),
    })
}

fn credential() -> NativeCredential {
    NativeCredential::Headless(
        crate::headless::Selection::from_inputs("client-secret", |name| -> Option<OsString> {
            match name {
                "FAB_RUST_TENANT" => Some(WORKSPACE.into()),
                "FAB_RUST_CLIENT_ID" => Some(ITEM.into()),
                "FAB_RUST_CLIENT_SECRET" => Some("synthetic-native-client-secret".into()),
                _ => None,
            }
        })
        .unwrap(),
    )
}

fn command(path: &str, extra: &[&str]) -> fabric_cli::parser::CancelCommand {
    let Invocation::Cancel(command) = fabric_cli::parser::parse_args(
        ["fab-rs", "cancel", path]
            .into_iter()
            .chain(extra.iter().copied()),
    )
    .unwrap() else {
        panic!("root cancel")
    };
    command
}

fn endpoint() -> EndpointBinding {
    EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap()
}

fn render(delivery: fabric_cli::cancel::Dispatch) -> serde_json::Value {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = delivery.write_to(&mut stdout, &mut stderr);
    let output = if exit == 0 {
        assert!(stderr.is_empty());
        stdout
    } else {
        assert!(stdout.is_empty());
        stderr
    };
    let output = String::from_utf8(output).unwrap();
    assert!(!output.contains("synthetic-native-cancel-token"));
    assert!(!output.contains("synthetic-native-client-secret"));
    assert!(!output.contains("synthetic-transport-secret"));
    serde_json::from_str(&output).unwrap()
}

#[tokio::test]
async fn native_headless_uses_lazy_credential_and_one_bound_cancel_without_metadata() {
    let http = wire([Reply::Response(200, TOKEN), Reply::Response(202, "null")]);
    let delivery = execute_cancel_injected(
        &command(PATH, &[]),
        (credential(), http.clone(), http.clone()),
        endpoint(),
        std::future::pending(),
    )
    .await;
    assert_eq!(delivery.exit_code(), 0);
    let output = render(delivery);
    assert_eq!(output["status"], "accepted");
    assert_eq!(output["data"]["instanceId"], INSTANCE);
    assert_eq!(output["meta"]["cancellation"]["confirmed"], false);
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].url().host_str(),
        Some("login.microsoftonline.com")
    );
    assert_eq!(requests[1].method(), Method::Post);
    assert_eq!(
        requests[1].url().as_str(),
        format!(
            "https://dailyapi.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/items/{ITEM}/jobs/instances/{INSTANCE}/cancel",
        )
    );
    assert!(http.replies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn native_preflight_precedes_even_lazy_credential_or_http_access() {
    for (path, extra, code) in [
        ("/invalid", vec![], "InvalidInput"),
        (PATH, vec!["--format", "text"], "explicitFormatNotSupported"),
        (
            PATH,
            vec!["--output_format", "text"],
            "explicitFormatNotSupported",
        ),
    ] {
        let http = wire([]);
        let output = render(
            execute_cancel_injected(
                &command(path, &extra),
                (credential(), http.clone(), http.clone()),
                endpoint(),
                std::future::pending(),
            )
            .await,
        );
        assert_eq!(output["error"]["code"], code);
        assert_eq!(output["meta"]["mutationMayHaveOccurred"], false);
        assert!(http.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn native_interruption_retains_uncertain_target_instead_of_dropping_dispatch() {
    let http = wire([Reply::Response(200, TOKEN), Reply::Pending]);
    let delivery = execute_cancel_injected(
        &command(PATH, &[]),
        (credential(), http.clone(), http.clone()),
        endpoint(),
        async { http.fabric_sent.notified().await },
    )
    .await
    .interrupted("timed_out");
    assert_eq!(delivery.exit_code(), 1);
    let output = render(delivery);
    assert_eq!(output["data"]["instanceId"], INSTANCE);
    assert_eq!(output["meta"]["mutationMayHaveOccurred"], true);
    assert_eq!(output["error"]["source"]["classification"], "Cancelled");
    assert_eq!(output["error"]["interruption"], "timed_out");
    assert_eq!(http.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn native_response_loss_does_not_refresh_retry_or_start_v1_job_host() {
    let http = wire([Reply::Response(200, TOKEN), Reply::Lost]);
    let output = render(
        execute_cancel_injected(
            &command(PATH, &[]),
            (credential(), http.clone(), http.clone()),
            endpoint(),
            std::future::pending(),
        )
        .await,
    );
    assert_eq!(output["status"], "error");
    assert_eq!(output["data"]["instanceId"], INSTANCE);
    assert_eq!(output["meta"]["mutationMayHaveOccurred"], true);
    assert_eq!(http.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn native_cancellation_before_acquisition_keeps_handle_without_remote_effects() {
    let http = wire([]);
    // The provider is lazy: connecting it is not token acquisition.
    let output = render(
        execute_cancel_injected(
            &command(PATH, &[]),
            (credential(), http.clone(), http.clone()),
            endpoint(),
            async {},
        )
        .await,
    );
    assert_eq!(output["data"]["instanceId"], INSTANCE);
    assert_eq!(output["meta"]["mutationMayHaveOccurred"], false);
    assert!(http.requests.lock().unwrap().is_empty());
}
