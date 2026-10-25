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
}
impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        self.methods.lock().unwrap().push(request.method());
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
