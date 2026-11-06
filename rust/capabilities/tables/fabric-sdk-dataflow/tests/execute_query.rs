// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline direct-consumer contracts with injected credential, transport and time.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    time::OffsetDateTime,
};
use fabric_core::{
    Audience, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin, TokenCredential,
};
use fabric_sdk_dataflow::{
    ArrowVersion, ExecuteQueryCompletion, ExecuteQueryErrorKind, ExecuteQueryRequest,
    ExecuteQueryTarget, MAX_QUERY_NAME_BYTES, MAX_REQUEST_BYTES, execute_query, wait_for_query,
};
use fabric_sdk_operations::{
    OperationAccept, OperationErrorKind, OperationExecutionError, OperationExecutor,
    OperationLimits, OperationRequest, OperationResponse, OperationSession,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::json;
use std::{
    collections::VecDeque,
    future::{Future, pending, poll_fn},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const DATAFLOW: &str = "22222222-2222-2222-2222-222222222222";
const OPERATION: &str = "33333333-3333-3333-3333-333333333333";
const ROOT: &str = "https://api.fabric.microsoft.com";
const ARROW: &str = "application/vnd.apache.arrow.stream";
const SOURCE: &str = "sensitive-source-value";
const BYTES: &[u8] = b"\xff\xff\xff\xff\x08\0\0\0native\x00\xff";

#[derive(Debug, Default)]
struct Credential(AtomicUsize);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "synthetic-token-never-used-live",
            OffsetDateTime::now_utc() + azure_core::time::Duration::hours(1),
        ))
    }
}

#[derive(Debug)]
struct Recorded {
    method: ResourceMethod,
    url: String,
    body: Vec<u8>,
    accept: Option<OperationAccept>,
    debug: String,
    limit: NonZeroUsize,
}

#[derive(Default)]
struct State {
    responses: VecDeque<Result<OperationResponse, OperationExecutionError>>,
    requests: Vec<Recorded>,
    waits: Vec<Duration>,
    wait_error: Option<OperationExecutionError>,
    cancel_on_dispatch: Option<Arc<AtomicBool>>,
    cancel_on_wait: Option<Arc<AtomicBool>>,
}

#[derive(Clone, Default)]
struct Executor(Arc<Mutex<State>>);

impl Executor {
    fn new(responses: impl IntoIterator<Item = OperationResponse>) -> Self {
        let result = Self::default();
        result
            .0
            .lock()
            .unwrap()
            .responses
            .extend(responses.into_iter().map(Ok));
        result
    }
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            let (cancelled, response) = {
                let mut state = self.0.lock().unwrap();
                state.requests.push(Recorded {
                    method: request.method(),
                    url: request.url().to_string(),
                    body: request.body().to_vec(),
                    accept: request.accept(),
                    debug: format!("{request:?}"),
                    limit: request.max_response_bytes(),
                });
                let cancelled = state.cancel_on_dispatch.take();
                let response = if cancelled.is_none() {
                    Some(
                        state
                            .responses
                            .pop_front()
                            .expect("unexpected request/replay"),
                    )
                } else {
                    None
                };
                (cancelled, response)
            };
            if let Some(cancelled) = cancelled {
                cancelled.store(true, Ordering::SeqCst);
                return poll_fn(|cx| {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                })
                .await;
            }
            response.unwrap()
        })
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(async move {
            let (cancelled, error) = {
                let mut state = self.0.lock().unwrap();
                state.waits.push(delay);
                (state.cancel_on_wait.take(), state.wait_error)
            };
            if let Some(cancelled) = cancelled {
                cancelled.store(true, Ordering::SeqCst);
                return poll_fn(|cx| {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                })
                .await;
            }
            error.map_or(Ok(()), Err)
        })
    }
}

fn selection() -> CredentialSelection {
    CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap()
}

fn target() -> ExecuteQueryTarget {
    ExecuteQueryTarget::new(WORKSPACE, DATAFLOW).unwrap()
}

fn request(version: ArrowVersion) -> ExecuteQueryRequest {
    ExecuteQueryRequest::new("Named \u{65e5} query", version).unwrap()
}

fn response(status: u16, body: &[u8], headers: &[(&str, &str)]) -> OperationResponse {
    OperationResponse {
        status,
        body: body.to_vec(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

fn arrow_response() -> OperationResponse {
    response(200, BYTES, &[("Content-Type", ARROW)])
}

fn accepted() -> OperationResponse {
    response(
        202,
        b"",
        &[
            ("x-ms-operation-id", OPERATION),
            ("Location", &format!("{ROOT}/v1/operations/{OPERATION}")),
            ("Retry-After", "0"),
        ],
    )
}

fn succeeded() -> OperationResponse {
    response(
        200,
        br#"{"status":"Succeeded"}"#,
        &[(
            "Location",
            &format!("{ROOT}/v1/operations/{OPERATION}/result"),
        )],
    )
}

#[tokio::test]
async fn exact_post_route_json_accept_and_immediate_bytes_for_both_versions() {
    for version in [ArrowVersion::V1, ArrowVersion::V2] {
        let credential = Credential::default();
        let selection = selection();
        let executor = Executor::new([arrow_response()]);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let request = request(version)
            .with_custom_mashup_document("section Section1;\nshared Named = \"\u{65e5}\";")
            .unwrap();
        let result = execute_query(
            &mut session,
            &target(),
            &request,
            ExecuteQueryCompletion::ReturnAccepted,
        )
        .await
        .unwrap();
        assert_eq!(result.status, 200);
        assert_eq!(result.body, BYTES);
        assert_eq!(result.headers["Content-Type"], ARROW);
        assert_eq!(credential.0.load(Ordering::SeqCst), 1);
        let state = executor.0.lock().unwrap();
        let sent = &state.requests[0];
        assert_eq!(sent.method, ResourceMethod::Post);
        assert_eq!(
            sent.url,
            format!("{ROOT}/v1/workspaces/{WORKSPACE}/dataflows/{DATAFLOW}/executeQuery")
        );
        assert_eq!(sent.accept, Some(version.accept()));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&sent.body).unwrap(),
            json!({"queryName":request.query_name(),"customMashupDocument":request.custom_mashup_document().unwrap()})
        );
        assert_eq!(sent.limit, OperationLimits::default().max_response_bytes);
        assert!(state.waits.is_empty());
        assert_eq!(state.requests.len(), 1);
    }
}

#[test]
fn omitted_optional_field_and_version_are_not_sent_as_body_properties() {
    let request = request(ArrowVersion::V2);
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({"queryName":"Named \u{65e5} query"})
    );
    assert_eq!(request.arrow_version(), ArrowVersion::V2);
    assert_eq!(request.custom_mashup_document(), None);
    assert_eq!(target().workspace_id(), WORKSPACE);
    assert_eq!(target().dataflow_id(), DATAFLOW);
    assert_eq!(
        ArrowVersion::V1.accept().as_str(),
        "application/vnd.apache.arrow.stream;pq-arrow-version=1"
    );
    assert_eq!(
        ArrowVersion::V2.accept().as_str(),
        "application/vnd.apache.arrow.stream;pq-arrow-version=2"
    );
}

#[tokio::test]
async fn invalid_target_request_and_options_do_not_acquire_credentials() {
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::default();
    let _session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    for invalid in [
        "",
        "name",
        "11111111-1111-1111-1111-11111111111",
        "111111111-111-1111-1111-111111111111",
        "gggggggg-1111-1111-1111-111111111111",
        "11111111-1111-1111-1111-111111111111/",
        "11111111-1111-1111-1111-111111111111?x=y",
        " 11111111-1111-1111-1111-111111111111",
        "11111111-1111-1111-1111-111111111111\n",
    ] {
        assert_eq!(
            ExecuteQueryTarget::new(invalid, DATAFLOW)
                .unwrap_err()
                .kind(),
            ExecuteQueryErrorKind::InvalidTarget
        );
        assert_eq!(
            ExecuteQueryTarget::new(WORKSPACE, invalid)
                .unwrap_err()
                .kind(),
            ExecuteQueryErrorKind::InvalidTarget
        );
    }
    for invalid in ["", " ", "\n", "a\0b", "a\tb", "a\nb"] {
        assert_eq!(
            ExecuteQueryRequest::new(invalid, ArrowVersion::V1)
                .unwrap_err()
                .kind(),
            ExecuteQueryErrorKind::InvalidRequest
        );
    }
    assert_eq!(
        ExecuteQueryRequest::new("q".repeat(MAX_QUERY_NAME_BYTES + 1), ArrowVersion::V1)
            .unwrap_err()
            .kind(),
        ExecuteQueryErrorKind::RequestLimit
    );
    for invalid in ["".to_owned(), " \n".to_owned(), "secret\0".to_owned()] {
        assert_eq!(
            request(ArrowVersion::V1)
                .with_custom_mashup_document(invalid)
                .unwrap_err()
                .kind(),
            ExecuteQueryErrorKind::InvalidRequest
        );
    }
    for invalid in [
        "x".repeat(MAX_REQUEST_BYTES + 1),
        "x\n".repeat(MAX_REQUEST_BYTES / 3),
    ] {
        assert_eq!(
            request(ArrowVersion::V1)
                .with_custom_mashup_document(invalid)
                .unwrap_err()
                .kind(),
            ExecuteQueryErrorKind::RequestLimit
        );
    }
    for invalid in [r#""0""#, r#""3""#, "1", "null", r#""V1""#] {
        assert!(serde_json::from_str::<ArrowVersion>(invalid).is_err());
    }
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(executor.0.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn accepted_does_not_poll_or_claim_result_and_wait_never_reposts() {
    let credential = Credential::default();
    let selection = selection();
    let first = Executor::new([accepted()]);
    let mut session = OperationSession::new(
        &credential,
        &selection,
        first.clone(),
        OperationLimits::default(),
        pending(),
    );
    let result = execute_query(
        &mut session,
        &target(),
        &request(ArrowVersion::V2),
        ExecuteQueryCompletion::ReturnAccepted,
    )
    .await
    .unwrap();
    assert_eq!(result.status, 202);
    assert_eq!(session.operation_id(), Some(OPERATION));
    assert_eq!(first.0.lock().unwrap().requests.len(), 1);
    assert!(first.0.lock().unwrap().waits.is_empty());
    let second = Executor::new([succeeded(), arrow_response()]);
    let mut wait_session = OperationSession::new(
        &credential,
        &selection,
        second.clone(),
        OperationLimits::default(),
        pending(),
    );
    let result = wait_for_query(&mut wait_session, result, ArrowVersion::V2)
        .await
        .unwrap();
    assert_eq!(result.body, BYTES);
    let state = second.0.lock().unwrap();
    assert_eq!(state.requests.len(), 2);
    assert!(
        state
            .requests
            .iter()
            .all(|request| request.method == ResourceMethod::Get)
    );
    assert_eq!(state.requests[0].accept, None);
    assert_eq!(state.requests[1].accept, Some(OperationAccept::ArrowV2));
    assert_eq!(
        state.requests[1].url,
        format!("{ROOT}/v1/operations/{OPERATION}/result")
    );
}

#[tokio::test]
async fn lro_polls_state_and_fetches_arrow_result_with_original_negotiation() {
    for version in [ArrowVersion::V1, ArrowVersion::V2] {
        let credential = Credential::default();
        let selection = selection();
        let executor = Executor::new([
            accepted(),
            response(200, br#"{"status":"Running"}"#, &[("Retry-After", "2")]),
            succeeded(),
            arrow_response(),
        ]);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let result = execute_query(
            &mut session,
            &target(),
            &request(version),
            ExecuteQueryCompletion::WaitForResult,
        )
        .await
        .unwrap();
        assert_eq!(result.body, BYTES);
        assert_eq!(session.operation_id(), Some(OPERATION));
        let state = executor.0.lock().unwrap();
        assert_eq!(state.waits, [Duration::ZERO, Duration::from_secs(2)]);
        assert_eq!(
            state
                .requests
                .iter()
                .map(|request| request.accept)
                .collect::<Vec<_>>(),
            [Some(version.accept()), None, None, Some(version.accept())]
        );
        assert_eq!(
            state
                .requests
                .iter()
                .map(|request| request.method)
                .collect::<Vec<_>>(),
            [
                ResourceMethod::Post,
                ResourceMethod::Get,
                ResourceMethod::Get,
                ResourceMethod::Get
            ]
        );
        assert!(
            state.requests[1..]
                .iter()
                .all(|request| request.body.is_empty())
        );
        assert_eq!(credential.0.load(Ordering::SeqCst), 4);
    }
}

#[tokio::test]
async fn service_errors_and_terminal_lro_errors_preserve_original_evidence() {
    for status in [400, 401, 403, 429, 500] {
        let credential = Credential::default();
        let selection = selection();
        let executor = Executor::new([response(
            status,
            br#"{"errorCode":"DataflowExecuteQueryError","message":"sensitive-source-value","extra":"original"}"#,
            &[("x-ms-request-id", "original-request")],
        )]);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let error = execute_query(
            &mut session,
            &target(),
            &request(ArrowVersion::V1),
            ExecuteQueryCompletion::WaitForResult,
        )
        .await
        .unwrap_err();
        let evidence = error.operation_error().unwrap();
        assert!(
            matches!(&evidence.kind, OperationErrorKind::Http { status: actual, code: Some(code), .. } if *actual == status && code == "DataflowExecuteQueryError")
        );
        assert_eq!(
            evidence.raw_service_error.as_ref().unwrap()["extra"],
            "original"
        );
        assert!(evidence.mutation_may_have_occurred);
        assert_eq!(executor.0.lock().unwrap().requests.len(), 1);
        assert!(!format!("{error:?} {error}").contains(SOURCE));
    }
    for terminal in ["Failed", "Cancelled"] {
        let credential = Credential::default();
        let selection = selection();
        let executor = Executor::new([
            accepted(),
            response(
                200,
                serde_json::to_string(&json!({"status":terminal,"error":{"errorCode":"NativeQuery","message":SOURCE,"extra":42}})).unwrap().as_bytes(),
                &[],
            ),
        ]);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let error = execute_query(
            &mut session,
            &target(),
            &request(ArrowVersion::V1),
            ExecuteQueryCompletion::WaitForResult,
        )
        .await
        .unwrap_err();
        let evidence = error.operation_error().unwrap();
        assert_eq!(evidence.operation_id.as_deref(), Some(OPERATION));
        assert!(
            matches!(&evidence.kind, OperationErrorKind::OperationFailed { status, .. } if status == terminal)
        );
        assert_eq!(evidence.raw_service_error.as_ref().unwrap()["extra"], 42);
        assert!(!format!("{error:?} {error}").contains(SOURCE));
        assert_eq!(executor.0.lock().unwrap().requests.len(), 2);
    }
}

#[tokio::test]
async fn unsafe_missing_duplicate_and_malformed_handles_never_poll() {
    for completion in [
        ExecuteQueryCompletion::ReturnAccepted,
        ExecuteQueryCompletion::WaitForResult,
    ] {
        let cases = [
            response(202, b"", &[]),
            response(202, b"", &[("x-ms-operation-id", "opaque-not-guid")]),
            response(
                202,
                b"",
                &[
                    ("x-ms-operation-id", OPERATION),
                    ("X-MS-OPERATION-ID", OPERATION),
                ],
            ),
            response(
                202,
                b"",
                &[
                    ("x-ms-operation-id", OPERATION),
                    (
                        "Location",
                        &format!("https://foreign.invalid/v1/operations/{OPERATION}"),
                    ),
                ],
            ),
            response(
                202,
                b"",
                &[
                    ("x-ms-operation-id", OPERATION),
                    ("Location", &format!("{ROOT}/v1/operations/{DATAFLOW}")),
                ],
            ),
            response(
                202,
                b"",
                &[(
                    "Location",
                    &format!("{ROOT}/v1/operations/{OPERATION}?token=secret"),
                )],
            ),
            response(202, b"", &[("Location", "/v1/operations/unsafe")]),
        ];
        for initial in cases {
            let credential = Credential::default();
            let selection = selection();
            let executor = Executor::new([initial]);
            let mut session = OperationSession::new(
                &credential,
                &selection,
                executor.clone(),
                OperationLimits::default(),
                pending(),
            );
            let error = execute_query(
                &mut session,
                &target(),
                &request(ArrowVersion::V1),
                completion,
            )
            .await
            .unwrap_err();
            assert!(error.operation_error().unwrap().mutation_may_have_occurred);
            assert_eq!(executor.0.lock().unwrap().requests.len(), 1);
            assert!(executor.0.lock().unwrap().waits.is_empty());
        }
    }
}

#[tokio::test]
async fn media_validation_never_silently_decodes_missing_json_or_invalid_arrow() {
    let cases = [
        (None, ExecuteQueryErrorKind::MissingContentType),
        (
            Some("application/json"),
            ExecuteQueryErrorKind::InvalidContentType,
        ),
        (
            Some("application/octet-stream"),
            ExecuteQueryErrorKind::InvalidContentType,
        ),
        (
            Some("application/vnd.apache.arrow.file"),
            ExecuteQueryErrorKind::InvalidContentType,
        ),
        (
            Some("application/vnd.apache.arrow.stream;pq-arrow-version=2"),
            ExecuteQueryErrorKind::ArrowVersionMismatch,
        ),
        (
            Some("application/vnd.apache.arrow.stream;pq-arrow-version=3"),
            ExecuteQueryErrorKind::InvalidContentType,
        ),
        (
            Some("application/vnd.apache.arrow.stream;pq-arrow-version="),
            ExecuteQueryErrorKind::InvalidContentType,
        ),
        (
            Some("application/vnd.apache.arrow.stream;pq-arrow-version=1;pq-arrow-version=1"),
            ExecuteQueryErrorKind::InvalidContentType,
        ),
        (
            Some("application/vnd.apache.arrow.stream;"),
            ExecuteQueryErrorKind::InvalidContentType,
        ),
        (
            Some("application/vnd.apache.arrow.stream,\napplication/json"),
            ExecuteQueryErrorKind::InvalidContentType,
        ),
    ];
    for (media, expected) in cases {
        let credential = Credential::default();
        let selection = selection();
        let mut result = response(200, SOURCE.as_bytes(), &[]);
        if let Some(media) = media {
            result.headers.insert("content-type".into(), media.into());
        }
        let executor = Executor::new([result]);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let error = execute_query(
            &mut session,
            &target(),
            &request(ArrowVersion::V1),
            ExecuteQueryCompletion::ReturnAccepted,
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), expected, "{media:?}");
        assert_eq!(error.response().unwrap().body, SOURCE.as_bytes());
        assert!(error.operation_error().unwrap().mutation_may_have_occurred);
        assert!(!format!("{error:?} {error}").contains(SOURCE));
        assert_eq!(executor.0.lock().unwrap().requests.len(), 1);
    }
}

#[tokio::test]
async fn valid_media_parameter_case_quoting_and_missing_version_policy() {
    for media in [
        ARROW,
        "Application/Vnd.Apache.Arrow.Stream; PQ-Arrow-Version = 1",
        "application/vnd.apache.arrow.stream;pq-arrow-version=\"1\"",
        "application/vnd.apache.arrow.stream;charset=utf-8;pq-arrow-version=1",
    ] {
        let credential = Credential::default();
        let selection = selection();
        let executor = Executor::new([response(200, BYTES, &[("cOnTeNt-TyPe", media)])]);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor,
            OperationLimits::default(),
            pending(),
        );
        assert_eq!(
            execute_query(
                &mut session,
                &target(),
                &request(ArrowVersion::V1),
                ExecuteQueryCompletion::ReturnAccepted,
            )
            .await
            .unwrap()
            .body,
            BYTES
        );
    }
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::new([response(
        200,
        BYTES,
        &[("content-type", ARROW), ("Content-Type", ARROW)],
    )]);
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor,
        OperationLimits::default(),
        pending(),
    );
    assert_eq!(
        execute_query(
            &mut session,
            &target(),
            &request(ArrowVersion::V1),
            ExecuteQueryCompletion::ReturnAccepted,
        )
        .await
        .unwrap_err()
        .kind(),
        ExecuteQueryErrorKind::InvalidContentType
    );
}

#[tokio::test]
async fn unexpected_success_status_and_result_media_retain_operation_id() {
    for status in [201, 204] {
        let credential = Credential::default();
        let selection = selection();
        let executor = Executor::new([response(status, b"", &[])]);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor,
            OperationLimits::default(),
            pending(),
        );
        assert_eq!(
            execute_query(
                &mut session,
                &target(),
                &request(ArrowVersion::V1),
                ExecuteQueryCompletion::ReturnAccepted,
            )
            .await
            .unwrap_err()
            .kind(),
            ExecuteQueryErrorKind::UnexpectedStatus
        );
    }
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::new([
        accepted(),
        succeeded(),
        response(200, b"{}", &[("Content-Type", "application/json")]),
    ]);
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor,
        OperationLimits::default(),
        pending(),
    );
    let error = execute_query(
        &mut session,
        &target(),
        &request(ArrowVersion::V1),
        ExecuteQueryCompletion::WaitForResult,
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ExecuteQueryErrorKind::InvalidContentType);
    assert_eq!(
        error.operation_error().unwrap().operation_id.as_deref(),
        Some(OPERATION)
    );
}

#[tokio::test]
async fn response_request_and_delay_bounds_are_not_clipped_success() {
    for mode in 0..4 {
        let credential = Credential::default();
        let selection = selection();
        let executor = Executor::new([accepted(), succeeded(), arrow_response()]);
        let mut limits = OperationLimits::default();
        match mode {
            0 => limits.max_response_bytes = NonZeroUsize::new(1).unwrap(),
            1 => limits.max_requests = NonZeroUsize::new(1).unwrap(),
            2 => limits.max_requests = NonZeroUsize::new(2).unwrap(),
            _ => {
                executor.0.lock().unwrap().responses[0]
                    .as_mut()
                    .unwrap()
                    .headers
                    .insert("Retry-After".into(), "61".into());
            }
        }
        let mut session =
            OperationSession::new(&credential, &selection, executor.clone(), limits, pending());
        let error = execute_query(
            &mut session,
            &target(),
            &request(ArrowVersion::V1),
            ExecuteQueryCompletion::WaitForResult,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.operation_error().unwrap().kind,
            OperationErrorKind::Limit
        );
        assert!(error.operation_error().unwrap().mutation_may_have_occurred);
        assert!(executor.0.lock().unwrap().requests.len() <= 2);
    }
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::new([arrow_response()]);
    let mut limits = OperationLimits::default();
    limits.max_response_bytes = NonZeroUsize::new(BYTES.len()).unwrap();
    let mut session = OperationSession::new(&credential, &selection, executor, limits, pending());
    assert_eq!(
        execute_query(
            &mut session,
            &target(),
            &request(ArrowVersion::V1),
            ExecuteQueryCompletion::ReturnAccepted,
        )
        .await
        .unwrap()
        .body,
        BYTES
    );
}

#[tokio::test]
async fn dispatch_failure_deadline_and_wait_failure_never_replay_or_cancel_remote() {
    for failure in [
        OperationExecutionError::Failed,
        OperationExecutionError::TimedOut,
        OperationExecutionError::Cancelled,
        OperationExecutionError::ResponseLimit,
    ] {
        let credential = Credential::default();
        let selection = selection();
        let executor = Executor::default();
        executor.0.lock().unwrap().responses.push_back(Err(failure));
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let error = execute_query(
            &mut session,
            &target(),
            &request(ArrowVersion::V1),
            ExecuteQueryCompletion::WaitForResult,
        )
        .await
        .unwrap_err();
        let evidence = error.operation_error().unwrap();
        assert!(evidence.mutation_may_have_occurred);
        assert_eq!(evidence.operation_id, None);
        assert_eq!(
            evidence.kind,
            if failure == OperationExecutionError::ResponseLimit {
                OperationErrorKind::Limit
            } else {
                OperationErrorKind::Execution(failure)
            }
        );
        assert_eq!(executor.0.lock().unwrap().requests.len(), 1);
    }
    for failure in [
        OperationExecutionError::TimedOut,
        OperationExecutionError::Cancelled,
    ] {
        let credential = Credential::default();
        let selection = selection();
        let executor = Executor::new([accepted()]);
        executor.0.lock().unwrap().wait_error = Some(failure);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let error = execute_query(
            &mut session,
            &target(),
            &request(ArrowVersion::V1),
            ExecuteQueryCompletion::WaitForResult,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.operation_error().unwrap().operation_id.as_deref(),
            Some(OPERATION)
        );
        assert_eq!(executor.0.lock().unwrap().requests.len(), 1);
    }
}

#[tokio::test]
async fn local_cancellation_before_during_dispatch_and_during_wait_keeps_uncertainty() {
    for stage in 0..3 {
        let credential = Credential::default();
        let selection = selection();
        let cancelled = Arc::new(AtomicBool::new(stage == 0));
        let signal = cancelled.clone();
        let executor = Executor::new([accepted()]);
        if stage == 1 {
            executor.0.lock().unwrap().cancel_on_dispatch = Some(cancelled);
        } else if stage == 2 {
            executor.0.lock().unwrap().cancel_on_wait = Some(cancelled);
        }
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits::default(),
            poll_fn(move |_| {
                if signal.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }),
        );
        let error = execute_query(
            &mut session,
            &target(),
            &request(ArrowVersion::V1),
            ExecuteQueryCompletion::WaitForResult,
        )
        .await
        .unwrap_err();
        let evidence = error.operation_error().unwrap();
        assert_eq!(evidence.kind, OperationErrorKind::Cancelled);
        assert_eq!(evidence.mutation_may_have_occurred, stage != 0);
        assert_eq!(
            evidence.operation_id.as_deref(),
            if stage == 2 { Some(OPERATION) } else { None }
        );
        assert_eq!(
            executor.0.lock().unwrap().requests.len(),
            usize::from(stage != 0)
        );
        assert_eq!(credential.0.load(Ordering::SeqCst), usize::from(stage != 0));
    }
}

#[tokio::test]
async fn debug_redacts_query_mashup_target_token_body_and_service_response() {
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::new([response(
        200,
        SOURCE.as_bytes(),
        &[("Content-Type", "application/json")],
    )]);
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let query = ExecuteQueryRequest::new(SOURCE, ArrowVersion::V1)
        .unwrap()
        .with_custom_mashup_document(SOURCE)
        .unwrap();
    let error = execute_query(
        &mut session,
        &target(),
        &query,
        ExecuteQueryCompletion::ReturnAccepted,
    )
    .await
    .unwrap_err();
    let diagnostic = format!(
        "{query:?} {:?} {error:?} {error} {}",
        target(),
        executor.0.lock().unwrap().requests[0].debug
    );
    for secret in [
        SOURCE,
        WORKSPACE,
        DATAFLOW,
        "synthetic-token-never-used-live",
    ] {
        assert!(!diagnostic.contains(secret), "{diagnostic}");
    }
}

#[tokio::test]
async fn explicit_selected_endpoint_is_used_for_initial_state_and_result_requests() {
    let credential = Arc::new(Credential::default());
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap();
    let context = RequestContext::with_endpoint(credential.clone(), selection(), endpoint);
    let mut initial = accepted();
    initial.headers.insert(
        "Location".into(),
        format!("https://dailyapi.fabric.microsoft.com/v1/operations/{OPERATION}"),
    );
    let mut state = succeeded();
    state.headers.insert(
        "Location".into(),
        format!("https://dailyapi.fabric.microsoft.com/v1/operations/{OPERATION}/result"),
    );
    let executor = Executor::new([initial, state, arrow_response()]);
    let mut session = OperationSession::with_context(
        &context,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    assert_eq!(
        execute_query(
            &mut session,
            &target(),
            &request(ArrowVersion::V2),
            ExecuteQueryCompletion::WaitForResult,
        )
        .await
        .unwrap()
        .body,
        BYTES
    );
    assert!(executor.0.lock().unwrap().requests.iter().all(|request| {
        request
            .url
            .starts_with("https://dailyapi.fabric.microsoft.com/v1/")
    }));
}

#[tokio::test]
async fn malformed_state_and_foreign_result_links_retain_handle_without_fetching_result() {
    let states = [
        response(200, b"not-json", &[]),
        response(200, b"[]", &[]),
        response(200, br#"{"status":"Unknown"}"#, &[]),
        response(
            200,
            br#"{"status":"Succeeded"}"#,
            &[(
                "Location",
                &format!("https://foreign.invalid/v1/operations/{OPERATION}/result"),
            )],
        ),
        response(
            200,
            br#"{"status":"Succeeded"}"#,
            &[(
                "Location",
                &format!("{ROOT}/v1/operations/{DATAFLOW}/result"),
            )],
        ),
    ];
    for state in states {
        let credential = Credential::default();
        let selection = selection();
        let executor = Executor::new([accepted(), state]);
        let mut session = OperationSession::new(
            &credential,
            &selection,
            executor.clone(),
            OperationLimits::default(),
            pending(),
        );
        let error = execute_query(
            &mut session,
            &target(),
            &request(ArrowVersion::V1),
            ExecuteQueryCompletion::WaitForResult,
        )
        .await
        .unwrap_err();
        let evidence = error.operation_error().unwrap();
        assert_eq!(evidence.operation_id.as_deref(), Some(OPERATION));
        assert!(matches!(
            evidence.kind,
            OperationErrorKind::InvalidResponse | OperationErrorKind::InvalidLink
        ));
        assert!(evidence.mutation_may_have_occurred);
        assert_eq!(executor.0.lock().unwrap().requests.len(), 2);
    }
}

#[tokio::test]
async fn valid_location_only_acceptance_and_empty_result_are_preserved() {
    let credential = Credential::default();
    let selection = selection();
    let mut initial = accepted();
    initial.headers.remove("x-ms-operation-id");
    let executor = Executor::new([
        initial,
        succeeded(),
        response(200, b"", &[("Content-Type", ARROW)]),
    ]);
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let result = execute_query(
        &mut session,
        &target(),
        &request(ArrowVersion::V1),
        ExecuteQueryCompletion::WaitForResult,
    )
    .await
    .unwrap();
    assert!(result.body.is_empty());
    assert_eq!(session.operation_id(), Some(OPERATION));
    assert_eq!(executor.0.lock().unwrap().requests.len(), 3);
}

#[tokio::test]
async fn wrong_audience_is_rejected_before_credentials_or_dispatch() {
    let credential = Credential::default();
    let selection = CredentialSelection::new("offline", "tenant", Audience::Arm).unwrap();
    let executor = Executor::default();
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let error = execute_query(
        &mut session,
        &target(),
        &request(ArrowVersion::V1),
        ExecuteQueryCompletion::ReturnAccepted,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.operation_error().unwrap().kind,
        OperationErrorKind::InvalidAudience
    );
    assert!(!error.operation_error().unwrap().mutation_may_have_occurred);
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(executor.0.lock().unwrap().requests.is_empty());
}

#[tokio::test]
async fn malformed_external_acceptance_and_reused_wait_session_are_not_replayed() {
    let credential = Credential::default();
    let selection = selection();
    let executor = Executor::new([succeeded(), arrow_response()]);
    let mut session = OperationSession::new(
        &credential,
        &selection,
        executor.clone(),
        OperationLimits::default(),
        pending(),
    );
    let error = wait_for_query(&mut session, arrow_response(), ArrowVersion::V1)
        .await
        .unwrap_err();
    assert_eq!(
        error.operation_error().unwrap().kind,
        OperationErrorKind::InvalidInput
    );
    assert_eq!(credential.0.load(Ordering::SeqCst), 0);
    assert!(executor.0.lock().unwrap().requests.is_empty());
    assert_eq!(
        wait_for_query(&mut session, accepted(), ArrowVersion::V1)
            .await
            .unwrap()
            .body,
        BYTES
    );
    let error = wait_for_query(&mut session, accepted(), ArrowVersion::V1)
        .await
        .unwrap_err();
    assert_eq!(
        error.operation_error().unwrap().kind,
        OperationErrorKind::InvalidInput
    );
    assert_eq!(executor.0.lock().unwrap().requests.len(), 2);
}
