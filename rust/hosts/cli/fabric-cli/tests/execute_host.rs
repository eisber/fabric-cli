// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline execution approval, native operation, and result publication contracts.
#![deny(unnameable_test_items)]

use arrow_array::{Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_cli::{
    execute::{
        ExecuteCommand, ResultFormat,
        host::{
            ExecutionApproval, FileDestination, PreparationError, PreparedExecution, read_input,
        },
    },
    parser::{Invocation, parse_args},
};
use fabric_sdk::{Audience, CredentialSelection};
use fabric_sdk_operations::{
    OperationErrorKind, OperationExecutionError, OperationExecutor, OperationLimits,
    OperationRequest, OperationResponse, OperationSession,
};
use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    io,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Dataflow/queries/Orders";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";

fn command(format: &str) -> ExecuteCommand {
    let Invocation::Execute(command) = parse_args([
        "fab-rs",
        "execute",
        TARGET,
        "--wait",
        "--format",
        format,
        "-o",
        "result.data",
    ])
    .unwrap() else {
        panic!("execution invocation")
    };
    command
}

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
            "synthetic-execute-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Approval(bool);

impl ExecutionApproval for Approval {
    fn approve<'a>(
        &'a mut self,
        prepared: &'a PreparedExecution,
    ) -> Pin<Box<dyn Future<Output = io::Result<bool>> + Send + 'a>> {
        assert!(prepared.approval_question().contains("Orders"));
        Box::pin(async { Ok(self.0) })
    }
}

struct Wire {
    replies: Mutex<VecDeque<OperationResponse>>,
    requests: Mutex<Vec<(String, Vec<u8>)>>,
    waiting: bool,
}

impl Wire {
    fn new(replies: Vec<OperationResponse>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
            waiting: false,
        }
    }
}

impl OperationExecutor for &Wire {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap()
                .push((request.url().path().into(), request.body().to_vec()));
            Ok(self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("query must not replay"))
        })
    }

    fn wait(
        &self,
        _: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(async move {
            if self.waiting {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
}

fn selection() -> CredentialSelection {
    CredentialSelection::new(
        "offline",
        "11111111-1111-1111-1111-111111111111",
        Audience::Fabric,
    )
    .unwrap()
}

fn native_result() -> OperationResponse {
    let batch = RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, true),
            Field::new("label", DataType::Utf8, true),
        ])),
        vec![
            Arc::new(Int64Array::from(vec![Some(i64::MAX), None, Some(0)])),
            Arc::new(StringArray::from(vec![
                Some("a,\"雪\"\nline"),
                Some(""),
                None,
            ])),
        ],
    )
    .unwrap();
    response_from_batch(batch)
}

fn response_from_batch(batch: RecordBatch) -> OperationResponse {
    let mut body = Vec::new();
    {
        let mut writer =
            arrow_ipc::writer::StreamWriter::try_new(&mut body, batch.schema().as_ref()).unwrap();
        writer.write(&batch).unwrap();
        writer.finish().unwrap();
    }
    OperationResponse {
        status: 200,
        body,
        headers: BTreeMap::from([(
            "content-type".into(),
            "application/vnd.apache.arrow.stream".into(),
        )]),
    }
}

#[tokio::test]
async fn native_pq_diagnostic_column_fails_before_success_in_every_output_format() {
    let batch = RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("value", DataType::Int64, true),
            Field::new("PQ Arrow Metadata", DataType::Utf8, true),
        ])),
        vec![
            Arc::new(Int64Array::from(vec![Some(7)])),
            Arc::new(StringArray::from(vec![Some(
                "{\"message\":\"synthetic mashup failure\"}",
            )])),
        ],
    )
    .unwrap();
    for format in ["json", "csv", "parquet"] {
        let response = response_from_batch(batch.clone());
        let original = response.body.clone();
        let wire = Wire::new(vec![response]);
        let credential = Credential::default();
        let selection = selection();
        let mut session = OperationSession::new(
            &credential,
            &selection,
            &wire,
            OperationLimits::default(),
            std::future::pending(),
        );
        let approved = PreparedExecution::new(&command(format), None)
            .unwrap()
            .approve(&mut Approval(true))
            .await
            .unwrap();
        let failure = approved.execute(&mut session).await.err().unwrap();
        let fabric_cli::execute::host::ExecutionFailure::Result {
            error,
            response,
            evidence,
        } = failure
        else {
            panic!("native diagnostic must not become tabular success");
        };
        assert!(matches!(
            error,
            fabric_sdk_tabular::TabularError::Metadata { .. }
        ));
        assert_eq!(response.body, original);
        assert!(evidence.mutation_may_have_occurred);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

fn accepted() -> OperationResponse {
    OperationResponse {
        status: 202,
        body: Vec::new(),
        headers: BTreeMap::from([
            ("x-ms-operation-id".into(), OPERATION.into()),
            (
                "location".into(),
                format!("https://api.fabric.microsoft.com/v1/operations/{OPERATION}"),
            ),
            ("retry-after".into(), "0".into()),
        ]),
    }
}

#[tokio::test]
async fn invalid_input_and_denied_approval_never_touch_credentials_or_transport() {
    let credential = Credential::default();
    for input in [
        b"".as_slice(),
        b"null",
        b"[]",
        b"{",
        b"{\"unknown\":true}",
        b"{\"arrowVersion\":0}",
        b"{\"arrowVersion\":3}",
        b"{\"arrowVersion\":\"1\"}",
        b"{\"arrowVersion\":null}",
        b"{\"customMashupDocument\":null}",
        b"{\"arrowVersion\":1,\"arrowVersion\":2}",
    ] {
        assert!(PreparedExecution::new(&command("json"), Some(input)).is_err());
    }
    let prepared = PreparedExecution::new(&command("json"), Some(b"{\"arrowVersion\":2}")).unwrap();
    assert!(matches!(
        prepared.approve(&mut Approval(false)).await,
        Err(PreparationError::ApprovalDenied)
    ));
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(read_input(&mut io::Cursor::new(Vec::<u8>::new())).is_err());
    assert!(read_input(&mut io::Cursor::new(vec![b' '; 1024 * 1024 + 1])).is_err());
}

#[tokio::test]
async fn approved_native_200_preserves_types_and_serializes_all_three_real_formats() {
    for (name, format) in [
        ("csv", ResultFormat::Csv),
        ("json", ResultFormat::Json),
        ("parquet", ResultFormat::Parquet),
    ] {
        let command = command(name);
        assert_eq!(command.format, format);
        let approved = PreparedExecution::new(&command, Some(b"{\"arrowVersion\":1}"))
            .unwrap()
            .approve(&mut Approval(true))
            .await
            .unwrap();
        let wire = Wire::new(vec![native_result()]);
        let credential = Credential::default();
        let selection = selection();
        let mut session = OperationSession::new(
            &credential,
            &selection,
            &wire,
            OperationLimits::default(),
            std::future::pending(),
        );
        let result = approved.execute(&mut session).await.unwrap();
        assert!(!result.bytes.is_empty());
        match format {
            ResultFormat::Json => {
                let json: serde_json::Value = serde_json::from_slice(&result.bytes).unwrap();
                assert_eq!(json["schema"][0]["name"], "id");
                assert_eq!(json["schema"][1]["name"], "label");
                assert_eq!(
                    json["rows"],
                    serde_json::json!([
                        [i64::MAX.to_string(), "a,\"雪\"\nline"],
                        [null, ""],
                        ["0", null],
                    ])
                );
            }
            ResultFormat::Csv => {
                // Parse actual CSV through Arrow's independent CSV reader.
                let schema = Arc::new(Schema::new(vec![
                    Field::new("id", DataType::Utf8, true),
                    Field::new("label", DataType::Utf8, true),
                ]));
                let mut reader = arrow_csv::ReaderBuilder::new(schema)
                    .with_header(true)
                    .build(io::Cursor::new(result.bytes))
                    .unwrap();
                let batch = reader.next().unwrap().unwrap();
                assert_eq!(batch.num_rows(), 3);
                let label = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                assert_eq!(label.value(0), "a,\"雪\"\nline");
            }
            ResultFormat::Parquet => {
                let mut reader =
                    parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
                        azure_core::Bytes::from(result.bytes),
                    )
                    .unwrap()
                    .build()
                    .unwrap();
                let batch = reader.next().unwrap().unwrap();
                assert_eq!(batch.num_rows(), 3);
                let id = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap();
                assert_eq!(id.value(0), i64::MAX);
                assert!(id.is_null(1));
                let label = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                assert_eq!(label.value(0), "a,\"雪\"\nline");
                assert_eq!(label.value(1), "");
                assert!(label.is_null(2));
            }
        }
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].0.ends_with("/executeQuery"));
        let body: serde_json::Value = serde_json::from_slice(&requests[0].1).unwrap();
        assert_eq!(body["queryName"], "Orders");
    }
}

#[tokio::test]
async fn empty_native_results_keep_the_declared_schema_in_all_formats() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, true)]));
    for format in ["json", "csv", "parquet"] {
        let response = response_from_batch(RecordBatch::new_empty(schema.clone()));
        let wire = Wire::new(vec![response]);
        let credential = Credential::default();
        let selection = selection();
        let mut session = OperationSession::new(
            &credential,
            &selection,
            &wire,
            OperationLimits::default(),
            std::future::pending(),
        );
        let approved = PreparedExecution::new(&command(format), None)
            .unwrap()
            .approve(&mut Approval(true))
            .await
            .unwrap();
        let result = approved.execute(&mut session).await.unwrap();
        match format {
            "json" => {
                let json: serde_json::Value = serde_json::from_slice(&result.bytes).unwrap();
                assert_eq!(json["schema"][0]["name"], "id");
                assert_eq!(json["rows"], serde_json::json!([]));
            }
            "csv" => assert_eq!(result.bytes, b"\"id\"\r\n"),
            _ => {
                let builder =
                    parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
                        azure_core::Bytes::from(result.bytes),
                    )
                    .unwrap();
                assert_eq!(builder.schema().field(0).name(), "id");
                assert_eq!(builder.metadata().file_metadata().num_rows(), 0);
            }
        }
    }
}

#[tokio::test]
async fn actual_202_state_and_result_preserve_operation_handle_without_resubmission() {
    let wire = Wire::new(vec![
        accepted(),
        OperationResponse {
            status: 200,
            body: br#"{"status":"Succeeded"}"#.to_vec(),
            headers: BTreeMap::new(),
        },
        native_result(),
    ]);
    let credential = Credential::default();
    let selection = selection();
    let mut session = OperationSession::new(
        &credential,
        &selection,
        &wire,
        OperationLimits::default(),
        std::future::pending(),
    );
    let approved = PreparedExecution::new(&command("json"), None)
        .unwrap()
        .approve(&mut Approval(true))
        .await
        .unwrap();
    let result = approved.execute(&mut session).await.unwrap();
    assert_eq!(result.evidence.operation_id.as_deref(), Some(OPERATION));
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].0.ends_with(OPERATION));
    assert!(requests[2].0.ends_with("/result"));
}

#[tokio::test]
async fn invalid_media_and_oversized_results_never_become_success_or_retry() {
    let mut invalid_media = native_result();
    invalid_media.headers = BTreeMap::from([("content-type".into(), "application/json".into())]);
    for (response, max_rows) in [(invalid_media, 100_000), (native_result(), 1)] {
        let mut command = command("json");
        command.max_rows = max_rows;
        let approved = PreparedExecution::new(&command, None)
            .unwrap()
            .approve(&mut Approval(true))
            .await
            .unwrap();
        let wire = Wire::new(vec![response]);
        let credential = Credential::default();
        let selection = selection();
        let mut session = OperationSession::new(
            &credential,
            &selection,
            &wire,
            OperationLimits::default(),
            std::future::pending(),
        );
        assert!(approved.execute(&mut session).await.is_err());
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        assert!(
            session
                .evidence(OperationErrorKind::Limit)
                .mutation_may_have_occurred
        );
    }
}

#[tokio::test]
async fn service_permission_failure_retains_classification_and_does_not_retry_submission() {
    let wire = Wire::new(vec![OperationResponse {
        status: 403,
        body: br#"{"errorCode":"InsufficientPrivileges","message":"Synthetic permission denial"}"#
            .to_vec(),
        headers: BTreeMap::new(),
    }]);
    let credential = Credential::default();
    let selection = selection();
    let mut session = OperationSession::new(
        &credential,
        &selection,
        &wire,
        OperationLimits::default(),
        std::future::pending(),
    );
    let approved = PreparedExecution::new(&command("json"), None)
        .unwrap()
        .approve(&mut Approval(true))
        .await
        .unwrap();
    let error = approved.execute(&mut session).await.err().unwrap();
    let fabric_cli::execute::host::ExecutionFailure::Query(error) = error else {
        panic!("original endpoint error");
    };
    assert!(matches!(
        error.operation_error().unwrap().kind,
        OperationErrorKind::Http { status: 403, .. }
    ));
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn local_cancellation_after_acceptance_preserves_original_operation_and_never_replays() {
    let mut wire = Wire::new(vec![accepted()]);
    wire.waiting = true;
    let credential = Credential::default();
    let selection = selection();
    let (signal, receiver) = tokio::sync::oneshot::channel::<()>();
    let mut session = OperationSession::new(
        &credential,
        &selection,
        &wire,
        OperationLimits::default(),
        async {
            let _ = receiver.await;
        },
    );
    let approved = PreparedExecution::new(&command("json"), None)
        .unwrap()
        .approve(&mut Approval(true))
        .await
        .unwrap();
    let query = approved.execute(&mut session);
    let interrupt = async {
        tokio::task::yield_now().await;
        signal.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(query, interrupt);
    let error = result.err().expect("cancellation is not success");
    assert!(matches!(
        error,
        fabric_cli::execute::host::ExecutionFailure::Query(_)
    ));
    assert_eq!(session.operation_id(), Some(OPERATION));
    assert!(
        session
            .evidence(OperationErrorKind::Cancelled)
            .mutation_may_have_occurred
    );
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn total_deadline_after_acceptance_preserves_the_original_handle() {
    let mut wire = Wire::new(vec![accepted()]);
    wire.waiting = true;
    let credential = Credential::default();
    let selection = selection();
    let mut session = OperationSession::new(
        &credential,
        &selection,
        &wire,
        OperationLimits::default(),
        tokio::time::sleep(Duration::from_millis(5)),
    );
    let approved = PreparedExecution::new(&command("json"), None)
        .unwrap()
        .approve(&mut Approval(true))
        .await
        .unwrap();
    let error = approved.execute(&mut session).await.err().unwrap();
    let fabric_cli::execute::host::ExecutionFailure::Query(error) = error else {
        panic!("deadline retains native error");
    };
    let evidence = error.operation_error().unwrap();
    assert_eq!(evidence.operation_id.as_deref(), Some(OPERATION));
    assert!(evidence.mutation_may_have_occurred);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

fn output_path() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        ".execute-output-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst),
    ))
}

#[test]
fn preflight_existing_destination_and_racing_publication_never_overwrite() {
    let path = output_path();
    let destination = FileDestination::preflight(&path).unwrap();
    std::fs::write(&path, b"other owner's result").unwrap();
    assert!(FileDestination::preflight(&path).is_err());
    let error = destination.publish(b"new result").unwrap_err();
    assert!(!error.partial);
    assert_eq!(std::fs::read(&path).unwrap(), b"other owner's result");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn publication_to_a_missing_parent_fails_without_a_successful_file() {
    let path = output_path();
    std::fs::create_dir(&path).unwrap();
    let file = path.join("result.json");
    let destination = FileDestination::preflight(&file).unwrap();
    std::fs::remove_dir(&path).unwrap();
    assert!(!destination.publish(b"complete").unwrap_err().partial);
    assert!(!file.exists());
}

#[tokio::test]
async fn publication_failure_after_completion_cannot_trigger_another_submission() {
    let wire = Wire::new(vec![native_result()]);
    let credential = Credential::default();
    let selection = selection();
    let mut session = OperationSession::new(
        &credential,
        &selection,
        &wire,
        OperationLimits::default(),
        std::future::pending(),
    );
    let approved = PreparedExecution::new(&command("json"), None)
        .unwrap()
        .approve(&mut Approval(true))
        .await
        .unwrap();
    let path = output_path();
    let destination = FileDestination::preflight(&path).unwrap();
    let result = approved.execute(&mut session).await.unwrap();
    std::fs::write(&path, b"racing owner").unwrap();
    assert!(destination.publish(&result.bytes).is_err());
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
    assert!(result.evidence.mutation_may_have_occurred);
    assert_eq!(std::fs::read(&path).unwrap(), b"racing owner");
    std::fs::remove_file(path).unwrap();
}
