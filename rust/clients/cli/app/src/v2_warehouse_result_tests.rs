// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline regression through native Warehouse acquisition and the real CLI envelope.

#![deny(unnameable_test_items)]

use super::*;
use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_sdk::{
    TokenCredential,
    items::{ItemExecutionError, ItemExecutor, ItemRequest, ItemResponse},
    observe::{ObserveError, ReadHandle},
    sql_definitions::DefinitionErrorKind,
    workspaces::{WorkspaceExecutionError, WorkspaceExecutor, WorkspaceRequest, WorkspaceResponse},
};
use fabric_sdk_operations::{
    OperationErrorKind, OperationExecutionError, OperationExecutor, OperationRequest,
    OperationResponse,
};
use std::{collections::VecDeque, future::pending, path::Path, pin::Pin, sync::Mutex};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const OP: &str = "33333333-3333-3333-3333-333333333333";

#[derive(Debug)]
struct Credential;

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        Ok(AccessToken::new("offline-only", OffsetDateTime::UNIX_EPOCH))
    }
}

struct Executor {
    replies: Mutex<VecDeque<(&'static str, String, OperationResponse)>>,
    bounds: Mutex<Vec<usize>>,
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<OperationResponse, OperationExecutionError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let (method, path, response) = self.replies.lock().unwrap().pop_front().unwrap();
            assert_eq!(
                format!("{:?}", request.method()).to_ascii_uppercase(),
                method
            );
            assert_eq!(request.url().path(), path);
            self.bounds
                .lock()
                .unwrap()
                .push(request.max_response_bytes().get());
            Ok(response)
        })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), OperationExecutionError>> + Send + '_>>
    {
        panic!("terminal fixture must not poll")
    }
}

impl ItemExecutor for Executor {
    fn execute<'a>(
        &'a self,
        _: ItemRequest<'a>,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<ItemResponse, ItemExecutionError>> + Send + 'a>,
    > {
        panic!("no generic item fallback")
    }
}

impl WorkspaceExecutor for Executor {
    fn execute<'a>(
        &'a self,
        _: WorkspaceRequest<'a>,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<WorkspaceResponse, WorkspaceExecutionError>>
                + Send
                + 'a,
        >,
    > {
        panic!("no workspace fallback")
    }
}

fn reply(status: u16, body: Vec<u8>, headers: &[(&str, String)]) -> OperationResponse {
    OperationResponse {
        status,
        body,
        headers: headers
            .iter()
            .map(|(key, value)| ((*key).into(), value.clone()))
            .collect(),
    }
}

fn accepted_result(body: Vec<u8>) -> Executor {
    Executor {
        replies: Mutex::new(VecDeque::from([
            (
                "GET",
                format!("/v1/workspaces/{WS}/warehouses/{ITEM}"),
                reply(
                    200,
                    serde_json::to_vec(&json!({
                        "id":ITEM,"workspaceId":WS,"type":"Warehouse","displayName":"Warehouse"
                    }))
                    .unwrap(),
                    &[],
                ),
            ),
            (
                "POST",
                format!("/v1/workspaces/{WS}/warehouses/{ITEM}/getDefinition"),
                reply(202, Vec::new(), &[("x-ms-operation-id", OP.into())]),
            ),
            (
                "GET",
                format!("/v1/operations/{OP}"),
                reply(
                    200,
                    br#"{"status":"Succeeded","privateStatus":"never-print-status"}"#.to_vec(),
                    &[(
                        "Location",
                        format!("https://api.fabric.microsoft.com/v1/operations/{OP}/result"),
                    )],
                ),
            ),
            (
                "GET",
                format!("/v1/operations/{OP}/result"),
                reply(
                    200,
                    body,
                    &[("Content-Type", "application/private-source".into())],
                ),
            ),
        ])),
        bounds: Mutex::new(Vec::new()),
    }
}

async fn acquire(executor: &Executor, max_bytes: usize) -> Result<(), GetCommandError> {
    let client = Client::new(
        Arc::new(Credential),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
    );
    let backend = SdkGetBackend {
        client: &client,
        items: executor,
        workspaces: executor,
        operations: executor,
        storage: None,
        documents: None,
    };
    let command = parse_get(
        [
            "get".to_owned(),
            format!("/{WS}.Workspace/{ITEM}.Warehouse/src/definition"),
            "--wait".to_owned(),
            "--max-bytes".to_owned(),
            max_bytes.to_string(),
        ]
        .into_iter()
        .map(std::ffi::OsString::from),
    )
    .unwrap();
    PreparedGet::preflight(command, Path::new("unused-for-stdout"))
        .unwrap()
        .acquire(&backend, pending())
        .await
        .map(|_| ())
}

#[tokio::test]
async fn warehouse_wait_content_failure_keeps_operation_in_cli_envelope() {
    for (mut body, kind) in [
        (b"{}".to_vec(), DefinitionErrorKind::MissingDefinition),
        (
            br#"{"definition":{"format":"future-private-source","parts":[]}}"#.to_vec(),
            DefinitionErrorKind::UnsupportedFormat,
        ),
        (
            b"malformed-private-source".to_vec(),
            DefinitionErrorKind::InvalidJson,
        ),
        (
            br#"{"definition":{"parts":[{"path":"private-source.sqlproj","payload":"AA==","payloadType":"FuturePrivateEncoding"}]}}"#.to_vec(),
            DefinitionErrorKind::UnsupportedPayloadType,
        ),
    ] {
        body.reserve(2048);
        let expected = body.clone();
        let allocation = body.as_ptr();
        let executor = accepted_result(body);
        let error = acquire(&executor, 4096).await.unwrap_err();
        assert!(executor.replies.lock().unwrap().is_empty());
        let GetCommandError::Backend { code, source } = &error else {
            panic!("content admission must retain typed backend evidence")
        };
        assert_eq!(*code, "DefinitionResultInvalid");
        let content = source
            .downcast_ref::<WarehouseDefinitionResultError>()
            .expect("not a bare DefinitionError");
        assert_eq!(content.error.kind, kind);
        let observation = content.observation();
        assert_eq!(
            observation.handle,
            ReadHandle::Operation {
                operation_id: OP.into()
            }
        );
        assert!(observation.terminal && observation.work_succeeded);
        assert_eq!(observation.raw_status["status"], "Succeeded");
        assert_eq!(observation.raw_status["privateStatus"], "never-print-status");
        assert_eq!(observation.requests, 2);
        let result = observation.result.as_ref().unwrap();
        assert_eq!(result.bytes, expected);
        assert_eq!(result.bytes.as_ptr(), allocation, "move, never clone result");
        assert_eq!(result.media_type.as_deref(), Some("application/private-source"));
        let diagnostic = format!("{error:?}\n{error}\n{content:?}\n{content}");
        for secret in [WS, ITEM, OP, "private-source", "never-print-status", "FuturePrivateEncoding"] {
            assert!(!diagnostic.contains(secret));
        }
        let mut stderr = Vec::new();
        assert_eq!(finish(Err(command_failure(error)), &mut stderr), 1);
        let envelope: Value = serde_json::from_slice(&stderr).unwrap();
        assert_eq!(envelope["error"]["code"], "GetFailed");
        assert_eq!(envelope["evidence"]["handle"], format!("/operations/{OP}"));
        assert_eq!(envelope["evidence"]["status"], "Succeeded");
        assert_eq!(envelope["evidence"].as_object().unwrap().len(), 2);
        let rendered = String::from_utf8(stderr).unwrap();
        assert!(!rendered.contains("future-private-source"));
        assert!(!rendered.contains("private-source"));
        assert!(!rendered.contains("FuturePrivateEncoding"));
        assert!(!rendered.contains("never-print-status"));
    }
}

#[tokio::test]
async fn warehouse_wait_oversized_result_keeps_existing_bound_failure_not_content_copy() {
    let executor = accepted_result(vec![b'x'; 513]);
    let error = acquire(&executor, 512).await.unwrap_err();
    assert!(executor.replies.lock().unwrap().is_empty());
    assert!(
        executor
            .bounds
            .lock()
            .unwrap()
            .iter()
            .all(|bound| *bound <= 512)
    );
    let GetCommandError::Backend { code, source } = &error else {
        panic!("oversized response must retain the observer bound error")
    };
    assert_eq!(*code, "ObservationFailed");
    assert!(
        source
            .downcast_ref::<WarehouseDefinitionResultError>()
            .is_none()
    );
    let observation = source.downcast_ref::<ObserveError>().unwrap();
    assert_eq!(observation.operation_error.kind, OperationErrorKind::Limit);
    assert_eq!(
        observation.operation_error.operation_id.as_deref(),
        Some(OP)
    );
    assert!(observation.operation_error.raw_service_error.is_none());
    let last = observation.last_observation.as_ref().unwrap();
    assert!(last.work_succeeded);
    assert!(
        last.result.is_none(),
        "oversized bytes are not retained or cloned"
    );
}
