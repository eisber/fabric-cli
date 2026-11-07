// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Direct SDK evidence for pinned Python ARM GET reconciliation, entirely offline.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::{Audience, CredentialSelection, ServiceOrigin, TokenCredential};
use fabric_sdk_operations::{
    ArmGetOperation, FabricOperation, OperationErrorKind, OperationExecutionError,
    OperationExecutor, OperationLimits, OperationRequest, OperationResponse, OperationSession,
    OperationWarning,
};
use fabric_sdk_workspaces::mutations::{CompletionPolicy, ResourceMethod};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
    time::Duration,
};

const RESOURCE: &str = "/subscriptions/sub/resourceGroups/group/providers/Microsoft.Fabric/capacities/Main?api-version=2023-11-01";
const POLL: &str =
    "https://management.azure.com/providers/Microsoft.Fabric/operations/op?api-version=2023-11-01";

#[derive(Debug, Default)]
struct Credential(Mutex<Vec<Vec<String>>>);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0
            .lock()
            .unwrap()
            .push(scopes.iter().map(|scope| (*scope).into()).collect());
        Ok(AccessToken::new(
            "offline-arm-operation",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Clone, Default)]
struct Executor {
    responses: Arc<Mutex<VecDeque<OperationResponse>>>,
    urls: Arc<Mutex<Vec<String>>>,
    origins: Arc<Mutex<Vec<ServiceOrigin>>>,
    waits: Arc<Mutex<Vec<Duration>>>,
    cancel_on_response: Option<Arc<AtomicBool>>,
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        assert_eq!(request.method(), ResourceMethod::Get);
        assert!(request.body().is_empty());
        self.urls.lock().unwrap().push(request.url().to_string());
        self.origins.lock().unwrap().push(request.origin());
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request");
        if let Some(flag) = &self.cancel_on_response {
            flag.store(true, Ordering::SeqCst);
        }
        Box::pin(ready(Ok(response)))
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.waits.lock().unwrap().push(delay);
        Box::pin(ready(Ok(())))
    }
}

fn selection(audience: Audience) -> CredentialSelection {
    CredentialSelection::new("offline", "tenant", audience).unwrap()
}

fn response(status: u16, body: Value) -> OperationResponse {
    OperationResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

fn accepted(status: u16, header: &str) -> OperationResponse {
    let mut response = response(
        status,
        json!({"id":"original","properties":{"source":true}}),
    );
    response.headers.insert(header.into(), POLL.into());
    response.headers.insert("Retry-After".into(), "0".into());
    response
}

#[tokio::test]
async fn arm_initial_201_and_202_async_headers_reuse_shared_terminal_poller_and_original_result() {
    for initial in [201, 202] {
        for terminal in ["Succeeded", "Completed"] {
            let credential = Credential::default();
            let selection = selection(Audience::Arm);
            let executor = Executor::default();
            let mut initial = accepted(initial, "Azure-AsyncOperation");
            initial.headers.insert(
                "Location".into(),
                "https://untrusted.invalid/ignored".into(),
            );
            let original = initial.body.clone();
            let mut running = response(200, json!({"status":"Running"}));
            running.headers.insert("Retry-After".into(), "0".into());
            let mut completed = response(200, json!({"status":terminal}));
            completed.headers.insert(
                "Location".into(),
                "https://untrusted.invalid/not-an-arm-result".into(),
            );
            executor
                .responses
                .lock()
                .unwrap()
                .extend([initial, running, completed]);
            let mut session = OperationSession::for_arm(
                &credential,
                &selection,
                executor.clone(),
                OperationLimits::default(),
                pending(),
            );
            let result = session
                .execute_arm_get(ArmGetOperation::new(RESOURCE).unwrap())
                .await
                .unwrap();
            assert_eq!(result.status, 200);
            assert_eq!(result.body, original);
            assert_eq!(
                *executor.urls.lock().unwrap(),
                [
                    format!("https://management.azure.com{RESOURCE}"),
                    POLL.into(),
                    POLL.into()
                ]
            );
            assert_eq!(*executor.origins.lock().unwrap(), [ServiceOrigin::Arm; 3]);
            assert_eq!(
                *credential.0.lock().unwrap(),
                vec![vec!["https://management.azure.com/.default".to_owned()]; 3]
            );
            let evidence = session.evidence(OperationErrorKind::Cancelled);
            assert_eq!(evidence.operation_id.as_deref(), Some("op"));
            assert!(!evidence.mutation_may_have_occurred);
            assert!(executor.responses.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn arm_location_polling_accepts_201_202_until_200_without_reading_terminal_json_or_result() {
    for initial in [201, 202] {
        let credential = Credential::default();
        let selection = selection(Audience::Arm);
        let executor = Executor::default();
        let initial = accepted(initial, "Location");
        let original = initial.body.clone();
        let mut running = response(202, Value::Null);
        running.headers.insert("Retry-After".into(), "0".into());
        let mut running201 = response(201, Value::Null);
        running201.headers.insert("Retry-After".into(), "0".into());
        executor.responses.lock().unwrap().extend([
            initial,
            running,
            running201,
            response(200, json!({"status":"Failed","error":{"code":"ignored"}})),
        ]);
        let mut session = OperationSession::for_arm(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let result = session
            .execute_arm_get(ArmGetOperation::new(RESOURCE).unwrap())
            .await
            .unwrap();
        assert_eq!(result.status, 200);
        assert_eq!(result.body, original);
        assert_eq!(executor.urls.lock().unwrap().len(), 4);
        assert!(
            executor.urls.lock().unwrap()[1..]
                .iter()
                .all(|url| url == POLL)
        );
    }
}

#[tokio::test]
async fn external_arm_acceptance_reconciles_without_replaying_and_counts_initial_budget() {
    let credential = Credential::default();
    let selection = selection(Audience::Arm);
    for budget in [1, 2] {
        let executor = Executor::default();
        if budget == 2 {
            executor
                .responses
                .lock()
                .unwrap()
                .push_back(response(200, json!({"status":"Succeeded"})));
        }
        let mut session = OperationSession::for_arm(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits {
                max_requests: NonZeroUsize::new(budget).unwrap(),
                ..OperationLimits::default()
            },
            pending(),
        );
        let result = session
            .reconcile_arm_get(accepted(201, "Azure-AsyncOperation"))
            .await;
        if budget == 1 {
            let failure = result.unwrap_err();
            assert_eq!(failure.kind, OperationErrorKind::Limit);
            assert_eq!(failure.operation_id.as_deref(), Some("op"));
            assert!(executor.urls.lock().unwrap().is_empty());
        } else {
            assert_eq!(result.unwrap().status, 200);
            assert_eq!(*executor.urls.lock().unwrap(), [POLL]);
        }
    }
}

#[tokio::test]
async fn arm_terminal_failure_cancellation_and_raw_service_evidence_remain_original() {
    for status in ["Failed", "Cancelled"] {
        let credential = Credential::default();
        let selection = selection(Audience::Arm);
        let executor = Executor::default();
        executor.responses.lock().unwrap().extend([
            accepted(202, "Azure-AsyncOperation"),
            response(
                200,
                json!({"status":status,"error":{"code":"source-code","detail":[1,2]}}),
            ),
        ]);
        let mut session = OperationSession::for_arm(
            &credential,
            &selection,
            executor,
            OperationLimits::default(),
            pending(),
        );
        let failure = session
            .execute_arm_get(ArmGetOperation::new(RESOURCE).unwrap())
            .await
            .unwrap_err();
        assert!(
            matches!(failure.kind, OperationErrorKind::OperationFailed { status: ref actual, .. } if actual == status)
        );
        assert_eq!(
            failure.raw_service_error.as_ref().unwrap()["detail"],
            json!([1, 2])
        );
        assert_eq!(failure.operation_id.as_deref(), Some("op"));
        assert!(!failure.mutation_may_have_occurred);
    }
}

#[tokio::test]
async fn arm_local_cancellation_after_acceptance_keeps_operation_id_without_replay() {
    let credential = Credential::default();
    let selection = selection(Audience::Arm);
    let flag = Arc::new(AtomicBool::new(false));
    let executor = Executor {
        cancel_on_response: Some(flag.clone()),
        ..Executor::default()
    };
    executor
        .responses
        .lock()
        .unwrap()
        .push_back(accepted(201, "Azure-AsyncOperation"));
    let cancelled = poll_fn(|_| {
        if flag.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let mut session = OperationSession::for_arm(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        cancelled,
    );
    let failure = session
        .execute_arm_get(ArmGetOperation::new(RESOURCE).unwrap())
        .await
        .unwrap_err();
    assert_eq!(failure.kind, OperationErrorKind::Cancelled);
    assert_eq!(failure.operation_id.as_deref(), Some("op"));
    assert!(!failure.mutation_may_have_occurred);
    assert_eq!(executor.urls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn arm_links_are_fixed_origin_bounded_and_required_before_any_poll() {
    for uri in [
        "https://management.azure.com.evil.invalid/op",
        "https://untrusted.invalid/management.azure.com/op",
        "http://management.azure.com/op",
        "https://management.azure.com@untrusted.invalid/op",
        "https://management.azure.com/a/../op",
        "https://management.azure.com/op#fragment",
        "/providers/Microsoft.Fabric/operations/op",
        "",
    ] {
        let credential = Credential::default();
        let selection = selection(Audience::Arm);
        let executor = Executor::default();
        let mut initial = accepted(202, "Azure-AsyncOperation");
        initial
            .headers
            .insert("Azure-AsyncOperation".into(), uri.into());
        executor.responses.lock().unwrap().push_back(initial);
        let mut session = OperationSession::for_arm(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let failure = session
            .execute_arm_get(ArmGetOperation::new(RESOURCE).unwrap())
            .await
            .unwrap_err();
        assert_eq!(failure.kind, OperationErrorKind::InvalidLink);
        assert_eq!(executor.urls.lock().unwrap().len(), 1);
        assert!(executor.waits.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn arm_read_reconciliation_does_not_change_default_fabric_or_audience_guards() {
    let credential = Credential::default();
    let arm = selection(Audience::Arm);
    let fabric = selection(Audience::Fabric);
    let executor = Executor::default();
    let mut default = OperationSession::new(
        &credential,
        &fabric,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    assert_eq!(
        default
            .execute_arm_get(ArmGetOperation::new(RESOURCE).unwrap())
            .await
            .unwrap_err()
            .kind,
        OperationErrorKind::InvalidInput
    );
    let mut wrong = OperationSession::for_arm(
        &credential,
        &fabric,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    assert_eq!(
        wrong
            .execute_arm_get(ArmGetOperation::new(RESOURCE).unwrap())
            .await
            .unwrap_err()
            .kind,
        OperationErrorKind::InvalidAudience
    );
    let mut arm = OperationSession::for_arm(
        &credential,
        &arm,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    assert!(
        arm.execute(
            FabricOperation::new(
                ResourceMethod::Get,
                "/v1/workspaces",
                vec![],
                CompletionPolicy::PythonSynchronous
            )
            .unwrap()
        )
        .await
        .is_err()
    );
    assert!(executor.urls.lock().unwrap().is_empty());
    assert!(credential.0.lock().unwrap().is_empty());
    for path in [
        "https://management.azure.com/subscriptions",
        "//management.azure.com/subscriptions",
        "/v1/workspaces",
        "/subscriptions/sub/../escape",
    ] {
        assert!(ArmGetOperation::new(path).is_err());
    }
    executor
        .responses
        .lock()
        .unwrap()
        .push_back(response(200, json!({"value":[]})));
    let response = default
        .execute(
            FabricOperation::new(
                ResourceMethod::Get,
                "/v1/workspaces",
                vec![],
                CompletionPolicy::ReturnAccepted,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(*executor.origins.lock().unwrap(), [ServiceOrigin::Fabric]);
    assert_eq!(
        *credential.0.lock().unwrap(),
        vec![vec!["https://api.fabric.microsoft.com/.default".to_owned()]]
    );
}

#[tokio::test]
async fn arm_shared_retry_delay_fallback_and_bound_are_preserved() {
    for delay in ["bad", "61"] {
        let credential = Credential::default();
        let selection = selection(Audience::Arm);
        let executor = Executor::default();
        let mut initial = accepted(202, "Azure-AsyncOperation");
        initial.headers.insert("Retry-After".into(), delay.into());
        executor.responses.lock().unwrap().push_back(initial);
        if delay == "bad" {
            executor
                .responses
                .lock()
                .unwrap()
                .push_back(response(200, json!({"status":"Completed"})));
        }
        let mut session = OperationSession::for_arm(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let result = session
            .execute_arm_get(ArmGetOperation::new(RESOURCE).unwrap())
            .await;
        if delay == "bad" {
            assert!(result.is_ok());
            assert_eq!(session.warnings(), [OperationWarning::InvalidRetryAfter]);
            assert_eq!(*executor.waits.lock().unwrap(), [Duration::from_secs(10)]);
        } else {
            assert_eq!(result.unwrap_err().kind, OperationErrorKind::Limit);
            assert!(executor.waits.lock().unwrap().is_empty());
        }
    }
}
