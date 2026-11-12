// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Existing create parser -> shared planner -> typed Lakehouse -> scripted HTTP.
//!
//! These tests do not stand in for the separately composed approval/output host.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_mutations::{FabricMutationBackend, v2_create};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_items::{
    create::{CreateTarget, LeafState, NativeId, Preflight, TargetScope},
    lakehouse_create::{LakehouseCreateInput, LakehouseCreateRequest, LakehouseCreateSchema},
};
use fabric_sdk_operations::{Audience, OperationLimits};
use fabric_sdk_workspaces::{
    CredentialSelection,
    mutations::{CompletionPolicy, ResourceError, ResourceOutcome},
};
use serde_json::json;
use std::{
    collections::VecDeque,
    future::{Future, pending, ready},
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

const ORIGIN: &str = "https://api.fabric.microsoft.com";
const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
const ITEM: &str = "22222222-2222-4222-8222-222222222222";
const OPERATION: &str = "33333333-3333-4333-8333-333333333333";
const HTTP_TIMEOUT: Duration = Duration::from_secs(240);

#[derive(Debug, Default)]
struct Credential(Mutex<usize>);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        *self.0.lock().unwrap() += 1;
        Ok(AccessToken::new(
            "offline-lakehouse-create-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct WireRequest {
    method: Method,
    url: String,
    body: Vec<u8>,
}

#[derive(Debug)]
struct Network {
    replies: Mutex<VecDeque<azure_core::Result<AsyncRawResponse>>>,
    requests: Mutex<Vec<WireRequest>>,
}

#[async_trait]
impl HttpClient for Network {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        assert_eq!(request.url().origin().ascii_serialization(), ORIGIN);
        self.requests.lock().unwrap().push(WireRequest {
            method: request.method(),
            url: request.url().as_str().to_owned(),
            body: Bytes::from(request.body()).to_vec(),
        });
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request, provisioning or mutation replay")
    }
}

struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay == HTTP_TIMEOUT {
            return Box::pin(pending());
        }
        assert!(delay <= Duration::from_secs(3));
        Box::pin(ready(()))
    }
}

struct Fixture {
    network: Arc<Network>,
    credential: Arc<Credential>,
}

impl Fixture {
    fn new(replies: Vec<azure_core::Result<AsyncRawResponse>>) -> Self {
        Self {
            network: Arc::new(Network {
                replies: Mutex::new(replies.into()),
                requests: Mutex::new(Vec::new()),
            }),
            credential: Arc::new(Credential::default()),
        }
    }

    fn http(&self) -> HttpExecutor {
        HttpExecutor::new(
            self.network.clone(),
            Arc::new(Timer),
            ExecutionOptions {
                timeout: HTTP_TIMEOUT,
                max_delay: Duration::from_secs(3),
            },
        )
        .unwrap()
    }
}

fn reply(
    status: u16,
    body: &str,
    headers: &[(&str, &str)],
) -> azure_core::Result<AsyncRawResponse> {
    let mut response_headers = Headers::new();
    for &(key, value) in headers {
        response_headers.insert(key.to_owned(), value.to_owned());
    }
    Ok(AsyncRawResponse::from_bytes(
        StatusCode::from(status),
        response_headers,
        body.as_bytes().to_vec(),
    ))
}

fn prepared(wait: bool) -> LakehouseCreateRequest {
    let path = format!("/{WORKSPACE}.Workspace/Lakehouse_1.Lakehouse");
    let mut args = vec!["create", path.as_str(), "--input", "-"];
    if wait {
        args.push("--wait");
    }
    let command = v2_create::parse_args(args).unwrap();
    assert_eq!(command.wait, wait);
    let input = LakehouseCreateInput::from_bytes(
        b" {\r\n\"displayName\":\"Lakehouse_1\",\"creationPayload\":{\"enableSchemas\":true}}\r\n",
    )
    .unwrap();
    let target = CreateTarget::new(
        TargetScope::NamedLeaf,
        NativeId::new(WORKSPACE).unwrap(),
        "Lakehouse_1",
        "Lakehouse",
    )
    .unwrap();
    let plan = command
        .plan(
            target,
            Preflight::Leaf(LeafState::Missing),
            &LakehouseCreateSchema::capabilities().unwrap(),
            Some(input.value().clone()),
            &LakehouseCreateSchema,
        )
        .unwrap();
    LakehouseCreateRequest::from_plan(&plan).unwrap()
}

async fn submit(
    fixture: &Fixture,
    wait: bool,
    max_requests: usize,
    max_response_bytes: usize,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<ResourceOutcome, ResourceError> {
    let http = fixture.http();
    let mut backend = FabricMutationBackend::new(
        fixture.credential.clone(),
        CredentialSelection::new("offline-account", "offline-tenant", Audience::Fabric).unwrap(),
        &http,
        OperationLimits {
            max_requests: NonZeroUsize::new(max_requests).unwrap(),
            max_response_bytes: NonZeroUsize::new(max_response_bytes).unwrap(),
            max_poll_delay: Duration::from_secs(3),
        },
        cancelled,
    );
    prepared(wait)
        .execute(
            &mut backend,
            if wait {
                CompletionPolicy::WaitForResult
            } else {
                CompletionPolicy::ReturnAccepted
            },
        )
        .await
}

fn operation_url() -> String {
    format!("{ORIGIN}/v1/operations/{OPERATION}")
}

fn waiting_replies() -> Vec<azure_core::Result<AsyncRawResponse>> {
    vec![
        reply(
            202,
            "",
            &[
                ("Location", &operation_url()),
                ("x-ms-operation-id", OPERATION),
                ("Retry-After", "1"),
            ],
        ),
        reply(200, r#"{"status":"Running"}"#, &[("Retry-After", "1")]),
        reply(
            200,
            r#"{"status":"Succeeded"}"#,
            &[("Location", &format!("{}/result", operation_url()))],
        ),
        reply(
            200,
            &json!({
                "id":ITEM,"workspaceId":WORKSPACE,"displayName":"Lakehouse_1",
                "type":"Lakehouse","futureField":{"keep":true}
            })
            .to_string(),
            &[],
        ),
    ]
}

#[tokio::test]
async fn native_201_retains_service_ids_sql_status_unknown_fields_and_only_one_post() {
    let data = json!({
        "id":ITEM,"workspaceId":WORKSPACE,"displayName":"Lakehouse_1","type":"Lakehouse",
        "properties":{
            "sqlEndpointProperties":{"id":"sql-endpoint-id","provisioningStatus":"InProgress"},
            "oneLakeFilesPath":"abfss://synthetic.invalid/Files",
            "oneLakeTablesPath":"abfss://synthetic.invalid/Tables"
        },
        "futureField":{"preserved":null}
    });
    for wait in [false, true] {
        let fixture = Fixture::new(vec![reply(201, &data.to_string(), &[])]);
        assert_eq!(
            submit(&fixture, wait, 1, 2048, pending()).await.unwrap(),
            ResourceOutcome::Completed {
                status: 201,
                data: Some(data.clone()),
            }
        );
        let requests = fixture.network.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, Method::Post);
        assert_eq!(
            requests[0].url,
            format!("{ORIGIN}/v1/workspaces/{WORKSPACE}/lakehouses")
        );
        let expected = prepared(wait);
        assert_eq!(
            requests[0].body,
            serde_json::to_vec(expected.body()).unwrap()
        );
        assert_eq!(*fixture.credential.0.lock().unwrap(), 1);
    }
}

#[tokio::test]
async fn native_202_without_wait_retains_operation_identity_not_resource_identity() {
    let fixture = Fixture::new(vec![reply(
        202,
        "",
        &[
            ("Location", &operation_url()),
            ("x-ms-operation-id", OPERATION),
        ],
    )]);
    assert_eq!(
        submit(&fixture, false, 1, 2048, pending()).await.unwrap(),
        ResourceOutcome::Accepted {
            operation_id: Some(OPERATION.into())
        }
    );
    assert_eq!(fixture.network.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn native_202_wait_polls_existing_engine_and_preserves_actual_result() {
    let fixture = Fixture::new(waiting_replies());
    let outcome = submit(&fixture, true, 4, 2048, pending()).await.unwrap();
    assert!(matches!(
        outcome,
        ResourceOutcome::Completed {status:200,data:Some(ref data)}
            if data["id"] == ITEM && data["futureField"]["keep"] == true
    ));
    let requests = fixture.network.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.method)
            .collect::<Vec<_>>(),
        [Method::Post, Method::Get, Method::Get, Method::Get]
    );
    assert_eq!(requests[1].url, operation_url());
    assert_eq!(requests[2].url, operation_url());
    assert_eq!(requests[3].url, format!("{}/result", operation_url()));
    assert!(requests[1..].iter().all(|request| request.body.is_empty()));
}

#[tokio::test]
async fn race_conflict_and_native_denial_remain_failures_without_skip_or_replay() {
    for (status, code) in [
        (409, "ItemDisplayNameAlreadyInUse"),
        (403, "InsufficientPrivileges"),
        (400, "CorruptedPayload"),
    ] {
        let raw = json!({
            "errorCode":code,"message":"synthetic-private-diagnostic",
            "providerExtension":{"preserved":true}
        });
        let fixture = Fixture::new(vec![reply(status, &raw.to_string(), &[])]);
        let error = submit(&fixture, false, 4, 2048, pending())
            .await
            .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.status, Some(status));
        assert_eq!(error.raw_service_error.as_deref(), Some(&raw));
        assert!(!error.message.contains("synthetic-private-diagnostic"));
        assert!(error.mutation_may_have_occurred);
        assert_eq!(fixture.network.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn cancellation_before_submit_has_zero_identity_and_send() {
    let fixture = Fixture::new(Vec::new());
    let error = submit(&fixture, true, 4, 2048, ready(()))
        .await
        .unwrap_err();
    assert_eq!(error.code, "LongRunningOperationCancelled");
    assert!(!error.mutation_may_have_occurred);
    assert_eq!(error.operation_id, None);
    assert!(fixture.network.requests.lock().unwrap().is_empty());
    assert_eq!(*fixture.credential.0.lock().unwrap(), 0);
}

#[tokio::test]
async fn exhausted_poll_budget_retains_handle_and_unknown_effect_without_replay() {
    for max_requests in 1..=3 {
        let fixture = Fixture::new(waiting_replies());
        let error = submit(&fixture, true, max_requests, 2048, pending())
            .await
            .unwrap_err();
        assert_eq!(error.code, "Limit");
        assert_eq!(error.operation_id.as_deref(), Some(OPERATION));
        assert!(error.mutation_may_have_occurred);
        let requests = fixture.network.requests.lock().unwrap();
        assert_eq!(requests.len(), max_requests);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == Method::Post)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn byte_limit_after_post_preserves_uncertainty_and_never_resubmits() {
    let fixture = Fixture::new(vec![reply(
        201,
        &json!({"native":"x".repeat(256)}).to_string(),
        &[],
    )]);
    let error = submit(&fixture, false, 4, 64, pending()).await.unwrap_err();
    assert!(error.mutation_may_have_occurred);
    assert_eq!(fixture.network.requests.lock().unwrap().len(), 1);
}
