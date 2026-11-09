// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Common link -> FabricMutationBackend -> OperationSession -> HttpExecutor.
//!
//! Only the final Azure HTTP client, credential, clock and approval are fakes.
//! Request observations deliberately exclude authorization headers.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    Bytes,
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_mutations::{
    CommandFuture, FabricMutationBackend, MutationError,
    v2_link::{
        self, LinkApproval, LinkEffects, LinkFailure, LinkOwner, LinkReceipt, LinkScope,
        PreparedLink, ResolvedLinkTarget,
    },
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_operations::{Audience, OperationLimits};
use fabric_sdk_workspaces::{
    CredentialSelection,
    mutations::{CompletionPolicy, ResourceOutcome},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending, ready},
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

const FABRIC: &str = "https://api.fabric.microsoft.com";
const FABRIC_SCOPE: &str = "https://api.fabric.microsoft.com/.default";
const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
const ITEM: &str = "22222222-2222-4222-8222-222222222222";
const SOURCE: &str = "33333333-3333-4333-8333-333333333333";
const CONNECTION: &str = "44444444-4444-4444-8444-444444444444";
const OPERATION: &str = "55555555-5555-4555-8555-555555555555";
const DATASOURCE: &str = "66666666-6666-4666-8666-666666666666";
const REQUEST_ID: &str = "77777777-7777-4777-8777-777777777777";
const TOKEN: &str = "synthetic-offline-v2-link-wire";
const HTTP_TIMEOUT: Duration = Duration::from_secs(240);

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
        assert_eq!(scopes, [FABRIC_SCOPE]);
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|scope| (*scope).to_owned()).collect());
        Ok(AccessToken::new(TOKEN, OffsetDateTime::UNIX_EPOCH))
    }
}

#[derive(Clone, Debug)]
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
        assert_eq!(request.url().origin().ascii_serialization(), FABRIC);
        assert_eq!(
            request.headers().get_str(&"authorization".into()).unwrap(),
            format!("Bearer {TOKEN}")
        );
        assert_eq!(
            request.headers().get_str(&"content-type".into()).unwrap(),
            "application/json"
        );
        self.requests.lock().unwrap().push(WireRequest {
            method: request.method(),
            url: request.url().as_str().to_owned(),
            body: Bytes::from(request.body()).to_vec(),
        });
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("an unexpected request or write replay reached the HTTP client")
    }
}

#[derive(Default)]
struct Timer {
    poll_delays: Mutex<Vec<Duration>>,
}

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        // Request deadlines must stay pending while scripted poll waits complete.
        if delay == HTTP_TIMEOUT {
            return Box::pin(pending());
        }
        assert!(delay <= Duration::from_secs(3));
        self.poll_delays.lock().unwrap().push(delay);
        Box::pin(ready(()))
    }
}

struct Approval {
    allowed: bool,
    calls: Vec<(ResolvedLinkTarget, LinkEffects)>,
}

impl LinkApproval for Approval {
    fn approve<'a>(
        &'a mut self,
        target: &'a ResolvedLinkTarget,
        effects: LinkEffects,
    ) -> CommandFuture<'a, Result<bool, MutationError>> {
        self.calls.push((target.clone(), effects));
        Box::pin(ready(Ok(self.allowed)))
    }
}

struct Fixture {
    network: Arc<Network>,
    credential: Arc<Credential>,
    timer: Arc<Timer>,
}

impl Fixture {
    fn new(replies: Vec<azure_core::Result<AsyncRawResponse>>) -> Self {
        Self {
            network: Arc::new(Network {
                replies: Mutex::new(replies.into()),
                requests: Mutex::new(Vec::new()),
            }),
            credential: Arc::new(Credential::default()),
            timer: Arc::new(Timer::default()),
        }
    }

    fn http(&self) -> HttpExecutor {
        HttpExecutor::new(
            self.network.clone(),
            self.timer.clone(),
            ExecutionOptions {
                timeout: HTTP_TIMEOUT,
                max_delay: Duration::from_secs(3),
            },
        )
        .unwrap()
    }

    fn requests(&self) -> Vec<WireRequest> {
        self.network.requests.lock().unwrap().clone()
    }

    fn assert_scopes(&self, count: usize) {
        let scopes = self.credential.scopes.lock().unwrap();
        assert_eq!(scopes.len(), count);
        assert!(scopes.iter().all(|scopes| scopes == &[FABRIC_SCOPE]));
    }
}

fn selection() -> CredentialSelection {
    CredentialSelection::new("synthetic-offline", "synthetic-tenant", Audience::Fabric).unwrap()
}

fn limits(max_requests: usize) -> OperationLimits {
    OperationLimits {
        max_requests: NonZeroUsize::new(max_requests).unwrap(),
        max_response_bytes: NonZeroUsize::new(1024).unwrap(),
        max_poll_delay: Duration::from_secs(3),
    }
}

fn reply(
    status: u16,
    body: &str,
    headers: &[(&str, &str)],
) -> azure_core::Result<AsyncRawResponse> {
    let mut response_headers = Headers::new();
    for &(name, value) in headers {
        response_headers.insert(name.to_owned(), value.to_owned());
    }
    Ok(AsyncRawResponse::from_bytes(
        StatusCode::from(status),
        response_headers,
        body.as_bytes().to_vec(),
    ))
}

fn resolved(owner: LinkOwner) -> ResolvedLinkTarget {
    let scope = match owner {
        LinkOwner::Lakehouse => LinkScope::LakehouseShortcut {
            path: "Files/landing".into(),
            name: "sales_ref".into(),
        },
        LinkOwner::KqlDatabase => LinkScope::KqlTableShortcut {
            name: "sales_ref".into(),
        },
        LinkOwner::DataAgent => LinkScope::DataAgentStagingDatasources,
        LinkOwner::SemanticModel => LinkScope::SemanticModelConnections,
        LinkOwner::Ontology => panic!("Ontology has no generic wire operation"),
    };
    ResolvedLinkTarget::new(WORKSPACE, ITEM, owner, scope).unwrap()
}

fn payload(owner: LinkOwner) -> Value {
    match owner {
        LinkOwner::Lakehouse => json!({
            "path": "Files/landing",
            "name": "sales_ref",
            "target": {
                "oneLake": {
                    "workspaceId": WORKSPACE,
                    "itemId": SOURCE,
                    "path": "Files/sales"
                }
            }
        }),
        LinkOwner::KqlDatabase => json!({
            "name": "sales_ref",
            "enableQueryAcceleration": false,
            "target": {
                "oneLake": {
                    "workspaceId": WORKSPACE,
                    "itemId": SOURCE,
                    "path": "Tables/sales"
                }
            }
        }),
        LinkOwner::DataAgent => json!({
            "type": "LakehouseTables",
            "lakehouseReference": {
                "referenceType": "ById",
                "workspaceId": WORKSPACE,
                "itemId": SOURCE
            }
        }),
        LinkOwner::SemanticModel => json!({
            "connectionBinding": {
                "id": CONNECTION,
                "connectivityType": "ShareableCloud",
                "connectionDetails": {
                    "type": "SQL",
                    "path": "server.example.invalid;database"
                }
            }
        }),
        LinkOwner::Ontology => panic!("Ontology has no generic native payload"),
    }
}

fn prepared(owner: LinkOwner, wait: bool) -> PreparedLink {
    let result = v2_link::prepare(resolved(owner), None, &payload(owner), wait).unwrap();
    assert_eq!(result.operation_id(), v2_link::OPERATION_ID);
    result
}

async fn submit(
    fixture: &Fixture,
    owner: LinkOwner,
    wait: bool,
    bounds: OperationLimits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<LinkReceipt, Box<LinkFailure>> {
    let request = prepared(owner, wait);
    let expected_effects = request.effects();
    let http = fixture.http();
    let mut backend = FabricMutationBackend::new(
        fixture.credential.clone(),
        selection(),
        &http,
        bounds,
        cancelled,
    );
    let mut approval = Approval {
        allowed: true,
        calls: Vec::new(),
    };
    let result = v2_link::execute(request, &mut backend, Some(&mut approval)).await;
    assert_eq!(approval.calls, [(resolved(owner), expected_effects)]);
    result
}

fn assert_post(request: &WireRequest, route: &str, body: &Value) {
    assert_eq!(request.method, Method::Post);
    assert_eq!(request.url, format!("{FABRIC}{route}"));
    assert_eq!(request.body, serde_json::to_vec(body).unwrap());
}

fn kql_route() -> String {
    format!("/v1/workspaces/{WORKSPACE}/kqlDatabases/{ITEM}/shortcuts")
}

fn operation_location() -> String {
    format!("{FABRIC}/v1/operations/{OPERATION}")
}

fn waiting_replies() -> Vec<azure_core::Result<AsyncRawResponse>> {
    let operation = operation_location();
    let result = format!("{operation}/result");
    vec![
        reply(
            202,
            "",
            &[
                ("Location", &operation),
                ("x-ms-operation-id", OPERATION),
                ("Retry-After", "2"),
            ],
        ),
        reply(200, r#"{"status":"Running"}"#, &[("Retry-After", "3")]),
        reply(200, r#"{"status":"Succeeded"}"#, &[("Location", &result)]),
        reply(
            200,
            r#"{"name":"sales_ref","enableQueryAcceleration":false,"actualResult":"retained"}"#,
            &[],
        ),
    ]
}

#[tokio::test]
async fn core_abort_201_retains_actual_shortcut_and_resource_location_never_triggers_polling() {
    let resource = format!(
        "{FABRIC}/v1/workspaces/{WORKSPACE}/items/{ITEM}/shortcuts/Files/landing/sales_ref"
    );
    let actual = json!({
        "path": "Files/landing",
        "name": "sales_ref",
        "target": {"type": "OneLake", "oneLake": {
            "workspaceId": WORKSPACE, "itemId": SOURCE, "path": "Files/sales"
        }},
        "actualResponse": "retained"
    });
    for wait in [false, true] {
        let fixture = Fixture::new(vec![reply(
            201,
            &actual.to_string(),
            &[("Location", &resource)],
        )]);
        let receipt = submit(&fixture, LinkOwner::Lakehouse, wait, limits(1), pending())
            .await
            .unwrap();
        assert_eq!(
            receipt.outcome,
            ResourceOutcome::Completed {
                status: 201,
                data: Some(actual.clone()),
            }
        );
        assert_eq!(receipt.target, resolved(LinkOwner::Lakehouse));
        assert_eq!(receipt.metadata()["httpStatus"], 201);
        assert_eq!(receipt.metadata()["status"], "succeeded");
        let requests = fixture.requests();
        assert_eq!(requests.len(), 1);
        assert_post(
            &requests[0],
            &format!(
                "/v1/workspaces/{WORKSPACE}/items/{ITEM}/shortcuts?shortcutConflictPolicy=Abort"
            ),
            &payload(LinkOwner::Lakehouse),
        );
        fixture.assert_scopes(1);
        assert!(fixture.timer.poll_delays.lock().unwrap().is_empty());
        assert!(fixture.network.replies.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn kql_no_wait_202_is_acceptance_only_and_dispatches_one_native_post() {
    let operation = operation_location();
    let fixture = Fixture::new(vec![reply(
        202,
        "",
        &[
            ("Location", &operation),
            ("x-ms-operation-id", OPERATION),
            ("Retry-After", "2"),
        ],
    )]);
    let receipt = submit(
        &fixture,
        LinkOwner::KqlDatabase,
        false,
        limits(1),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(
        receipt.outcome,
        ResourceOutcome::Accepted {
            operation_id: Some(OPERATION.into())
        }
    );
    assert_eq!(receipt.metadata()["status"], "accepted");
    assert_eq!(receipt.metadata()["httpStatus"], Value::Null);
    assert_eq!(receipt.metadata()["operationId"], OPERATION);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    assert_post(&requests[0], &kql_route(), &payload(LinkOwner::KqlDatabase));
    assert!(!requests[0].url.contains('?'));
    fixture.assert_scopes(1);
    assert!(fixture.timer.poll_delays.lock().unwrap().is_empty());
}

#[tokio::test]
async fn kql_wait_uses_bounded_running_succeeded_and_actual_result_get_without_write_replay() {
    let fixture = Fixture::new(waiting_replies());
    let receipt = submit(&fixture, LinkOwner::KqlDatabase, true, limits(4), pending())
        .await
        .unwrap();
    assert_eq!(
        receipt.outcome,
        ResourceOutcome::Completed {
            status: 200,
            data: Some(json!({
                "name": "sales_ref",
                "enableQueryAcceleration": false,
                "actualResult": "retained"
            }))
        }
    );
    assert_eq!(receipt.metadata()["httpStatus"], 200);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 4);
    assert_post(&requests[0], &kql_route(), &payload(LinkOwner::KqlDatabase));
    assert_eq!(
        requests
            .iter()
            .map(|request| request.method)
            .collect::<Vec<_>>(),
        [Method::Post, Method::Get, Method::Get, Method::Get]
    );
    assert_eq!(requests[1].url, operation_location());
    assert_eq!(requests[2].url, operation_location());
    assert_eq!(requests[3].url, format!("{}/result", operation_location()));
    assert!(requests[1..].iter().all(|request| request.body.is_empty()));
    assert_eq!(
        *fixture.timer.poll_delays.lock().unwrap(),
        [Duration::from_secs(2), Duration::from_secs(3)]
    );
    fixture.assert_scopes(4);
    assert!(fixture.network.replies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn request_budget_covers_initial_post_each_poll_and_result_get_without_resubmission() {
    for maximum in 1..=3 {
        let fixture = Fixture::new(waiting_replies());
        let failure = submit(
            &fixture,
            LinkOwner::KqlDatabase,
            true,
            limits(maximum),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(failure.error.code, "Limit");
        assert!(failure.observed.is_none());
        let evidence = failure.error.operation.unwrap();
        assert_eq!(evidence.operation_id.as_deref(), Some(OPERATION));
        assert!(evidence.mutation_may_have_occurred);
        let requests = fixture.requests();
        assert_eq!(requests.len(), maximum);
        assert_eq!(requests[0].method, Method::Post);
        assert!(
            requests[1..]
                .iter()
                .all(|request| request.method == Method::Get && request.body.is_empty())
        );
        assert_eq!(fixture.network.replies.lock().unwrap().len(), 4 - maximum);
        fixture.assert_scopes(maximum);
    }
}

#[tokio::test]
async fn datasource_201_retains_actual_returned_uuid_and_only_changes_staging_collection() {
    let actual = json!({
        "id": DATASOURCE,
        "type": "LakehouseTables",
        "displayName": "Actual source",
        "providerExtension": {"preserved": true}
    });
    let fixture = Fixture::new(vec![reply(201, &actual.to_string(), &[])]);
    let receipt = submit(&fixture, LinkOwner::DataAgent, false, limits(1), pending())
        .await
        .unwrap();
    assert_eq!(
        receipt.outcome,
        ResourceOutcome::Completed {
            status: 201,
            data: Some(actual)
        }
    );
    assert_eq!(receipt.target, resolved(LinkOwner::DataAgent));
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    assert_post(
        &requests[0],
        &format!("/v1/workspaces/{WORKSPACE}/dataAgents/{ITEM}/staging/datasources"),
        &payload(LinkOwner::DataAgent),
    );
    fixture.assert_scopes(1);
    assert!(fixture.timer.poll_delays.lock().unwrap().is_empty());
}

#[tokio::test]
async fn model_bind_200_empty_is_real_completion_without_a_fabricated_result_or_gateway_call() {
    assert_eq!(
        prepared(LinkOwner::SemanticModel, true)
            .operation()
            .completion(),
        CompletionPolicy::WaitForCompletion
    );
    let fixture = Fixture::new(vec![reply(200, "", &[])]);
    let receipt = submit(
        &fixture,
        LinkOwner::SemanticModel,
        true,
        limits(1),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(
        receipt.outcome,
        ResourceOutcome::Completed {
            status: 200,
            data: None
        }
    );
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    assert_post(
        &requests[0],
        &format!("/v1/workspaces/{WORKSPACE}/semanticModels/{ITEM}/bindConnection"),
        &payload(LinkOwner::SemanticModel),
    );
    fixture.assert_scopes(1);
    assert!(fixture.timer.poll_delays.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invalid_completed_identities_retain_the_exact_observation_and_possible_mutation() {
    for (owner, actual) in [
        (
            LinkOwner::Lakehouse,
            json!({"path": "Files/other", "name": "sales_ref", "providerField": "retained"}),
        ),
        (
            LinkOwner::Lakehouse,
            json!({"path": "Files/landing", "name": "other", "providerField": "retained"}),
        ),
        (
            LinkOwner::KqlDatabase,
            json!({"name": "other", "providerField": "retained"}),
        ),
        (
            LinkOwner::DataAgent,
            json!({"id": "not-a-uuid", "providerField": "retained"}),
        ),
    ] {
        let fixture = Fixture::new(vec![reply(201, &actual.to_string(), &[])]);
        let failure = submit(&fixture, owner, false, limits(1), pending())
            .await
            .unwrap_err();
        assert_eq!(failure.error.code, "InvalidResponse");
        assert_eq!(failure.target, resolved(owner));
        assert_eq!(
            failure.observed,
            Some(ResourceOutcome::Completed {
                status: 201,
                data: Some(actual)
            })
        );
        let evidence = failure.error.operation.unwrap();
        assert_eq!(evidence.status, Some(201));
        assert!(evidence.mutation_may_have_occurred);
        assert_eq!(fixture.requests().len(), 1);
        fixture.assert_scopes(1);
    }
}

#[tokio::test]
async fn forbidden_retains_service_code_raw_details_request_id_and_mutation_evidence() {
    let raw = json!({
        "errorCode": "NativeLinkDenied",
        "message": "Synthetic provider denial",
        "requestId": REQUEST_ID,
        "moreDetails": [{"errorCode": "SyntheticDetail", "message": "Detail retained"}],
        "providerExtension": {"retained": true}
    });
    let fixture = Fixture::new(vec![reply(403, &raw.to_string(), &[])]);
    let failure = submit(&fixture, LinkOwner::Lakehouse, false, limits(1), pending())
        .await
        .unwrap_err();
    assert_eq!(failure.target, resolved(LinkOwner::Lakehouse));
    assert_eq!(failure.error.code, "NativeLinkDenied");
    assert!(!failure.error.message.contains("Synthetic provider denial"));
    assert!(failure.observed.is_none());
    let evidence = failure.error.operation.unwrap();
    assert_eq!(evidence.status, Some(403));
    assert!(evidence.mutation_may_have_occurred);
    assert_eq!(evidence.operation_id, None);
    assert_eq!(evidence.raw_service_error.as_deref(), Some(&raw));
    let service = evidence.service_error.unwrap();
    assert_eq!(service.error_code, "NativeLinkDenied");
    assert_eq!(service.additional_fields["requestId"], REQUEST_ID);
    assert_eq!(service.additional_fields["moreDetails"], raw["moreDetails"]);
    assert_eq!(fixture.requests().len(), 1);
    fixture.assert_scopes(1);
}

#[tokio::test]
async fn cancellation_before_submission_sends_no_request_and_acquires_no_credential() {
    let fixture = Fixture::new(Vec::new());
    let failure = submit(&fixture, LinkOwner::KqlDatabase, true, limits(4), ready(()))
        .await
        .unwrap_err();
    assert_eq!(failure.error.code, "LongRunningOperationCancelled");
    assert!(failure.observed.is_none());
    let evidence = failure.error.operation.unwrap();
    assert!(!evidence.mutation_may_have_occurred);
    assert_eq!(evidence.operation_id, None);
    assert_eq!(evidence.status, None);
    assert!(fixture.requests().is_empty());
    fixture.assert_scopes(0);
    assert!(fixture.timer.poll_delays.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unsafe_initial_poll_and_result_locations_never_forward_tokens_or_replay_post() {
    let safe = operation_location();
    let foreign = format!("https://example.invalid/v1/operations/{OPERATION}");
    let foreign_result = format!("{foreign}/result");
    for (wait, replies, expected_requests) in [
        (
            false,
            vec![reply(
                202,
                "",
                &[("Location", &foreign), ("x-ms-operation-id", OPERATION)],
            )],
            1,
        ),
        (
            true,
            vec![reply(
                202,
                "",
                &[("Location", &foreign), ("x-ms-operation-id", OPERATION)],
            )],
            1,
        ),
        (
            true,
            vec![
                reply(
                    202,
                    "",
                    &[
                        ("Location", &safe),
                        ("x-ms-operation-id", OPERATION),
                        ("Retry-After", "2"),
                    ],
                ),
                reply(200, r#"{"status":"Running"}"#, &[("Location", &foreign)]),
            ],
            2,
        ),
        (
            true,
            vec![
                reply(
                    202,
                    "",
                    &[
                        ("Location", &safe),
                        ("x-ms-operation-id", OPERATION),
                        ("Retry-After", "2"),
                    ],
                ),
                reply(
                    200,
                    r#"{"status":"Succeeded"}"#,
                    &[("Location", &foreign_result)],
                ),
            ],
            2,
        ),
    ] {
        let fixture = Fixture::new(replies);
        let failure = submit(&fixture, LinkOwner::KqlDatabase, wait, limits(4), pending())
            .await
            .unwrap_err();
        assert_eq!(failure.error.code, "InvalidLink");
        let evidence = failure.error.operation.unwrap();
        assert_eq!(evidence.operation_id.as_deref(), Some(OPERATION));
        assert!(evidence.mutation_may_have_occurred);
        let requests = fixture.requests();
        assert_eq!(requests.len(), expected_requests);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == Method::Post)
                .count(),
            1
        );
        assert!(
            requests
                .iter()
                .all(|request| request.url.starts_with(FABRIC))
        );
        fixture.assert_scopes(expected_requests);
        assert!(fixture.network.replies.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn response_byte_bound_is_enforced_after_post_without_replay_or_synthetic_completion() {
    let fixture = Fixture::new(vec![reply(
        201,
        r#"{"name":"sales_ref","path":"Files/landing","extension":"too large"}"#,
        &[],
    )]);
    let mut bounds = limits(1);
    bounds.max_response_bytes = NonZeroUsize::new(16).unwrap();
    let failure = submit(&fixture, LinkOwner::Lakehouse, false, bounds, pending())
        .await
        .unwrap_err();
    assert_eq!(failure.error.code, "Limit");
    assert!(failure.observed.is_none());
    assert!(failure.error.operation.unwrap().mutation_may_have_occurred);
    assert_eq!(fixture.requests().len(), 1);
    fixture.assert_scopes(1);
}

#[tokio::test]
async fn lost_post_response_is_not_replayed_even_with_an_unused_success_response_available() {
    let fixture = Fixture::new(vec![
        Err(azure_core::Error::with_message(
            azure_core::error::ErrorKind::Other,
            "Synthetic lost mutation response",
        )),
        reply(200, "", &[]),
    ]);
    let failure = submit(
        &fixture,
        LinkOwner::SemanticModel,
        false,
        limits(4),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.error.code, "ExecutionFailed");
    assert!(failure.error.operation.unwrap().mutation_may_have_occurred);
    assert_eq!(fixture.requests().len(), 1);
    assert_eq!(fixture.network.replies.lock().unwrap().len(), 1);
    fixture.assert_scopes(1);
}

#[tokio::test]
async fn missing_or_declined_approval_never_reaches_credentials_or_http() {
    for missing in [false, true] {
        let fixture = Fixture::new(Vec::new());
        let http = fixture.http();
        let mut backend = FabricMutationBackend::new(
            fixture.credential.clone(),
            selection(),
            &http,
            limits(1),
            pending(),
        );
        let mut approval = Approval {
            allowed: false,
            calls: Vec::new(),
        };
        let failure = v2_link::execute(
            prepared(LinkOwner::Lakehouse, false),
            &mut backend,
            if missing { None } else { Some(&mut approval) },
        )
        .await
        .unwrap_err();
        assert_eq!(failure.error.code, "ApprovalRequired");
        assert!(failure.error.operation.is_none());
        assert!(failure.observed.is_none());
        assert_eq!(approval.calls.len(), usize::from(!missing));
        assert!(fixture.requests().is_empty());
        fixture.assert_scopes(0);
    }
}
