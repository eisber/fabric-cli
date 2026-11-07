// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic public-contract tests. No credentials, source dumps or live I/O.

#![deny(unnameable_test_items)]

use azure_core::http::Method;
use fabric_sdk_api::ApiError;
use fabric_sdk_relevance::{
    ArtifactType, DISCOVER_ARTIFACTS_ACTION, DISCOVER_ARTIFACTS_PATH, DiscoveryCoverage,
    DiscoveryExecutor, DiscoveryFuture, DiscoveryRequest, DiscoveryScope, RELEVANCE_ENGINE_PREFIX,
    RelevanceClient, RelevanceConfidence, RelevanceError,
};
use fabric_sdk_workspaces::workspaces::{WorkspaceExecutionError, WorkspaceResponse};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{pending, ready},
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const QUERY_ID: &str = "12345678-1234-1234-1234-1234567890ab";
const EMPTY_ID: &str = "00000000-0000-0000-0000-000000000000";

struct TestExecutor {
    response: Mutex<Option<WorkspaceResponse>>,
    requests: Mutex<Vec<DiscoveryRequest>>,
    fail: Option<ApiError>,
    cancel_on_dispatch: AtomicBool,
    started: AtomicBool,
    dropped: AtomicBool,
}

impl TestExecutor {
    fn new(response: WorkspaceResponse) -> Self {
        Self {
            response: Mutex::new(Some(response)),
            requests: Mutex::default(),
            fail: None,
            cancel_on_dispatch: AtomicBool::new(false),
            started: AtomicBool::new(false),
            dropped: AtomicBool::new(false),
        }
    }
}

struct DropGuard<'a>(&'a AtomicBool);

impl Drop for DropGuard<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl DiscoveryExecutor for TestExecutor {
    fn discover<'a>(
        &'a self,
        request: &'a DiscoveryRequest,
        _: NonZeroUsize,
    ) -> DiscoveryFuture<'a> {
        Box::pin(async move {
            self.started.store(true, Ordering::SeqCst);
            self.requests.lock().unwrap().push(request.clone());
            if self.cancel_on_dispatch.load(Ordering::SeqCst) {
                let _guard = DropGuard(&self.dropped);
                pending::<()>().await;
            }
            if let Some(error) = &self.fail {
                return Err(error.clone());
            }
            Ok(self.response.lock().unwrap().take().unwrap())
        })
    }
}

fn request(text: &str, max_results: usize) -> DiscoveryRequest {
    DiscoveryRequest::new(
        text,
        max_results,
        DiscoveryScope::Tenant,
        vec![ArtifactType::Report, ArtifactType::SemanticModel],
    )
    .unwrap()
}

fn artifact() -> Value {
    json!({
        "documentType": "Report",
        "displayName": "Synthetic report",
        "score": 0.875,
        "searchScore": null,
        "matchedProperties": ["displayName"],
        "matchedSignals": [],
        "url": "https://app.powerbi.com/synthetic",
        "objectId": "11111111-1111-1111-1111-111111111111",
        "workspaceId": "22222222-2222-2222-2222-222222222222",
        "workspaceName": "Synthetic workspace",
        "relevance": 3,
    })
}

fn payload(artifacts: Vec<Value>) -> Value {
    json!({"queryId": QUERY_ID, "artifacts": artifacts})
}

fn response(status: u16, value: Value) -> WorkspaceResponse {
    WorkspaceResponse {
        status,
        headers: BTreeMap::new(),
        body: serde_json::to_vec(&value).unwrap(),
    }
}

fn client_with(response: WorkspaceResponse) -> (RelevanceClient, Arc<TestExecutor>) {
    let executor = Arc::new(TestExecutor::new(response));
    (RelevanceClient::new(executor.clone()), executor)
}

fn bytes_limit() -> NonZeroUsize {
    NonZeroUsize::new(1024 * 1024).unwrap()
}

#[test]
fn input_validation_never_widens_scope_types_or_text() {
    for text in ["", " ", "\r\n\t"] {
        assert_eq!(
            DiscoveryRequest::new(text, 1, DiscoveryScope::Tenant, vec![ArtifactType::Report]),
            Err(RelevanceError::InvalidText)
        );
    }
    assert!(
        DiscoveryRequest::new(
            "a".repeat(500),
            50,
            DiscoveryScope::Tenant,
            ArtifactType::ALL.to_vec()
        )
        .is_ok()
    );
    assert!(
        DiscoveryRequest::new(
            "🦀".repeat(250),
            1,
            DiscoveryScope::Tenant,
            vec![ArtifactType::Report]
        )
        .is_ok()
    );
    for text in ["a".repeat(501), "🦀".repeat(251)] {
        assert_eq!(
            DiscoveryRequest::new(text, 1, DiscoveryScope::Tenant, vec![ArtifactType::Report]),
            Err(RelevanceError::InvalidText)
        );
    }
    for top in [0, 51, usize::MAX] {
        assert_eq!(
            DiscoveryRequest::new("x", top, DiscoveryScope::Tenant, vec![ArtifactType::Report]),
            Err(RelevanceError::InvalidMaxResults)
        );
    }
    for types in [vec![], vec![ArtifactType::Report, ArtifactType::Report]] {
        assert_eq!(
            DiscoveryRequest::new("x", 1, DiscoveryScope::Tenant, types),
            Err(RelevanceError::InvalidTypes)
        );
    }
    assert_eq!(
        DiscoveryRequest::new(
            "x",
            1,
            DiscoveryScope::Workspace {
                id: "exact-workspace".into()
            },
            vec![ArtifactType::Report]
        ),
        Err(RelevanceError::InvalidWorkspaceId)
    );
    for kind in [
        "Unknown",
        "Workspace",
        "RdlReport",
        "report",
        "Lakehouse",
        "",
    ] {
        assert_eq!(
            kind.parse::<ArtifactType>(),
            Err(RelevanceError::UnsupportedType)
        );
    }
    for kind in [ArtifactType::RdlReport, ArtifactType::Workspace] {
        assert_eq!(
            DiscoveryRequest::new("x", 1, DiscoveryScope::Tenant, vec![kind]),
            Err(RelevanceError::UnsupportedType)
        );
    }
    assert_eq!(
        DiscoveryRequest::new("x", 1, DiscoveryScope::Tenant, ArtifactType::ALL.to_vec())
            .unwrap()
            .body()["discoveryScope"]["allowedArtifactTypes"],
        json!([2, 3, 5003, 16000, 16001, 21005])
    );
    assert!(
        !format!("{:?}", request("private-search-utterance", 1))
            .contains("private-search-utterance")
    );
}

#[tokio::test]
async fn workspace_ids_bind_exactly_without_name_resolution_or_widening() {
    let id = "22222222-2222-2222-2222-222222222222";
    let input = DiscoveryRequest::new(
        "x",
        1,
        DiscoveryScope::Workspace { id: id.into() },
        vec![ArtifactType::Report],
    )
    .unwrap();
    assert_eq!(
        input.body(),
        json!({
            "discoveryScope": {
                "queryScope":"Workspace",
                "artifactObjectIds":[id],
                "allowedArtifactTypes":[3]
            },
            "top":1,
            "userIntent":"search-intent"
        })
    );
    let (client, executor) = client_with(response(200, payload(vec![artifact()])));
    let result = client
        .discover(&input, bytes_limit(), pending())
        .await
        .unwrap();
    assert_eq!(result.artifacts[0].workspace_id.as_deref(), Some(id));
    assert_eq!(executor.requests.lock().unwrap()[0].scope(), input.scope());
    for id in ["", "workspace-name", EMPTY_ID] {
        assert_eq!(
            DiscoveryRequest::new(
                "x",
                1,
                DiscoveryScope::Workspace { id: id.into() },
                vec![ArtifactType::Report],
            ),
            Err(RelevanceError::InvalidWorkspaceId)
        );
    }
}

#[tokio::test]
async fn workspace_response_cannot_escape_the_requested_exact_id() {
    let input = DiscoveryRequest::new(
        "x",
        1,
        DiscoveryScope::Workspace {
            id: "22222222-2222-2222-2222-222222222222".into(),
        },
        vec![ArtifactType::Report],
    )
    .unwrap();
    for value in [Value::Null, json!("33333333-3333-3333-3333-333333333333")] {
        let mut row = artifact();
        row["workspaceId"] = value;
        let (client, _) = client_with(response(200, payload(vec![row])));
        let failure = client
            .discover_observed(&input, bytes_limit(), pending())
            .await
            .unwrap_err();
        assert_eq!(failure.error, RelevanceError::InvalidResponse);
        assert_eq!(failure.http_status, Some(200));
        assert!(failure.response_bytes.is_some());
    }
}

#[tokio::test]
async fn sends_exact_direct_post_query_and_numeric_request_enums_once() {
    let (client, executor) = client_with(response(200, payload(vec![artifact()])));
    assert!(executor.requests.lock().unwrap().is_empty());
    let input = request(" revenue & café + /? ", 7);
    let result = client
        .discover(&input, bytes_limit(), pending())
        .await
        .unwrap();
    assert_eq!(result.coverage, DiscoveryCoverage::RankedTopK);
    assert_eq!(result.artifacts[0].document_type, ArtifactType::Report);
    assert_eq!(
        result.artifacts[0].relevance,
        RelevanceConfidence::HighlyRelevant
    );
    let requests = executor.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let sent = &requests[0];
    assert_eq!(sent.method(), Method::Post);
    assert_eq!(RELEVANCE_ENGINE_PREFIX, "relevanceengine/v1");
    assert_eq!(DISCOVER_ARTIFACTS_ACTION, "discoverArtifacts");
    assert_eq!(sent.action(), DISCOVER_ARTIFACTS_ACTION);
    assert_eq!(sent.path(), DISCOVER_ARTIFACTS_PATH);
    assert_eq!(
        DISCOVER_ARTIFACTS_PATH,
        "/relevanceengine/v1/discoverArtifacts"
    );
    assert_eq!(
        sent.query(),
        vec![("searchText".to_owned(), input.text().to_owned())]
    );
    assert_eq!(
        sent.body(),
        json!({
            "discoveryScope":{"queryScope":"Global","allowedArtifactTypes":[3,2]},
            "top":7,
            "userIntent":"search-intent"
        })
    );
}

#[tokio::test]
async fn preserves_rank_duplicates_nullable_fields_and_unknown_optional_metadata() {
    let mut row = artifact();
    row["displayName"] = Value::Null;
    row["workspaceId"] = Value::Null;
    row["matchedSignals"] = Value::Null;
    row["futureOptional"] = json!({"preserved": true});
    row["semanticRerankerScore"] = json!(2.25);
    row["searchVectorScores"] = json!([{
        "queryIndex":0,"fieldName":"caption","vectorSimilarity":0.8,
        "searchScoreSource":"NotReturned","futureVectorField":"kept"
    }]);
    let mut body = payload(vec![row.clone(), row]);
    body["diagnostics"] = json!({"semanticRanking":true,"futureDiagnostic":17});
    body["futureEnvelope"] = json!([1, 2, 3]);
    let (client, executor) = client_with(response(200, body));
    let result = client
        .discover(&request("x", 2), bytes_limit(), pending())
        .await
        .unwrap();
    assert_eq!(result.artifacts.len(), 2);
    assert_eq!(result.artifacts[0], result.artifacts[1]);
    assert_eq!(result.artifacts[0].display_name, None);
    assert_eq!(result.artifacts[0].matched_signals, None);
    assert_eq!(
        result.artifacts[0].additional["futureOptional"],
        json!({"preserved":true})
    );
    assert_eq!(result.artifacts[0].semantic_reranker_score, Some(2.25));
    assert_eq!(
        result.diagnostics.unwrap().additional["futureDiagnostic"],
        17
    );
    assert_eq!(result.additional["futureEnvelope"], json!([1, 2, 3]));
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn disabled_is_unknown_and_nonzero_empty_is_only_ranked_top_k() {
    for (id, coverage) in [
        (EMPTY_ID, DiscoveryCoverage::DisabledOrUnknown),
        (QUERY_ID, DiscoveryCoverage::RankedTopK),
    ] {
        let (client, _) = client_with(response(200, json!({"queryId":id,"artifacts":[]})));
        let result = client
            .discover(&request("x", 1), bytes_limit(), pending())
            .await
            .unwrap();
        assert_eq!(result.coverage, coverage);
        assert!(result.artifacts.is_empty());
    }
}

#[tokio::test]
async fn malformed_required_optional_scope_and_count_data_never_becomes_empty_success() {
    let mut invalid = vec![
        Value::Null,
        json!([]),
        json!({}),
        json!({"queryId":QUERY_ID}),
        json!({"queryId":QUERY_ID,"artifacts":null}),
        json!({"queryId":"not-a-guid","artifacts":[]}),
        json!({"queryId":EMPTY_ID,"artifacts":[artifact()]}),
        payload(vec![artifact(), artifact()]),
    ];
    for field in [
        "documentType",
        "displayName",
        "score",
        "searchScore",
        "matchedProperties",
        "matchedSignals",
        "url",
        "objectId",
        "workspaceId",
        "workspaceName",
        "relevance",
    ] {
        let mut row = artifact();
        row.as_object_mut().unwrap().remove(field);
        invalid.push(payload(vec![row]));
    }
    for (field, value) in [
        ("documentType", json!(3)),
        ("documentType", json!("Lakehouse")),
        ("displayName", json!(5)),
        ("searchScore", json!("0.8")),
        ("matchedSignals", json!([false])),
        ("relevance", json!("HighlyRelevant")),
        ("relevance", json!(5)),
        ("score", Value::Null),
        ("semanticRerankerScore", json!("bad")),
        ("relatedToArtifacts", json!([{}])),
        ("isPreparedForCopilot", json!("yes")),
        ("searchVectorScores", json!([{}])),
    ] {
        let mut row = artifact();
        row[field] = value;
        invalid.push(payload(vec![row]));
    }
    let mut wrong_diagnostics = payload(vec![]);
    wrong_diagnostics["diagnostics"] = json!({"semanticRanking":"bad"});
    invalid.push(wrong_diagnostics);
    for body in invalid {
        let (client, _) = client_with(response(200, body.clone()));
        assert_eq!(
            client
                .discover(&request("x", 1), bytes_limit(), pending())
                .await,
            Err(RelevanceError::InvalidResponse),
            "unexpected acceptance: {body}"
        );
    }
    let (client, _) = client_with(response(200, payload(vec![artifact()])));
    let model_only = DiscoveryRequest::new(
        "x",
        1,
        DiscoveryScope::Tenant,
        vec![ArtifactType::SemanticModel],
    )
    .unwrap();
    assert_eq!(
        client.discover(&model_only, bytes_limit(), pending()).await,
        Err(RelevanceError::InvalidResponse)
    );
}

#[tokio::test]
async fn non_success_statuses_fail_without_post_replay() {
    for status in [202, 204, 301, 400, 401, 403, 404, 429, 500] {
        let (client, executor) = client_with(response(
            status,
            json!({"error":{"code":"SyntheticFailure","message":"untrusted-service-message"}}),
        ));
        let result = client
            .discover(&request("x", 1), bytes_limit(), pending())
            .await;
        if (200..300).contains(&status) {
            assert_eq!(
                result,
                Err(RelevanceError::Api(ApiError::UnsupportedStatus(status)))
            );
        } else {
            assert!(
                matches!(result, Err(RelevanceError::Service(_))),
                "{status}"
            );
        }
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
        assert!(!format!("{result:?}").contains("untrusted-service-message"));
    }
    for bytes in [b"not-json".to_vec(), b"[]".to_vec(), b"{}".to_vec()] {
        let (client, executor) = client_with(WorkspaceResponse {
            status: 500,
            headers: BTreeMap::new(),
            body: bytes,
        });
        assert!(matches!(
            client
                .discover(&request("x", 1), bytes_limit(), pending())
                .await,
            Err(RelevanceError::InvalidErrorResponse { status: 500 })
        ));
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn nested_service_errors_preserve_verified_metadata_and_retry_after() {
    let mut failure = response(
        429,
        json!({
            "error": {
                "code": "SyntheticThrottle",
                "message": "untrusted diagnostic",
                "details": [{"code":"Limit", "target":"top", "message":"bounded"}],
                "innererror": {"trace":"service-trace","context":"service-context"},
                "pbi.error": {"code":"SyntheticThrottle","parameters":{"limit":"60"}},
                "futureError": {"preserved":true}
            },
            "futureEnvelope": "preserved"
        }),
    );
    failure.headers.insert("Retry-After".into(), "17".into());
    failure
        .headers
        .insert("X-MS-Error-Code".into(), "HeaderCode".into());
    let (client, executor) = client_with(failure);
    let result = client
        .discover(&request("x", 1), bytes_limit(), pending())
        .await;
    assert!(!format!("{result:?}").contains("untrusted diagnostic"));
    let Err(RelevanceError::Service(failure)) = result else {
        panic!("expected structured service failure");
    };
    assert_eq!(failure.status, 429);
    assert_eq!(failure.retry_after.as_deref(), Some("17"));
    assert_eq!(failure.header_code.as_deref(), Some("HeaderCode"));
    assert_eq!(failure.additional["futureEnvelope"], "preserved");
    let error = failure.error.unwrap();
    assert_eq!(error.code, "SyntheticThrottle");
    assert_eq!(error.message.as_deref(), Some("untrusted diagnostic"));
    assert_eq!(error.details.unwrap()[0].target.as_deref(), Some("top"));
    assert_eq!(
        error.innererror.unwrap().context.as_deref(),
        Some("service-context")
    );
    assert_eq!(error.power_bi_error.unwrap()["parameters"]["limit"], "60");
    assert_eq!(error.additional["futureError"], json!({"preserved":true}));
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn malformed_error_fields_fail_and_bodyless_or_legacy_failures_preserve_status() {
    for body in [
        json!({"error":{"code":3}}),
        json!({"error":{"code":"Failure","message":[]}}),
        json!({"error":{"code":"Failure","details":[{"target":5}]}}),
        json!({"error":{"code":"Failure","innererror":7}}),
        json!({"error":{"code":"Failure","pbi.error":[]}}),
        json!({"error":{}}),
        json!({"error":null}),
        json!({"error":{"code":""}}),
        json!({"errorCode":"catalog-only-shape"}),
    ] {
        let (client, _) = client_with(response(500, body));
        assert_eq!(
            client
                .discover(&request("x", 1), bytes_limit(), pending())
                .await,
            Err(RelevanceError::InvalidErrorResponse { status: 500 })
        );
    }
    let (client, _) = client_with(response(
        400,
        json!({"code":"LegacyFailure","message":"legacy"}),
    ));
    let result = client
        .discover(&request("x", 1), bytes_limit(), pending())
        .await;
    let Err(RelevanceError::Service(failure)) = result else {
        panic!("expected legacy-compatible service failure");
    };
    assert_eq!(failure.status, 400);
    assert_eq!(failure.error.unwrap().code, "LegacyFailure");

    let (client, _) = client_with(WorkspaceResponse {
        status: 403,
        body: Vec::new(),
        headers: BTreeMap::new(),
    });
    let result = client
        .discover(&request("x", 1), bytes_limit(), pending())
        .await;
    let Err(RelevanceError::Service(failure)) = result else {
        panic!("expected bodyless service failure");
    };
    assert_eq!(failure.status, 403);
    assert_eq!(failure.error, None);
}

#[tokio::test]
async fn shared_adapter_authentication_and_execution_errors_remain_distinct() {
    for error in [
        ApiError::UnsupportedRequest,
        ApiError::InvalidAudience,
        ApiError::Credential,
        ApiError::Execution(WorkspaceExecutionError::TimedOut),
    ] {
        let mut executor = TestExecutor::new(response(200, payload(vec![])));
        executor.fail = Some(error.clone());
        let result = RelevanceClient::new(Arc::new(executor))
            .discover(&request("x", 1), bytes_limit(), pending())
            .await;
        assert_eq!(result, Err(RelevanceError::Api(error)));
    }
}

#[tokio::test]
async fn observed_success_measures_original_response_bytes_not_reserialization() {
    let body = b" \n{\"queryId\": \"12345678-1234-1234-1234-1234567890ab\", \"artifacts\": []}\t\n"
        .to_vec();
    let actual_bytes = body.len();
    assert_ne!(
        actual_bytes,
        serde_json::to_vec(&payload(vec![])).unwrap().len()
    );
    let (client, executor) = client_with(WorkspaceResponse {
        status: 200,
        body,
        headers: BTreeMap::new(),
    });
    let observation = client
        .discover_observed(&request("x", 1), bytes_limit(), pending())
        .await
        .unwrap();
    assert_eq!(observation.http_status, 200);
    assert_eq!(observation.response_bytes, actual_bytes);
    assert_eq!(observation.result.coverage, DiscoveryCoverage::RankedTopK);
    assert!(observation.result.artifacts.is_empty());
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn observed_complete_response_failures_retain_exact_status_and_body_length() {
    let cases = [
        (
            "decode",
            WorkspaceResponse {
                status: 200,
                body: b"bad-json".to_vec(),
                headers: BTreeMap::new(),
            },
            bytes_limit(),
        ),
        ("status", response(202, payload(vec![])), bytes_limit()),
        (
            "service",
            response(429, json!({"error":{"code":"Throttle"}})),
            bytes_limit(),
        ),
        (
            "bodyless",
            WorkspaceResponse {
                status: 403,
                body: Vec::new(),
                headers: BTreeMap::new(),
            },
            bytes_limit(),
        ),
        (
            "error-decode",
            response(500, json!({"error":{"code":3}})),
            bytes_limit(),
        ),
        (
            "limit",
            response(200, payload(vec![])),
            NonZeroUsize::new(1).unwrap(),
        ),
    ];
    for (name, response, bound) in cases {
        let actual_status = response.status;
        let actual_bytes = response.body.len();
        let (client, _) = client_with(response);
        let failure = client
            .discover_observed(&request("x", 1), bound, pending())
            .await
            .unwrap_err();
        assert_eq!(failure.http_status, Some(actual_status));
        assert_eq!(failure.response_bytes, Some(actual_bytes));
        assert_eq!(failure.retry_after_seconds, None);
        match (name, failure.error) {
            ("decode", RelevanceError::InvalidResponse)
            | ("status", RelevanceError::Api(ApiError::UnsupportedStatus(202)))
            | ("error-decode", RelevanceError::InvalidErrorResponse { status: 500 })
            | ("limit", RelevanceError::Api(ApiError::Limit)) => {}
            ("service", RelevanceError::Service(failure)) => assert_eq!(failure.status, 429),
            ("bodyless", RelevanceError::Service(failure)) => assert_eq!(failure.status, 403),
            _ => panic!("unexpected failure classification"),
        }
    }
}

#[tokio::test]
async fn observed_executor_failures_and_precancellation_have_no_invented_response_facts() {
    for error in [
        ApiError::Credential,
        ApiError::InvalidAudience,
        ApiError::Limit,
        ApiError::Execution(WorkspaceExecutionError::ResponseLimit),
        ApiError::Execution(WorkspaceExecutionError::TimedOut),
        ApiError::Http {
            status: 503,
            code: "NoRawResponse".into(),
        },
    ] {
        let mut executor = TestExecutor::new(response(200, payload(vec![])));
        executor.fail = Some(error.clone());
        let failure = RelevanceClient::new(Arc::new(executor))
            .discover_observed(&request("x", 1), bytes_limit(), pending())
            .await
            .unwrap_err();
        assert_eq!(failure.error, RelevanceError::Api(error));
        assert_eq!(failure.http_status, None);
        assert_eq!(failure.response_bytes, None);
        assert_eq!(failure.retry_after_seconds, None);
    }
    let (client, executor) = client_with(response(200, payload(vec![])));
    let failure = client
        .discover_observed(&request("x", 1), bytes_limit(), ready(()))
        .await
        .unwrap_err();
    assert_eq!(failure.error, RelevanceError::Api(ApiError::Cancelled));
    assert_eq!(failure.http_status, None);
    assert_eq!(failure.response_bytes, None);
    assert_eq!(failure.retry_after_seconds, None);
    assert!(executor.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn observed_retry_after_is_only_the_actual_valid_delta_seconds() {
    for (value, expected) in [
        ("17", Some(17)),
        (" 0 ", Some(0)),
        ("+17", None),
        ("-1", None),
        ("1.5", None),
        ("", None),
        ("18446744073709551616", None),
        ("Wed, 21 Oct 2037 07:28:00 GMT", None),
    ] {
        let mut response = response(
            429,
            json!({"error":{"code":"Throttle","message":"private-service-data"}}),
        );
        response.headers.insert("Retry-After".into(), value.into());
        let actual_bytes = response.body.len();
        let (client, executor) = client_with(response);
        let failure = client
            .discover_observed(&request("x", 1), bytes_limit(), pending())
            .await
            .unwrap_err();
        assert_eq!(failure.http_status, Some(429));
        assert_eq!(failure.response_bytes, Some(actual_bytes));
        assert_eq!(failure.retry_after_seconds, expected);
        assert!(!format!("{failure:?}").contains("private-service-data"));
        assert!(!failure.to_string().contains("private-service-data"));
        assert_eq!(executor.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn response_bounds_and_invalid_json_fail_explicitly() {
    let (client, _) = client_with(response(200, payload(vec![])));
    assert_eq!(
        client
            .discover(&request("x", 1), NonZeroUsize::new(1).unwrap(), pending())
            .await,
        Err(RelevanceError::Api(ApiError::Limit))
    );
    let (client, _) = client_with(WorkspaceResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: b"{not-json".to_vec(),
    });
    assert_eq!(
        client
            .discover(&request("x", 1), bytes_limit(), pending())
            .await,
        Err(RelevanceError::InvalidResponse)
    );
}

#[tokio::test]
async fn cancellation_before_and_during_dispatch_drops_local_work() {
    let (client, executor) = client_with(response(200, payload(vec![])));
    let result = client
        .discover(&request("x", 1), bytes_limit(), ready(()))
        .await;
    assert_eq!(result, Err(RelevanceError::Api(ApiError::Cancelled)));
    assert!(executor.requests.lock().unwrap().is_empty());

    let (client, executor) = client_with(response(200, payload(vec![])));
    executor.cancel_on_dispatch.store(true, Ordering::SeqCst);
    let cancelled = std::future::poll_fn(|cx| {
        if executor.started.load(Ordering::SeqCst) {
            std::task::Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    });
    let result = client
        .discover(&request("x", 1), bytes_limit(), cancelled)
        .await;
    assert_eq!(result, Err(RelevanceError::Api(ApiError::Cancelled)));
    assert!(executor.dropped.load(Ordering::SeqCst));
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}
