// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! SQLDatabase metadata consumers using the shared operation engine.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    error::ErrorKind,
    time::OffsetDateTime,
};
use fabric_core::{Audience, CredentialSelection, RequestContext, TokenCredential};
use fabric_sdk_operations::{
    OperationExecutionError, OperationExecutor, OperationLimits, OperationRequest,
    OperationResponse, OperationSession,
};
use fabric_sdk_sql_database::{
    ContinuationToken, FolderScope, ListSqlDatabases, PageCoverage, SqlDatabaseId,
    SqlDatabaseRequests, SqlDatabaseTarget, SqlManagementError, SqlResponseErrorKind, WorkspaceId,
};
use fabric_sdk_workspaces::mutations::{
    ResourceError, ResourceMethod, ResourceOperation, ResourceOperations, ResourceOutcome,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::{Future, pending, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
const DATABASE: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const ROOT: &str = "https://api.fabric.microsoft.com";
const TOKEN: &str = "synthetic-offline-sql-metadata-token";

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
    scopes: Mutex<Vec<Vec<String>>>,
    fail: bool,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|value| (*value).to_owned()).collect());
        if self.fail {
            return Err(azure_core::Error::with_message(
                ErrorKind::Other,
                "sensitive-provider-detail",
            ));
        }
        Ok(AccessToken::new(TOKEN, OffsetDateTime::UNIX_EPOCH))
    }
}

#[derive(Debug)]
struct RecordedRequest {
    method: ResourceMethod,
    url: String,
    body: Vec<u8>,
}

#[derive(Clone, Default)]
struct Executor {
    replies: Arc<Mutex<VecDeque<Result<OperationResponse, OperationExecutionError>>>>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    waits: Arc<AtomicUsize>,
}

impl Executor {
    fn new(replies: impl IntoIterator<Item = OperationResponse>) -> Self {
        Self {
            replies: Arc::new(Mutex::new(replies.into_iter().map(Ok).collect())),
            ..Self::default()
        }
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.token().token.secret(), TOKEN);
            assert_eq!(request.max_response_bytes().get(), 16_384);
            assert!(!format!("{request:?}").contains(TOKEN));
            self.requests.lock().unwrap().push(RecordedRequest {
                method: request.method(),
                url: request.url().to_string(),
                body: request.body().to_vec(),
            });
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected extra request or fallback")
        })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        self.waits.fetch_add(1, Ordering::SeqCst);
        Box::pin(ready(Err(OperationExecutionError::Failed)))
    }
}

fn selection(audience: Audience) -> CredentialSelection {
    CredentialSelection::new(
        "offline-selected-account",
        "offline-selected-tenant",
        audience,
    )
    .unwrap()
}

fn limits() -> OperationLimits {
    OperationLimits {
        max_requests: NonZeroUsize::new(1).unwrap(),
        max_response_bytes: NonZeroUsize::new(16_384).unwrap(),
        max_poll_delay: Duration::ZERO,
    }
}

fn target() -> SqlDatabaseTarget {
    SqlDatabaseTarget::new(
        WORKSPACE.parse().unwrap(),
        DATABASE.parse().unwrap(),
        "SQLDatabase",
    )
    .unwrap()
}

fn row() -> Value {
    json!({
        "id":DATABASE, "workspaceId":WORKSPACE,
        "displayName":"offline database", "type":"SQLDatabase"
    })
}

fn response(status: u16, body: Value) -> OperationResponse {
    OperationResponse {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: BTreeMap::new(),
    }
}

#[tokio::test]
async fn public_get_uses_shared_session_and_only_the_injected_fabric_credential() {
    let credential = Arc::new(Credential::default());
    let context = RequestContext::new(credential.clone(), selection(Audience::Fabric));
    let executor = Executor::new([response(200, row())]);
    let mut session =
        OperationSession::with_context(&context, executor.clone(), limits(), pending());
    let result = SqlDatabaseRequests::get(&target())
        .unwrap()
        .execute(&mut session)
        .await
        .unwrap();
    assert_eq!(result.id.to_string(), DATABASE);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *credential.scopes.lock().unwrap(),
        [vec![Audience::Fabric.scope().to_owned()]]
    );
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, ResourceMethod::Get);
    assert_eq!(
        requests[0].url,
        format!("{ROOT}/v1/workspaces/{WORKSPACE}/sqlDatabases/{DATABASE}")
    );
    assert!(requests[0].body.is_empty());
    assert_eq!(executor.waits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn single_page_keeps_cursor_and_does_not_follow_even_a_foreign_uri() {
    let credential = Credential::default();
    let selection = selection(Audience::Fabric);
    let executor = Executor::new([response(
        200,
        json!({
            "value":[row()],
            "continuationToken":"next-sensitive-token",
            "continuationUri":"https://untrusted.invalid/never-follow"
        }),
    )]);
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        limits(),
        pending(),
    );
    let page = SqlDatabaseRequests::list_page(
        &WORKSPACE.parse().unwrap(),
        &ListSqlDatabases {
            recursive: false,
            root_folder: FolderScope::Workspace,
            continuation_token: Some(ContinuationToken::new("a+b/%2F &?=日").unwrap()),
        },
    )
    .unwrap()
    .execute(&mut session)
    .await
    .unwrap();
    assert_eq!(page.coverage(), PageCoverage::Incomplete);
    assert_eq!(executor.calls(), 1);
    assert_eq!(
        executor.requests.lock().unwrap()[0].url,
        format!(
            "{ROOT}/v1/workspaces/{WORKSPACE}/sqlDatabases?recursive=false\
             &continuationToken=a%2Bb%2F%252F+%26%3F%3D%E6%97%A5"
        )
    );
    assert_eq!(executor.waits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_ids_owner_or_tokens_do_not_dispatch_or_acquire_credentials() {
    let credential = Credential::default();
    let selection = selection(Audience::Fabric);
    let executor = Executor::default();
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        limits(),
        pending(),
    );
    assert!("../escape".parse::<WorkspaceId>().is_err());
    assert!("/items/other".parse::<SqlDatabaseId>().is_err());
    assert!(ContinuationToken::new("sensitive\n").is_err());
    for owner in ["Warehouse", "SQLEndpoint", "SQLDatabase "] {
        if let Ok(target) =
            SqlDatabaseTarget::new(WORKSPACE.parse().unwrap(), DATABASE.parse().unwrap(), owner)
        {
            let _ = SqlDatabaseRequests::get(&target)
                .unwrap()
                .execute(&mut session)
                .await;
            panic!("wrong owner was admitted");
        }
    }
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert_eq!(executor.calls(), 0);
}

#[tokio::test]
async fn service_errors_keep_evidence_without_clone_enrichment_or_fallback() {
    for status in [401, 403, 404, 409, 429, 500] {
        let credential = Credential::default();
        let selection = selection(Audience::Fabric);
        let executor = Executor::new([response(
            status,
            json!({
                "errorCode":"NativeServiceError",
                "message":"sensitive-service-error",
                "requestId":"sensitive-request-id"
            }),
        )]);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            limits(),
            pending(),
        );
        let error = SqlDatabaseRequests::get(&target())
            .unwrap()
            .execute(&mut session)
            .await
            .unwrap_err();
        assert!(!format!("{error:?} {error}").contains("sensitive"));
        let SqlManagementError::Operation(error) = error else {
            panic!("shared evidence was replaced");
        };
        assert_eq!(error.status, Some(status));
        assert!(error.raw_service_error.is_some());
        assert!(!error.mutation_may_have_occurred);
        assert_eq!(executor.calls(), 1);
        assert_eq!(executor.waits.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn unexpected_statuses_and_invalid_json_are_not_success_or_hidden_waits() {
    for status in [201, 202, 204] {
        let credential = Credential::default();
        let selection = selection(Audience::Fabric);
        let mut initial = response(status, row());
        if status == 202 {
            initial
                .headers
                .insert("x-ms-operation-id".into(), DATABASE.into());
            initial.headers.insert(
                "Location".into(),
                format!("{ROOT}/v1/operations/{DATABASE}"),
            );
        }
        let executor = Executor::new([initial]);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            limits(),
            pending(),
        );
        let error = SqlDatabaseRequests::get(&target())
            .unwrap()
            .execute(&mut session)
            .await
            .unwrap_err();
        match error {
            SqlManagementError::Operation(error) => {
                assert_eq!(error.status, Some(status));
                assert!(!error.mutation_may_have_occurred);
            }
            SqlManagementError::Response(error) => {
                assert_eq!(error.http_status, status);
                assert_eq!(error.kind, SqlResponseErrorKind::UnexpectedStatus);
            }
        }
        assert_eq!(executor.calls(), 1);
        assert_eq!(executor.waits.load(Ordering::SeqCst), 0);
    }

    let credential = Credential::default();
    let selection = selection(Audience::Fabric);
    let executor = Executor::new([OperationResponse {
        status: 200,
        body: b"not-json-sensitive".to_vec(),
        headers: BTreeMap::new(),
    }]);
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        limits(),
        pending(),
    );
    let error = SqlDatabaseRequests::get(&target())
        .unwrap()
        .execute(&mut session)
        .await
        .unwrap_err();
    assert!(!format!("{error:?} {error}").contains("not-json-sensitive"));
    assert_eq!(executor.calls(), 1);
}

#[tokio::test]
async fn identity_mismatch_rejects_the_whole_page_without_fallback() {
    let credential = Credential::default();
    let selection = selection(Audience::Fabric);
    let mut mismatched = row();
    mismatched["workspaceId"] = json!(DATABASE);
    let executor = Executor::new([response(
        200,
        json!({"value":[row(), mismatched], "continuationToken":"unconsumed"}),
    )]);
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        limits(),
        pending(),
    );
    let error = SqlDatabaseRequests::list_page(
        &WORKSPACE.parse().unwrap(),
        &ListSqlDatabases {
            recursive: true,
            root_folder: FolderScope::Workspace,
            continuation_token: None,
        },
    )
    .unwrap()
    .execute(&mut session)
    .await
    .unwrap_err();
    let SqlManagementError::Response(error) = error else {
        panic!("expected identity failure");
    };
    assert_eq!(error.kind, SqlResponseErrorKind::IdentityMismatch);
    assert_eq!(executor.calls(), 1);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn malformed_native_shapes_fail_through_the_actual_shared_session() {
    let mut invalid_rows = vec![
        json!([]),
        json!([DATABASE, WORKSPACE, "positional-name", "SQLDatabase"]),
    ];
    for (field, invalid) in [
        ("type", json!({"SQLDatabase": null})),
        ("type", json!(["SQLDatabase"])),
        ("properties", json!([])),
        (
            "properties",
            json!([null, null, null, null, null, null, null]),
        ),
        (
            "properties",
            json!({"connectionString": {"unexpected": []}}),
        ),
        ("properties", json!({"serverFqdn": []})),
        ("properties", json!({"databaseName": {}})),
        ("properties", json!({"earliestRestorePoint": []})),
        ("properties", json!({"latestRestorePoint": {}})),
        ("properties", json!({"backupRetentionDays": []})),
        ("properties", json!({"collation": {}})),
    ] {
        let mut data = row();
        data[field] = invalid;
        invalid_rows.push(data);
    }
    let mut cases = vec![
        (true, json!([[]])),
        (true, json!({"value": {}})),
        (true, json!({"value": null})),
        (true, json!({"value": "not-an-array"})),
    ];
    for data in invalid_rows {
        cases.push((false, data.clone()));
        cases.push((true, json!({"value": [row(), data]})));
    }
    for (is_list, body) in cases {
        let credential = Credential::default();
        let selected = selection(Audience::Fabric);
        let executor = Executor::new([response(200, body)]);
        let mut session = OperationSession::new(
            &credential,
            &selected,
            executor.clone(),
            limits(),
            pending(),
        );
        let error = if is_list {
            SqlDatabaseRequests::list_page(
                &WORKSPACE.parse().unwrap(),
                &ListSqlDatabases {
                    recursive: false,
                    root_folder: FolderScope::Workspace,
                    continuation_token: None,
                },
            )
            .unwrap()
            .execute(&mut session)
            .await
            .err()
        } else {
            SqlDatabaseRequests::get(&target())
                .unwrap()
                .execute(&mut session)
                .await
                .err()
        }
        .expect("malformed metadata must not become successful terminal coverage");
        let SqlManagementError::Response(error) = error else {
            panic!("expected typed metadata shape failure");
        };
        assert_eq!(error.kind, SqlResponseErrorKind::InvalidShape);
        assert_eq!(error.http_status, 200);
        assert_eq!(executor.calls(), 1);
        assert_eq!(executor.waits.load(Ordering::SeqCst), 0);
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn cancellation_wrong_audience_and_credential_failure_do_not_fall_back() {
    let credential = Credential::default();
    let selected = selection(Audience::Fabric);
    let executor = Executor::default();
    let mut session = OperationSession::new(
        &credential,
        &selected,
        executor.clone(),
        limits(),
        ready(()),
    );
    assert!(
        SqlDatabaseRequests::get(&target())
            .unwrap()
            .execute(&mut session)
            .await
            .is_err()
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert_eq!(executor.calls(), 0);

    let wrong = selection(Audience::OneLake);
    let mut session =
        OperationSession::new(&credential, &wrong, executor.clone(), limits(), pending());
    assert!(
        SqlDatabaseRequests::get(&target())
            .unwrap()
            .execute(&mut session)
            .await
            .is_err()
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert_eq!(executor.calls(), 0);

    let failed = Credential {
        fail: true,
        ..Credential::default()
    };
    let mut session =
        OperationSession::new(&failed, &selected, executor.clone(), limits(), pending());
    let error = SqlDatabaseRequests::get(&target())
        .unwrap()
        .execute(&mut session)
        .await
        .unwrap_err();
    assert!(!format!("{error:?} {error}").contains("sensitive-provider-detail"));
    assert_eq!(failed.calls.load(Ordering::SeqCst), 1);
    assert_eq!(executor.calls(), 0);
}

struct AcceptedBoundary {
    calls: usize,
}

impl ResourceOperations for AcceptedBoundary {
    fn execute(
        &mut self,
        operation: ResourceOperation,
    ) -> Pin<Box<dyn Future<Output = Result<ResourceOutcome, ResourceError>> + Send + '_>> {
        assert_eq!(operation.method(), ResourceMethod::Get);
        self.calls += 1;
        Box::pin(ready(Ok(ResourceOutcome::Accepted {
            operation_id: Some(DATABASE.into()),
        })))
    }
}

#[tokio::test]
async fn supplied_acceptance_evidence_is_retained_and_never_reported_completed() {
    let mut boundary = AcceptedBoundary { calls: 0 };
    let error = SqlDatabaseRequests::get(&target())
        .unwrap()
        .execute(&mut boundary)
        .await
        .unwrap_err();
    let SqlManagementError::Response(error) = error else {
        panic!("expected incomplete acceptance");
    };
    assert_eq!(error.kind, SqlResponseErrorKind::UnexpectedAcceptance);
    assert_eq!(error.http_status, 202);
    assert_eq!(error.operation_id.as_deref(), Some(DATABASE));
    assert_eq!(boundary.calls, 1);
}

#[tokio::test]
async fn transport_timeout_cancellation_and_bounds_keep_shared_failure_evidence() {
    for failure in [
        OperationExecutionError::Failed,
        OperationExecutionError::TimedOut,
        OperationExecutionError::Cancelled,
        OperationExecutionError::ResponseLimit,
        OperationExecutionError::RetriesExhausted,
    ] {
        let credential = Credential::default();
        let selected = selection(Audience::Fabric);
        let executor = Executor::default();
        executor.replies.lock().unwrap().push_back(Err(failure));
        let mut session = OperationSession::new(
            &credential,
            &selected,
            executor.clone(),
            limits(),
            pending(),
        );
        let error = SqlDatabaseRequests::get(&target())
            .unwrap()
            .execute(&mut session)
            .await
            .unwrap_err();
        let SqlManagementError::Operation(error) = error else {
            panic!("expected shared execution failure");
        };
        assert_eq!(
            error.code,
            match failure {
                OperationExecutionError::Failed => "ExecutionFailed",
                OperationExecutionError::TimedOut => "TimedOut",
                OperationExecutionError::Cancelled => "LongRunningOperationCancelled",
                OperationExecutionError::ResponseLimit => "Limit",
                OperationExecutionError::RetriesExhausted => "MaxRetriesExceeded",
            }
        );
        assert!(!error.mutation_may_have_occurred);
        assert_eq!(executor.calls(), 1);
        assert_eq!(executor.waits.load(Ordering::SeqCst), 0);
    }

    let credential = Credential::default();
    let selected = selection(Audience::Fabric);
    let mut oversized = row();
    oversized["description"] = json!("s".repeat(16_384));
    let executor = Executor::new([response(200, oversized)]);
    let mut session = OperationSession::new(
        &credential,
        &selected,
        executor.clone(),
        limits(),
        pending(),
    );
    let error = SqlDatabaseRequests::get(&target())
        .unwrap()
        .execute(&mut session)
        .await
        .unwrap_err();
    let SqlManagementError::Operation(error) = error else {
        panic!("expected shared response bound");
    };
    assert_eq!(error.code, "Limit");
    assert_eq!(executor.calls(), 1);
}
