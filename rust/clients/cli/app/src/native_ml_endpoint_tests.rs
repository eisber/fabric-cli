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
use fabric_cli::parser::{Invocation, parse_args};
use fabric_sdk::{Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin};
use fabric_sdk_http::ExecutionOptions;
use fabric_sdk_transport::TokioTimer;
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const MODEL: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
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
        Ok(AccessToken::new(
            "synthetic-ml-endpoint-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug, Default)]
struct Wire {
    replies: Mutex<VecDeque<(StatusCode, Headers, Vec<u8>)>>,
    requests: Mutex<Vec<Request>>,
    pending_after_replies: bool,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(
            request.url().host_str(),
            Some("msitapi.fabric.microsoft.com")
        );
        self.requests.lock().unwrap().push(request.clone());
        let reply = self.replies.lock().unwrap().pop_front();
        if self.pending_after_replies && reply.is_none() {
            return std::future::pending().await;
        }
        let (status, headers, body) = reply.expect("unexpected dispatch or mutation replay");
        Ok(AsyncRawResponse::from_bytes(status, headers, body))
    }
}

fn reply(status: StatusCode, body: Value) -> (StatusCode, Headers, Vec<u8>) {
    (status, Headers::new(), serde_json::to_vec(&body).unwrap())
}

fn accepted(location: &str) -> (StatusCode, Headers, Vec<u8>) {
    let mut headers = Headers::new();
    headers.insert("x-ms-operation-id", OPERATION);
    headers.insert("location", location.to_owned());
    headers.insert("retry-after", "0");
    (StatusCode::Accepted, headers, b"null".to_vec())
}

fn parse(leaf: &[&str]) -> EndpointCommand {
    let Invocation::MlEndpoint(command) = parse_args(
        ["fab-rs", "artifact", "ml-endpoint"]
            .into_iter()
            .chain(leaf.iter().copied())
            .chain(["--workspace-id", WORKSPACE, "--model-id", MODEL]),
    )
    .unwrap() else {
        panic!("root ML endpoint dispatch required")
    };
    command
}

fn configured(wire: Arc<Wire>) -> (Client, HttpExecutor, Arc<Credential>) {
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "msitapi.fabric.microsoft.com").unwrap();
    let credential = Arc::new(Credential::default());
    let sdk = Client::with_context(RequestContext::with_endpoint(
        credential.clone(),
        CredentialSelection::new("synthetic-account", "synthetic-tenant", Audience::Fabric)
            .unwrap(),
        endpoint.clone(),
    ));
    let executor = HttpExecutor::new(wire, Arc::new(TokioTimer), ExecutionOptions::default())
        .unwrap()
        .with_endpoint(endpoint);
    (sdk, executor, credential)
}

async fn invoke(
    wire: Arc<Wire>,
    leaf: &[&str],
    timeout: Duration,
) -> (CommandOutcome, Value, Arc<Credential>) {
    let command = parse(leaf);
    let (sdk, executor, credential) = configured(wire);
    let mut state = crate::batch_state::BatchState::capture();
    let delivery = super::super::execute_authenticated_injected(
        super::super::NativeCommand::MlEndpoint(&command, Instant::now() + timeout),
        sdk,
        executor,
        "/",
        fabric_cli_context::HomeContext::Unspecified,
        super::super::settings::NativeSettings::select(None).unwrap(),
        &mut state,
        false,
        None,
    )
    .await
    .unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let outcome = delivery.write_to(&mut out, &mut err).unwrap();
    let bytes = if matches!(outcome, CommandOutcome::Returned(0)) {
        assert!(err.is_empty());
        out
    } else {
        assert!(out.is_empty());
        err
    };
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains("synthetic-ml-endpoint-token"));
    (outcome, serde_json::from_slice(&bytes).unwrap(), credential)
}

#[tokio::test]
async fn actual_root_native_dispatch_uses_msit_fabric_audience_without_item_discovery() {
    for (leaf, suffix, data) in [
        (
            vec!["settings", "get"],
            "",
            json!({"defaultVersionName":"1","extra":false}),
        ),
        (
            vec!["serving-version", "list"],
            "/versions",
            json!({"value":[],"continuationToken":"next","extra":null}),
        ),
        (
            vec!["serving-version", "get", "--version", "01"],
            "/versions/01",
            json!({"versionName":"01","status":"FutureStatus","extra":[]}),
        ),
    ] {
        let wire = Arc::new(Wire {
            replies: Mutex::new(VecDeque::from([reply(StatusCode::Ok, data.clone())])),
            ..Wire::default()
        });
        let (outcome, output, credential) =
            invoke(wire.clone(), &leaf, Duration::from_secs(10)).await;
        assert_eq!(outcome, CommandOutcome::Returned(0));
        assert_eq!(output["data"], data);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), Method::Get);
        assert_eq!(
            requests[0].url().as_str(),
            format!(
                "https://msitapi.fabric.microsoft.com/v1/workspaces/{WORKSPACE}/mlmodels/{MODEL}/endpoint{suffix}"
            )
        );
        assert_eq!(
            *credential.scopes.lock().unwrap(),
            vec![vec!["https://api.fabric.microsoft.com/.default".to_owned()]]
        );
    }
}

#[tokio::test]
async fn actual_root_default_reads_settings_then_exact_configured_version() {
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([
            reply(StatusCode::Ok, json!({"defaultVersionName":"01"})),
            reply(
                StatusCode::Ok,
                json!({"versionName":"01","status":"Active"}),
            ),
        ])),
        ..Wire::default()
    });
    let (outcome, output, _) = invoke(
        wire.clone(),
        &["serving-version", "get", "--default"],
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(outcome, CommandOutcome::Returned(0));
    assert_eq!(output["selection"]["versionName"], "01");
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].url().path().ends_with("/endpoint"));
    assert!(requests[1].url().path().ends_with("/versions/01"));
}

#[tokio::test]
async fn accepted_activation_and_score_are_not_completed_or_predictions() {
    for leaf in [
        vec!["serving-version", "activate", "--version", "1", "--force"],
        vec![
            "score",
            "--default",
            "--force",
            "--body",
            r#"{"inputs":[]}"#,
        ],
    ] {
        let wire = Arc::new(Wire {
            replies: Mutex::new(VecDeque::from([accepted(&format!(
                "https://msitapi.fabric.microsoft.com/v1/operations/{OPERATION}"
            ))])),
            ..Wire::default()
        });
        let (outcome, output, _) = invoke(wire.clone(), &leaf, Duration::from_secs(10)).await;
        assert_eq!(outcome, CommandOutcome::Returned(0));
        assert_eq!(
            output,
            json!({"status":"accepted","httpStatus":202,"operationId":OPERATION})
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn waiting_score_returns_native_predictions_from_result_not_state() {
    let native = json!({"predictions":[false,0,null,[],{}],"future":""});
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([
            accepted(&format!(
                "https://msitapi.fabric.microsoft.com/v1/operations/{OPERATION}"
            )),
            reply(StatusCode::Ok, json!({"status":"Succeeded"})),
            reply(StatusCode::Ok, native.clone()),
        ])),
        ..Wire::default()
    });
    let (outcome, output, _) = invoke(
        wire.clone(),
        &[
            "score",
            "--version",
            "01",
            "--force",
            "--wait",
            "--body",
            r#"{"inputs":[false,0,null]}"#,
        ],
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(outcome, CommandOutcome::Returned(0));
    assert_eq!(output["data"], native);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method(), Method::Post);
    assert!(requests[0].url().path().ends_with("/versions/01/score"));
    assert!(
        requests[2]
            .url()
            .path()
            .ends_with(&format!("/operations/{OPERATION}/result"))
    );
}

#[tokio::test]
async fn foreign_lro_link_is_rejected_without_following_or_replaying() {
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([accepted(&format!(
            "https://api.fabric.microsoft.com/v1/operations/{OPERATION}"
        ))])),
        ..Wire::default()
    });
    let (outcome, output, _) = invoke(
        wire.clone(),
        &[
            "serving-version",
            "activate",
            "--version",
            "1",
            "--force",
            "--wait",
        ],
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(outcome, CommandOutcome::Returned(1));
    assert_eq!(output["error"]["code"], "InvalidLink");
    assert_eq!(output["error"]["mutationMayHaveOccurred"], true);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn timeout_preserves_operation_and_uncertain_effect_after_one_mutation() {
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([accepted(&format!(
            "https://msitapi.fabric.microsoft.com/v1/operations/{OPERATION}"
        ))])),
        pending_after_replies: true,
        ..Wire::default()
    });
    let (outcome, output, _) = invoke(
        wire.clone(),
        &[
            "serving-version",
            "activate",
            "--version",
            "1",
            "--force",
            "--wait",
        ],
        Duration::from_millis(30),
    )
    .await;
    assert_eq!(outcome, CommandOutcome::Stop(1));
    assert_eq!(output["error"]["code"], "timed_out");
    assert_eq!(output["error"]["operationId"], OPERATION);
    assert_eq!(output["error"]["mutationMayHaveOccurred"], true);
    assert_eq!(
        wire.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method() == Method::Post)
            .count(),
        1
    );
}

#[tokio::test]
async fn provider_failure_retains_raw_fields_and_actual_status_on_stderr() {
    let raw = json!({"errorCode":"Denied","message":"synthetic-provider-error","future":{"present":false}});
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([reply(StatusCode::Forbidden, raw.clone())])),
        ..Wire::default()
    });
    let (outcome, output, _) =
        invoke(wire.clone(), &["settings", "get"], Duration::from_secs(10)).await;
    assert_eq!(outcome, CommandOutcome::Returned(1));
    assert_eq!(output["error"]["httpStatus"], 403);
    assert_eq!(output["error"]["serviceError"], raw);
    assert_eq!(output["error"]["mutationMayHaveOccurred"], false);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn expired_preconnect_deadline_does_not_poll_credential_work() {
    let result: Result<(), Failure> = before_connect(
        async { panic!("expired authentication must not start") },
        Instant::now() - Duration::from_secs(1),
    )
    .await;
    assert_eq!(result.unwrap_err().0, "timed_out");
}
