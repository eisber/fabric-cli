// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parent-owned native Lakehouse contracts over injected credentials and shared
//! HTTP/operation/job engines. All service responses and credentials are synthetic.

#![deny(unnameable_test_items)]

#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, Body, Method},
    time::OffsetDateTime,
};
use fabric_core::{Audience, CredentialSelection, ServiceOrigin};
use fabric_sdk_http::{ExecutionError, ExecutionOptions, HttpExecutor};
use fabric_sdk_jobs::{
    JobError, JobExecutionError, JobOperationReconciler, JobState, SharedJobOperations,
    WaitOptions, cancellable,
};
use fabric_sdk_operations::OperationLimits;
use fabric_sdk_tables::{
    LoadMode, LoadOutcome, SourceKind, TableClient, TableError, TableErrorKind, TableExecutor,
    TableFuture, TableLimits, TableRequest, TableResponse,
    lakehouse::{
        ExistingOneLakeSource, LakehouseErrorKind, LakehouseLoadFormat, LakehouseLoadLimits,
        LakehouseLoadOptions, LakehouseMaintenanceOptions, LakehouseTable, VacuumRetention,
    },
};
use serde_json::{Value, json};
use std::{
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
};
use support::{ScriptedHttp, TestJobExecutor, TestTimer, accepted, response};

const OP: &str = "22222222-2222-2222-2222-222222222222";

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<String>>,
    fail_on_call: AtomicUsize,
    pending_on_call: AtomicUsize,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes.len(), 1);
        assert!(matches!(
            scopes[0],
            "https://storage.azure.com/.default" | "https://api.fabric.microsoft.com/.default"
        ));
        let call = {
            let mut observed = self.scopes.lock().unwrap();
            observed.push(scopes[0].to_owned());
            observed.len()
        };
        if self.fail_on_call.load(Ordering::SeqCst) == call {
            return Err(azure_core::Error::new(
                azure_core::error::ErrorKind::Other,
                "private-provider-error",
            ));
        }
        if self.pending_on_call.load(Ordering::SeqCst) == call {
            pending::<()>().await;
        }
        Ok(AccessToken {
            token: "offline-native-lakehouse".into(),
            expires_on: OffsetDateTime::UNIX_EPOCH,
        })
    }
}

fn client(credential: Arc<Credential>) -> TableClient {
    TableClient::new(
        credential,
        &CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        &CredentialSelection::new("offline", "tenant", Audience::OneLake).unwrap(),
    )
    .unwrap()
}

fn table(schema: Option<&str>) -> LakehouseTable {
    LakehouseTable::new("ws", "item", schema.map(str::to_owned), "orders").unwrap()
}

fn source(kind: SourceKind) -> ExistingOneLakeSource {
    ExistingOneLakeSource::new("ws", "item", "Files/source data", kind).unwrap()
}

fn csv(mode: LoadMode) -> LakehouseLoadOptions {
    LakehouseLoadOptions::new(
        mode,
        LakehouseLoadFormat::Csv {
            header: Some(true),
            delimiter: Some("\t".into()),
        },
    )
}

fn head(kind: SourceKind) -> AsyncRawResponse {
    response(
        200,
        "",
        &[(
            "x-ms-resource-type",
            match kind {
                SourceKind::File => "file",
                SourceKind::Folder => "directory",
            },
        )],
    )
}

fn load_accepted() -> AsyncRawResponse {
    response(
        202,
        "",
        &[
            (
                "Location",
                &format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
            ),
            ("x-ms-operation-id", OP),
            ("Retry-After", "1"),
        ],
    )
}

/// Test-only seam composition; there is no replacement production HTTP/LRO policy.
struct WireTables {
    execution: Arc<TestJobExecutor>,
    network: Arc<ScriptedHttp>,
    credential: Arc<Credential>,
    load_budgets: Mutex<Vec<usize>>,
    fail_submission: AtomicBool,
}

impl WireTables {
    fn new(
        credential: Arc<Credential>,
        replies: impl IntoIterator<Item = AsyncRawResponse>,
        timer: Arc<TestTimer>,
    ) -> Self {
        let network = ScriptedHttp::new(replies);
        Self {
            execution: Arc::new(TestJobExecutor {
                http: HttpExecutor::new(network.clone(), timer, ExecutionOptions::default())
                    .unwrap(),
            }),
            network,
            credential,
            load_budgets: Mutex::new(Vec::new()),
            fail_submission: AtomicBool::new(false),
        }
    }
}

fn execution_error(error: ExecutionError) -> JobExecutionError {
    match error {
        ExecutionError::TimedOut => JobExecutionError::TimedOut,
        ExecutionError::ResponseLimit => JobExecutionError::ResponseLimit,
        ExecutionError::RetriesExhausted => JobExecutionError::RetriesExhausted,
        _ => JobExecutionError::Failed,
    }
}

fn table_failure(error: JobError, mutation: bool) -> TableError {
    let (kind, operation_id, shared_mutation) = match error {
        JobError::Operation(error) => (
            TableErrorKind::SharedOperation(Box::new(error.clone())),
            error.operation_id,
            error.mutation_may_have_occurred,
        ),
        JobError::Cancelled => (TableErrorKind::Cancelled, None, false),
        JobError::Execution(JobExecutionError::ResponseLimit) => {
            (TableErrorKind::Limit, None, false)
        }
        JobError::Execution(JobExecutionError::TimedOut) => (TableErrorKind::TimedOut, None, false),
        _ => (TableErrorKind::Execution, None, false),
    };
    TableError {
        kind,
        operation_id,
        mutation_may_have_occurred: mutation || shared_mutation,
    }
}

impl TableExecutor for WireTables {
    fn read<'a>(
        &'a self,
        request: TableRequest<'a>,
    ) -> TableFuture<'a, Result<TableResponse, TableError>> {
        Box::pin(async move {
            assert_eq!(request.method(), "HEAD");
            assert!(request.body().is_empty());
            assert_eq!(request.token().token.secret(), "offline-native-lakehouse");
            assert!(!format!("{request:?}").contains("offline-native-lakehouse"));
            let result = self
                .execution
                .http
                .execute_bound(
                    ServiceOrigin::OneLake,
                    Method::Head,
                    request.url(),
                    request.token(),
                    request.body(),
                    &[],
                    request.max_response_bytes(),
                )
                .await
                .map_err(|error| {
                    table_failure(JobError::Execution(execution_error(error)), false)
                })?;
            Ok(TableResponse {
                status: result.status,
                body: result.body,
                headers: result.headers,
            })
        })
    }

    fn load<'a>(
        &'a self,
        request: TableRequest<'a>,
        cancelled: TableFuture<'a, ()>,
    ) -> TableFuture<'a, Result<LoadOutcome, TableError>> {
        Box::pin(async move {
            assert_eq!(request.method(), "POST");
            assert!(std::ptr::addr_eq(
                request.credential(),
                self.credential.as_ref() as &dyn TokenCredential
            ));
            self.load_budgets
                .lock()
                .unwrap()
                .push(request.max_requests().get());
            let mut cancelled = pin!(cancelled);
            if self.fail_submission.load(Ordering::SeqCst) {
                self.network.fail.store(true, Ordering::SeqCst);
            }
            let received = cancellable(cancelled.as_mut(), async {
                self.execution
                    .http
                    .execute_fabric(
                        Method::Post,
                        request.url(),
                        request.token(),
                        request.body(),
                        request.max_response_bytes(),
                    )
                    .await
                    .map_err(|error| JobError::Execution(execution_error(error)))
            })
            .await
            .map_err(|error| table_failure(error, true))?;
            match received.status {
                200 | 201 => Ok(LoadOutcome::Completed),
                204 => Ok(LoadOutcome::NoOutput),
                202 if received.headers.keys().any(|name| {
                    name.eq_ignore_ascii_case("Location")
                        || name.eq_ignore_ascii_case("x-ms-operation-id")
                }) =>
                {
                    let operations = SharedJobOperations::from_job_executor(
                        self.credential.clone(),
                        request.selection().clone(),
                        self.execution.clone(),
                        OperationLimits {
                            max_requests: request.max_requests(),
                            max_response_bytes: request.max_response_bytes(),
                            ..OperationLimits::default()
                        },
                    );
                    let reconciled = operations
                        .reconcile_with_cancellation(&received, Box::pin(cancelled.as_mut()))
                        .await
                        .map_err(|error| table_failure(error, true))?;
                    match reconciled.status {
                        200 | 201 => Ok(LoadOutcome::Completed),
                        204 => Ok(LoadOutcome::NoOutput),
                        202 => Ok(LoadOutcome::Accepted {
                            operation_id: Some(OP.into()),
                        }),
                        _ => panic!("unexpected shared operation response"),
                    }
                }
                202 => Ok(LoadOutcome::Accepted { operation_id: None }),
                status => {
                    let data = serde_json::from_slice::<Value>(&received.body).unwrap_or_default();
                    Err(TableError {
                        kind: TableErrorKind::Http {
                            status,
                            code: data["errorCode"].as_str().map(str::to_owned),
                            message: data["message"].as_str().map(str::to_owned),
                        },
                        operation_id: None,
                        mutation_may_have_occurred: true,
                    })
                }
            }
        })
    }
}

#[test]
fn identity_and_existing_source_validation_are_explicit_and_not_path_inferred() {
    let target = table(Some("sales.eu"));
    assert_eq!(target.workspace(), "ws");
    assert_eq!(target.lakehouse(), "item");
    assert_eq!(target.schema(), Some("sales.eu"));
    assert_eq!(target.name(), "orders");
    assert_eq!(table(None).schema(), None);
    for name in ["", "123", "a.b", "a/b", "a\\b", "a%2fb", "a b", "a\nb"] {
        assert!(LakehouseTable::new("ws", "item", None, name).is_err());
    }
    assert!(LakehouseTable::new("ws", "item", None, "a".repeat(256)).is_ok());
    assert!(LakehouseTable::new("ws", "item", None, "a".repeat(257)).is_err());
    assert!(LakehouseTable::new("ws", "item", None, "_").is_ok());
    assert!(LakehouseTable::new("ws", "item", None, "123abc").is_ok());
    for parent in ["", "../other", "x%2fy", "x?y", "x#y", "https:", "x y"] {
        assert!(LakehouseTable::new(parent, "item", None, "orders").is_err());
        assert!(LakehouseTable::new("ws", parent, None, "orders").is_err());
    }
    for schema in ["", ".", "..", "sales/orders", "sales\\orders", "sales\n"] {
        assert!(LakehouseTable::new("ws", "item", Some(schema.into()), "orders").is_err());
    }
    for path in [
        "Files",
        "Files/",
        "files/a.csv",
        "Tables/a.csv",
        "/Files/a.csv",
        "Files/../a.csv",
        "Files/a%2fb.csv",
        "Files/a?sig=secret",
        "Files/a#fragment",
        "C:\\data.csv",
        "https://example.test/data.csv",
    ] {
        assert!(
            ExistingOneLakeSource::new("ws", "item", path, SourceKind::File).is_err(),
            "{path}"
        );
    }
    assert_eq!(
        source(SourceKind::Folder).path().local_path(),
        "Files/source data"
    );
    assert_eq!(source(SourceKind::Folder).kind(), SourceKind::Folder);
    assert_eq!(
        LakehouseLoadOptions::new(
            LoadMode::Append,
            LakehouseLoadFormat::Csv {
                header: None,
                delimiter: None
            },
        )
        .payload(&source(SourceKind::Folder))
        .unwrap(),
        json!({
            "relativePath":"Files/source data", "pathType":"Folder", "mode":"Append",
            "formatOptions":{"format":"Csv"}, "recursive":false,
        })
    );
}

#[tokio::test]
async fn native_payloads_use_real_remote_source_mode_format_and_file_extension_without_upload() {
    for kind in [SourceKind::File, SourceKind::Folder] {
        for mode in [LoadMode::Append, LoadMode::Overwrite] {
            for format in [
                LakehouseLoadFormat::Csv {
                    header: Some(false),
                    delimiter: Some(";".into()),
                },
                LakehouseLoadFormat::Parquet,
            ] {
                let credential = Arc::new(Credential::default());
                let sdk = client(credential.clone());
                let wire = WireTables::new(
                    credential.clone(),
                    [head(kind), response(200, "", &[])],
                    Arc::new(TestTimer::default()),
                );
                let mut options = LakehouseLoadOptions::new(mode, format.clone());
                options.file_extension = Some(".CSV".into());
                options.recursive = kind == SourceKind::Folder;
                let outcome = table(None)
                    .load(
                        &sdk,
                        &wire,
                        &source(kind),
                        &options,
                        LakehouseLoadLimits::default(),
                        pending(),
                    )
                    .await
                    .unwrap();
                assert_eq!(outcome, LoadOutcome::Completed);
                assert_eq!(
                    *credential.scopes.lock().unwrap(),
                    [Audience::OneLake.scope(), Audience::Fabric.scope()]
                );
                let requests = wire.network.requests.lock().unwrap();
                assert_eq!(requests.len(), 2, "one HEAD and one load, never an upload");
                assert_eq!(requests[0].method(), Method::Head);
                assert_eq!(
                    requests[0].url().as_str(),
                    "https://onelake.dfs.fabric.microsoft.com/ws/item/Files/source%20data"
                );
                assert_eq!(requests[1].method(), Method::Post);
                assert_eq!(
                    requests[1].url().as_str(),
                    "https://api.fabric.microsoft.com/v1/workspaces/ws/lakehouses/item/tables/orders/load"
                );
                let Body::Bytes(bytes) = requests[1].body() else {
                    panic!("expected native JSON bytes")
                };
                let body: Value = serde_json::from_slice(bytes).unwrap();
                assert_eq!(body["relativePath"], "Files/source data");
                assert_eq!(body["fileExtension"], ".CSV");
                assert!(body.get("extension").is_none());
                assert_eq!(
                    body["mode"],
                    if mode == LoadMode::Append {
                        "Append"
                    } else {
                        "Overwrite"
                    }
                );
                assert_eq!(
                    body["pathType"],
                    if kind == SourceKind::File {
                        "File"
                    } else {
                        "Folder"
                    }
                );
                if kind == SourceKind::Folder {
                    assert_eq!(body["recursive"], true);
                } else {
                    assert!(body.get("recursive").is_none());
                }
                assert_eq!(
                    body["formatOptions"],
                    match format {
                        LakehouseLoadFormat::Csv { .. } => {
                            json!({"format": "Csv", "header": false, "delimiter": ";"})
                        }
                        LakehouseLoadFormat::Parquet => json!({"format": "Parquet"}),
                    }
                );
                assert_eq!(*wire.load_budgets.lock().unwrap(), [99]);
            }
        }
    }
}

#[tokio::test]
async fn schema_gate_cross_parent_and_invalid_native_options_precede_credentials_and_io() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let wire = WireTables::new(credential.clone(), [], Arc::new(TestTimer::default()));
    let options = csv(LoadMode::Append);
    let error = table(Some("sales"))
        .load(
            &sdk,
            &wire,
            &source(SourceKind::File),
            &options,
            LakehouseLoadLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind,
        LakehouseErrorKind::SchemaLoadUnavailable
    ));
    for (workspace, item) in [("other", "item"), ("ws", "other")] {
        let foreign =
            ExistingOneLakeSource::new(workspace, item, "Files/source data", SourceKind::File)
                .unwrap();
        let error = table(None)
            .load(
                &sdk,
                &wire,
                &foreign,
                &options,
                LakehouseLoadLimits::default(),
                pending(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error.kind, LakehouseErrorKind::InvalidInput));
        assert!(!error.mutation_may_have_occurred);
    }
    let mut invalid = options.clone();
    invalid.recursive = true;
    assert!(invalid.payload(&source(SourceKind::File)).is_err());
    for extension in ["", "..", "bad/ext", "bad\\ext", "bad%2fext", "bad\next"] {
        let mut invalid = options.clone();
        invalid.file_extension = Some(extension.into());
        assert!(invalid.payload(&source(SourceKind::Folder)).is_err());
    }
    for delimiter in ["", "\0", "\n", "\r"] {
        let invalid = LakehouseLoadOptions::new(
            LoadMode::Overwrite,
            LakehouseLoadFormat::Csv {
                header: None,
                delimiter: Some(delimiter.into()),
            },
        );
        assert!(
            table(None)
                .load(
                    &sdk,
                    &wire,
                    &source(SourceKind::File),
                    &invalid,
                    LakehouseLoadLimits::default(),
                    pending()
                )
                .await
                .is_err()
        );
    }
    assert!(wire.network.requests.lock().unwrap().is_empty());
    assert!(credential.scopes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn properties_failure_or_source_kind_mismatch_never_dispatches_load() {
    for status in [403, 404] {
        let credential = Arc::new(Credential::default());
        let sdk = client(credential.clone());
        let wire = WireTables::new(
            credential.clone(),
            [response(
                status,
                r#"{"message":"private-source-error"}"#,
                &[],
            )],
            Arc::new(TestTimer::default()),
        );
        let error = table(None)
            .load(
                &sdk,
                &wire,
                &source(SourceKind::File),
                &csv(LoadMode::Append),
                LakehouseLoadLimits::default(),
                pending(),
            )
            .await
            .unwrap_err();
        assert!(!error.mutation_may_have_occurred);
        assert!(!error.to_string().contains("private-source-error"));
        assert_eq!(wire.network.requests.lock().unwrap().len(), 1);
        assert_eq!(credential.scopes.lock().unwrap().len(), 1);
    }
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let wire = WireTables::new(
        credential,
        [head(SourceKind::Folder)],
        Arc::new(TestTimer::default()),
    );
    let error = table(None)
        .load(
            &sdk,
            &wire,
            &source(SourceKind::File),
            &csv(LoadMode::Append),
            LakehouseLoadLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind,
        LakehouseErrorKind::SourceKindMismatch {
            expected: SourceKind::File,
            actual: SourceKind::Folder,
        }
    ));
    assert!(!error.mutation_may_have_occurred);
    assert_eq!(wire.network.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn recipe_and_request_bounds_are_enforced_at_the_exact_boundary() {
    let src = source(SourceKind::File);
    let options = csv(LoadMode::Append);
    let length = fabric_core_json::json_dumps(&options.payload(&src).unwrap()).len();
    for bound in [length - 1, length, length + 1] {
        let credential = Arc::new(Credential::default());
        let sdk = client(credential.clone());
        let wire = WireTables::new(
            credential.clone(),
            [head(SourceKind::File), response(200, "", &[])],
            Arc::new(TestTimer::default()),
        );
        let result = table(None)
            .load(
                &sdk,
                &wire,
                &src,
                &options,
                LakehouseLoadLimits {
                    max_request_bytes: NonZeroUsize::new(bound).unwrap(),
                    ..LakehouseLoadLimits::default()
                },
                pending(),
            )
            .await;
        assert_eq!(result.is_ok(), bound >= length);
        assert_eq!(
            wire.network.requests.lock().unwrap().len(),
            if bound < length { 0 } else { 2 }
        );
    }
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let wire = WireTables::new(credential.clone(), [], Arc::new(TestTimer::default()));
    let error = table(None)
        .load(
            &sdk,
            &wire,
            &src,
            &options,
            LakehouseLoadLimits {
                table: TableLimits {
                    max_requests: NonZeroUsize::new(1).unwrap(),
                    ..TableLimits::default()
                },
                ..LakehouseLoadLimits::default()
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error.kind, LakehouseErrorKind::Limit));
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(wire.network.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn token_failure_and_cancellation_before_dispatch_have_no_write_uncertainty() {
    for fail_on_call in [1, 2] {
        let credential = Arc::new(Credential::default());
        credential
            .fail_on_call
            .store(fail_on_call, Ordering::SeqCst);
        let sdk = client(credential.clone());
        let wire = WireTables::new(
            credential,
            [head(SourceKind::File)],
            Arc::new(TestTimer::default()),
        );
        let error = table(None)
            .load(
                &sdk,
                &wire,
                &source(SourceKind::File),
                &csv(LoadMode::Append),
                LakehouseLoadLimits::default(),
                pending(),
            )
            .await
            .unwrap_err();
        assert!(!error.mutation_may_have_occurred);
        assert!(!error.to_string().contains("private-provider-error"));
        assert_eq!(
            wire.network.requests.lock().unwrap().len(),
            fail_on_call - 1
        );
    }
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let wire = WireTables::new(credential.clone(), [], Arc::new(TestTimer::default()));
    let error = table(None)
        .load(
            &sdk,
            &wire,
            &source(SourceKind::File),
            &csv(LoadMode::Append),
            LakehouseLoadLimits::default(),
            ready(()),
        )
        .await
        .unwrap_err();
    assert!(!error.mutation_may_have_occurred);
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(wire.network.requests.lock().unwrap().is_empty());

    let credential = Arc::new(Credential::default());
    credential.pending_on_call.store(2, Ordering::SeqCst);
    let sdk = client(credential.clone());
    let wire = WireTables::new(
        credential,
        [head(SourceKind::File)],
        Arc::new(TestTimer::default()),
    );
    let cancel = AtomicBool::new(false);
    let target = table(None);
    let src = source(SourceKind::File);
    let options = csv(LoadMode::Append);
    let mut future = Box::pin(target.load(
        &sdk,
        &wire,
        &src,
        &options,
        LakehouseLoadLimits::default(),
        poll_fn(|_| {
            if cancel.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }),
    ));
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    cancel.store(true, Ordering::SeqCst);
    let error = future.await.unwrap_err();
    assert!(!error.mutation_may_have_occurred);
    assert_eq!(wire.network.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn bare_202_and_no_output_are_not_fabricated_completion() {
    for (status, expected) in [
        (202, LoadOutcome::Accepted { operation_id: None }),
        (204, LoadOutcome::NoOutput),
    ] {
        let credential = Arc::new(Credential::default());
        let sdk = client(credential.clone());
        let wire = WireTables::new(
            credential,
            [head(SourceKind::File), response(status, "", &[])],
            Arc::new(TestTimer::default()),
        );
        assert_eq!(
            table(None)
                .load(
                    &sdk,
                    &wire,
                    &source(SourceKind::File),
                    &csv(LoadMode::Append),
                    LakehouseLoadLimits::default(),
                    pending()
                )
                .await
                .unwrap(),
            expected,
        );
        assert_eq!(wire.network.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn native_submission_reuses_shared_operation_polling_and_retry_after() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let timer = Arc::new(TestTimer::default());
    let wire = WireTables::new(
        credential.clone(),
        [
            head(SourceKind::File),
            load_accepted(),
            response(
                200,
                r#"{"status":"Succeeded"}"#,
                &[(
                    "Location",
                    &format!("https://api.fabric.microsoft.com/v1/operations/{OP}/result"),
                )],
            ),
            response(200, "{}", &[]),
        ],
        timer.clone(),
    );
    assert_eq!(
        table(None)
            .load(
                &sdk,
                &wire,
                &source(SourceKind::File),
                &csv(LoadMode::Append),
                LakehouseLoadLimits::default(),
                pending()
            )
            .await
            .unwrap(),
        LoadOutcome::Completed,
    );
    let requests = wire.network.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == Method::Post)
            .count(),
        1
    );
    assert_eq!(requests[2].url().path(), format!("/v1/operations/{OP}"));
    assert_eq!(
        requests[3].url().path(),
        format!("/v1/operations/{OP}/result")
    );
    assert_eq!(*timer.waits.lock().unwrap(), [1]);
    assert_eq!(credential.scopes.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn hostile_monitor_unknown_numeric_and_failed_states_keep_receipt_without_replay() {
    for poll in [
        r#"{"status":"FutureState"}"#,
        r#"{"Status":1}"#,
        r#"{"status":"Failed","error":{"errorCode":"NativeFailure","message":"private-load-error"}}"#,
    ] {
        let credential = Arc::new(Credential::default());
        let sdk = client(credential.clone());
        let wire = WireTables::new(
            credential,
            [
                head(SourceKind::File),
                load_accepted(),
                response(200, poll, &[]),
            ],
            Arc::new(TestTimer::default()),
        );
        let error = table(None)
            .load(
                &sdk,
                &wire,
                &source(SourceKind::File),
                &csv(LoadMode::Append),
                LakehouseLoadLimits {
                    table: TableLimits {
                        max_requests: NonZeroUsize::new(3).unwrap(),
                        ..TableLimits::default()
                    },
                    ..LakehouseLoadLimits::default()
                },
                pending(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.operation_id.as_deref(), Some(OP));
        assert!(error.mutation_may_have_occurred);
        assert!(!error.to_string().contains("private-load-error"));
        assert_eq!(wire.network.requests.lock().unwrap().len(), 3);
    }
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let wire = WireTables::new(
        credential,
        [
            head(SourceKind::File),
            response(
                202,
                "",
                &[
                    (
                        "Location",
                        "https://untrusted.example/v1/operations/not-trusted",
                    ),
                    ("x-ms-operation-id", OP),
                ],
            ),
        ],
        Arc::new(TestTimer::default()),
    );
    let error = table(None)
        .load(
            &sdk,
            &wire,
            &source(SourceKind::File),
            &csv(LoadMode::Append),
            LakehouseLoadLimits::default(),
            pending(),
        )
        .await
        .unwrap_err();
    assert!(error.mutation_may_have_occurred);
    assert_eq!(wire.network.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn lost_submission_response_and_response_limit_are_uncertain_never_replayed() {
    for lost_response in [false, true] {
        let credential = Arc::new(Credential::default());
        let sdk = client(credential.clone());
        let wire = WireTables::new(
            credential,
            [head(SourceKind::File), response(200, "123456789", &[])],
            Arc::new(TestTimer::default()),
        );
        wire.fail_submission.store(lost_response, Ordering::SeqCst);
        let error = table(None)
            .load(
                &sdk,
                &wire,
                &source(SourceKind::File),
                &csv(LoadMode::Append),
                LakehouseLoadLimits {
                    table: TableLimits {
                        max_response_bytes: NonZeroUsize::new(8).unwrap(),
                        ..TableLimits::default()
                    },
                    ..LakehouseLoadLimits::default()
                },
                pending(),
            )
            .await
            .unwrap_err();
        assert!(error.mutation_may_have_occurred);
        assert_eq!(wire.network.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn lro_request_limit_and_local_cancellation_retain_accepted_operation() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let wire = WireTables::new(
        credential,
        [head(SourceKind::File), load_accepted()],
        Arc::new(TestTimer::default()),
    );
    let error = table(None)
        .load(
            &sdk,
            &wire,
            &source(SourceKind::File),
            &csv(LoadMode::Append),
            LakehouseLoadLimits {
                table: TableLimits {
                    max_requests: NonZeroUsize::new(2).unwrap(),
                    ..TableLimits::default()
                },
                ..LakehouseLoadLimits::default()
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.operation_id.as_deref(), Some(OP));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(wire.network.requests.lock().unwrap().len(), 2);

    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let timer = Arc::new(TestTimer::default());
    timer.pending_wait.store(true, Ordering::SeqCst);
    let wire = WireTables::new(credential, [head(SourceKind::File), load_accepted()], timer);
    let cancel = AtomicBool::new(false);
    let target = table(None);
    let src = source(SourceKind::File);
    let options = csv(LoadMode::Append);
    let mut future = Box::pin(target.load(
        &sdk,
        &wire,
        &src,
        &options,
        LakehouseLoadLimits::default(),
        poll_fn(|_| {
            if cancel.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }),
    ));
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    cancel.store(true, Ordering::SeqCst);
    let error = future.await.unwrap_err();
    assert_eq!(error.operation_id.as_deref(), Some(OP));
    assert!(error.mutation_may_have_occurred);
    assert_eq!(
        wire.network.requests.lock().unwrap().len(),
        2,
        "no remote cancel or replay"
    );
}

#[tokio::test]
async fn dropping_a_shared_load_wait_does_not_issue_remote_cancel_or_replay() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let timer = Arc::new(TestTimer::default());
    timer.pending_wait.store(true, Ordering::SeqCst);
    let wire = WireTables::new(credential, [head(SourceKind::File), load_accepted()], timer);
    let target = table(None);
    let src = source(SourceKind::File);
    let options = csv(LoadMode::Append);
    let mut future = Box::pin(target.load(
        &sdk,
        &wire,
        &src,
        &options,
        LakehouseLoadLimits::default(),
        pending(),
    ));
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(future);
    assert_eq!(wire.network.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn compatibility_schema_rejects_v1_rewriting_that_would_read_a_different_schema() {
    let credential = Arc::new(Credential::default());
    let sdk = client(credential.clone());
    let wire = WireTables::new(credential.clone(), [], Arc::new(TestTimer::default()));
    let error = table(Some("sales.Shortcut"))
        .delta_log_schema_compatibility(&sdk, &wire, TableLimits::default(), pending())
        .await
        .unwrap_err();
    assert!(matches!(error.kind, LakehouseErrorKind::InvalidInput));
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(wire.network.requests.lock().unwrap().is_empty());
}

#[test]
fn maintenance_native_payload_and_retention_policy_are_exact() {
    let target = table(Some("sales.eu"));
    assert_eq!(
        LakehouseMaintenanceOptions::optimize()
            .execution_data(&target)
            .unwrap(),
        json!({"tableName":"orders","schemaName":"sales.eu","optimizeSettings":{}})
    );
    assert_eq!(
        LakehouseMaintenanceOptions::Optimize {
            v_order: true,
            z_order_by: vec!["customerId".into(), "region".into()],
        }
        .execution_data(&target)
        .unwrap(),
        json!({"tableName":"orders","schemaName":"sales.eu","optimizeSettings":{
            "vOrder":true,"zOrderBy":["customerId","region"],
        }})
    );
    assert_eq!(
        LakehouseMaintenanceOptions::vacuum()
            .execution_data(&table(None))
            .unwrap(),
        json!({"tableName":"orders","vacuumSettings":{"retentionPeriod":"7:00:00:00"}})
    );
    for hours in [i64::MIN, -1, 0, 167, i64::MAX] {
        assert!(VacuumRetention::from_hours(hours).is_err(), "{hours}");
    }
    for (hours, period) in [
        (168, "7:00:00:00"),
        (169, "7:01:00:00"),
        (192, "8:00:00:00"),
    ] {
        let retention = VacuumRetention::from_hours(hours).unwrap();
        assert_eq!(retention.hours(), hours as u64);
        assert_eq!(retention.retention_period(), period);
    }
    assert_eq!(VacuumRetention::default().hours(), 168);
    assert!(
        LakehouseMaintenanceOptions::Optimize {
            v_order: false,
            z_order_by: vec!["".into()],
        }
        .execution_data(&target)
        .is_err()
    );
}

#[tokio::test]
async fn maintenance_returns_real_handle_and_shared_wait_preserves_exact_table_and_retry_after() {
    let http = ScriptedHttp::new([
        response(
            202,
            "",
            &[
                (
                    "Location",
                    "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/jobs/instances/run",
                ),
                ("Retry-After", "2"),
            ],
        ),
        response(
            200,
            r#"{"id":"run","status":"InProgress"}"#,
            &[("Retry-After", "3")],
        ),
        response(200, r#"{"id":"run","status":"Completed","future":42}"#, &[]),
    ]);
    let timer = Arc::new(TestTimer::default());
    let jobs = support::sdk(http.clone(), timer.clone());
    let target = table(Some("sales.eu"));
    let receipt = target
        .submit_maintenance(&jobs, &LakehouseMaintenanceOptions::optimize(), pending())
        .await
        .unwrap();
    assert_eq!(receipt.table(), &target);
    assert_eq!(receipt.handle().id(), "run");
    assert_eq!(receipt.handle().accepted_response().status, 202);
    assert_eq!(
        http.requests.lock().unwrap().len(),
        1,
        "acceptance does not implicitly wait"
    );
    let mut states = Vec::new();
    let status = receipt
        .wait(
            &jobs,
            WaitOptions::default(),
            |status| {
                states.push(status.state());
                Ok(())
            },
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(status.state(), JobState::Completed);
    assert_eq!(status.as_value()["future"], 42);
    assert_eq!(states, [JobState::InProgress, JobState::Completed]);
    assert_eq!(*timer.waits.lock().unwrap(), [2, 3]);
    let requests = http.requests.lock().unwrap();
    assert_eq!(
        requests[0].url().path(),
        "/v1/workspaces/ws/items/item/jobs/instances"
    );
    assert_eq!(requests[0].url().query(), Some("jobType=TableMaintenance"));
    let Body::Bytes(bytes) = requests[0].body() else {
        panic!("native execution data")
    };
    assert_eq!(
        serde_json::from_slice::<Value>(bytes).unwrap(),
        json!({
            "executionData":{"tableName":"orders","schemaName":"sales.eu","optimizeSettings":{}}
        })
    );
    assert_eq!(
        requests[1].url().path(),
        "/v1/workspaces/ws/items/item/jobs/instances/run"
    );
}

#[tokio::test]
async fn maintenance_unknown_deduped_cancelled_failed_and_observer_failure_keep_real_evidence() {
    for (state, expected) in [
        ("FutureState", JobState::Unknown),
        ("Deduped", JobState::Deduped),
        ("Cancelled", JobState::Cancelled),
        ("Failed", JobState::Failed),
    ] {
        let http = ScriptedHttp::new([
            accepted(),
            response(
                200,
                &format!(
                    r#"{{"id":"run","status":"{state}","failureReason":"private-job-error"}}"#
                ),
                &[],
            ),
        ]);
        let jobs = support::sdk(http.clone(), Arc::new(TestTimer::default()));
        let receipt = table(None)
            .submit_maintenance(&jobs, &LakehouseMaintenanceOptions::vacuum(), pending())
            .await
            .unwrap();
        let result = receipt
            .wait(&jobs, WaitOptions::default(), |_| Ok(()), pending())
            .await;
        if expected == JobState::Failed {
            let error = result.unwrap_err();
            assert_eq!(error.operation_id.as_deref(), Some("run"));
            assert_eq!(
                error.maintenance_receipt.as_ref().unwrap().table(),
                receipt.table()
            );
            assert!(error.mutation_may_have_occurred);
            assert!(!error.to_string().contains("private-job-error"));
            assert!(matches!(error.kind, LakehouseErrorKind::Job(error)
                if matches!(*error, JobError::Failed { ref content } if content["status"] == "Failed")));
        } else {
            let status = result.unwrap();
            assert_eq!(status.state(), expected);
            assert_ne!(status.state(), JobState::Completed);
            assert_eq!(status.status(), state);
        }
        assert_eq!(http.requests.lock().unwrap().len(), 2);
    }
    let http = ScriptedHttp::new([
        accepted(),
        response(200, r#"{"id":"run","status":"Completed"}"#, &[]),
    ]);
    let jobs = support::sdk(http.clone(), Arc::new(TestTimer::default()));
    let receipt = table(None)
        .submit_maintenance(&jobs, &LakehouseMaintenanceOptions::optimize(), pending())
        .await
        .unwrap();
    let error = receipt
        .wait(
            &jobs,
            WaitOptions::default(),
            |_| Err(JobError::Observer),
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.maintenance_receipt.unwrap().handle().id(), "run");
    assert_eq!(http.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn maintenance_timeout_local_cancel_and_explicit_remote_cancel_are_distinct() {
    let http = ScriptedHttp::new([
        accepted(),
        response(202, "", &[]),
        response(200, r#"{"id":"run","status":"InProgress"}"#, &[]),
    ]);
    let jobs = support::sdk(http.clone(), Arc::new(TestTimer::default()));
    let receipt = table(None)
        .submit_maintenance(&jobs, &LakehouseMaintenanceOptions::optimize(), pending())
        .await
        .unwrap();
    let error = receipt
        .wait(
            &jobs,
            WaitOptions {
                timeout: Some(0),
                polling_interval: None,
            },
            |_| Ok(()),
            pending(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error.kind, LakehouseErrorKind::Job(ref error)
        if matches!(**error, JobError::TimedOut { .. })));
    assert_eq!(error.maintenance_receipt.unwrap().handle().id(), "run");
    assert_eq!(
        http.requests.lock().unwrap().len(),
        1,
        "timeout must not cancel"
    );
    let error = receipt
        .wait(&jobs, WaitOptions::default(), |_| Ok(()), ready(()))
        .await
        .unwrap_err();
    assert!(error.mutation_may_have_occurred);
    assert_eq!(
        http.requests.lock().unwrap().len(),
        1,
        "local cancel must not cancel remotely"
    );
    let cancellation = receipt
        .request_cancellation(&jobs, pending())
        .await
        .unwrap();
    assert_eq!(cancellation.accepted_response().status, 202);
    assert_eq!(cancellation.id(), receipt.handle().id());
    let status = receipt.status(&jobs, pending()).await.unwrap();
    assert_eq!(
        status.state(),
        JobState::InProgress,
        "cancel acceptance is not terminal"
    );
    assert_eq!(http.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn maintenance_pre_cancel_invalid_options_and_lost_response_do_not_replay() {
    let http = ScriptedHttp::new([]);
    let jobs = support::sdk(http.clone(), Arc::new(TestTimer::default()));
    let error = table(None)
        .submit_maintenance(&jobs, &LakehouseMaintenanceOptions::optimize(), ready(()))
        .await
        .unwrap_err();
    assert!(!error.mutation_may_have_occurred);
    let invalid = LakehouseMaintenanceOptions::Optimize {
        v_order: true,
        z_order_by: vec!["bad\ncolumn".into()],
    };
    assert!(
        table(None)
            .submit_maintenance(&jobs, &invalid, pending())
            .await
            .is_err()
    );
    assert!(http.requests.lock().unwrap().is_empty());
    http.fail.store(true, Ordering::SeqCst);
    let error = table(None)
        .submit_maintenance(&jobs, &LakehouseMaintenanceOptions::vacuum(), pending())
        .await
        .unwrap_err();
    assert!(error.mutation_may_have_occurred);
    assert!(
        error.maintenance_receipt.is_none(),
        "lost acceptance cannot invent a handle"
    );
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}
