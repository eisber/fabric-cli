// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Direct SDK consumer: no facade, CLI, deployment or identity-backend dependency.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::{Audience, CredentialSelection, TokenCredential};
use fabric_sdk_operations::{
    FabricOperation, OperationErrorKind, OperationExecutionError, OperationExecutor,
    OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceMethod, ResourceOperation, ResourceOperations, ResourceOutcome,
    WorkspaceMutations,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const ROOT: &str = "https://api.fabric.microsoft.com";

#[derive(Debug, Default)]
struct Credential(AtomicUsize);
#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "synthetic-shared-operation",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Clone, Default)]
struct Executor {
    responses: Arc<Mutex<VecDeque<OperationResponse>>>,
    methods: Arc<Mutex<Vec<ResourceMethod>>>,
    urls: Arc<Mutex<Vec<String>>>,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
}
impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.methods.lock().unwrap().push(request.method());
        self.urls.lock().unwrap().push(request.url().to_string());
        self.bodies.lock().unwrap().push(request.body().to_vec());
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request");
        Box::pin(std::future::ready(Ok(response)))
    }
    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

fn selection() -> CredentialSelection {
    CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap()
}

fn response(status: u16, body: &str) -> OperationResponse {
    OperationResponse {
        status,
        body: body.as_bytes().into(),
        headers: BTreeMap::new(),
    }
}

#[tokio::test]
async fn explicit_continuation_reaches_domain_mutations_without_state_in_fresh_operations() {
    for (method, action, body) in [
        (ResourceMethod::Patch, "", json!({"displayName":"Updated"})),
        (
            ResourceMethod::Post,
            "unassignWorkspaces",
            json!({"workspacesIds":["workspace"]}),
        ),
    ] {
        let credential = Credential::default();
        let selection = selection();
        let executor = Executor::default();
        for token in [Some("*~ +%literal/日"), None] {
            executor
                .responses
                .lock()
                .unwrap()
                .push_back(response(202, ""));
            let mut segments = vec!["admin", "domains", "domain"];
            if !action.is_empty() {
                segments.push(action);
            }
            let mut operation = ResourceOperation::new(
                method,
                &segments,
                Some(body.clone()),
                CompletionPolicy::PythonSynchronous,
            )
            .unwrap();
            if let Some(token) = token {
                operation = operation.with_continuation_token(token).unwrap();
            }
            let mut session = OperationSession::new(
                &credential,
                &selection,
                executor.clone(),
                OperationLimits::default(),
                pending(),
            );
            assert_eq!(
                ResourceOperations::execute(&mut session, operation)
                    .await
                    .unwrap(),
                ResourceOutcome::Accepted { operation_id: None }
            );
        }
        let expected_route = format!(
            "{ROOT}/v1/admin/domains/domain{}",
            if action.is_empty() {
                String::new()
            } else {
                format!("/{action}")
            }
        );
        assert_eq!(
            *executor.urls.lock().unwrap(),
            [
                format!("{expected_route}?continuationToken=%2A~+%2B%25literal%2F%E6%97%A5"),
                expected_route,
            ]
        );
        assert_eq!(*executor.methods.lock().unwrap(), [method, method]);
        assert_eq!(
            *executor.bodies.lock().unwrap(),
            [
                serde_json::to_vec(&body).unwrap(),
                serde_json::to_vec(&body).unwrap()
            ]
        );
        assert_eq!(credential.0.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn explicit_token_does_not_change_cancellation_audience_or_response_limits() {
    for (audience, cancel, limit, expected) in [
        (Audience::Fabric, true, 1024, OperationErrorKind::Cancelled),
        (
            Audience::OneLake,
            false,
            1024,
            OperationErrorKind::InvalidAudience,
        ),
        (Audience::Fabric, false, 1, OperationErrorKind::Limit),
    ] {
        let credential = Credential::default();
        let selection = CredentialSelection::new("offline", "tenant", audience).unwrap();
        let executor = Executor::default();
        executor
            .responses
            .lock()
            .unwrap()
            .push_back(response(200, "{}"));
        let cancellation: Pin<Box<dyn Future<Output = ()> + Send>> = if cancel {
            Box::pin(std::future::ready(()))
        } else {
            Box::pin(pending())
        };
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits {
                max_response_bytes: std::num::NonZeroUsize::new(limit).unwrap(),
                ..OperationLimits::default()
            },
            cancellation,
        );
        let operation = ResourceOperation::new(
            ResourceMethod::Patch,
            &["admin", "domains", "domain"],
            Some(json!({})),
            CompletionPolicy::PythonSynchronous,
        )
        .unwrap()
        .with_continuation_token("opaque")
        .unwrap();
        let error = session
            .execute(FabricOperation::from_resource(&operation).unwrap())
            .await
            .unwrap_err();
        assert_eq!(error.kind, expected);
        let dispatched = usize::from(!cancel && audience == Audience::Fabric);
        assert_eq!(executor.urls.lock().unwrap().len(), dispatched);
        assert_eq!(credential.0.load(Ordering::SeqCst), dispatched);
        assert_eq!(error.mutation_may_have_occurred, dispatched == 1);
    }
}

#[tokio::test]
async fn continuation_is_not_copied_to_poll_requests_and_request_budget_still_bounds_reconciliation()
 {
    for maximum in [1, 2] {
        let credential = Credential::default();
        let selection = selection();
        let executor = Executor::default();
        executor
            .responses
            .lock()
            .unwrap()
            .extend([accepted(), response(200, r#"{"status":"Succeeded"}"#)]);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits {
                max_requests: std::num::NonZeroUsize::new(maximum).unwrap(),
                ..OperationLimits::default()
            },
            pending(),
        );
        let operation = ResourceOperation::new(
            ResourceMethod::Post,
            &["admin", "domains", "domain", "unassignWorkspaces"],
            Some(json!({"workspacesIds":["workspace"]})),
            CompletionPolicy::PythonSynchronous,
        )
        .unwrap()
        .with_continuation_token("source-read-token")
        .unwrap();
        let result = session
            .execute(FabricOperation::from_resource(&operation).unwrap())
            .await;
        if maximum == 1 {
            let error = result.unwrap_err();
            assert_eq!(error.kind, OperationErrorKind::Limit);
            assert!(error.mutation_may_have_occurred);
            assert_eq!(
                error.operation_id.as_deref(),
                Some("22222222-2222-2222-2222-222222222222")
            );
        } else {
            assert_eq!(result.unwrap().status, 200);
            assert_eq!(
                executor.urls.lock().unwrap()[1],
                format!("{ROOT}/v1/operations/22222222-2222-2222-2222-222222222222")
            );
        }
        assert_eq!(executor.methods.lock().unwrap().len(), maximum);
    }
}

#[tokio::test]
async fn direct_workspace_sdk_consumer_uses_shared_policy_without_cli_or_facade() {
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::default();
    executor
        .responses
        .lock()
        .unwrap()
        .push_back(response(201, r#"{"id":"created"}"#));
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let result = WorkspaceMutations::create(&mut session, json!({"displayName":"Created"}))
        .await
        .unwrap();
    assert_eq!(
        result,
        ResourceOutcome::Completed {
            status: 201,
            data: Some(json!({"id":"created"}))
        }
    );
    assert_eq!(*executor.methods.lock().unwrap(), [ResourceMethod::Post]);
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn service_error_data_and_request_id_survive_the_generic_policy() {
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::default();
    executor.responses.lock().unwrap().push_back(response(403, r#"{"errorCode":"Denied","message":"sensitive service diagnostic","requestId":"synthetic-request"}"#));
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor,
        OperationLimits::default(),
        pending(),
    );
    let error = ResourceOperations::execute(
        &mut session,
        ResourceOperation::new(
            ResourceMethod::Delete,
            &["workspaces", "ws"],
            None,
            CompletionPolicy::ReturnAccepted,
        )
        .unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "Denied");
    assert_eq!(error.status, Some(403));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(error.service_error.unwrap().error_code, "Denied");
    assert!(!error.message.contains("sensitive service diagnostic"));
}

#[tokio::test]
async fn cancellation_before_credentials_prevents_any_request_and_marks_no_side_effect() {
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::default();
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        std::future::ready(()),
    );
    let error = session
        .execute(
            FabricOperation::new(
                ResourceMethod::Delete,
                "/v1/workspaces/ws",
                Vec::new(),
                CompletionPolicy::WaitForCompletion,
            )
            .unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, OperationErrorKind::Cancelled);
    assert!(!error.mutation_may_have_occurred);
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(executor.methods.lock().unwrap().is_empty());
}

#[tokio::test]
async fn validated_encoded_resource_names_do_not_reopen_the_raw_string_path_boundary() {
    let operation = ResourceOperation::encoded(
        ResourceMethod::Get,
        &[
            "workspaces",
            "ws",
            "items",
            "item",
            "shortcuts",
            "Files",
            "a b#\u{65e5}%literal",
        ],
        None,
        CompletionPolicy::PythonSynchronous,
    )
    .unwrap()
    .with_query(&[("name", "a&b=+")])
    .unwrap();
    assert_eq!(
        operation.path(),
        "/v1/workspaces/ws/items/item/shortcuts/Files/a%20b%23%E6%97%A5%25literal?name=a%26b%3D%2B"
    );
    assert!(
        FabricOperation::new(
            ResourceMethod::Get,
            operation.path(),
            Vec::new(),
            CompletionPolicy::PythonSynchronous
        )
        .is_err()
    );
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::default();
    executor
        .responses
        .lock()
        .unwrap()
        .push_back(response(200, "{}"));
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    ResourceOperations::execute(&mut session, operation)
        .await
        .unwrap();
    assert_eq!(
        executor.urls.lock().unwrap()[0],
        "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/shortcuts/Files/a%20b%23%E6%97%A5%25literal?name=a%26b%3D%2B"
    );
}

#[test]
fn encoded_constructor_rejects_separators_dot_segments_controls_and_get_payloads() {
    for name in ["", "..", ".", "a/b", "a\\b", "name\r\nheader: value"] {
        assert!(
            ResourceOperation::encoded(
                ResourceMethod::Get,
                &["shortcuts", name],
                None,
                CompletionPolicy::PythonSynchronous
            )
            .is_err()
        );
    }
    assert!(
        ResourceOperation::encoded(
            ResourceMethod::Get,
            &["shortcuts", "name"],
            Some(json!({})),
            CompletionPolicy::PythonSynchronous
        )
        .is_err()
    );
    assert!(
        ResourceOperation::new(
            ResourceMethod::Get,
            &["shortcuts", "a b"],
            None,
            CompletionPolicy::PythonSynchronous
        )
        .is_err()
    );
}

#[test]
fn generic_boundary_rejects_other_origins_traversal_and_get_bodies() {
    for path in [
        "https://outside.invalid/v1/workspaces",
        "//outside.invalid/v1/workspaces",
        "/v1/workspaces/../capacities",
        "/v1/workspaces/%2e%2e/capacities",
        "/v1/workspaces#fragment",
        "/v1//workspaces",
    ] {
        assert!(matches!(
            FabricOperation::new(
                ResourceMethod::Get,
                path,
                Vec::new(),
                CompletionPolicy::ReturnAccepted
            ),
            Err(OperationErrorKind::InvalidInput)
        ));
    }
    assert!(
        FabricOperation::new(
            ResourceMethod::Get,
            "/v1/workspaces",
            b"{}".to_vec(),
            CompletionPolicy::ReturnAccepted
        )
        .is_err()
    );
}

#[tokio::test]
async fn malformed_source_poll_body_fails_without_inventing_a_running_state() {
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::default();
    let mut accepted = response(202, "{}");
    accepted.headers.insert(
        "x-ms-operation-id".into(),
        "22222222-2222-2222-2222-222222222222".into(),
    );
    executor
        .responses
        .lock()
        .unwrap()
        .extend([accepted, response(200, "[]")]);
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let error = session
        .execute(
            FabricOperation::new(
                ResourceMethod::Post,
                "/v1/workspaces/ws/items",
                b"{}".to_vec(),
                CompletionPolicy::PythonSynchronous,
            )
            .unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, OperationErrorKind::InvalidResponse);
    assert!(error.mutation_may_have_occurred);
    assert_eq!(executor.methods.lock().unwrap().len(), 2);
}

fn accepted() -> OperationResponse {
    let mut accepted = response(202, "{}");
    accepted.headers.insert(
        "x-ms-operation-id".into(),
        "22222222-2222-2222-2222-222222222222".into(),
    );
    accepted
}

#[tokio::test]
async fn external_acceptance_reconciliation_only_issues_gets_and_never_replays_the_mutation() {
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::default();
    executor.responses.lock().unwrap().extend([
        response(200, r#"{"status":"Succeeded"}"#),
        response(200, r#"{"id":"actual-result"}"#),
    ]);
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let result = session
        .reconcile_accepted(accepted(), CompletionPolicy::WaitForResult)
        .await
        .unwrap();
    assert_eq!(result.body, br#"{"id":"actual-result"}"#);
    assert_eq!(
        *executor.methods.lock().unwrap(),
        [ResourceMethod::Get, ResourceMethod::Get]
    );
    assert_eq!(credential.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn external_acceptance_retains_operation_id_when_cancelled_before_any_poll() {
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::default();
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        std::future::ready(()),
    );
    let error = session
        .reconcile_accepted(accepted(), CompletionPolicy::PythonSynchronous)
        .await
        .unwrap_err();
    assert_eq!(error.kind, OperationErrorKind::Cancelled);
    assert_eq!(
        error.operation_id.as_deref(),
        Some("22222222-2222-2222-2222-222222222222")
    );
    assert!(error.mutation_may_have_occurred);
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(executor.methods.lock().unwrap().is_empty());
}

#[tokio::test]
async fn external_initial_request_and_response_bytes_count_against_the_real_bounds() {
    let credential = Credential::default();
    let selection = selection();
    for limits in [
        OperationLimits {
            max_requests: std::num::NonZeroUsize::new(1).unwrap(),
            ..OperationLimits::default()
        },
        OperationLimits {
            max_response_bytes: std::num::NonZeroUsize::new(1).unwrap(),
            ..OperationLimits::default()
        },
    ] {
        let executor = Executor::default();
        let mut session =
            OperationSession::new(&credential, &selection, executor.clone(), limits, pending());
        let error = session
            .reconcile_accepted(accepted(), CompletionPolicy::WaitForResult)
            .await
            .unwrap_err();
        assert_eq!(error.kind, OperationErrorKind::Limit);
        assert!(error.mutation_may_have_occurred);
        assert!(executor.methods.lock().unwrap().is_empty());
    }
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn external_acceptance_cannot_silently_reset_a_used_session_or_follow_foreign_links() {
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::default();
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    session
        .reconcile_accepted(accepted(), CompletionPolicy::ReturnAccepted)
        .await
        .unwrap();
    assert_eq!(
        session
            .reconcile_accepted(accepted(), CompletionPolicy::ReturnAccepted)
            .await
            .unwrap_err()
            .kind,
        OperationErrorKind::InvalidInput
    );
    let mut foreign = accepted();
    foreign.headers.insert(
        "Location".into(),
        "https://outside.invalid/v1/operations/22222222-2222-2222-2222-222222222222".into(),
    );
    let mut fresh = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let error = fresh
        .reconcile_accepted(foreign, CompletionPolicy::PythonSynchronous)
        .await
        .unwrap_err();
    assert_eq!(error.kind, OperationErrorKind::InvalidLink);
    assert!(error.mutation_may_have_occurred);
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(executor.methods.lock().unwrap().is_empty());
}
