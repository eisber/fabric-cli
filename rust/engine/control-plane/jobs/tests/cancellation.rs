// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline bounded cancellation contracts, independent of status/LRO workflows.

#![deny(unnameable_test_items)]

#[allow(dead_code)]
#[path = "../../../../tests/compatibility/jobs/support.rs"]
mod support;

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    http::Method,
};
use fabric_sdk_http::{ExecutionOptions, HttpExecutor};
use fabric_sdk_jobs::{
    Audience, CredentialSelection, EndpointBinding, JobClient, JobError, JobExecutionError,
    JobExecutor, JobFuture, JobLimits, JobRequest, JobRequestHeaders, JobResponse, RequestContext,
    ServiceOrigin, TokenCredential,
    cancellation::{self, CancellationError, NotebookRun},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    error::Error,
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};
use support::{ScriptedHttp, TestCredential, TestJobExecutor, TestTimer, client, response, sdk};

const WORKSPACE: &str = "11111111-1111-1111-1111-111111111111";
const ITEM: &str = "22222222-2222-2222-2222-222222222222";
const INSTANCE: &str = "33333333-3333-3333-3333-333333333333";
const OTHER: &str = "44444444-4444-4444-4444-444444444444";
const HOST: &str = "api.fabric.microsoft.com";

fn run() -> NotebookRun {
    NotebookRun::new(WORKSPACE, ITEM, INSTANCE).unwrap()
}

fn instance_path() -> String {
    format!("/v1/workspaces/{WORKSPACE}/items/{ITEM}/jobs/instances/{INSTANCE}")
}

fn location(host: &str) -> String {
    format!("https://{host}{}", instance_path())
}

fn selection(audience: Audience) -> CredentialSelection {
    CredentialSelection::new("offline", "synthetic", audience).unwrap()
}

fn assert_after_dispatch(error: &CancellationError) {
    assert_eq!(error.target, run());
    assert!(error.mutation_may_have_occurred);
}

#[test]
fn identifiers_require_exact_ascii_guid_shape_in_all_three_positions() {
    for invalid in [
        "",
        "run",
        "11111111111111111111111111111111",
        "{11111111-1111-1111-1111-111111111111}",
        "11111111-1111-1111-1111-11111111111",
        "11111111-1111-1111-1111-1111111111111",
        "11111111_1111-1111-1111-111111111111",
        "1111111-11111-1111-1111-111111111111",
        "g1111111-1111-1111-1111-111111111111",
        "１１１１１１１１-1111-1111-1111-111111111111",
        "11111111-1111-1111-1111-11111111111é",
        "11111111-1111-1111-1111-11111111111/",
        "11111111-1111-1111-1111-11111111111\\",
        "11111111-1111-1111-1111-11111111111%",
        "11111111-1111-1111-1111-11111111111?",
        "11111111-1111-1111-1111-11111111111#",
        "11111111-1111-1111-1111-11111111111 ",
        "11111111-1111-1111-1111-11111111111\n",
        "11111111-1111-1111-1111-11111111111\0",
        " 11111111-1111-1111-1111-111111111111",
        "11111111-1111-1111-1111-111111111111 ",
    ] {
        for ids in [
            [invalid, ITEM, INSTANCE],
            [WORKSPACE, invalid, INSTANCE],
            [WORKSPACE, ITEM, invalid],
        ] {
            assert!(matches!(
                NotebookRun::new(ids[0], ids[1], ids[2]),
                Err(JobError::InvalidIdentifier)
            ));
        }
    }
}

#[test]
fn valid_guid_spelling_and_nil_are_preserved_without_metadata_inference() {
    for id in [
        WORKSPACE,
        "abcdefab-cdef-abcd-efab-cdefabcdefab",
        "ABCDEFAB-CDEF-ABCD-EFAB-CDEFABCDEFAB",
        "00000000-0000-0000-0000-000000000000",
    ] {
        let target = NotebookRun::new(id, id, id).unwrap();
        assert_eq!(target.workspace_id(), id);
        assert_eq!(target.item_id(), id);
        assert_eq!(target.instance_id(), id);
        assert_eq!(target.clone(), target);
        assert!(!format!("{target:?}").contains(id));
    }
    assert_ne!(run(), NotebookRun::new(WORKSPACE, ITEM, OTHER).unwrap());
}

#[tokio::test]
async fn accepted_request_is_one_empty_post_with_fabric_scope_and_raw_response() {
    let location = location(HOST);
    let body = r#"{"status":"InProgress","futureField":{"acceptedOnly":true}}"#;
    let http = ScriptedHttp::new([response(
        202,
        body,
        &[
            ("Location", &location),
            ("Retry-After", "0"),
            ("x-ms-request-id", "synthetic-request"),
            ("x-ms-operation-id", "not-a-job-id"),
        ],
    )]);
    let credential = Arc::new(TestCredential::default());
    let timer = Arc::new(TestTimer::default());
    let client = client(
        credential.clone(),
        http.clone(),
        timer.clone(),
        JobLimits::default(),
    );
    let target = run();
    let receipt = cancellation::request(&client, &target, pending())
        .await
        .unwrap();
    assert_eq!(receipt.target(), &target);
    assert_eq!(receipt.endpoint(), client.context().endpoint());
    assert_eq!(receipt.response().status, 202);
    assert_eq!(receipt.response().body, body.as_bytes());
    assert_eq!(receipt.response().headers["location"], location);
    assert_eq!(receipt.response().headers["retry-after"], "0");
    assert_eq!(
        receipt.response().headers["x-ms-request-id"],
        "synthetic-request"
    );
    assert_eq!(
        receipt.response().headers["x-ms-operation-id"],
        "not-a-job-id"
    );
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert!(timer.waits.lock().unwrap().is_empty());
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method(), Method::Post);
    assert_eq!(requests[0].url().as_str(), format!("{location}/cancel"));
    let azure_core::http::Body::Bytes(body) = requests[0].body() else {
        panic!("cancellation must use a bounded byte body");
    };
    assert!(body.is_empty());
    let debug = format!("{receipt:?}");
    assert!(debug.contains("terminal_cancellation_confirmed: false"));
    assert!(!debug.contains(WORKSPACE));
    assert!(!debug.contains("synthetic-request"));
}

#[tokio::test]
async fn selected_fabric_endpoint_binds_the_post_and_receipt_without_changing_scope() {
    for host in [
        HOST,
        "dailyapi.fabric.microsoft.com",
        "msitapi.fabric.microsoft.com",
    ] {
        let location = location(host);
        let http = ScriptedHttp::new([response(202, "", &[("location", &location)])]);
        let credential: Arc<dyn TokenCredential> = Arc::new(TestCredential::default());
        let endpoint = EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap();
        let timer = Arc::new(TestTimer::default());
        let client = JobClient::with_context(
            RequestContext::with_endpoint(
                credential.clone(),
                selection(Audience::Fabric),
                endpoint.clone(),
            ),
            Arc::new(TestJobExecutor {
                http: HttpExecutor::new(http.clone(), timer.clone(), ExecutionOptions::default())
                    .unwrap()
                    .with_endpoint(endpoint.clone()),
            }),
            JobLimits::default(),
        );
        let receipt = cancellation::request(&client, &run(), pending())
            .await
            .unwrap();
        assert_eq!(receipt.endpoint(), &endpoint);
        assert!(Arc::ptr_eq(client.context().credential(), &credential));
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url().as_str(), format!("{location}/cancel"));
        assert!(timer.waits.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn location_guid_case_matches_identity_in_each_position_without_changing_request_spelling() {
    let spellings = [
        "abcdefab-cdef-abcd-efab-cdefabcdefab",
        "bcdefabc-defa-bcde-fabc-defabcdefabc",
        "cdefabcd-efab-cdef-abcd-efabcdefabcd",
    ]
    .map(|id| {
        [
            id.to_owned(),
            id.to_ascii_uppercase(),
            id.chars()
                .enumerate()
                .map(|(index, character)| {
                    if index % 2 == 0 {
                        character.to_ascii_uppercase()
                    } else {
                        character
                    }
                })
                .collect::<String>(),
        ]
    });
    let targets: [NotebookRun; 3] = std::array::from_fn(|index| {
        NotebookRun::new(
            &spellings[0][index],
            &spellings[1][index],
            &spellings[2][index],
        )
        .unwrap()
    });
    for target in targets {
        for workspace in &spellings[0] {
            for item in &spellings[1] {
                for instance in &spellings[2] {
                    let location = format!(
                        "https://{HOST}/v1/workspaces/{workspace}/items/{item}/jobs/instances/{instance}"
                    );
                    let http = ScriptedHttp::new([response(202, "", &[("location", &location)])]);
                    let timer = Arc::new(TestTimer::default());
                    let client = sdk(http.clone(), timer.clone());
                    let receipt = cancellation::request(&client, &target, pending())
                        .await
                        .unwrap();
                    assert_eq!(receipt.target(), &target);
                    assert_eq!(receipt.response().status, 202);
                    assert_eq!(receipt.response().headers["location"], location);
                    assert_eq!(receipt.endpoint(), client.context().endpoint());
                    let requests = http.requests.lock().unwrap();
                    assert_eq!(requests.len(), 1);
                    assert_eq!(
                        requests[0].url().as_str(),
                        format!(
                            "https://{HOST}/v1/workspaces/{}/items/{}/jobs/instances/{}/cancel",
                            target.workspace_id(),
                            target.item_id(),
                            target.instance_id()
                        )
                    );
                    assert!(timer.waits.lock().unwrap().is_empty());
                }
            }
        }
    }
}

#[tokio::test]
async fn missing_location_retains_the_known_run_and_never_uses_operation_headers() {
    let http = ScriptedHttp::new([response(
        202,
        "not JSON and not a terminal result",
        &[
            ("x-ms-operation-id", "unrelated-operation"),
            ("retry-after", "5"),
        ],
    )]);
    let timer = Arc::new(TestTimer::default());
    let client = sdk(http.clone(), timer.clone());
    let receipt = cancellation::request(&client, &run(), pending())
        .await
        .unwrap();
    assert_eq!(receipt.target(), &run());
    assert_eq!(receipt.target().instance_id(), INSTANCE);
    assert_eq!(
        receipt.response().body,
        b"not JSON and not a terminal result"
    );
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    assert!(timer.waits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn service_rejections_keep_original_error_data_and_are_not_replayed() {
    let value = json!({
        "errorCode": "JobAlreadyCompleted",
        "message": "provider-secret-body",
        "requestId": "body-request-id",
        "moreDetails": [{"errorCode": "FutureDetail", "message": "nested-provider-text"}],
        "futureField": {"preserved": true}
    });
    for status in [401, 403, 404, 409, 429, 500, 502, 503, 504] {
        let http = ScriptedHttp::new([response(
            status,
            &value.to_string(),
            &[
                ("x-ms-public-api-error-code", "ProviderPolicyDenied"),
                ("x-ms-error-code", "LowerPriority"),
                ("x-ms-request-id", "header-request-id"),
                ("retry-after", "0"),
            ],
        )]);
        let timer = Arc::new(TestTimer::default());
        let client = sdk(http.clone(), timer.clone());
        let error = cancellation::request(&client, &run(), pending())
            .await
            .unwrap_err();
        assert_after_dispatch(&error);
        assert!(Error::source(&error).unwrap().is::<JobError>());
        let JobError::Http {
            status: actual_status,
            code,
            service_error,
            raw_service_error,
            headers,
        } = &error.source
        else {
            panic!("must retain the original service failure");
        };
        assert_eq!(*actual_status, status);
        assert_eq!(
            code,
            match status {
                401 | 403 => "ProviderPolicyDenied",
                404 => "NotFound",
                _ => "JobAlreadyCompleted",
            }
        );
        assert!(service_error.is_some());
        assert_eq!(raw_service_error.as_deref(), Some(&value));
        assert_eq!(headers["x-ms-request-id"], "header-request-id");
        assert_eq!(headers["retry-after"], "0");
        let received = error.response.as_ref().unwrap();
        assert_eq!(received.status, status);
        assert_eq!(received.headers, *headers);
        assert_eq!(received.body, value.to_string().as_bytes());
        assert_eq!(&error.endpoint, client.context().endpoint());
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        assert!(timer.waits.lock().unwrap().is_empty());
        for output in [error.to_string(), format!("{error:?}")] {
            for secret in [
                WORKSPACE,
                ITEM,
                INSTANCE,
                "provider-secret-body",
                "nested-provider-text",
                "header-request-id",
                "synthetic-offline-token",
                "ProviderPolicyDenied",
            ] {
                assert!(!output.contains(secret));
            }
        }
    }
}

#[tokio::test]
async fn success_shaped_wrong_statuses_and_redirects_are_not_accepted_or_replayed() {
    for status in [
        200, 201, 203, 204, 205, 206, 207, 208, 226, 301, 302, 307, 308,
    ] {
        let location = location(HOST);
        let http = ScriptedHttp::new([response(
            status,
            "",
            &[("location", &location), ("retry-after", "0")],
        )]);
        let timer = Arc::new(TestTimer::default());
        let client = sdk(http.clone(), timer.clone());
        let error = cancellation::request(&client, &run(), pending())
            .await
            .unwrap_err();
        assert_after_dispatch(&error);
        assert!(matches!(error.source, JobError::UnexpectedStatus(actual) if actual == status));
        let received = error.response.as_ref().unwrap();
        assert_eq!(received.status, status);
        assert_eq!(received.headers["location"], location);
        assert_eq!(&error.endpoint, client.context().endpoint());
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        assert!(timer.waits.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn invalid_audience_or_endpoint_fails_before_credentials_and_dispatch() {
    for (origin, audience) in [
        (ServiceOrigin::Fabric, Audience::OneLake),
        (ServiceOrigin::Fabric, Audience::Arm),
        (ServiceOrigin::OneLake, Audience::Fabric),
        (ServiceOrigin::Arm, Audience::Fabric),
    ] {
        let http = ScriptedHttp::new([]);
        let credential = Arc::new(TestCredential::default());
        let endpoint = EndpointBinding::with_host(origin, HOST).unwrap();
        let client = JobClient::with_context(
            RequestContext::with_endpoint(credential.clone(), selection(audience), endpoint),
            Arc::new(TestJobExecutor {
                http: HttpExecutor::new(
                    http.clone(),
                    Arc::new(TestTimer::default()),
                    ExecutionOptions::default(),
                )
                .unwrap(),
            }),
            JobLimits::default(),
        );
        let error = cancellation::request(&client, &run(), pending())
            .await
            .unwrap_err();
        assert_eq!(error.target, run());
        assert!(!error.mutation_may_have_occurred);
        assert!(error.response.is_none());
        assert_eq!(&error.endpoint, client.context().endpoint());
        assert!(matches!(
            error.source,
            JobError::InvalidAudience | JobError::InvalidEndpoint
        ));
        assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
        assert!(http.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn pre_cancel_wins_before_credentials_or_mutation() {
    let http = ScriptedHttp::new([]);
    let credential = Arc::new(TestCredential::default());
    let client = client(
        credential.clone(),
        http.clone(),
        Arc::new(TestTimer::default()),
        JobLimits::default(),
    );
    let error = cancellation::request(&client, &run(), ready(()))
        .await
        .unwrap_err();
    assert!(matches!(error.source, JobError::Cancelled));
    assert!(!error.mutation_may_have_occurred);
    assert!(error.response.is_none());
    assert_eq!(&error.endpoint, client.context().endpoint());
    assert_eq!(error.target, run());
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(http.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn credential_failure_preserves_the_error_without_dispatch() {
    let http = ScriptedHttp::new([]);
    let credential = Arc::new(TestCredential::default());
    credential.failed.store(true, Ordering::SeqCst);
    let client = client(
        credential.clone(),
        http.clone(),
        Arc::new(TestTimer::default()),
        JobLimits::default(),
    );
    let error = cancellation::request(&client, &run(), pending())
        .await
        .unwrap_err();
    assert!(matches!(error.source, JobError::Credential));
    assert!(!error.mutation_may_have_occurred);
    assert!(error.response.is_none());
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert!(http.requests.lock().unwrap().is_empty());
}

struct Dropped<'a>(&'a AtomicBool);

impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[derive(Debug, Default)]
struct PendingCredential {
    calls: AtomicUsize,
    dropped: AtomicBool,
}

#[async_trait]
impl TokenCredential for PendingCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, ["https://api.fabric.microsoft.com/.default"]);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _dropped = Dropped(&self.dropped);
        pending().await
    }
}

#[tokio::test]
async fn cancellation_during_pending_credentials_drops_local_work_without_mutation() {
    let http = ScriptedHttp::new([]);
    let credential = Arc::new(PendingCredential::default());
    let client = client(
        credential.clone(),
        http.clone(),
        Arc::new(TestTimer::default()),
        JobLimits::default(),
    );
    let cancelled = poll_fn(|cx| {
        if credential.calls.load(Ordering::SeqCst) != 0 {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    });
    let error = cancellation::request(&client, &run(), cancelled)
        .await
        .unwrap_err();
    assert!(matches!(error.source, JobError::Cancelled));
    assert!(!error.mutation_may_have_occurred);
    assert!(error.response.is_none());
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert!(credential.dropped.load(Ordering::SeqCst));
    assert!(http.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_during_pending_http_is_uncertain_and_never_replayed() {
    let http = ScriptedHttp::new([]);
    http.pending.store(true, Ordering::SeqCst);
    let timer = Arc::new(TestTimer::default());
    let client = sdk(http.clone(), timer.clone());
    let cancelled = poll_fn(|cx| {
        if !http.requests.lock().unwrap().is_empty() {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    });
    let error = cancellation::request(&client, &run(), cancelled)
        .await
        .unwrap_err();
    assert!(matches!(error.source, JobError::Cancelled));
    assert_after_dispatch(&error);
    assert!(error.response.is_none());
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    assert!(timer.waits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn lost_response_is_uncertain_with_original_transport_error_and_no_retry() {
    let http = ScriptedHttp::new([]);
    http.fail.store(true, Ordering::SeqCst);
    let timer = Arc::new(TestTimer::default());
    let client = sdk(http.clone(), timer.clone());
    let error = cancellation::request(&client, &run(), pending())
        .await
        .unwrap_err();
    assert_after_dispatch(&error);
    assert!(matches!(
        error.source,
        JobError::Execution(JobExecutionError::Failed)
    ));
    assert!(error.response.is_none());
    assert_eq!(&error.endpoint, client.context().endpoint());
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    assert!(timer.waits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unsafe_or_different_locations_fail_after_dispatch_without_followup() {
    let valid = location(HOST);
    let path = instance_path();
    for location in [
        String::new(),
        format!("https://foreign.invalid{path}"),
        valid.replacen(WORKSPACE, OTHER, 1),
        valid.replacen(ITEM, OTHER, 1),
        valid.replacen(INSTANCE, OTHER, 1),
        format!("https://{HOST}/v1/operations/{INSTANCE}"),
        format!("https://{HOST}/v1/workspaces/{WORKSPACE}/items/{ITEM}"),
        format!("{valid}/cancel"),
        format!("{valid}/"),
        format!("{valid}?"),
        format!("{valid}?jobType=RunNotebook"),
        format!("{valid}#"),
        format!("{valid}#secret"),
        valid.replacen("/jobs/", "/%6aobs/", 1),
        valid.replacen("/jobs/", "/jobs%2F", 1),
        valid.replacen(
            INSTANCE,
            "%33%33%33%33%33%33%33%33-3333-3333-3333-333333333333",
            1,
        ),
        valid.replacen("/jobs/", "/else/../jobs/", 1),
        valid.replacen("/jobs/", "/%2e/jobs/", 1),
        valid.replacen("/jobs/", "//jobs/", 1),
        valid.replacen("/v1/", "/V1/", 1),
        valid.replacen("/workspaces/", "/Workspaces/", 1),
        valid.replacen("/items/", "/Items/", 1),
        valid.replacen("/jobs/", "/Jobs/", 1),
        valid.replacen("/instances/", "/Instances/", 1),
        valid.replacen("https://", "http://", 1),
        valid.replacen("https://", "HTTPS://", 1),
        valid.replacen(HOST, "API.fabric.microsoft.com", 1),
        valid.replacen(HOST, &format!("user@{HOST}"), 1),
        valid.replacen(HOST, &format!("{HOST}:444"), 1),
        valid.replacen(HOST, &format!("{HOST}."), 1),
        valid.replacen(HOST, "api%2efabric.microsoft.com", 1),
        valid.replacen("/v1/", "\\v1\\", 1),
        format!(" {valid}"),
        format!("{valid}\n"),
        path.clone(),
    ] {
        let http = ScriptedHttp::new([response(202, "", &[("location", &location)])]);
        let timer = Arc::new(TestTimer::default());
        let client = sdk(http.clone(), timer.clone());
        let error = cancellation::request(&client, &run(), pending())
            .await
            .unwrap_err();
        assert!(
            matches!(error.source, JobError::InvalidResponse),
            "unexpected error for synthetic Location"
        );
        assert_after_dispatch(&error);
        assert_eq!(http.requests.lock().unwrap().len(), 1);
        assert!(timer.waits.lock().unwrap().is_empty());
    }
}

struct HeaderExecutor {
    response: JobResponse,
    requests: Mutex<Vec<(JobRequestHeaders, NonZeroUsize)>>,
}

#[tokio::test]
async fn invalid_accepted_location_retains_exact_endpoint_and_received_response() {
    let endpoint =
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap();
    let expected = JobResponse {
        status: 202,
        body: b"accepted provider response".to_vec(),
        headers: BTreeMap::from([
            (
                "Location".into(),
                location(endpoint.host()).replacen(INSTANCE, OTHER, 1),
            ),
            ("x-ms-request-id".into(), "received-request-id".into()),
            (
                "x-ms-correlation-request-id".into(),
                "received-correlation-id".into(),
            ),
        ]),
    };
    let executor = Arc::new(HeaderExecutor {
        response: expected.clone(),
        requests: Mutex::new(Vec::new()),
    });
    let client = JobClient::with_context(
        RequestContext::with_endpoint(
            Arc::new(TestCredential::default()),
            selection(Audience::Fabric),
            endpoint.clone(),
        ),
        executor.clone(),
        JobLimits {
            max_response_bytes: NonZeroUsize::new(expected.body.len()).unwrap(),
            ..JobLimits::default()
        },
    );
    let error = cancellation::request(&client, &run(), pending())
        .await
        .unwrap_err();
    assert!(matches!(error.source, JobError::InvalidResponse));
    assert_after_dispatch(&error);
    assert_eq!(error.endpoint, endpoint);
    assert!(!error.response_body_omitted);
    let received = error.response.as_ref().unwrap();
    assert_eq!(received.status, 202);
    assert_eq!(received.body, expected.body);
    assert_eq!(received.headers, expected.headers);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
    for output in [error.to_string(), format!("{error:?}")] {
        assert!(!output.contains("received-request-id"));
        assert!(!output.contains("received-correlation-id"));
        assert!(!output.contains("accepted provider response"));
        assert!(!output.contains(endpoint.host()));
    }
}

impl JobExecutor for HeaderExecutor {
    fn supports_request_headers(&self, _: &JobRequestHeaders) -> bool {
        true
    }

    fn execute<'a>(
        &'a self,
        request: JobRequest<'a>,
    ) -> JobFuture<'a, Result<JobResponse, JobExecutionError>> {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap()
                .push((request.headers().clone(), request.max_response_bytes()));
            Ok(self.response.clone())
        })
    }

    fn wait(&self, _: Duration) -> JobFuture<'_, Result<(), JobExecutionError>> {
        panic!("bounded cancellation must not wait")
    }
}

#[tokio::test]
async fn command_local_client_preserves_headers_limits_and_original_client() {
    let headers = JobRequestHeaders::with_skill(Some("synthetic-cancel-skill")).unwrap();
    let limit = NonZeroUsize::new(321).unwrap();
    let response_headers = BTreeMap::from([
        ("LoCaTiOn".into(), location(HOST)),
        ("X-Ms-Request-Id".into(), "raw-header-spelling".into()),
    ]);
    let executor = Arc::new(HeaderExecutor {
        response: JobResponse {
            status: 202,
            body: b"accepted, not terminal".to_vec(),
            headers: response_headers.clone(),
        },
        requests: Mutex::new(Vec::new()),
    });
    let credential: Arc<dyn TokenCredential> = Arc::new(TestCredential::default());
    let original = JobClient::new(
        credential.clone(),
        selection(Audience::Fabric),
        executor.clone(),
        JobLimits {
            max_response_bytes: limit,
            ..JobLimits::default()
        },
    );
    let scoped = original.with_request_headers(headers.clone()).unwrap();
    let receipt = cancellation::request(&scoped, &run(), pending())
        .await
        .unwrap();
    assert_eq!(receipt.response().headers, response_headers);
    assert_eq!(receipt.response().body, b"accepted, not terminal");
    assert_eq!(
        *executor.requests.lock().unwrap(),
        [(headers.clone(), limit)]
    );
    assert!(original.request_headers().is_empty());
    assert_eq!(scoped.request_headers(), &headers);
    assert!(Arc::ptr_eq(scoped.context().credential(), &credential));
}

#[tokio::test]
async fn every_location_header_is_checked_even_with_different_header_casing() {
    let executor = Arc::new(HeaderExecutor {
        response: JobResponse {
            status: 202,
            body: Vec::new(),
            headers: BTreeMap::from([
                ("Location".into(), location(HOST)),
                ("location".into(), location("dailyapi.fabric.microsoft.com")),
            ]),
        },
        requests: Mutex::new(Vec::new()),
    });
    let client = JobClient::new(
        Arc::new(TestCredential::default()),
        selection(Audience::Fabric),
        executor.clone(),
        JobLimits::default(),
    );
    let error = cancellation::request(&client, &run(), pending())
        .await
        .unwrap_err();
    assert!(matches!(error.source, JobError::InvalidResponse));
    assert_after_dispatch(&error);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn original_response_limit_is_enforced_after_dispatch() {
    let http = ScriptedHttp::new([response(202, "too large", &[])]);
    let client = client(
        Arc::new(TestCredential::default()),
        http.clone(),
        Arc::new(TestTimer::default()),
        JobLimits {
            max_response_bytes: NonZeroUsize::new(1).unwrap(),
            ..JobLimits::default()
        },
    );
    let error = cancellation::request(&client, &run(), pending())
        .await
        .unwrap_err();
    assert!(matches!(
        error.source,
        JobError::Execution(JobExecutionError::ResponseLimit)
    ));
    assert_after_dispatch(&error);
    assert!(error.response.is_none());
    assert!(!error.response_body_omitted);
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn oversized_executor_response_omits_body_but_retains_status_and_headers() {
    let expected = JobResponse {
        status: 202,
        body: vec![b'x'; 64 * 1024],
        headers: BTreeMap::from([("x-ms-request-id".into(), "oversized-request-id".into())]),
    };
    let executor = Arc::new(HeaderExecutor {
        response: expected.clone(),
        requests: Mutex::new(Vec::new()),
    });
    let client = JobClient::new(
        Arc::new(TestCredential::default()),
        selection(Audience::Fabric),
        executor.clone(),
        JobLimits {
            max_response_bytes: NonZeroUsize::new(1).unwrap(),
            ..JobLimits::default()
        },
    );
    let error = cancellation::request(&client, &run(), pending())
        .await
        .unwrap_err();
    assert!(matches!(
        error.source,
        JobError::Execution(JobExecutionError::ResponseLimit)
    ));
    assert_after_dispatch(&error);
    assert_eq!(&error.endpoint, client.context().endpoint());
    assert!(error.response_body_omitted);
    let received = error.response.as_ref().unwrap();
    assert_eq!(received.status, expected.status);
    assert_eq!(received.headers, expected.headers);
    assert!(received.body.is_empty());
    assert_eq!(received.body.capacity(), 0);
    assert_eq!(executor.requests.lock().unwrap().len(), 1);
}

#[test]
fn constructing_then_dropping_an_unpolled_request_has_no_side_effects() {
    let http = ScriptedHttp::new([]);
    let credential = Arc::new(TestCredential::default());
    let client = client(
        credential.clone(),
        http.clone(),
        Arc::new(TestTimer::default()),
        JobLimits::default(),
    );
    let target = run();
    let request = cancellation::request(&client, &target, pending());
    drop(request);
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    assert!(http.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dropping_a_polled_request_drops_pending_credentials_without_http() {
    let http = ScriptedHttp::new([]);
    let credential = Arc::new(PendingCredential::default());
    let client = client(
        credential.clone(),
        http.clone(),
        Arc::new(TestTimer::default()),
        JobLimits::default(),
    );
    let target = run();
    {
        let mut request = pin!(cancellation::request(&client, &target, pending()));
        poll_fn(|cx| {
            assert!(request.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(!credential.dropped.load(Ordering::SeqCst));
    }
    assert!(credential.dropped.load(Ordering::SeqCst));
    assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
    assert!(http.requests.lock().unwrap().is_empty());
}

#[test]
fn failure_diagnostics_do_not_echo_arbitrary_provider_data_or_route() {
    let error = CancellationError {
        source: JobError::Http {
            status: 409,
            code: "synthetic-token-and-route".into(),
            service_error: None,
            raw_service_error: Some(Box::new(Value::String("synthetic-token-and-route".into()))),
            headers: BTreeMap::from([(
                "location".into(),
                format!("{}?synthetic-token-and-route", location(HOST)),
            )]),
        },
        target: run(),
        endpoint: EndpointBinding::public(ServiceOrigin::Fabric),
        response: Some(JobResponse {
            status: 409,
            body: b"synthetic-token-and-route".to_vec(),
            headers: BTreeMap::from([(
                "x-ms-request-id".into(),
                "synthetic-token-and-route".into(),
            )]),
        }),
        response_body_omitted: false,
        mutation_may_have_occurred: true,
    };
    for text in [error.to_string(), format!("{error:?}")] {
        assert!(!text.contains("synthetic-token-and-route"));
        assert!(!text.contains(WORKSPACE));
        assert!(!text.contains(HOST));
    }
    assert!(Error::source(&error).unwrap().is::<JobError>());
}
