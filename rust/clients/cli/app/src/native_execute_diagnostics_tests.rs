// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Actual injected native execution consumers; no live auth or service calls.
#![deny(unnameable_test_items)]

use super::*;
use fabric_sdk_operations::OperationError;
use fabric_sdk_tabular::{
    MetadataClassification, MetadataEvidence, MetadataLocation, TabularError,
};
use serde_json::{Value, json};

const PRIVATE: &str = "synthetic-secret-connection-string";

fn assert_safe_failure(delivery: get::Delivery, expected: &str, stop: bool) -> Value {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        delivery.write_to(&mut stdout, &mut stderr).unwrap(),
        if stop {
            CommandOutcome::Stop(1)
        } else {
            CommandOutcome::Returned(1)
        },
    );
    assert!(stdout.is_empty());
    let text = std::str::from_utf8(&stderr).unwrap();
    assert!(!text.contains(PRIVATE));
    assert!(!text.contains("pq-arrow-version=1"));
    assert!(!text.contains("arrowVersion"));
    let payload: Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(payload["guidance"]["classification"]["condition"], expected);
    assert_eq!(payload["guidance"]["automaticActionsTaken"], false);
    assert_eq!(payload["guidance"]["queryReplayed"], false);
    assert!(!payload["guidance"]["checks"].as_array().unwrap().is_empty());
    payload
}

#[tokio::test]
async fn native_service_diagnostics_are_structured_redacted_and_never_replay_in_any_format() {
    for (native_code, expected, message) in [
        ("GatewayNotReachable", "gateway_unavailable", PRIVATE),
        (
            "DM_GWPipeline_Gateway_DataSourceAccessError",
            "gateway_source_access",
            PRIVATE,
        ),
        (
            "DataflowExecuteQueryError",
            "query_execution_failed",
            PRIVATE,
        ),
        (
            "UnknownCode",
            "unknown",
            "GatewayNotReachable: use Arrow V1",
        ),
        ("UnverifiedArrow2GatewayError", "unknown", PRIVATE),
    ] {
        for format in ["json", "csv", "parquet"] {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("not-created.data");
            let body = json!({
                "errorCode": native_code,
                "message": message,
                "connectionString": PRIVATE,
            });
            let wire = Arc::new(Wire {
                replies: Mutex::new(VecDeque::from([(
                    StatusCode::BadRequest,
                    Headers::new(),
                    serde_json::to_vec(&body).unwrap(),
                )])),
                requests: Mutex::new(Vec::new()),
            });
            let delivery = invoke_with_output(
                Arc::clone(&wire),
                Duration::from_secs(30),
                format,
                None,
                Some(&destination),
            )
            .await;
            let get::Delivery::Execution(inner) = &delivery else {
                panic!("execution delivery");
            };
            assert_eq!(
                inner
                    .evidence
                    .as_ref()
                    .unwrap()
                    .raw_service_error
                    .as_deref(),
                Some(&body)
            );
            let payload = assert_safe_failure(delivery, expected, false);
            assert_eq!(payload["operation"]["httpStatus"], 400);
            assert_eq!(
                payload["operation"]["serviceError"],
                json!({ "redacted": true })
            );
            assert_eq!(payload["operation"]["mutationMayHaveOccurred"], true);
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
            assert!(wire.replies.lock().unwrap().is_empty());
            assert!(!destination.exists());
        }
    }
}

#[tokio::test]
async fn native_forbidden_without_a_documented_code_is_not_a_source_credential_diagnosis() {
    let wire = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([(
            StatusCode::Forbidden,
            Headers::new(),
            serde_json::to_vec(&json!({
                "errorCode": "UnknownCode",
                "message": "invalid credentials, GatewayNotReachable",
                "token": PRIVATE,
            }))
            .unwrap(),
        )])),
        requests: Mutex::new(Vec::new()),
    });
    let payload = assert_safe_failure(
        invoke(Arc::clone(&wire), Duration::from_secs(30)).await,
        "unknown",
        false,
    );
    assert_eq!(payload["error"]["code"], "Forbidden");
    assert_eq!(payload["operation"]["httpStatus"], 403);
    assert_eq!(
        payload["guidance"]["classification"]["basis"],
        "unclassified"
    );
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn native_terminal_failure_preserves_id_without_inventing_a_failed_http_status() {
    let wire = accepted_wire();
    {
        let mut replies = wire.replies.lock().unwrap();
        replies.pop_back();
        replies.pop_back();
        replies.push_back((
            StatusCode::Ok,
            Headers::new(),
            serde_json::to_vec(&json!({
                "status": "Failed",
                "error": {
                    "errorCode": "DataflowExecuteQueryError",
                    "message": PRIVATE,
                    "moreDetails": [{
                        "errorCode": "GatewayNotReachable",
                        "message": PRIVATE,
                    }],
                },
            }))
            .unwrap(),
        ));
    }
    let payload = assert_safe_failure(
        invoke(Arc::clone(&wire), Duration::from_secs(30)).await,
        "gateway_unavailable",
        false,
    );
    assert_eq!(payload["operation"]["id"], OPERATION);
    assert!(payload["operation"]["httpStatus"].is_null());
    assert_eq!(payload["operation"]["mutationMayHaveOccurred"], true);
    assert_eq!(payload["operation"]["remoteCancellationRequested"], false);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method() == Method::Post)
            .count(),
        1
    );
}

#[tokio::test]
async fn native_pq_metadata_is_never_parsed_into_known_service_codes_or_partial_success() {
    for value in [
        json!({ "errorCode": "GatewayNotReachable", "message": PRIVATE }).to_string(),
        format!("{{\"error\":{{\"errorCode\":\"GatewayNotReachable\",\"secret\":\"{PRIVATE}\"}}}}"),
        "{\"errorCode\":".into(),
        format!("{}{}", "[".repeat(256), PRIVATE),
        PRIVATE.repeat(2048),
    ] {
        for format in ["json", "csv", "parquet"] {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("not-created.data");
            let batch = RecordBatch::try_new(
                Arc::new(Schema::new(vec![
                    Field::new("value", DataType::Int64, true),
                    Field::new("PQ Arrow Metadata", DataType::Utf8, true),
                ])),
                vec![
                    Arc::new(Int64Array::from(vec![Some(7)])),
                    Arc::new(StringArray::from(vec![Some(value.as_str())])),
                ],
            )
            .unwrap();
            let wire = Arc::new(Wire {
                replies: Mutex::new(VecDeque::from([arrow_response(batch)])),
                requests: Mutex::new(Vec::new()),
            });
            let payload = assert_safe_failure(
                invoke_with_output(
                    Arc::clone(&wire),
                    Duration::from_secs(30),
                    format,
                    None,
                    Some(&destination),
                )
                .await,
                "unknown",
                true,
            );
            assert!(payload["operation"]["httpStatus"].is_null());
            assert_eq!(payload["metadataEvidence"][0]["key"], "PQ Arrow Metadata");
            assert_eq!(payload["metadataEvidence"][0]["value"], "<redacted>");
            assert_eq!(payload["metadataEvidence"][0]["classification"], "Unknown");
            assert_eq!(payload["metadataEvidenceRedacted"], true);
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
            assert!(!destination.exists());
        }
    }
}

#[tokio::test]
async fn native_malformed_body_or_detail_cannot_gain_a_gateway_diagnosis() {
    for body in [
        json!({ "errorCode": "UnknownCode", "message": null }),
        json!({
            "errorCode": "DataflowExecuteQueryError",
            "message": PRIVATE,
            "moreDetails": [{"errorCode": "GatewayNotReachable", "message": 7}],
        }),
    ] {
        let mut headers = Headers::new();
        if body["message"].is_null() {
            headers.insert("x-ms-error-code", "GatewayNotReachable");
        }
        let wire = Arc::new(Wire {
            replies: Mutex::new(VecDeque::from([(
                StatusCode::BadRequest,
                headers,
                serde_json::to_vec(&body).unwrap(),
            )])),
            requests: Mutex::new(Vec::new()),
        });
        let payload = assert_safe_failure(
            invoke(Arc::clone(&wire), Duration::from_secs(30)).await,
            "unknown",
            false,
        );
        assert_eq!(payload["operation"]["httpStatus"], 400);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn rendering_preserves_original_pq_bytes_and_bounds_only_the_redacted_presentation() {
    let error = ExecutionFailure::Result {
        error: TabularError::Metadata {
            evidence: (0..9)
                .map(|index| MetadataEvidence {
                    location: MetadataLocation::Field(index),
                    key: PRIVATE.into(),
                    value: PRIVATE.into(),
                    classification: MetadataClassification::Unknown,
                })
                .collect(),
        },
        response: fabric_sdk_operations::OperationResponse {
            status: 200,
            headers: Default::default(),
            body: PRIVATE.as_bytes().to_vec(),
        },
        evidence: Box::new(OperationError {
            kind: OperationErrorKind::InvalidResponse,
            operation_id: Some(OPERATION.into()),
            mutation_may_have_occurred: true,
            raw_service_error: None,
        }),
    };
    let delivery = diagnostics::failure(&error);
    let payload = assert_safe_failure(get::Delivery::Execution(delivery), "unknown", true);
    assert_eq!(payload["metadataEvidenceCount"], 9);
    assert_eq!(payload["metadataEvidence"].as_array().unwrap().len(), 8);
    let ExecutionFailure::Result {
        error,
        response,
        evidence,
    } = &error
    else {
        panic!("original result failure");
    };
    assert_eq!(response.body, PRIVATE.as_bytes());
    assert_eq!(evidence.operation_id.as_deref(), Some(OPERATION));
    let TabularError::Metadata { evidence } = error else {
        panic!("original metadata");
    };
    assert_eq!(evidence.len(), 9);
    assert!(
        evidence
            .iter()
            .all(|entry| entry.value == PRIVATE && entry.key == PRIVATE)
    );
}

#[test]
fn output_failure_debug_redacts_raw_service_and_io_diagnostics() {
    let error = WriteFailure {
        error: io::Error::other(PRIVATE),
        evidence: Some(ExecutionEvidence {
            operation_id: Some(OPERATION.into()),
            mutation_may_have_occurred: true,
            http_status: Some(403),
            raw_service_error: Some(Box::new(json!({ "secret": PRIVATE }))),
        }),
    };
    assert!(!format!("{error:?}").contains(PRIVATE));
    assert!(!format!("{error}").contains(PRIVATE));
    assert_eq!(
        error
            .evidence
            .as_ref()
            .unwrap()
            .raw_service_error
            .as_deref()
            .unwrap()["secret"],
        PRIVATE
    );
}
