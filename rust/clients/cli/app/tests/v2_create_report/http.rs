// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual shared HTTP-executor and header-before-body-failure contracts.

use super::*;
use azure_core::http::{
    AsyncRawResponse, HttpClient, Method, Request, StatusCode, headers::Headers,
};
use fabric_sdk_http::{
    ExecutionFuture, ExecutionOptions, ExecutionTimer, ExecutionTrace, HttpExecutor,
};

enum Reply {
    Json(StatusCode, Vec<u8>),
    Accepted(Vec<u8>),
    BrokenAcceptedBody,
}

fn acceptance_headers() -> Headers {
    let mut headers = Headers::new();
    headers.insert("x-ms-operation-id", OP);
    headers.insert(
        "location",
        format!("https://api.fabric.microsoft.com/v1/operations/{OP}"),
    );
    headers.insert("retry-after", "0");
    headers
}

struct HttpWire {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Request>>,
}

impl std::fmt::Debug for HttpWire {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HttpWire(<offline>)")
    }
}

#[async_trait]
impl HttpClient for HttpWire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        match self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("no HTTP mutation replay")
        {
            Reply::Json(status, body) => {
                Ok(AsyncRawResponse::from_bytes(status, Headers::new(), body))
            }
            Reply::Accepted(body) => Ok(AsyncRawResponse::from_bytes(
                StatusCode::Accepted,
                acceptance_headers(),
                body,
            )),
            Reply::BrokenAcceptedBody => {
                let body = futures_util::stream::once(async {
                    Err(azure_core::Error::with_message(
                        azure_core::error::ErrorKind::Io,
                        "offline-private-stream-failure",
                    ))
                });
                Ok(AsyncRawResponse::new(
                    StatusCode::Accepted,
                    acceptance_headers(),
                    Box::pin(body),
                ))
            }
        }
    }
}

#[derive(Debug)]
struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(tokio::time::sleep(delay))
    }
}

#[tokio::test]
async fn real_http_executor_sends_exact_native_report_body_and_retains_acceptance_on_stream_failure()
 {
    let temp = tempfile::tempdir().unwrap();
    let raw = format!(
        " \r\n{}\r\n",
        serde_json::to_string_pretty(&body()).unwrap()
    )
    .into_bytes();
    for broken_body in [false, true] {
        let prepared = prepare(temp.path(), &[], &raw);
        let (client, credential) = client(Audience::Fabric);
        let wire = Arc::new(HttpWire {
            replies: Mutex::new(VecDeque::from([
                Reply::Json(StatusCode::Ok, br#"{"value":[]}"#.to_vec()),
                if broken_body {
                    Reply::BrokenAcceptedBody
                } else {
                    Reply::Json(
                        StatusCode::Created,
                        serde_json::to_vec(&metadata()).unwrap(),
                    )
                },
            ])),
            requests: Mutex::new(Vec::new()),
        });
        let http =
            HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default()).unwrap();
        let trace = ExecutionTrace::default();
        let observe = || {
            let evidence = trace.snapshot();
            CreateTransportEvidence {
                http_status: evidence.status,
                operation_id: evidence.operation_id,
            }
        };
        let mut approval = Approval::allow(raw.clone());
        let delivery = prepared
            .execute(
                &ReportCreateBinding,
                &client,
                &http,
                CreateExecution {
                    executor: http.clone().with_trace(trace.clone()),
                    observe: &observe,
                },
                &mut approval,
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(credential.0.load(Ordering::SeqCst), 2);
        assert_eq!(delivery.report().value()["meta"]["completed"], !broken_body);
        if broken_body {
            assert_ne!(delivery.report().exit_code(), 0);
            assert_eq!(delivery.report().value()["meta"]["httpStatus"], 202);
            assert_eq!(
                delivery.report().value()["meta"]["submission"]["httpStatus"],
                202
            );
            assert_eq!(
                delivery.report().value()["meta"]["observation"]["httpStatus"],
                202
            );
            assert_eq!(
                delivery.report().value()["meta"]["operationHandle"],
                format!("/operations/{OP}")
            );
            assert_eq!(
                delivery.report().error().unwrap().operation_id.as_deref(),
                Some(OP)
            );
            assert_eq!(
                delivery.report().value()["meta"]["mutationMayHaveOccurred"],
                true
            );
            assert!(
                !delivery
                    .report()
                    .value()
                    .to_string()
                    .contains("offline-private-stream-failure")
            );
        } else {
            assert_eq!(delivery.report().exit_code(), 0);
            assert_eq!(delivery.report().value()["meta"]["resourceId"], REPORT);
        }
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method(), Method::Get);
        assert_eq!(
            requests[0].url().as_str(),
            format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/items?recursive=false")
        );
        assert_eq!(requests[1].method(), Method::Post);
        assert_eq!(
            requests[1].url().as_str(),
            format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/reports")
        );
        assert_eq!(azure_core::Bytes::from(requests[1].body()).as_ref(), raw);
    }
}

#[tokio::test]
async fn real_http_receipt_keeps_report_submission_separate_from_failed_or_malformed_observation() {
    let temp = tempfile::tempdir().unwrap();
    let raw = serde_json::to_vec(&body()).unwrap();
    for malformed_result in [false, true] {
        let prepared = prepare(temp.path(), &["--wait", "--project", "missing"], &raw);
        let (client, _) = client(Audience::Fabric);
        let mut replies = VecDeque::from([
            Reply::Json(StatusCode::Ok, br#"{"value":[]}"#.to_vec()),
            Reply::Accepted(serde_json::to_vec(&metadata()).unwrap()),
        ]);
        if malformed_result {
            replies.push_back(Reply::Json(
                StatusCode::Ok,
                br#"{"status":"Succeeded"}"#.to_vec(),
            ));
            replies.push_back(Reply::Json(StatusCode::Ok, b"{invalid-result".to_vec()));
        } else {
            replies.push_back(Reply::Json(
                StatusCode::Forbidden,
                br#"{"errorCode":"DeniedPoll"}"#.to_vec(),
            ));
        }
        let wire = Arc::new(HttpWire {
            replies: Mutex::new(replies),
            requests: Mutex::new(Vec::new()),
        });
        let http =
            HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default()).unwrap();
        let trace = ExecutionTrace::default();
        let observe = || {
            let evidence = trace.snapshot();
            CreateTransportEvidence {
                http_status: evidence.status,
                operation_id: evidence.operation_id,
            }
        };
        let mut approval = Approval::allow(raw.clone());
        let delivery = prepared
            .execute(
                &ReportCreateBinding,
                &client,
                &http,
                CreateExecution {
                    executor: http.clone().with_trace(trace.clone()),
                    observe: &observe,
                },
                &mut approval,
                pending(),
            )
            .await
            .unwrap();
        assert_ne!(delivery.report().exit_code(), 0);
        let mut output = Vec::new();
        delivery.deliver(&mut output).unwrap();
        let receipt: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(receipt["data"], Value::Null);
        assert_eq!(receipt["meta"]["submission"]["httpStatus"], 202);
        assert_eq!(receipt["meta"]["submission"]["resource"]["id"], REPORT);
        assert_eq!(
            receipt["meta"]["submission"]["operationHandle"],
            format!("/operations/{OP}")
        );
        assert_eq!(
            receipt["meta"]["observation"]["httpStatus"],
            if malformed_result { 200 } else { 403 }
        );
        assert_eq!(receipt["meta"]["completed"], false);
        assert_eq!(receipt["meta"]["mutationMayHaveOccurred"], true);
        assert_eq!(receipt["meta"]["anticipatedEffects"], json!([]));
        assert_eq!(receipt["meta"]["remoteReplayed"], false);
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), if malformed_result { 4 } else { 3 });
        assert_eq!(
            requests[0].url().as_str(),
            format!("https://api.fabric.microsoft.com/v1/workspaces/{WS}/items?recursive=false")
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method() == Method::Post)
                .count(),
            1
        );
        assert_eq!(azure_core::Bytes::from(requests[1].body()).as_ref(), raw);
    }
}
