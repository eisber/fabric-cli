// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual CLI handlers -> focused SDK -> shared operation engine -> shared HTTP.

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_context::{Element, ItemType, NodeKind, WorkspaceType};
use fabric_cli_mutations::{
    CommandFuture, CreateOptions, FabricMutationBackend, MutationError, MutationHost,
    MutationMetadata, MutationUi, NoticeKind,
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use fabric_sdk_operations::OperationLimits;
use fabric_sdk_workspaces::{
    CredentialSelection,
    mutations::{
        CompletionPolicy, ResourceMethod, ResourceOperation, ResourceOperations, ResourceOutcome,
    },
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::pending,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

const OP: &str = "22222222-2222-2222-2222-222222222222";

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
            "synthetic-offline-policy",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

#[derive(Debug)]
struct Network {
    replies: Mutex<VecDeque<azure_core::Result<AsyncRawResponse>>>,
    requests: Mutex<Vec<Request>>,
}

#[async_trait]
impl HttpClient for Network {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected replay/request")
    }
}

#[derive(Default)]
struct Timer(Mutex<Vec<Duration>>);

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay == Duration::from_secs(240) {
            return Box::pin(pending());
        }
        self.0.lock().unwrap().push(delay);
        Box::pin(std::future::ready(()))
    }
}

struct Ui;
impl MutationUi for Ui {
    fn notice(&mut self, _: NoticeKind, _: &str) {}
    fn confirm<'a>(&'a mut self, _: &'a str) -> CommandFuture<'a, bool> {
        Box::pin(std::future::ready(true))
    }
}

struct Metadata;
impl MutationMetadata for Metadata {
    fn read<'a>(
        &'a mut self,
        _: &'a Arc<Element>,
    ) -> CommandFuture<'a, Result<Value, MutationError>> {
        Box::pin(std::future::ready(Ok(json!({"description":"old"}))))
    }
}

fn selection() -> CredentialSelection {
    CredentialSelection::new("offline", "tenant", fabric_sdk_operations::Audience::Fabric).unwrap()
}

fn reply(
    status: u16,
    body: &str,
    headers: &[(&str, &str)],
) -> azure_core::Result<AsyncRawResponse> {
    let mut result = Headers::new();
    for &(key, value) in headers {
        result.insert(key.to_owned(), value.to_owned());
    }
    Ok(AsyncRawResponse::from_bytes(
        StatusCode::from(status),
        result,
        body.as_bytes().to_vec(),
    ))
}

fn network(replies: Vec<azure_core::Result<AsyncRawResponse>>) -> Arc<Network> {
    Arc::new(Network {
        replies: Mutex::new(replies.into()),
        requests: Mutex::new(Vec::new()),
    })
}

fn target(id: Option<&str>) -> Arc<Element> {
    let workspace = Element::child(
        "Team",
        Some("ws".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Element::tenant("Tenant", "tenant"),
    )
    .unwrap();
    Element::child(
        "Lake",
        id.map(str::to_owned),
        NodeKind::Item(ItemType::parse("Lakehouse").unwrap()),
        workspace,
    )
    .unwrap()
}

#[tokio::test]
async fn actual_create_handler_waits_running_succeeded_and_result_through_shared_http() {
    let location = format!("https://api.fabric.microsoft.com/v1/operations/{OP}");
    let result_location = format!("{location}/result");
    let network = network(vec![
        reply(
            202,
            "",
            &[
                ("x-ms-operation-id", OP),
                ("Location", &location),
                ("Retry-After", "1"),
            ],
        ),
        reply(200, r#"{"status":"Running"}"#, &[]),
        reply(
            200,
            r#"{"status":"Succeeded"}"#,
            &[("Location", &result_location)],
        ),
        reply(
            200,
            r#"{"id":"created","displayName":"Lake","type":"Lakehouse"}"#,
            &[],
        ),
    ]);
    let timer = Arc::new(Timer::default());
    let http =
        HttpExecutor::new(network.clone(), timer.clone(), ExecutionOptions::default()).unwrap();
    let mut backend = FabricMutationBackend::new(
        Arc::new(Credential),
        selection(),
        &http,
        OperationLimits::default(),
        pending(),
    );
    let mut ui = Ui;
    let mut metadata = Metadata;
    let result = MutationHost {
        operations: &mut backend,
        ui: &mut ui,
        metadata: &mut metadata,
    }
    .create(
        &target(None),
        &CreateOptions {
            params: json!({"run":"true","enableSchemas":"true"})
                .as_object()
                .unwrap()
                .clone(),
            ..CreateOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        result.outputs[0].message.as_deref(),
        Some("'Lake.Lakehouse' created")
    );
    assert_eq!(result.outputs[0].data.as_ref().unwrap()["id"], "created");
    let requests = network.requests.lock().unwrap();
    assert_eq!(
        requests.iter().map(Request::method).collect::<Vec<_>>(),
        [Method::Post, Method::Get, Method::Get, Method::Get]
    );
    assert_eq!(requests[0].url().path(), "/v1/workspaces/ws/lakehouses");
    assert_eq!(requests[3].url().as_str(), result_location);
    assert_eq!(
        *timer.0.lock().unwrap(),
        [Duration::from_secs(1), Duration::from_secs(10)]
    );
}

#[tokio::test]
async fn generic_patch_and_delete_lost_responses_are_not_replayed() {
    for method in [ResourceMethod::Patch, ResourceMethod::Delete] {
        let network = network(vec![Err(azure_core::Error::with_message(
            azure_core::error::ErrorKind::Other,
            "synthetic transport failure",
        ))]);
        let http = HttpExecutor::new(
            network.clone(),
            Arc::new(Timer::default()),
            ExecutionOptions::default(),
        )
        .unwrap();
        let mut backend = FabricMutationBackend::new(
            Arc::new(Credential),
            selection(),
            &http,
            OperationLimits::default(),
            pending(),
        );
        let error = backend
            .execute(
                ResourceOperation::new(
                    method,
                    &["workspaces", "ws", "items", "item"],
                    None,
                    CompletionPolicy::WaitForCompletion,
                )
                .unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "ExecutionFailed");
        assert!(error.mutation_may_have_occurred);
        assert_eq!(network.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn request_bound_retains_accepted_id_without_inventing_a_created_resource() {
    let location = format!("https://api.fabric.microsoft.com/v1/operations/{OP}");
    let network = network(vec![reply(202, "", &[("Location", &location)])]);
    let http = HttpExecutor::new(
        network.clone(),
        Arc::new(Timer::default()),
        ExecutionOptions::default(),
    )
    .unwrap();
    let mut backend = FabricMutationBackend::new(
        Arc::new(Credential),
        selection(),
        &http,
        OperationLimits {
            max_requests: NonZeroUsize::new(1).unwrap(),
            ..OperationLimits::default()
        },
        pending(),
    );
    let error = backend
        .execute(
            ResourceOperation::new(
                ResourceMethod::Post,
                &["workspaces", "ws", "items"],
                Some(json!({"type":"Lakehouse"})),
                CompletionPolicy::WaitForResult,
            )
            .unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "Limit");
    assert_eq!(error.operation_id.as_deref(), Some(OP));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(network.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn accepted_delete_and_204_completion_remain_distinct() {
    let network = network(vec![
        reply(202, "", &[("x-ms-operation-id", OP)]),
        reply(204, "", &[]),
    ]);
    let http = HttpExecutor::new(
        network.clone(),
        Arc::new(Timer::default()),
        ExecutionOptions::default(),
    )
    .unwrap();
    let mut backend = FabricMutationBackend::new(
        Arc::new(Credential),
        selection(),
        &http,
        OperationLimits::default(),
        pending(),
    );
    for expected in [
        ResourceOutcome::Accepted {
            operation_id: Some(OP.into()),
        },
        ResourceOutcome::Completed {
            status: 204,
            data: None,
        },
    ] {
        assert_eq!(
            backend
                .execute(
                    ResourceOperation::new(
                        ResourceMethod::Delete,
                        &["workspaces", "ws"],
                        None,
                        CompletionPolicy::ReturnAccepted,
                    )
                    .unwrap()
                )
                .await
                .unwrap(),
            expected
        );
    }
}
