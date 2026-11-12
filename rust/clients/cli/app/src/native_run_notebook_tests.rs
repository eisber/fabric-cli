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
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use std::{collections::VecDeque, future::pending, sync::Mutex};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const RUN: &str = "33333333-3333-3333-3333-333333333333";
const TARGET: &str =
    "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook";

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
            "synthetic-native-run",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Wire {
    requests: Mutex<Vec<Request>>,
    replies: Mutex<VecDeque<(StatusCode, Headers, Vec<u8>)>>,
}
#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        let (status, headers, body) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Native Run must not replay");
        Ok(AsyncRawResponse::from_bytes(status, headers, body))
    }
}

#[tokio::test]
async fn actual_native_dispatch_uses_exact_selected_endpoint_and_native_payload_once() {
    let directory = tempfile::Builder::new()
        .prefix("native-run-own-")
        .tempdir()
        .unwrap();
    let file = directory.path().join("input.json");
    let input = br#"{"parameters":[{"name":"trial","type":"Boolean","value":false}],"executionData":{"compute":"Spark","computeConfiguration":{"driverCores":4,"numExecutors":1}}}"#;
    std::fs::write(&file, input).unwrap();
    let destination = directory.path().join("receipt.json");
    let fabric_cli::parser::Invocation::NativeTabular(command) = fabric_cli::parser::parse_args([
        "fab-rs",
        "artifact",
        "notebook",
        "run",
        TARGET,
        "--input",
        file.to_str().unwrap(),
        "--wait",
        "--output",
        destination.to_str().unwrap(),
    ])
    .unwrap() else {
        panic!("actual root facade not mounted")
    };
    let prepared = read_prepared(&command).unwrap();
    let mut headers = Headers::new();
    headers.insert("location", format!("https://msitapi.fabric.microsoft.com/v1/workspaces/{WS}/items/{ITEM}/jobs/instances/{RUN}"));
    headers.insert("retry-after", "0");
    let wire = Arc::new(Wire {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(VecDeque::from([
            (StatusCode::Accepted, headers, Vec::new()),
            (
                StatusCode::Ok,
                Headers::new(),
                format!("{{\"id\":\"{RUN}\",\"itemId\":\"{ITEM}\",\"status\":\"Completed\"}}")
                    .into_bytes(),
            ),
        ])),
    });
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "msitapi.fabric.microsoft.com").unwrap();
    let sdk = Client::with_context(RequestContext::with_endpoint(
        Arc::new(Credential),
        CredentialSelection::new("synthetic", WS, Audience::Fabric).unwrap(),
        endpoint.clone(),
    ));
    let executor = HttpExecutor::new(
        wire.clone(),
        Arc::new(fabric_sdk_transport::TokioTimer),
        ExecutionOptions::default(),
    )
    .unwrap()
    .with_endpoint(endpoint);
    let output = NativeOutput::prepare(prepared.output()).unwrap();
    let result = dispatch_prepared(
        &prepared,
        &sdk,
        executor,
        Instant::now() + Duration::from_secs(5),
        &output,
        pending(),
    )
    .await;
    assert_eq!(result.exit_code(), 0, "{}", result.value());
    assert_eq!(result.value()["status"], "completed");
    assert_eq!(result.value()["meta"]["handle"]["id"], RUN);
    assert_eq!(result.value()["meta"]["applicationSuccessVerified"], false);
    let receipt_bytes = std::fs::read(&destination).unwrap();
    assert!(receipt_bytes.ends_with(b"\n"));
    assert!(!receipt_bytes.ends_with(b"\n\n"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&receipt_bytes).unwrap(),
        *result.value()
    );
    assert!(NativeOutput::prepare(destination.to_str().unwrap()).is_err());
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method(), Method::Post);
    assert_eq!(
        requests[0].url().host_str(),
        Some("msitapi.fabric.microsoft.com")
    );
    assert_eq!(
        requests[0].url().path(),
        format!("/v1/workspaces/{WS}/notebooks/{ITEM}/jobs/execute/instances")
    );
    assert_eq!(requests[0].url().query(), Some("beta=false"));
    let azure_core::http::Body::Bytes(body) = requests[0].body() else {
        panic!("native JSON bytes")
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(body).unwrap(),
        serde_json::from_slice::<serde_json::Value>(input).unwrap()
    );
    assert_eq!(requests[1].method(), Method::Get);
    assert!(
        requests
            .iter()
            .all(|request| request.method() != Method::Delete)
    );
}

#[test]
fn malformed_file_input_is_rejected_by_native_preparation_before_identity() {
    let directory = tempfile::Builder::new()
        .prefix("native-run-input-")
        .tempdir()
        .unwrap();
    let file = directory.path().join("input.json");
    std::fs::write(&file, br#"{"parameters":[],"parameters":[]}"#).unwrap();
    let fabric_cli::parser::Invocation::NativeTabular(command) = fabric_cli::parser::parse_args([
        "fab-rs",
        "run",
        TARGET,
        "--input",
        file.to_str().unwrap(),
    ])
    .unwrap() else {
        panic!("actual root not mounted")
    };
    assert!(matches!(
        read_prepared(&command),
        Err(("InvalidInput", _, 2))
    ));
}

#[tokio::test]
async fn racing_output_is_never_overwritten_or_committed_twice() {
    let directory = tempfile::Builder::new()
        .prefix("native-run-race-")
        .tempdir()
        .unwrap();
    let path = directory.path().join("receipt.json");
    let output = NativeOutput::prepare(path.to_str().unwrap()).unwrap();
    std::fs::write(&path, b"racing-owner-data").unwrap();
    assert_eq!(
        output.commit(b"{\"status\":\"accepted\"}\n").await,
        Err(RunError::Output)
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"racing-owner-data");
    assert!(!output.partial.load(Ordering::SeqCst));
    assert_eq!(
        output.commit(b"second attempt").await,
        Err(RunError::Output)
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"racing-owner-data");
}

#[test]
fn notebook_transport_does_not_inherit_the_shorter_legacy_exchange_caps() {
    for remaining in [
        Duration::from_secs(300),
        Duration::from_secs(86_400),
        Duration::from_millis(1),
    ] {
        let options = transport_options(remaining);
        assert_eq!(options.connect_timeout, remaining);
        assert_eq!(options.read_timeout, remaining);
        assert_eq!(options.write_timeout, remaining);
    }
}
