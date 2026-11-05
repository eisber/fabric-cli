// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
#![deny(unnameable_test_items)]

use super::*;
use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_sdk::{Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin};
use fabric_sdk_http::ExecutionOptions;
use fabric_sdk_transport::TokioTimer;
use std::{
    collections::VecDeque,
    sync::{Mutex, mpsc},
    time::Duration,
};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Dataflow/queries/Orders";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";

#[derive(Debug)]
struct Credential;

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        Ok(AccessToken::new(
            "synthetic-query-token",
            OffsetDateTime::UNIX_EPOCH,
        ))
    }
}

struct Approved;

impl ExecutionApproval for Approved {
    fn approve<'a>(
        &'a mut self,
        _: &'a PreparedExecution,
    ) -> Pin<Box<dyn Future<Output = io::Result<bool>> + Send + 'a>> {
        Box::pin(async { Ok(true) })
    }
}

#[derive(Debug)]
struct Wire {
    replies: Mutex<VecDeque<(StatusCode, Headers, Vec<u8>)>>,
    requests: Mutex<Vec<Request>>,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        let (status, headers, body) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("query may not replay after output failure");
        Ok(AsyncRawResponse::from_bytes(status, headers, body))
    }
}

fn result() -> (StatusCode, Headers, Vec<u8>) {
    let batch = RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new(
            "value",
            DataType::Int64,
            true,
        )])),
        vec![Arc::new(Int64Array::from(vec![Some(7), None]))],
    )
    .unwrap();
    let mut body = Vec::new();
    {
        let mut writer =
            arrow_ipc::writer::StreamWriter::try_new(&mut body, batch.schema().as_ref()).unwrap();
        writer.write(&batch).unwrap();
        writer.finish().unwrap();
    }
    let mut headers = Headers::new();
    headers.insert("content-type", "application/vnd.apache.arrow.stream");
    (StatusCode::Ok, headers, body)
}

fn accepted_wire() -> Arc<Wire> {
    let mut headers = Headers::new();
    headers.insert("x-ms-operation-id", OPERATION);
    headers.insert(
        "location",
        format!("https://msitapi.fabric.microsoft.com/v1/operations/{OPERATION}"),
    );
    headers.insert("retry-after", "0");
    Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([
            (StatusCode::Accepted, headers, Vec::new()),
            (
                StatusCode::Ok,
                Headers::new(),
                br#"{"status":"Succeeded"}"#.to_vec(),
            ),
            result(),
        ])),
        requests: Mutex::new(Vec::new()),
    })
}

async fn invoke(wire: Arc<Wire>, timeout: Duration) -> get::Delivery {
    invoke_format(wire, timeout, "json").await
}

async fn invoke_format(wire: Arc<Wire>, timeout: Duration, format: &str) -> get::Delivery {
    let fabric_cli::parser::Invocation::Execute(command) =
        fabric_cli::parser::parse_args(["fab-rs", "execute", TARGET, "--wait", "--format", format])
            .unwrap()
    else {
        panic!("root execute")
    };
    let approved = PreparedExecution::new(&command, None)
        .unwrap()
        .approve(&mut Approved)
        .await
        .unwrap();
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "msitapi.fabric.microsoft.com").unwrap();
    let context = RequestContext::with_endpoint(
        Arc::new(Credential),
        CredentialSelection::new(
            "offline",
            "11111111-1111-1111-1111-111111111111",
            Audience::Fabric,
        )
        .unwrap(),
        endpoint.clone(),
    );
    let sdk = Client::with_context(context);
    let executor = HttpExecutor::new(wire, Arc::new(TokioTimer), ExecutionOptions::default())
        .unwrap()
        .with_endpoint(endpoint);
    let mut state = BatchState::capture();
    super::super::execute_authenticated_injected(
        NativeCommand::Execution(&approved, Instant::now() + timeout),
        sdk,
        executor,
        "/",
        fabric_cli_context::HomeContext::Unspecified,
        super::super::settings::NativeSettings::select(None).unwrap(),
        &mut state,
        false,
        None,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn root_authenticated_composition_uses_shared_http_and_clean_data_stdout() {
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([result()])),
        requests: Mutex::new(Vec::new()),
    });
    let delivery = invoke(wire.clone(), Duration::from_secs(30)).await;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        delivery.write_to(&mut stdout, &mut stderr).unwrap(),
        CommandOutcome::Returned(0)
    );
    let json: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert!(json.is_object());
    assert!(stderr.is_empty());
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method(), Method::Post);
    assert_eq!(
        requests[0].url().host_str(),
        Some("msitapi.fabric.microsoft.com")
    );
    assert!(requests[0].url().path().ends_with("/executeQuery"));
    assert!(
        requests[0]
            .headers()
            .get_str(&"accept".into())
            .unwrap()
            .contains("arrow")
    );
}

struct BrokenOutput;

impl Write for BrokenOutput {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn output_failure_retains_evidence_and_never_resubmits_query() {
    let wire = accepted_wire();
    let delivery = invoke(wire.clone(), Duration::from_secs(30)).await;
    let error = delivery
        .write_to(&mut BrokenOutput, &mut Vec::new())
        .unwrap_err();
    assert!(error.to_string().contains("Query will not be replayed"));
    assert!(error.to_string().contains(OPERATION));
    let original = error
        .get_ref()
        .unwrap()
        .downcast_ref::<WriteFailure>()
        .unwrap();
    assert_eq!(
        original.evidence.as_ref().unwrap().operation_id.as_deref(),
        Some(OPERATION)
    );
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == Method::Post)
            .count(),
        1
    );
}

#[tokio::test]
async fn elapsed_total_deadline_has_no_success_data_and_never_dispatches_query() {
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::new()),
        requests: Mutex::new(Vec::new()),
    });
    let delivery = invoke(wire.clone(), Duration::ZERO).await;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        delivery.write_to(&mut stdout, &mut stderr).unwrap(),
        CommandOutcome::Stop(1)
    );
    assert!(stdout.is_empty());
    let diagnostic: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(diagnostic["error"]["code"], "timed_out");
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[test]
fn native_unknown_metadata_evidence_is_retained_on_stderr_not_data_stdout() {
    let error = fabric_sdk_tabular::TabularError::Metadata {
        evidence: vec![fabric_sdk_tabular::MetadataEvidence {
            location: fabric_sdk_tabular::MetadataLocation::Field(1),
            key: "PQ Arrow Metadata".into(),
            value: "{\"message\":\"synthetic failure\"}".into(),
            classification: fabric_sdk_tabular::MetadataClassification::Unknown,
        }],
    };
    let delivery = Delivery::failed("execution_failed", "Native diagnostic present.", None, 1)
        .retain_tabular_evidence(&error);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        delivery.write_to(&mut stdout, &mut stderr).unwrap(),
        CommandOutcome::Returned(1)
    );
    assert!(stdout.is_empty());
    let diagnostic: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(
        diagnostic["metadataEvidence"][0]["classification"],
        "Unknown"
    );
    assert_eq!(
        diagnostic["metadataEvidence"][0]["value"],
        "{\"message\":\"synthetic failure\"}"
    );
}

struct Release(Option<mpsc::Sender<()>>);

impl Release {
    fn release(mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

struct SilentLockedPrompt<'a> {
    _lock: io::StderrLock<'a>,
}

impl Write for SilentLockedPrompt<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct BlockedInput {
    entered: Option<mpsc::Sender<()>>,
    release: Option<mpsc::Receiver<()>>,
    bytes: io::Cursor<Vec<u8>>,
}

impl io::Read for BlockedInput {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = {
            let input = io::BufRead::fill_buf(self)?;
            let count = input.len().min(bytes.len());
            bytes[..count].copy_from_slice(&input[..count]);
            count
        };
        io::BufRead::consume(self, count);
        Ok(count)
    }
}

impl io::BufRead for BlockedInput {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if let Some(receiver) = self.release.take() {
            if let Some(sender) = self.entered.take() {
                let _ = sender.send(());
            }
            receiver
                .recv_timeout(Duration::from_secs(30))
                .map_err(io::Error::other)?;
        }
        io::BufRead::fill_buf(&mut self.bytes)
    }

    fn consume(&mut self, count: usize) {
        io::BufRead::consume(&mut self.bytes, count);
    }
}

#[test]
fn abandoned_approval_releases_stderr_before_input_and_after_runtime_shutdown() {
    let (entered, started) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let release = Release(Some(release));
    let (finished, completion) = mpsc::channel();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let worker = runtime.spawn_blocking(move || {
        let mut input = BlockedInput {
            entered: Some(entered),
            release: Some(gate),
            bytes: io::Cursor::new(b"no\n".to_vec()),
        };
        let stderr = io::stderr();
        let answer = read_approval_confirmation(
            &mut input,
            SilentLockedPrompt {
                _lock: stderr.lock(),
            },
            "offline approval",
        );
        let _ = finished.send(());
        answer
    });
    let entered = started.recv_timeout(Duration::from_secs(5)).is_ok();
    let result = runtime.block_on(before_submission(
        async {
            worker
                .await
                .map_err(|_| failure("input_failed", "Synthetic worker failed.", 1))?
                .map_err(|_| failure("input_failed", "Synthetic input failed.", 1))
        },
        Instant::now() + Duration::from_millis(25),
    ));
    let result = super::super::finish_runtime(runtime, result);
    let (observed, observer) = mpsc::channel();
    let observer_thread = std::thread::spawn(move || {
        let stderr = io::stderr();
        let _lock = stderr.lock();
        let _ = observed.send(());
    });
    let stderr_available = observer.recv_timeout(Duration::from_secs(2)).is_ok();
    release.release();
    let worker_finished = completion.recv_timeout(Duration::from_secs(5)).is_ok();
    observer_thread.join().unwrap();
    assert!(entered && worker_finished);
    assert_eq!(result.unwrap_err().0, "timed_out");
    assert!(
        stderr_available,
        "an abandoned approval must not block error reporting"
    );
}

struct BlockedWriter {
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    release: mpsc::Receiver<()>,
    finished: Option<mpsc::Sender<()>>,
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for BlockedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let prefix = bytes.len().min(3);
        self.bytes
            .lock()
            .unwrap()
            .extend_from_slice(&bytes[..prefix]);
        if let Some(entered) = self.entered.take() {
            let _ = entered.send(());
            self.release
                .recv_timeout(Duration::from_secs(30))
                .map_err(io::Error::other)?;
        }
        self.bytes
            .lock()
            .unwrap()
            .extend_from_slice(&bytes[prefix..]);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for BlockedWriter {
    fn drop(&mut self) {
        if let Some(finished) = self.finished.take() {
            let _ = finished.send(());
        }
    }
}

struct OutputGate {
    writer: BlockedWriter,
    started: tokio::sync::oneshot::Receiver<()>,
    release: Release,
    finished: mpsc::Receiver<()>,
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl OutputGate {
    fn new() -> Self {
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, receiver) = mpsc::channel();
        let (finished, completion) = mpsc::channel();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        Self {
            writer: BlockedWriter {
                entered: Some(entered),
                release: receiver,
                finished: Some(finished),
                bytes: Arc::clone(&bytes),
            },
            started,
            release: Release(Some(release)),
            finished: completion,
            bytes,
        }
    }
}

fn assert_abandoned_output(publication: Publication, code: &str, exit: u8) {
    let Publication::Diagnostic(delivery) = publication else {
        panic!("blocked output must never be reported complete");
    };
    assert_eq!(delivery.outcome, CommandOutcome::Stop(exit));
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        delivery.write_to(&mut stdout, &mut stderr).unwrap(),
        CommandOutcome::Stop(exit)
    );
    assert!(stdout.is_empty());
    let diagnostic: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(diagnostic["error"]["code"], code);
    assert_eq!(diagnostic["operation"]["id"], OPERATION);
    assert_eq!(diagnostic["operation"]["mutationMayHaveOccurred"], true);
    assert_eq!(
        diagnostic["operation"]["remoteCancellationRequested"],
        false
    );
    assert!(
        diagnostic["error"]["message"]
            .as_str()
            .unwrap()
            .contains("may be partial")
    );
}

#[tokio::test]
async fn blocked_stdout_deadline_stops_host_with_original_operation_and_partial_evidence() {
    let wire = accepted_wire();
    let get::Delivery::Execution(delivery) =
        invoke(Arc::clone(&wire), Duration::from_secs(30)).await
    else {
        panic!("execution delivery");
    };
    let OutputGate {
        writer,
        started,
        release,
        finished,
        bytes,
    } = OutputGate::new();
    let (publication, entered) = tokio::join!(
        delivery.publish_stdout(
            writer,
            Instant::now() + Duration::from_millis(500),
            std::future::pending(),
        ),
        started,
    );
    let prefix = bytes.lock().unwrap().clone();
    release.release();
    let completed = finished.recv_timeout(Duration::from_secs(5)).is_ok();
    assert!(entered.is_ok() && completed);
    assert_eq!(prefix.len(), 3);
    assert_abandoned_output(publication, "timed_out", 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn blocked_stdout_cancellation_stops_shell_instead_of_reusing_stdout() {
    let wire = accepted_wire();
    let get::Delivery::Execution(delivery) =
        invoke(Arc::clone(&wire), Duration::from_secs(30)).await
    else {
        panic!("execution delivery");
    };
    let OutputGate {
        writer,
        started,
        release,
        finished,
        bytes,
    } = OutputGate::new();
    let publication = delivery
        .publish_stdout(
            writer,
            Instant::now() + Duration::from_secs(30),
            async move {
                started.await.unwrap();
                failure("cancelled", "Synthetic local cancellation.", 2)
            },
        )
        .await;
    let prefix = bytes.lock().unwrap().clone();
    release.release();
    let completed = finished.recv_timeout(Duration::from_secs(5)).is_ok();
    assert!(completed);
    assert_eq!(prefix.len(), 3);
    assert_abandoned_output(publication, "cancelled", 2);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn expired_output_deadline_never_starts_an_owned_writer() {
    let wire = accepted_wire();
    let get::Delivery::Execution(delivery) =
        invoke(Arc::clone(&wire), Duration::from_secs(30)).await
    else {
        panic!("execution delivery");
    };
    let OutputGate {
        writer,
        mut started,
        release,
        finished,
        bytes,
    } = OutputGate::new();
    let publication = delivery
        .publish_stdout(writer, Instant::now(), std::future::pending())
        .await;
    release.release();
    assert!(finished.recv_timeout(Duration::from_secs(5)).is_ok());
    assert!(started.try_recv().is_err());
    assert!(bytes.lock().unwrap().is_empty());
    assert_abandoned_output(publication, "timed_out", 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

struct CapturedOutput {
    bytes: Arc<Mutex<Vec<u8>>>,
    flushed: Arc<AtomicU8>,
    fail_flush: bool,
}

impl Write for CapturedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushed.store(1, Ordering::SeqCst);
        if self.fail_flush {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn bounded_stdout_requires_a_successful_flush_without_replaying_the_query() {
    for fail_flush in [false, true] {
        let wire = accepted_wire();
        let get::Delivery::Execution(delivery) =
            invoke(Arc::clone(&wire), Duration::from_secs(30)).await
        else {
            panic!("execution delivery");
        };
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let flushed = Arc::new(AtomicU8::new(0));
        let publication = delivery
            .publish_stdout(
                CapturedOutput {
                    bytes: Arc::clone(&bytes),
                    flushed: Arc::clone(&flushed),
                    fail_flush,
                },
                Instant::now() + Duration::from_secs(30),
                std::future::pending(),
            )
            .await;
        if fail_flush {
            assert_abandoned_output(publication, "output_failed", 1);
        } else {
            assert!(matches!(
                publication,
                Publication::Complete(CommandOutcome::Returned(0))
            ));
        }
        assert_eq!(flushed.load(Ordering::SeqCst), 1);
        let json: serde_json::Value = serde_json::from_slice(&bytes.lock().unwrap()).unwrap();
        assert!(json.is_object());
        assert_eq!(wire.requests.lock().unwrap().len(), 3);
    }
}

#[test]
fn windows_console_policy_requires_a_file_or_redirect_before_host_effects() {
    for format in ["json", "csv"] {
        for output in [None, Some("-"), Some("unicode-result.data")] {
            let mut args = vec!["fab-rs", "execute", TARGET, "--wait", "--format", format];
            if let Some(output) = output {
                args.extend(["--output", output]);
            }
            let fabric_cli::parser::Invocation::Execute(command) =
                fabric_cli::parser::parse_args(args).unwrap()
            else {
                panic!("execution command");
            };
            if command.output.is_none() {
                let error = preflight_native_output(&command, true).unwrap_err();
                assert_eq!(error.0, "UnsupportedCapability");
                assert_eq!(error.2, 1);
                assert!(error.1.contains("--output FILE"));
                assert!(error.1.contains("redirect stdout"));
                assert!(error.1.contains("No query was submitted"));
            } else {
                preflight_native_output(&command, true).unwrap();
            }
            // A redirected Windows stream or a non-Windows UTF-8 terminal
            // does not acquire the attached-console restriction.
            preflight_native_output(&command, false).unwrap();
        }
    }
}

fn unicode_result() -> (StatusCode, Headers, Vec<u8>) {
    let batch = RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new("text", DataType::Utf8, false)])),
        vec![Arc::new(StringArray::from(vec!["\u{e9}", "\u{96ea}"]))],
    )
    .unwrap();
    let mut body = Vec::new();
    {
        let mut writer =
            arrow_ipc::writer::StreamWriter::try_new(&mut body, batch.schema().as_ref()).unwrap();
        writer.write(&batch).unwrap();
        writer.finish().unwrap();
    }
    let mut headers = Headers::new();
    headers.insert("content-type", "application/vnd.apache.arrow.stream");
    (StatusCode::Ok, headers, body)
}

#[tokio::test]
async fn redirected_stdout_and_files_preserve_exact_csv_json_unicode_bytes() {
    for format in ["json", "csv"] {
        let wire = Arc::new(Wire {
            replies: Mutex::new(VecDeque::from([unicode_result()])),
            requests: Mutex::new(Vec::new()),
        });
        let get::Delivery::Execution(delivery) =
            invoke_format(Arc::clone(&wire), Duration::from_secs(30), format).await
        else {
            panic!("execution delivery");
        };
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let flushed = Arc::new(AtomicU8::new(0));
        let publication = delivery
            .publish_stdout(
                CapturedOutput {
                    bytes: Arc::clone(&bytes),
                    flushed: Arc::clone(&flushed),
                    fail_flush: false,
                },
                Instant::now() + Duration::from_secs(30),
                std::future::pending(),
            )
            .await;
        assert!(matches!(
            publication,
            Publication::Complete(CommandOutcome::Returned(0))
        ));
        let data = bytes.lock().unwrap().clone();
        assert!(data.windows(2).any(|bytes| bytes == [0xc3, 0xa9]));
        assert!(data.windows(3).any(|bytes| bytes == [0xe9, 0x9b, 0xaa]));
        if format == "csv" {
            assert_eq!(
                data,
                "\"text\"\r\n\"\u{e9}\"\r\n\"\u{96ea}\"\r\n".as_bytes()
            );
        } else {
            let json: serde_json::Value = serde_json::from_slice(&data).unwrap();
            assert_eq!(json["rows"], serde_json::json!([["\u{e9}"], ["\u{96ea}"]]));
        }
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            ".execute-native-unicode-{}-{format}",
            std::process::id()
        ));
        let destination = FileDestination::preflight(&path).unwrap();
        destination.publish(&data).unwrap();
        let published = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(published, data);
        assert_eq!(flushed.load(Ordering::SeqCst), 1);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}
