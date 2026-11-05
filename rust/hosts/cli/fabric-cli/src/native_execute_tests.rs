// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
#![deny(unnameable_test_items)]

use super::*;
use arrow_array::{Int64Array, RecordBatch};
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
use std::{collections::VecDeque, sync::Mutex, time::Duration};

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

async fn invoke(wire: Arc<Wire>, timeout: Duration) -> get::Delivery {
    let fabric_cli::parser::Invocation::Execute(command) =
        fabric_cli::parser::parse_args(["fab-rs", "execute", TARGET, "--wait"]).unwrap()
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
    let mut accepted_headers = Headers::new();
    accepted_headers.insert("x-ms-operation-id", OPERATION);
    accepted_headers.insert(
        "location",
        format!("https://msitapi.fabric.microsoft.com/v1/operations/{OPERATION}"),
    );
    accepted_headers.insert("retry-after", "0");
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([
            (StatusCode::Accepted, accepted_headers, Vec::new()),
            (
                StatusCode::Ok,
                Headers::new(),
                br#"{"status":"Succeeded"}"#.to_vec(),
            ),
            result(),
        ])),
        requests: Mutex::new(Vec::new()),
    });
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
