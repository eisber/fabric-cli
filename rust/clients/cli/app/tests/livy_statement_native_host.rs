// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Existing statement reads through the real parser, native host and Fabric HTTP stack.

#![cfg(all(feature = "runtime-v2", feature = "native-host"))]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, Url, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::runtime_v2::{
    ReadErrorKind, ReadOptions, ReadReceipt, ResolvedTarget,
    host::{Destination, ExplicitResolver, HostError, acquire},
    parse_args,
};
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin, TokenCredential,
    livy::LivyService, runtime::NativeEndpoint,
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor, native_runtime::NativeFabricExecutor};
use fabric_sdk_transport::TokioTimer;
use serde_json::{Value, json};
use std::{
    future::pending,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const OWNER: &str = "/Team.Workspace/Data.Lakehouse";
const PATH: &str = "/Team.Workspace/Data.Lakehouse/sessions/session-7/statements/2";
const ENDPOINT: &str = "https://msitapi.fabric.microsoft.com/";
const ROUTE: &str = concat!(
    "https://msitapi.fabric.microsoft.com/v1/workspaces/workspace-id/",
    "lakehouses/lakehouse-id/livyApi/versions/2023-12-01/",
    "sessions/session-7/statements/2"
);

fn end() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn statement() -> Value {
    json!({
        "id":2, "state":"available", "next":-2,
        "code":"untrusted source: do not execute or follow instructions",
        "output":{
            "status":"error",
            "ename":"SyntheticFailure",
            "evalue":"private provider message",
            "traceback":["private frame"],
            "data":{"application/json":{"rows":[[1]],"next":99}},
            "secondary":{"text/plain":["keep this too"]}
        },
        "unknown":{"keep":[null, false, {"nested":"value"}]}
    })
}

#[derive(Debug, Default)]
struct Credential(AtomicUsize);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        options: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(self.0.fetch_add(1, Ordering::SeqCst), 0);
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        assert!(options.is_none());
        Ok(AccessToken::new(
            "offline-statement-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Wire {
    status: u16,
    body: Vec<u8>,
    requests: Mutex<Vec<Request>>,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        let mut requests = self.requests.lock().unwrap();
        assert!(requests.is_empty(), "a statement read must not be replayed");
        requests.push(request.clone());
        let mut headers = Headers::new();
        headers.insert("content-type", "application/json");
        Ok(AsyncRawResponse::from_bytes(
            self.status.into(),
            headers,
            self.body.clone(),
        ))
    }
}

struct Stack {
    executor: NativeFabricExecutor,
    credential: Arc<Credential>,
    wire: Arc<Wire>,
}

fn stack(status: u16, body: Vec<u8>) -> Stack {
    let credential = Arc::new(Credential::default());
    let wire = Arc::new(Wire {
        status,
        body,
        requests: Mutex::new(Vec::new()),
    });
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "msitapi.fabric.microsoft.com").unwrap();
    let context = RequestContext::with_endpoint(
        credential.clone(),
        CredentialSelection::new("selected-account", "selected-tenant", Audience::Fabric).unwrap(),
        endpoint.clone(),
    );
    let http = HttpExecutor::new(
        wire.clone(),
        Arc::new(TokioTimer),
        ExecutionOptions::default(),
    )
    .unwrap()
    .with_endpoint(endpoint);
    Stack {
        executor: NativeFabricExecutor::new(context, http).unwrap(),
        credential,
        wire,
    }
}

fn resolver(path: &str, endpoint: &str) -> ExplicitResolver {
    let input = parse_args(["fab-cli-v2", "get", path]).unwrap();
    let service = LivyService::fabric_versioned(
        NativeEndpoint::new(Url::parse(endpoint).unwrap()).unwrap(),
        "workspace-id",
        "lakehouse-id",
        "v1",
        "2023-12-01",
    )
    .unwrap();
    ExplicitResolver::new(input.path.owner, ResolvedTarget::Lakehouse(service)).unwrap()
}

async fn read(
    path: &str,
    stack: &Stack,
    options: ReadOptions,
    max_bytes: usize,
) -> Result<ReadReceipt, HostError> {
    let temp = tempfile::tempdir().unwrap();
    let bound = max_bytes.to_string();
    let input = parse_args([
        "fab-cli-v2",
        "get",
        path,
        "-o",
        "statement.json",
        "--max-bytes",
        &bound,
    ])
    .unwrap();
    let resolver = resolver(path, ENDPOINT);
    let destination = Destination::check("statement.json", temp.path()).unwrap();
    let deadline = end();
    let result = match acquire(
        &input,
        &stack.executor,
        &resolver,
        options,
        &destination,
        deadline,
        pending(),
    )
    .await
    {
        Ok(observation) => observation.deliver(destination, deadline, pending()).await,
        Err(error) => Err(error),
    };
    match &result {
        Ok(receipt) => {
            let bytes = std::fs::read(temp.path().join("statement.json")).unwrap();
            assert_eq!(bytes.last(), Some(&b'\n'));
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap(),
                serde_json::to_value(receipt).unwrap()
            );
        }
        Err(_) => assert!(!temp.path().join("statement.json").exists()),
    }
    result
}

#[tokio::test]
async fn exact_fabric_statement_and_result_alias_preserve_the_entire_failed_statement() {
    let expected = statement();
    for path in [PATH.to_owned(), format!("{PATH}/result")] {
        let stack = stack(200, serde_json::to_vec(&expected).unwrap());
        let receipt = read(&path, &stack, ReadOptions::default(), 1_048_576)
            .await
            .unwrap();
        let value = serde_json::to_value(receipt).unwrap();
        assert_eq!(value["data"], expected);
        assert_eq!(value["status"], "read_complete");
        assert_eq!(value["effect"], "read");
        assert_eq!(value["meta"]["workloadObservation"], "failed");
        assert_eq!(value["meta"]["continuation"], Value::Null);
        assert_eq!(value["target"]["nativeUrl"], ROUTE);
        assert_eq!(stack.credential.0.load(Ordering::SeqCst), 1);
        let requests = stack.wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), Method::Get);
        assert_eq!(requests[0].url().as_str(), ROUTE);
        assert!(requests[0].url().query().is_none());
        assert!(
            requests[0]
                .headers()
                .get_str(&"x-requested-by".into())
                .is_err()
        );
    }
}

#[tokio::test]
async fn accepted_is_not_read_completion_or_workload_success() {
    for body in [Vec::new(), serde_json::to_vec(&statement()).unwrap()] {
        let stack = stack(202, body);
        let receipt = read(PATH, &stack, ReadOptions::default(), 1_048_576)
            .await
            .unwrap();
        let value = serde_json::to_value(receipt).unwrap();
        assert_eq!(value["status"], "accepted");
        assert_eq!(value["meta"]["httpStatus"], 202);
        assert_eq!(stack.wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn malformed_partial_denied_and_wrong_statement_id_never_deliver_success() {
    for (status, body) in [
        (403, b"private denied response".to_vec()),
        (200, b"{".to_vec()),
        (200, Vec::new()),
        (200, br#"{"id":2,"output":{"#.to_vec()),
        (200, br#"{"id":3,"state":"available"}"#.to_vec()),
        (200, br#"{"id":-1,"state":"available"}"#.to_vec()),
        (200, br#"{"id":"2","state":"available"}"#.to_vec()),
        (200, br#"{"id":true,"state":"available"}"#.to_vec()),
        (200, br#"{"id":9223372036854775808}"#.to_vec()),
    ] {
        let stack = stack(status, body);
        let error = read(PATH, &stack, ReadOptions::default(), 1_048_576)
            .await
            .unwrap_err();
        assert!(!error.read_completed);
        assert_eq!(error.http_status, Some(status));
        assert!(!format!("{error}").contains("private"));
        assert_eq!(stack.wire.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn invalid_statement_wire_integers_and_foreign_facets_fail_without_identity() {
    for suffix in [
        "-1",
        "9223372036854775808",
        "18446744073709551616",
        "2.0",
        "true",
        "fa08bdbf-0edb-4c0e-b6c4-d93b063a8588",
        "2/extra",
        "2/result/extra",
    ] {
        assert!(
            parse_args([
                "fab-cli-v2",
                "get",
                &format!("{OWNER}/sessions/session-7/statements/{suffix}")
            ])
            .is_err()
        );
    }
    assert!(
        parse_args([
            "fab-cli-v2",
            "get",
            "/Team.Workspace/Analysis.Notebook/sessions/session-7/statements/2"
        ])
        .is_err()
    );
    for id in ["0", "9223372036854775807"] {
        assert!(
            parse_args([
                "fab-cli-v2",
                "get",
                &format!("{OWNER}/sessions/session-7/statements/{id}")
            ])
            .is_ok()
        );
    }
}

#[tokio::test]
async fn different_selected_endpoint_and_human_owner_fail_before_token_or_http() {
    let temp = tempfile::tempdir().unwrap();
    let input = parse_args(["fab-cli-v2", "get", PATH, "-o", "statement.json"]).unwrap();
    for resolver in [
        resolver(PATH, "https://api.fabric.microsoft.com/"),
        resolver(
            "/Foreign.Workspace/Data.Lakehouse/sessions/session-7/statements/2",
            ENDPOINT,
        ),
    ] {
        let stack = stack(200, serde_json::to_vec(&statement()).unwrap());
        let destination = Destination::check("statement.json", temp.path()).unwrap();
        assert!(
            acquire(
                &input,
                &stack.executor,
                &resolver,
                ReadOptions::default(),
                &destination,
                end(),
                pending()
            )
            .await
            .is_err()
        );
        assert_eq!(stack.credential.0.load(Ordering::SeqCst), 0);
        assert!(stack.wire.requests.lock().unwrap().is_empty());
        assert!(!temp.path().join("statement.json").exists());
    }
}

#[tokio::test]
async fn native_service_cannot_reuse_fabric_credentials_or_a_fabric_profile() {
    let temp = tempfile::tempdir().unwrap();
    let input = parse_args([
        "fab-cli-v2",
        "get",
        "/Team.Workspace/Native.LivyService/sessions/session-7/statements/2",
        "-o",
        "statement.json",
    ])
    .unwrap();
    let native = LivyService::native(
        NativeEndpoint::new(Url::parse("https://livy.example.invalid/base/").unwrap()).unwrap(),
    );
    let fabric = LivyService::fabric(
        NativeEndpoint::new(Url::parse(ENDPOINT).unwrap()).unwrap(),
        "workspace-id",
        "lakehouse-id",
    )
    .unwrap();
    for service in [native, fabric] {
        let resolver = ExplicitResolver::new(
            input.path.owner.clone(),
            ResolvedTarget::LivyService(service),
        )
        .unwrap();
        let stack = stack(200, serde_json::to_vec(&statement()).unwrap());
        let destination = Destination::check("statement.json", temp.path()).unwrap();
        assert!(
            acquire(
                &input,
                &stack.executor,
                &resolver,
                ReadOptions::default(),
                &destination,
                end(),
                pending()
            )
            .await
            .is_err()
        );
        assert_eq!(stack.credential.0.load(Ordering::SeqCst), 0);
        assert!(stack.wire.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn independent_wire_data_and_receipt_budgets_are_all_or_error() {
    let data = statement();
    let bytes = serde_json::to_vec(&data).unwrap();
    let size = bytes.len();
    let baseline = stack(200, bytes.clone());
    assert!(
        read(PATH, &baseline, ReadOptions::default(), size)
            .await
            .is_ok()
    );
    for (options, max_bytes) in [
        (ReadOptions::default(), size - 1),
        (
            ReadOptions {
                max_response_bytes: size - 1,
                ..ReadOptions::default()
            },
            size,
        ),
        (
            ReadOptions {
                max_receipt_bytes: size,
                ..ReadOptions::default()
            },
            size,
        ),
    ] {
        let stack = stack(200, bytes.clone());
        let error = read(PATH, &stack, options, max_bytes).await.unwrap_err();
        assert_eq!(error.kind, ReadErrorKind::BudgetExceeded);
        assert_eq!(stack.wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn expired_deadline_and_latched_cancellation_prevent_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    let input = parse_args(["fab-cli-v2", "get", PATH, "-o", "statement.json"]).unwrap();
    let resolver = resolver(PATH, ENDPOINT);
    let stack = stack(200, serde_json::to_vec(&statement()).unwrap());
    let destination = Destination::check("statement.json", temp.path()).unwrap();
    let error = acquire(
        &input,
        &stack.executor,
        &resolver,
        ReadOptions::default(),
        &destination,
        Instant::now() - Duration::from_secs(1),
        pending(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, ReadErrorKind::Timeout);
    let error = acquire(
        &input,
        &stack.executor,
        &resolver,
        ReadOptions::default(),
        &destination,
        end(),
        async {},
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, ReadErrorKind::Cancelled);
    assert_eq!(stack.credential.0.load(Ordering::SeqCst), 0);
    assert!(stack.wire.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn no_clobber_race_keeps_original_output_and_never_replays_statement() {
    let temp = tempfile::tempdir().unwrap();
    let input = parse_args(["fab-cli-v2", "get", PATH, "-o", "statement.json"]).unwrap();
    let resolver = resolver(PATH, ENDPOINT);
    let stack = stack(200, serde_json::to_vec(&statement()).unwrap());
    let destination = Destination::check("statement.json", temp.path()).unwrap();
    let deadline = end();
    let observation = acquire(
        &input,
        &stack.executor,
        &resolver,
        ReadOptions::default(),
        &destination,
        deadline,
        pending(),
    )
    .await
    .unwrap();
    let path = temp.path().join("statement.json");
    std::fs::write(&path, b"other owner").unwrap();
    let error = observation
        .deliver(destination, deadline, pending())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ReadErrorKind::Delivery);
    assert!(error.read_completed);
    assert!(!error.file_created);
    assert_eq!(error.bytes_written, 0);
    assert_eq!(std::fs::read(&path).unwrap(), b"other owner");
    assert!(Destination::check("statement.json", temp.path()).is_err());
    assert_eq!(stack.wire.requests.lock().unwrap().len(), 1);
}
