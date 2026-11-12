// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native API fixtures, not Python config-deployment or live-tenant oracles.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_deploy::stage::{
    ApprovedDeployment, DeploymentApproval, DeploymentError, DeploymentErrorKind as Kind,
    DeploymentOutcome, DeploymentRequest, PreparedDeployment, SelectionIntent, StageReader,
};
use fabric_sdk::{Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin};
use fabric_sdk_operations::{
    OperationErrorKind, OperationExecutionError, OperationExecutor, OperationLimits,
    OperationRequest, OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};

const PIPELINE: &str = "11111111-1111-1111-1111-111111111111";
const SOURCE: &str = "22222222-2222-2222-2222-222222222222";
const TARGET: &str = "33333333-3333-3333-3333-333333333333";
const WORKSPACE: &str = "44444444-4444-4444-4444-444444444444";
const OTHER_WORKSPACE: &str = "55555555-5555-5555-5555-555555555555";
const ITEM: &str = "66666666-6666-6666-6666-666666666666";
const OPERATION: &str = "77777777-7777-7777-7777-777777777777";
const DEPLOYMENT: &str = "88888888-8888-8888-8888-888888888888";

fn payload() -> Value {
    json!({"sourceStageId":SOURCE,"targetStageId":TARGET,
        "items":[{"sourceItemId":ITEM,"itemType":"Notebook"}],"note":"private-note"})
}

fn request(body: &Value, intent: SelectionIntent) -> Result<DeploymentRequest, DeploymentError> {
    DeploymentRequest::from_json(
        PIPELINE,
        TARGET,
        &serde_json::to_vec(body).unwrap(),
        128 * 1024,
        intent,
    )
}

fn response(status: u16, body: Value) -> OperationResponse {
    OperationResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

fn stage(id: &str, workspace: &str, order: u64) -> OperationResponse {
    response(200, json!({"id":id,"workspaceId":workspace,"order":order}))
}

fn accepted() -> OperationResponse {
    OperationResponse {
        status: 202,
        body: Vec::new(),
        headers: BTreeMap::from([
            (
                "Location".into(),
                format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}"),
            ),
            ("X-MS-Operation-ID".into(), OPERATION.into()),
            ("Deployment-ID".into(), DEPLOYMENT.into()),
            ("Retry-After".into(), "0".into()),
        ]),
    }
}

fn completed(status: &str) -> OperationResponse {
    response(
        200,
        json!({"id":DEPLOYMENT,"type":"Deploy","sourceStageId":SOURCE,"targetStageId":TARGET,"status":status}),
    )
}

#[derive(Debug)]
struct Credential;
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        Ok(AccessToken::new(
            "offline-stage-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Harness {
    context: RequestContext,
    replies: Mutex<VecDeque<Result<OperationResponse, OperationExecutionError>>>,
    calls: Mutex<Vec<(ResourceMethod, String, Vec<u8>)>>,
}

impl Harness {
    fn new(
        replies: impl IntoIterator<Item = Result<OperationResponse, OperationExecutionError>>,
    ) -> Self {
        Self {
            context: RequestContext::with_endpoint(
                Arc::new(Credential),
                CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
                EndpointBinding::public(ServiceOrigin::Fabric),
            ),
            replies: Mutex::new(replies.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }
    async fn prepare(
        &self,
        source_order: u64,
        target_order: u64,
    ) -> Result<PreparedDeployment, DeploymentError> {
        self.replies
            .lock()
            .unwrap()
            .push_front(Ok(stage(TARGET, OTHER_WORKSPACE, target_order)));
        self.replies
            .lock()
            .unwrap()
            .push_front(Ok(stage(SOURCE, WORKSPACE, source_order)));
        let reader = StageReader {
            context: &self.context,
            executor: &self,
            limits: OperationLimits::default(),
        };
        let source = reader.read_stage(PIPELINE, SOURCE, pending()).await?;
        let target = reader.read_stage(PIPELINE, TARGET, pending()).await?;
        PreparedDeployment::new(
            request(&payload(), SelectionIntent::Selected)?,
            source,
            target,
        )
    }
    async fn approved(&self) -> ApprovedDeployment {
        self.prepare(0, 1)
            .await
            .unwrap()
            .authorize(&mut Approve)
            .unwrap()
    }
    fn post_count(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _, _)| *method == ResourceMethod::Post)
            .count()
    }
}

impl OperationExecutor for &Harness {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(
                request.url().host_str(),
                Some(self.context.endpoint().base_url().host_str().unwrap())
            );
            self.calls.lock().unwrap().push((
                request.method(),
                request.url().path().into(),
                request.body().to_vec(),
            ));
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected extra request")
        })
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(ready(Ok(())))
    }
}

struct Approve;
impl DeploymentApproval for Approve {
    fn authorize(&mut self, deployment: &PreparedDeployment) -> Result<(), DeploymentError> {
        assert_eq!(deployment.request().pipeline_id(), PIPELINE);
        assert_eq!(deployment.request().body(), &payload());
        assert_eq!(deployment.workspaces(), (WORKSPACE, OTHER_WORKSPACE));
        Ok(())
    }
}

#[test]
fn explicit_scope_and_exact_native_payload() {
    let prepared = request(&payload(), SelectionIntent::Selected).unwrap();
    assert_eq!(prepared.body(), &payload());
    assert_eq!(prepared.intent(), SelectionIntent::Selected);
    assert!(!format!("{prepared:?}").contains("private-note"));
    let mut all = payload();
    all.as_object_mut().unwrap().remove("items");
    assert!(
        request(&all, SelectionIntent::AllSupported)
            .unwrap()
            .body()
            .get("items")
            .is_none()
    );
    assert_eq!(
        request(&all, SelectionIntent::Selected).err().unwrap().kind,
        Kind::InvalidInput
    );
    assert_eq!(
        request(&payload(), SelectionIntent::AllSupported)
            .err()
            .unwrap()
            .kind,
        Kind::InvalidInput
    );
}

#[test]
fn null_empty_duplicate_and_unknown_branches_never_become_all_items() {
    for items in [
        Value::Null,
        json!([]),
        json!([{"sourceItemId":ITEM,"itemType":"Notebook"},{"sourceItemId":ITEM,"itemType":"Report"}]),
    ] {
        let mut body = payload();
        body["items"] = items;
        for intent in [SelectionIntent::Selected, SelectionIntent::AllSupported] {
            assert_eq!(
                request(&body, intent).err().unwrap().kind,
                Kind::InvalidInput
            );
        }
    }
    for key in [
        "options",
        "createdWorkspaceDetails",
        "deploymentPlan",
        "config",
        "prune",
        "rollback",
    ] {
        let mut body = payload();
        body[key] = json!({});
        assert_eq!(
            request(&body, SelectionIntent::Selected)
                .err()
                .unwrap()
                .kind,
            Kind::UnsupportedCapability
        );
    }
    let mut body = payload();
    body["items"][0]["itemType"] = json!("DeploymentPlan");
    assert_eq!(
        request(&body, SelectionIntent::Selected)
            .err()
            .unwrap()
            .kind,
        Kind::UnsupportedCapability
    );
}

#[test]
fn exact_service_boundaries_and_target_validation() {
    let mut body = payload();
    body["items"] = Value::Array(
        (1..=300)
            .map(|n| {
                json!({
                    "sourceItemId":format!("00000000-0000-0000-0000-{n:012x}"),"itemType":"Notebook"
                })
            })
            .collect(),
    );
    assert!(request(&body, SelectionIntent::Selected).is_ok());
    body["items"]
        .as_array_mut()
        .unwrap()
        .push(json!({"sourceItemId":ITEM,"itemType":"Notebook"}));
    assert_eq!(
        request(&body, SelectionIntent::Selected)
            .err()
            .unwrap()
            .kind,
        Kind::InvalidInput
    );
    for length in [1024, 1025] {
        let mut body = payload();
        body["note"] = json!("n".repeat(length));
        assert_eq!(
            request(&body, SelectionIntent::Selected).is_ok(),
            length == 1024
        );
    }
    for target in [SOURCE, ITEM] {
        let mut body = payload();
        body["targetStageId"] = json!(target);
        assert_eq!(
            request(&body, SelectionIntent::Selected)
                .err()
                .unwrap()
                .kind,
            Kind::TargetMismatch
        );
    }
    assert!(
        DeploymentRequest::from_json("../", TARGET, b"{}", 2, SelectionIntent::AllSupported)
            .is_err()
    );
    assert!(
        DeploymentRequest::from_json(PIPELINE, TARGET, b"{}", 1, SelectionIntent::AllSupported)
            .is_err()
    );
}

#[test]
fn duplicate_native_fields_are_rejected_before_value_map_collapse() {
    let valid = serde_json::to_string(&payload()).unwrap();
    for bytes in [
        valid.replacen(
            "\"sourceStageId\":",
            &format!("\"sourceStageId\":\"{SOURCE}\",\"sourceStageId\":"),
            1,
        ),
        valid.replacen("\"itemType\":", "\"itemType\":\"Report\",\"itemType\":", 1),
        valid.replacen(
            "\"sourceItemId\":",
            &format!("\"sourceItemId\":\"{ITEM}\",\"sourceItemId\":"),
            1,
        ),
        valid.replacen(
            "\"sourceStageId\":",
            &format!("\"sourceSt\\u0061geId\":\"{SOURCE}\",\"sourceStageId\":"),
            1,
        ),
    ] {
        assert_eq!(
            DeploymentRequest::from_json(
                PIPELINE,
                TARGET,
                bytes.as_bytes(),
                65536,
                SelectionIntent::Selected
            )
            .err()
            .unwrap()
            .kind,
            Kind::InvalidInput
        );
    }
}

#[tokio::test]
async fn stage_preflight_uses_exact_native_owner_routes() {
    let harness = Harness::new([]);
    harness.prepare(3, 4).await.unwrap();
    let calls = harness.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, ResourceMethod::Get);
    assert_eq!(
        calls[0].1,
        format!("/v1/deploymentPipelines/{PIPELINE}/stages/{SOURCE}")
    );
    assert_eq!(
        calls[1].1,
        format!("/v1/deploymentPipelines/{PIPELINE}/stages/{TARGET}")
    );
    assert!(calls.iter().all(|(_, _, body)| body.is_empty()));
}

#[tokio::test]
async fn backward_and_skipped_stages_are_explicitly_unsupported() {
    for orders in [(0, 2), (1, 0), (0, 0)] {
        let harness = Harness::new([]);
        assert_eq!(
            harness
                .prepare(orders.0, orders.1)
                .await
                .err()
                .unwrap()
                .kind,
            Kind::UnsupportedCapability
        );
        assert_eq!(harness.post_count(), 0);
    }
}

#[tokio::test]
async fn substituted_stage_pipeline_and_missing_workspace_fail_before_post() {
    let harness = Harness::new([
        Ok(stage(ITEM, WORKSPACE, 0)),
        Ok(response(200, json!({"id":SOURCE,"order":0}))),
        Ok(stage(SOURCE, WORKSPACE, 0)),
        Ok(stage(TARGET, OTHER_WORKSPACE, 1)),
    ]);
    let executor = &harness;
    let reader = StageReader {
        context: &harness.context,
        executor: &executor,
        limits: OperationLimits::default(),
    };
    assert_eq!(
        reader
            .read_stage(PIPELINE, SOURCE, pending())
            .await
            .err()
            .unwrap()
            .kind,
        Kind::TargetMismatch
    );
    assert_eq!(
        reader
            .read_stage(PIPELINE, SOURCE, pending())
            .await
            .err()
            .unwrap()
            .kind,
        Kind::UnsupportedCapability
    );
    let source = reader.read_stage(ITEM, SOURCE, pending()).await.unwrap();
    let target = reader
        .read_stage(PIPELINE, TARGET, pending())
        .await
        .unwrap();
    assert_eq!(
        PreparedDeployment::new(
            request(&payload(), SelectionIntent::Selected).unwrap(),
            source,
            target
        )
        .err()
        .unwrap()
        .kind,
        Kind::TargetMismatch
    );
    assert_eq!(harness.post_count(), 0);
}

#[tokio::test]
async fn accepted_preserves_two_distinct_handles_and_exact_single_post() {
    let harness = Harness::new([Ok(accepted())]);
    let receipt = harness
        .approved()
        .await
        .execute(
            &harness.context,
            &(&harness),
            OperationLimits::default(),
            false,
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.outcome, DeploymentOutcome::Accepted);
    assert_eq!(receipt.http_status, Some(202));
    assert_eq!(receipt.operation_id.as_deref(), Some(OPERATION));
    assert_eq!(receipt.deployment_id.as_deref(), Some(DEPLOYMENT));
    assert!(receipt.mutation_may_have_occurred);
    assert!(!receipt.to_value().to_string().contains("private-note"));
    let calls = harness.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[2].0, ResourceMethod::Post);
    assert_eq!(
        calls[2].1,
        format!("/v1/deploymentPipelines/{PIPELINE}/deploy")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&calls[2].2).unwrap(),
        payload()
    );
}

#[tokio::test]
async fn waiting_reuses_shared_poller_and_retains_submission_not_poll_http_status() {
    let harness = Harness::new([
        Ok(accepted()),
        Ok(response(200, json!({"status":"Running"}))),
        Ok(response(200, json!({"status":"Succeeded"}))),
    ]);
    let receipt = harness
        .approved()
        .await
        .execute(
            &harness.context,
            &(&harness),
            OperationLimits::default(),
            true,
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.outcome, DeploymentOutcome::Succeeded);
    assert_eq!(receipt.http_status, Some(202));
    assert_eq!(receipt.deployment_id.as_deref(), Some(DEPLOYMENT));
    assert_eq!(harness.post_count(), 1);
    assert_eq!(
        harness.calls.lock().unwrap()[3].1,
        format!("/v1/operations/{OPERATION}")
    );
    assert!(receipt.execution_plan.is_none());
}

#[tokio::test]
async fn immediate_success_and_partial_failure_are_not_http_success_equivalence() {
    let harness = Harness::new([Ok(completed("Succeeded"))]);
    let result = harness
        .approved()
        .await
        .execute(
            &harness.context,
            &(&harness),
            OperationLimits::default(),
            false,
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(result.outcome, DeploymentOutcome::Succeeded);
    assert_eq!(result.deployment_id.as_deref(), Some(DEPLOYMENT));
    for partial in [false, true] {
        let mut reply = completed("Failed");
        let mut value: Value = serde_json::from_slice(&reply.body).unwrap();
        value["executionPlan"] =
            json!({"steps":[{"index":0,"status":if partial {"Succeeded"} else {"Failed"}}]});
        reply.body = serde_json::to_vec(&value).unwrap();
        let harness = Harness::new([Ok(reply)]);
        let failure = harness
            .approved()
            .await
            .execute(
                &harness.context,
                &(&harness),
                OperationLimits::default(),
                false,
                pending(),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(failure.kind, Kind::DeploymentFailed);
        assert_eq!(
            failure.receipt.unwrap().outcome,
            if partial {
                DeploymentOutcome::Partial
            } else {
                DeploymentOutcome::Failed
            }
        );
        assert_eq!(harness.post_count(), 1);
    }
}

#[tokio::test]
async fn permission_transport_timeout_and_failed_wait_retain_uncertainty_without_replay() {
    let harness = Harness::new([Ok(response(
        403,
        json!({"errorCode":"InsufficientPrivileges","message":"private-service-message"}),
    ))]);
    let failure = harness
        .approved()
        .await
        .execute(
            &harness.context,
            &(&harness),
            OperationLimits::default(),
            false,
            pending(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(failure.kind, Kind::PermissionDenied);
    assert_eq!(failure.receipt.as_ref().unwrap().http_status, Some(403));
    assert!(matches!(
        failure.operation.unwrap().kind,
        OperationErrorKind::Http { status: 403, .. }
    ));
    for cause in [
        OperationExecutionError::Failed,
        OperationExecutionError::TimedOut,
    ] {
        let harness = Harness::new([Err(cause)]);
        let failure = harness
            .approved()
            .await
            .execute(
                &harness.context,
                &(&harness),
                OperationLimits::default(),
                false,
                pending(),
            )
            .await
            .err()
            .unwrap();
        assert!(failure.receipt.unwrap().mutation_may_have_occurred);
        assert_eq!(harness.post_count(), 1);
    }
    let harness = Harness::new([
        Ok(accepted()),
        Ok(response(
            200,
            json!({"status":"Failed","error":{"errorCode":"NativeFailure","message":"private-service-message"}}),
        )),
    ]);
    let failure = harness
        .approved()
        .await
        .execute(
            &harness.context,
            &(&harness),
            OperationLimits::default(),
            true,
            pending(),
        )
        .await
        .err()
        .unwrap();
    let receipt = failure.receipt.as_ref().unwrap();
    assert_eq!(receipt.operation_id.as_deref(), Some(OPERATION));
    assert_eq!(receipt.deployment_id.as_deref(), Some(DEPLOYMENT));
    assert!(!format!("{failure:?}").contains("private-service-message"));
    assert_eq!(harness.post_count(), 1);
}

#[tokio::test]
async fn malformed_acceptance_unknown_state_and_wrong_results_are_not_success() {
    for mutation in 0..4 {
        let mut reply = accepted();
        match mutation {
            0 => {
                reply.headers.remove("Deployment-ID");
            }
            1 => {
                reply
                    .headers
                    .insert("deployment-id".into(), DEPLOYMENT.into());
            }
            2 => {
                reply
                    .headers
                    .insert("Deployment-ID".into(), "invalid".into());
            }
            _ => {
                reply.headers.insert(
                    "Location".into(),
                    format!("https://attacker.invalid/v1/operations/{OPERATION}"),
                );
            }
        }
        let harness = Harness::new([Ok(reply)]);
        let failure = harness
            .approved()
            .await
            .execute(
                &harness.context,
                &(&harness),
                OperationLimits::default(),
                true,
                pending(),
            )
            .await
            .err()
            .unwrap();
        assert!(failure.receipt.unwrap().mutation_may_have_occurred);
        assert_eq!(harness.calls.lock().unwrap().len(), 3);
    }
    for reply in [
        completed("Unexpected"),
        response(200, json!({})),
        response(
            200,
            json!({"status":"Succeeded","id":DEPLOYMENT,"sourceStageId":ITEM,"targetStageId":TARGET}),
        ),
    ] {
        let harness = Harness::new([Ok(reply)]);
        assert!(
            harness
                .approved()
                .await
                .execute(
                    &harness.context,
                    &(&harness),
                    OperationLimits::default(),
                    false,
                    pending()
                )
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn approval_decline_and_pre_dispatch_cancellation_make_zero_posts() {
    struct Deny;
    impl DeploymentApproval for Deny {
        fn authorize(&mut self, _: &PreparedDeployment) -> Result<(), DeploymentError> {
            Err(DeploymentError {
                kind: Kind::ApprovalDenied,
                operation: None,
                receipt: None,
            })
        }
    }
    let harness = Harness::new([]);
    assert_eq!(
        harness
            .prepare(0, 1)
            .await
            .unwrap()
            .authorize(&mut Deny)
            .err()
            .unwrap()
            .kind,
        Kind::ApprovalDenied
    );
    let failure = harness
        .approved()
        .await
        .execute(
            &harness.context,
            &(&harness),
            OperationLimits::default(),
            false,
            ready(()),
        )
        .await
        .err()
        .unwrap();
    assert!(!failure.receipt.unwrap().mutation_may_have_occurred);
    assert_eq!(harness.post_count(), 0);
}

#[tokio::test]
async fn context_substitution_is_rejected_before_credential_or_mutation_work() {
    let harness = Harness::new([]);
    let prepared = harness.prepare(0, 1).await.unwrap();
    let other = RequestContext::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    );
    assert_eq!(
        prepared.require_context(&other).err().unwrap().kind,
        Kind::TargetMismatch
    );
    let failure = prepared
        .authorize(&mut Approve)
        .unwrap()
        .execute(
            &other,
            &(&harness),
            OperationLimits::default(),
            false,
            pending(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(failure.kind, Kind::TargetMismatch);
    assert!(failure.receipt.is_none());
    assert_eq!(harness.post_count(), 0);
}

#[tokio::test]
async fn post_acceptance_cancellation_and_request_budget_keep_both_handles() {
    for cancellation in [true, false] {
        let harness = Harness::new([Ok(accepted())]);
        let approved = harness.approved().await;
        let signal = poll_fn(|_| {
            if cancellation && harness.post_count() > 0 {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let limits = if cancellation {
            OperationLimits::default()
        } else {
            OperationLimits {
                max_requests: NonZeroUsize::new(1).unwrap(),
                ..Default::default()
            }
        };
        let failure = approved
            .execute(&harness.context, &(&harness), limits, true, signal)
            .await
            .err()
            .unwrap();
        assert!(matches!(
            failure.operation.unwrap().kind,
            OperationErrorKind::Cancelled | OperationErrorKind::Limit
        ));
        let receipt = failure.receipt.unwrap();
        assert_eq!(receipt.operation_id.as_deref(), Some(OPERATION));
        assert_eq!(receipt.deployment_id.as_deref(), Some(DEPLOYMENT));
        assert_eq!(receipt.outcome, DeploymentOutcome::Unknown);
        assert!(receipt.mutation_may_have_occurred);
        assert_eq!(harness.calls.lock().unwrap().len(), 3);
        assert_eq!(harness.post_count(), 1);
    }
}

#[tokio::test]
async fn default_step_receipt_preserves_effect_ids_but_omits_raw_service_text() {
    let mut reply = completed("Failed");
    let mut value: Value = serde_json::from_slice(&reply.body).unwrap();
    value["executionPlan"] = json!({"steps":[{
        "index":0,"status":"Succeeded","description":"private-service-text",
        "sourceAndTarget":{"sourceItemId":ITEM,"targetItemId":DEPLOYMENT,"itemType":"Notebook","sourceItemDisplayName":"private-service-text"},
        "error":{"errorCode":"NativeFailure","message":"private-service-text"}
    }]});
    reply.body = serde_json::to_vec(&value).unwrap();
    let harness = Harness::new([Ok(reply)]);
    let failure = harness
        .approved()
        .await
        .execute(
            &harness.context,
            &(&harness),
            OperationLimits::default(),
            false,
            pending(),
        )
        .await
        .err()
        .unwrap();
    let receipt = failure.receipt.unwrap();
    assert_eq!(receipt.outcome, DeploymentOutcome::Partial);
    let safe = receipt.to_value();
    assert_eq!(
        safe["evidence"]["executionPlan"]["steps"][0]["sourceAndTarget"]["sourceItemId"],
        ITEM
    );
    assert!(!safe.to_string().contains("private-service-text"));
    assert!(
        receipt
            .execution_plan
            .unwrap()
            .to_string()
            .contains("private-service-text")
    );
}

#[tokio::test]
async fn supplied_execution_steps_must_corroborate_success() {
    for step in [
        json!({"index":0,"status":"Failed","error":{"errorCode":"NativeFailure"}}),
        json!({"index":0,"status":"Running"}),
        json!({"index":0,"status":"NotStarted"}),
        json!({"index":0,"status":"Unexpected"}),
        json!({"index":0,"status":"Succeeded","error":{"errorCode":"NativeFailure"}}),
    ] {
        let mut reply = completed("Succeeded");
        let mut value: Value = serde_json::from_slice(&reply.body).unwrap();
        value["executionPlan"] = json!({"steps":[step]});
        reply.body = serde_json::to_vec(&value).unwrap();
        let harness = Harness::new([Ok(reply)]);
        let failure = harness
            .approved()
            .await
            .execute(
                &harness.context,
                &(&harness),
                OperationLimits::default(),
                false,
                pending(),
            )
            .await
            .expect_err("contradictory plan cannot establish success");
        let receipt = failure.receipt.unwrap();
        assert_eq!(receipt.outcome, DeploymentOutcome::Unknown);
        assert_eq!(receipt.deployment_id.as_deref(), Some(DEPLOYMENT));
        assert!(receipt.execution_plan.is_some());
        assert!(receipt.mutation_may_have_occurred);
        assert_eq!(harness.post_count(), 1);
    }

    for plan in [
        Value::Null,
        json!({"steps":[]}),
        json!({"steps":[{"index":0,"status":"Succeeded","error":null}]}),
    ] {
        let mut reply = completed("Succeeded");
        let mut value: Value = serde_json::from_slice(&reply.body).unwrap();
        value["executionPlan"] = plan;
        reply.body = serde_json::to_vec(&value).unwrap();
        let harness = Harness::new([Ok(reply)]);
        let receipt = harness
            .approved()
            .await
            .execute(
                &harness.context,
                &(&harness),
                OperationLimits::default(),
                false,
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.outcome, DeploymentOutcome::Succeeded);
        assert_eq!(harness.post_count(), 1);
    }
}

#[tokio::test]
async fn cancellation_after_immediate_response_retains_native_identity_and_effects() {
    for status in ["Succeeded", "Failed"] {
        let mut reply = completed(status);
        let mut value: Value = serde_json::from_slice(&reply.body).unwrap();
        value["executionPlan"] = json!({"steps":[{"index":0,"status":"Succeeded","description":"private-service-text"}]});
        reply.body = serde_json::to_vec(&value).unwrap();
        let harness = Harness::new([Ok(reply)]);
        let approved = harness.approved().await;
        let signal = poll_fn(|_| {
            if harness.post_count() > 0 {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let failure = approved
            .execute(
                &harness.context,
                &(&harness),
                OperationLimits::default(),
                false,
                signal,
            )
            .await
            .err()
            .unwrap();
        assert_eq!(failure.kind, Kind::Cancelled);
        let receipt = failure.receipt.unwrap();
        assert_eq!(receipt.outcome, DeploymentOutcome::Unknown);
        assert_eq!(receipt.http_status, Some(200));
        assert_eq!(receipt.deployment_id.as_deref(), Some(DEPLOYMENT));
        assert_eq!(receipt.execution_plan, Some(value["executionPlan"].clone()));
        assert_eq!(
            serde_json::from_slice::<Value>(&receipt.initial_response.as_ref().unwrap().body)
                .unwrap(),
            value
        );
        assert!(receipt.mutation_may_have_occurred);
        assert!(
            !receipt
                .to_value()
                .to_string()
                .contains("private-service-text")
        );
        assert!(!format!("{receipt:?}").contains("private-service-text"));
        assert_eq!(harness.post_count(), 1);
    }
}

#[tokio::test]
async fn interrupted_foreign_or_malformed_responses_do_not_invent_effect_identity() {
    for value in [
        json!({"id":DEPLOYMENT,"type":"Deploy","sourceStageId":ITEM,"targetStageId":TARGET,"status":"Succeeded"}),
        json!({"id":"invalid","type":"Deploy","sourceStageId":SOURCE,"targetStageId":TARGET,"status":"Succeeded"}),
        Value::Null,
    ] {
        let harness = Harness::new([Ok(response(200, value))]);
        let approved = harness.approved().await;
        let signal = poll_fn(|_| {
            if harness.post_count() > 0 {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let failure = approved
            .execute(
                &harness.context,
                &(&harness),
                OperationLimits::default(),
                false,
                signal,
            )
            .await
            .err()
            .unwrap();
        assert_eq!(failure.kind, Kind::Cancelled);
        let receipt = failure.receipt.unwrap();
        assert_eq!(receipt.outcome, DeploymentOutcome::Unknown);
        assert!(receipt.deployment_id.is_none());
        assert!(receipt.execution_plan.is_none());
        assert!(receipt.initial_response.is_some());
        assert!(receipt.mutation_may_have_occurred);
        assert_eq!(harness.post_count(), 1);
    }
}

#[tokio::test]
async fn cancelled_created_response_preserves_native_deployment_identity_and_effects() {
    let mut native = completed("Succeeded");
    native.status = 201;
    let harness = Harness::new([Ok(native)]);
    let approved = harness.approved().await;
    let signal = poll_fn(|_| {
        if harness.post_count() > 0 {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let failure = approved
        .execute(
            &harness.context,
            &(&harness),
            OperationLimits::default(),
            false,
            signal,
        )
        .await
        .err()
        .unwrap();
    assert_eq!(failure.kind, Kind::Cancelled);
    let receipt = failure.receipt.unwrap();
    assert_eq!(receipt.http_status, Some(201));
    assert_eq!(receipt.deployment_id.as_deref(), Some(DEPLOYMENT));
    assert!(receipt.initial_response.is_some());
    assert!(receipt.mutation_may_have_occurred);
    assert_eq!(harness.post_count(), 1);
}
