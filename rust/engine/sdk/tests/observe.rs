// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Direct, offline returned-handle API coverage. No submission or live credentials.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenRequestOptions},
    error::ErrorKind,
    time::OffsetDateTime,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
    TokenCredential,
    observe::{Observation, ObserveErrorKind, ObserveOptions, ReadHandle, observe},
};
use fabric_sdk_operations::{
    OperationErrorKind, OperationExecutionError, OperationExecutor, OperationRequest,
    OperationResponse,
};
use fabric_sdk_workspaces::mutations::ResourceMethod;
use serde_json::json;
use std::{
    collections::VecDeque,
    future::{Future, pending, poll_fn, ready},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

const ROOT: &str = "https://dailyapi.fabric.microsoft.com";
const OPERATION: &str = "/v1/operations/operation_1";
const RESULT: &str = "https://dailyapi.fabric.microsoft.com/v1/operations/operation_1/result";
const JOB: &str = "/v1/workspaces/workspace_1/items/item_1/jobs/instances/job_1";
const TOKEN: &str = "synthetic-observation-test-only";

#[derive(Debug, Default)]
struct Credential {
    scopes: Mutex<Vec<Vec<String>>>,
    fail: bool,
    block: Option<Arc<AtomicBool>>,
    dropped: AtomicUsize,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.scopes
            .lock()
            .unwrap()
            .push(scopes.iter().map(|scope| (*scope).to_owned()).collect());
        if let Some(flag) = &self.block {
            let _guard = Dropped(&self.dropped);
            block_and_cancel(flag).await;
        }
        if self.fail {
            return Err(azure_core::Error::with_message(
                ErrorKind::Other,
                "private credential diagnostic",
            ));
        }
        Ok(AccessToken::new(TOKEN, OffsetDateTime::UNIX_EPOCH))
    }
}

#[derive(Clone, Debug)]
struct Recorded {
    url: String,
    bound: usize,
}

#[derive(Default)]
struct Executor {
    replies: Mutex<VecDeque<Result<OperationResponse, OperationExecutionError>>>,
    requests: Mutex<Vec<Recorded>>,
    waits: Mutex<Vec<Duration>>,
    block_request: Option<(usize, Arc<AtomicBool>)>,
    block_wait: Option<Arc<AtomicBool>>,
    wait_error: Option<OperationExecutionError>,
    dropped_requests: AtomicUsize,
    dropped_waits: AtomicUsize,
}

impl OperationExecutor for Executor {
    fn execute<'a>(
        &'a self,
        request: OperationRequest<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<OperationResponse, OperationExecutionError>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(request.method(), ResourceMethod::Get);
            assert!(request.body().is_empty());
            assert_eq!(request.token().token.secret(), TOKEN);
            let count = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(Recorded {
                    url: request.url().to_string(),
                    bound: request.max_response_bytes().get(),
                });
                requests.len()
            };
            if let Some((number, flag)) = &self.block_request
                && count == *number
            {
                let _guard = Dropped(&self.dropped_requests);
                block_and_cancel(flag).await;
            }
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected GET")
        })
    }

    fn wait(
        &self,
        delay: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<(), OperationExecutionError>> + Send + '_>> {
        Box::pin(async move {
            assert!(!delay.is_zero(), "observation must never busy-loop");
            self.waits.lock().unwrap().push(delay);
            if let Some(flag) = &self.block_wait {
                let _guard = Dropped(&self.dropped_waits);
                block_and_cancel(flag).await;
            }
            self.wait_error.map_or(Ok(()), Err)
        })
    }
}

struct Dropped<'a>(&'a AtomicUsize);

impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

async fn block_and_cancel(flag: &AtomicBool) {
    poll_fn(|cx| {
        flag.store(true, Ordering::SeqCst);
        cx.waker().wake_by_ref();
        Poll::<()>::Pending
    })
    .await
}

fn cancellation(flag: Arc<AtomicBool>) -> impl Future<Output = ()> + Send {
    let mut completed = false;
    poll_fn(move |_| {
        assert!(!completed, "completed cancellation was polled again");
        if flag.load(Ordering::SeqCst) {
            completed = true;
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
}

fn selection(audience: Audience) -> CredentialSelection {
    CredentialSelection::new("offline-account", "offline-tenant", audience).unwrap()
}

fn client(credential: Arc<Credential>) -> Client {
    Client::with_context(RequestContext::with_endpoint(
        credential,
        selection(Audience::Fabric),
        EndpointBinding::with_host(ServiceOrigin::Fabric, "dailyapi.fabric.microsoft.com").unwrap(),
    ))
}

fn operation() -> ReadHandle {
    ReadHandle::Operation {
        operation_id: "operation_1".into(),
    }
}

fn job() -> ReadHandle {
    ReadHandle::Job {
        workspace_id: "workspace_1".into(),
        item_id: "item_1".into(),
        job_id: "job_1".into(),
    }
}

fn options() -> ObserveOptions {
    ObserveOptions {
        wait: false,
        result: false,
        max_requests: NonZeroUsize::new(4).unwrap(),
        max_response_bytes: NonZeroUsize::new(4096).unwrap(),
        max_delay: Duration::from_secs(5),
    }
}

fn response(status: u16, body: &[u8], headers: &[(&str, &str)]) -> OperationResponse {
    OperationResponse {
        status,
        body: body.to_vec(),
        headers: headers
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
    }
}

fn state(status: &str, headers: &[(&str, &str)]) -> OperationResponse {
    response(
        200,
        &serde_json::to_vec(&json!({"status": status, "futureField": null})).unwrap(),
        headers,
    )
}

fn executor(responses: impl IntoIterator<Item = OperationResponse>) -> Executor {
    Executor {
        replies: Mutex::new(responses.into_iter().map(Ok).collect()),
        ..Default::default()
    }
}

fn assert_requests(executor: &Executor, paths: &[&str]) {
    let requests = executor.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.url.as_str())
            .collect::<Vec<_>>(),
        paths
            .iter()
            .map(|path| format!("{ROOT}{path}"))
            .collect::<Vec<_>>()
    );
    assert!(requests.iter().all(|request| request.bound == 4096));
}

fn assert_scopes(credential: &Credential, count: usize) {
    let scopes = credential.scopes.lock().unwrap();
    assert_eq!(scopes.len(), count);
    assert!(
        scopes
            .iter()
            .all(|scopes| scopes == &[Audience::Fabric.scope()])
    );
}

#[tokio::test]
async fn read_once_neither_waits_nor_follows_location_or_acceptance_headers() {
    let credential = Arc::new(Credential::default());
    let executor = executor([state(
        "Running",
        &[
            ("Location", "https://untrusted.invalid/another-resource"),
            ("x-ms-operation-id", "another-operation"),
            ("Retry-After", "not-a-delay"),
        ],
    )]);
    let observed = observe(
        &client(credential.clone()),
        &executor,
        &operation(),
        options(),
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(observed.handle, operation());
    assert_eq!(
        observed.raw_status,
        json!({"status":"Running","futureField":null})
    );
    assert!(!observed.terminal);
    assert!(!observed.work_succeeded);
    assert!(observed.result.is_none());
    assert_eq!(observed.requests, 1);
    assert!(executor.waits.lock().unwrap().is_empty());
    assert_requests(&executor, &[OPERATION]);
    assert_scopes(&credential, 1);
}

#[tokio::test]
async fn successful_status_http_is_not_successful_work() {
    for handle in [operation(), job()] {
        for status in ["Failed", "Cancelled"] {
            let raw = json!({
                "status":status,
                "error":{"errorCode":"Denied","message":"private service message"},
                "futureField":{"keep":true}
            });
            let executor = executor([response(200, &serde_json::to_vec(&raw).unwrap(), &[])]);
            let observed = observe(
                &client(Arc::new(Credential::default())),
                &executor,
                &handle,
                options(),
                pending(),
            )
            .await
            .unwrap();
            assert_eq!(observed.raw_status, raw);
            assert!(observed.terminal);
            assert!(!observed.work_succeeded);
            assert_eq!(observed.requests, 1);
            assert!(observed.result.is_none());
        }
    }
}

#[tokio::test]
async fn finite_status_domains_are_specific_to_the_handle_kind() {
    for (handle, statuses) in [
        (
            operation(),
            vec![
                ("NotStarted", false, false),
                ("Running", false, false),
                ("Succeeded", true, true),
                ("Completed", false, false),
                ("InProgress", false, false),
                ("Deduped", false, false),
                ("FutureState", false, false),
            ],
        ),
        (
            job(),
            vec![
                ("NotStarted", false, false),
                ("InProgress", false, false),
                ("Completed", true, true),
                ("Deduped", true, false),
                ("Succeeded", false, false),
                ("Running", false, false),
                ("FutureState", false, false),
            ],
        ),
    ] {
        for (status, terminal, succeeded) in statuses {
            let executor = executor([state(status, &[])]);
            let outcome = observe(
                &client(Arc::new(Credential::default())),
                &executor,
                &handle,
                options(),
                pending(),
            )
            .await;
            let known = match &handle {
                ReadHandle::Operation { .. } => {
                    matches!(status, "NotStarted" | "Running" | "Succeeded")
                }
                ReadHandle::Job { .. } => {
                    matches!(
                        status,
                        "NotStarted" | "InProgress" | "Completed" | "Deduped"
                    )
                }
            };
            if known {
                let observed = outcome.unwrap();
                assert_eq!(observed.terminal, terminal);
                assert_eq!(observed.work_succeeded, succeeded);
            } else {
                let error = outcome.unwrap_err();
                assert_eq!(error.kind, ObserveErrorKind::UnknownState);
                let last = error.last_observation.unwrap();
                assert_eq!(last.raw_status["status"], status);
                assert!(!last.terminal);
                assert!(!last.work_succeeded);
            }
            assert!(executor.waits.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn waits_use_only_the_exact_handle_and_finite_caller_timer() {
    for (handle, active, success, path) in [
        (operation(), "Running", "Succeeded", OPERATION),
        (job(), "InProgress", "Completed", JOB),
    ] {
        let credential = Arc::new(Credential::default());
        let executor = executor([
            state("NotStarted", &[("retry-after", "2")]),
            state(
                active,
                &[
                    ("Retry-After", "3"),
                    ("Location", "https://untrusted.invalid/never-follow"),
                ],
            ),
            state(success, &[]),
        ]);
        let observed = observe(
            &client(credential.clone()),
            &executor,
            &handle,
            ObserveOptions {
                wait: true,
                max_requests: NonZeroUsize::new(3).unwrap(),
                ..options()
            },
            pending(),
        )
        .await
        .unwrap();
        assert!(observed.terminal);
        assert!(observed.work_succeeded);
        assert_eq!(observed.requests, 3);
        assert_eq!(
            *executor.waits.lock().unwrap(),
            [Duration::from_secs(2), Duration::from_secs(3)]
        );
        assert_requests(&executor, &[path, path, path]);
        assert_scopes(&credential, 3);
    }
}

#[tokio::test]
async fn result_is_explicit_advertised_and_preserves_binary_bytes_and_media_type() {
    let credential = Arc::new(Credential::default());
    let bytes = [0, 255, 13, 10, 128, 42];
    let executor = executor([
        state("Succeeded", &[("location", RESULT)]),
        response(
            200,
            &bytes,
            &[("content-type", "application/octet-stream; custom=value")],
        ),
    ]);
    let observed = observe(
        &client(credential.clone()),
        &executor,
        &operation(),
        ObserveOptions {
            result: true,
            max_requests: NonZeroUsize::new(2).unwrap(),
            ..options()
        },
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(observed.raw_status["status"], "Succeeded");
    assert_eq!(observed.requests, 2);
    let result = observed.result.unwrap();
    assert_eq!(result.bytes, bytes);
    assert_eq!(
        result.media_type.as_deref(),
        Some("application/octet-stream; custom=value")
    );
    assert!(executor.waits.lock().unwrap().is_empty());
    assert_requests(&executor, &[OPERATION, "/v1/operations/operation_1/result"]);
    assert_scopes(&credential, 2);
}

#[tokio::test]
async fn waiting_then_result_counts_initial_poll_and_result_exactly() {
    let executor = executor([
        state("Running", &[]),
        state("Succeeded", &[("Location", RESULT)]),
        response(200, b"", &[]),
    ]);
    let observed = observe(
        &client(Arc::new(Credential::default())),
        &executor,
        &operation(),
        ObserveOptions {
            wait: true,
            result: true,
            max_requests: NonZeroUsize::new(3).unwrap(),
            ..options()
        },
        pending(),
    )
    .await
    .unwrap();
    assert_eq!(observed.requests, 3);
    let result = observed.result.unwrap();
    assert!(result.bytes.is_empty());
    assert!(result.media_type.is_none());
    assert_eq!(*executor.waits.lock().unwrap(), [Duration::from_secs(1)]);
    assert_requests(
        &executor,
        &[OPERATION, OPERATION, "/v1/operations/operation_1/result"],
    );
}

#[tokio::test]
async fn result_does_not_imply_wait_or_assume_an_operation_has_a_result() {
    for (status, expected) in [
        ("Running", ObserveErrorKind::ResultNotReady),
        ("Succeeded", ObserveErrorKind::ResultNotAdvertised),
    ] {
        let executor = executor([state(status, &[])]);
        let error = observe(
            &client(Arc::new(Credential::default())),
            &executor,
            &operation(),
            ObserveOptions {
                result: true,
                ..options()
            },
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, expected);
        assert_eq!(error.requests, 1);
        assert_eq!(error.last_observation.unwrap().raw_status["status"], status);
        assert!(executor.waits.lock().unwrap().is_empty());
        assert_requests(&executor, &[OPERATION]);
    }
}

#[tokio::test]
async fn advertised_result_is_not_fetched_without_explicit_demand() {
    let executor = executor([state("Succeeded", &[("Location", RESULT)])]);
    let observed = observe(
        &client(Arc::new(Credential::default())),
        &executor,
        &operation(),
        options(),
        pending(),
    )
    .await
    .unwrap();
    assert!(observed.work_succeeded);
    assert!(observed.result.is_none());
    assert_eq!(observed.requests, 1);
    assert_requests(&executor, &[OPERATION]);
}

#[tokio::test]
async fn job_result_is_unsupported_before_any_credential_or_http() {
    for wait in [false, true] {
        let credential = Arc::new(Credential::default());
        let executor = Executor::default();
        let error = observe(
            &client(credential.clone()),
            &executor,
            &job(),
            ObserveOptions {
                wait,
                result: true,
                ..options()
            },
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ObserveErrorKind::Unsupported);
        assert_eq!(error.handle, job());
        assert_eq!(error.requests, 0);
        assert!(error.last_observation.is_none());
        assert!(error.operation_error.operation_id.is_none());
        assert_scopes(&credential, 0);
        assert_requests(&executor, &[]);
    }
}

#[tokio::test]
async fn explicit_result_remote_error_is_never_a_success_shaped_fallback() {
    let raw = json!({"errorCode":"OperationHasNoResult","message":"private service diagnostic"});
    let executor = executor([
        state("Succeeded", &[("Location", RESULT)]),
        response(
            400,
            &serde_json::to_vec(&raw).unwrap(),
            &[("x-ms-public-api-error-code", "OperationHasNoResult")],
        ),
    ]);
    let error = observe(
        &client(Arc::new(Credential::default())),
        &executor,
        &operation(),
        ObserveOptions {
            result: true,
            ..options()
        },
        pending(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        &error.operation_error.kind,
        OperationErrorKind::Http { status: 400, code: Some(code), .. }
            if code == "OperationHasNoResult"
    ));
    assert_eq!(
        error.operation_error.raw_service_error.as_deref(),
        Some(&raw)
    );
    assert_eq!(error.requests, 2);
    let last = error.last_observation.unwrap();
    assert_eq!(last.raw_status["status"], "Succeeded");
    assert!(last.result.is_none());
    assert_eq!(last.requests, 2);
}

#[tokio::test]
async fn result_demand_on_failed_work_errors_without_polling_or_result_get() {
    let executor = executor([state("Failed", &[("Location", RESULT)])]);
    let error = observe(
        &client(Arc::new(Credential::default())),
        &executor,
        &operation(),
        ObserveOptions {
            result: true,
            ..options()
        },
        pending(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        &error.operation_error.kind,
        OperationErrorKind::OperationFailed { status, .. } if status == "Failed"
    ));
    assert_eq!(error.requests, 1);
    assert_requests(&executor, &[OPERATION]);
    assert!(executor.waits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unsuccessful_terminal_wait_preserves_typed_work_error_and_last_state() {
    for (handle, status) in [
        (operation(), "Failed"),
        (operation(), "Cancelled"),
        (job(), "Failed"),
        (job(), "Cancelled"),
        (job(), "Deduped"),
    ] {
        let raw_error = json!({"errorCode":"PrivateCode","message":"private work diagnostic"});
        let raw = json!({"status":status,"error":raw_error});
        let executor = executor([response(200, &serde_json::to_vec(&raw).unwrap(), &[])]);
        let error = observe(
            &client(Arc::new(Credential::default())),
            &executor,
            &handle,
            ObserveOptions {
                wait: true,
                ..options()
            },
            pending(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            &error.operation_error.kind,
            OperationErrorKind::OperationFailed { status: actual, error: Some(_) }
                if actual == status
        ));
        assert_eq!(
            error.operation_error.raw_service_error.as_deref(),
            Some(&raw_error)
        );
        assert!(!error.operation_error.mutation_may_have_occurred);
        assert_eq!(error.handle, handle);
        assert_eq!(error.requests, 1);
        let last = error.last_observation.unwrap();
        assert_eq!(last.raw_status, raw);
        assert!(last.terminal);
        assert!(!last.work_succeeded);
    }
}

#[tokio::test]
async fn all_dispatches_share_one_budget_and_exhaustion_never_sleeps_or_gets_tokens() {
    for budget in [1, 2, 3] {
        let credential = Arc::new(Credential::default());
        let executor = executor((0..budget).map(|_| state("Running", &[])));
        let error = observe(
            &client(credential.clone()),
            &executor,
            &operation(),
            ObserveOptions {
                wait: true,
                max_requests: NonZeroUsize::new(budget).unwrap(),
                ..options()
            },
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.operation_error.kind, OperationErrorKind::Limit);
        assert_eq!(error.requests, budget);
        assert_eq!(
            error.last_observation.unwrap().raw_status["status"],
            "Running"
        );
        assert_eq!(executor.waits.lock().unwrap().len(), budget - 1);
        assert_scopes(&credential, budget);
        assert_requests(&executor, &vec![OPERATION; budget]);
    }
    let credential = Arc::new(Credential::default());
    let executor = executor([state("Succeeded", &[("Location", RESULT)])]);
    let error = observe(
        &client(credential.clone()),
        &executor,
        &operation(),
        ObserveOptions {
            result: true,
            max_requests: NonZeroUsize::new(1).unwrap(),
            ..options()
        },
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.operation_error.kind, OperationErrorKind::Limit);
    assert_eq!(error.requests, 1);
    assert!(error.last_observation.unwrap().work_succeeded);
    assert_scopes(&credential, 1);
    assert_requests(&executor, &[OPERATION]);
}

#[tokio::test]
async fn malformed_zero_and_overflowing_retry_after_never_busy_loop() {
    for value in [
        "",
        "0",
        "000",
        "-1",
        "+1",
        " 1",
        "1 ",
        "1.5",
        "Thu, 05 Nov 2026 22:30:00 GMT",
        "18446744073709551616",
    ] {
        let executor = executor([state("Running", &[("Retry-After", value)])]);
        let error = observe(
            &client(Arc::new(Credential::default())),
            &executor,
            &operation(),
            ObserveOptions {
                wait: true,
                ..options()
            },
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ObserveErrorKind::InvalidRetryAfter, "{value:?}");
        assert_eq!(error.requests, 1);
        assert!(executor.waits.lock().unwrap().is_empty());
        assert_requests(&executor, &[OPERATION]);
    }
}

#[tokio::test]
async fn retry_after_exceeding_bound_is_an_error_not_an_unbounded_or_clamped_sleep() {
    for headers in [
        vec![("Retry-After", "6")],
        vec![("Retry-After", "18446744073709551615")],
        vec![],
    ] {
        let executor = executor([state("Running", &headers)]);
        let error = observe(
            &client(Arc::new(Credential::default())),
            &executor,
            &operation(),
            ObserveOptions {
                wait: true,
                max_delay: Duration::from_millis(500),
                ..options()
            },
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.operation_error.kind, OperationErrorKind::Limit);
        assert_eq!(error.requests, 1);
        assert!(executor.waits.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn duplicate_case_insensitive_protocol_headers_are_not_arbitrarily_selected() {
    let executor = executor([state(
        "Running",
        &[("Retry-After", "1"), ("retry-after", "2")],
    )]);
    let error = observe(
        &client(Arc::new(Credential::default())),
        &executor,
        &operation(),
        ObserveOptions {
            wait: true,
            ..options()
        },
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.operation_error.kind,
        OperationErrorKind::InvalidResponse
    );
    assert!(executor.waits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn duplicate_result_advertisement_and_media_headers_are_explicit_errors() {
    for duplicate_media in [false, true] {
        let responses = if duplicate_media {
            vec![
                state("Succeeded", &[("Location", RESULT)]),
                response(
                    200,
                    b"opaque bytes",
                    &[
                        ("Content-Type", "application/json"),
                        ("content-type", "text/plain"),
                    ],
                ),
            ]
        } else {
            vec![state(
                "Succeeded",
                &[("Location", RESULT), ("location", RESULT)],
            )]
        };
        let executor = executor(responses);
        let error = observe(
            &client(Arc::new(Credential::default())),
            &executor,
            &operation(),
            ObserveOptions {
                result: true,
                ..options()
            },
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.operation_error.kind,
            OperationErrorKind::InvalidResponse
        );
        assert_eq!(error.requests, if duplicate_media { 2 } else { 1 });
        assert!(error.last_observation.unwrap().work_succeeded);
    }
}

#[tokio::test]
async fn invalid_identifiers_in_every_position_fail_before_credentials() {
    let mut invalid = vec![
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "%2f",
        "a?b",
        "a#b",
        "a b",
        "a\nb",
        "a\0b",
        "é",
        "https://evil.invalid",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    invalid.push("a".repeat(257));
    for id in invalid {
        for handle in [
            ReadHandle::Operation {
                operation_id: id.clone(),
            },
            ReadHandle::Job {
                workspace_id: id.clone(),
                item_id: "item_1".into(),
                job_id: "job_1".into(),
            },
            ReadHandle::Job {
                workspace_id: "workspace_1".into(),
                item_id: id.clone(),
                job_id: "job_1".into(),
            },
            ReadHandle::Job {
                workspace_id: "workspace_1".into(),
                item_id: "item_1".into(),
                job_id: id.clone(),
            },
        ] {
            let credential = Arc::new(Credential::default());
            let executor = Executor::default();
            let error = observe(
                &client(credential.clone()),
                &executor,
                &handle,
                options(),
                pending(),
            )
            .await
            .unwrap_err();
            assert_eq!(error.operation_error.kind, OperationErrorKind::InvalidInput);
            assert_eq!(error.requests, 0);
            assert_eq!(error.handle, handle);
            assert_scopes(&credential, 0);
            assert_requests(&executor, &[]);
        }
    }
}

#[tokio::test]
async fn invalid_audience_service_and_wait_bound_fail_before_credentials() {
    for (audience, service, max_delay, expected) in [
        (
            Audience::Arm,
            ServiceOrigin::Fabric,
            Duration::from_secs(1),
            OperationErrorKind::InvalidAudience,
        ),
        (
            Audience::Fabric,
            ServiceOrigin::PowerBi,
            Duration::from_secs(1),
            OperationErrorKind::InvalidInput,
        ),
        (
            Audience::Fabric,
            ServiceOrigin::Fabric,
            Duration::ZERO,
            OperationErrorKind::InvalidInput,
        ),
    ] {
        let credential = Arc::new(Credential::default());
        let client = Client::with_context(RequestContext::with_endpoint(
            credential.clone(),
            selection(audience),
            EndpointBinding::public(service),
        ));
        let executor = Executor::default();
        let error = observe(
            &client,
            &executor,
            &operation(),
            ObserveOptions {
                wait: true,
                max_delay,
                ..options()
            },
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.operation_error.kind, expected);
        assert_eq!(error.requests, 0);
        assert_scopes(&credential, 0);
        assert_requests(&executor, &[]);
    }
}

#[tokio::test]
async fn malformed_and_nonstring_statuses_fail_explicitly_retaining_parsed_json() {
    for value in [
        json!({}),
        json!({"status":null}),
        json!({"status":42}),
        json!({"status":["Succeeded"]}),
        json!([]),
        json!("Succeeded"),
    ] {
        let executor = executor([response(200, &serde_json::to_vec(&value).unwrap(), &[])]);
        let error = observe(
            &client(Arc::new(Credential::default())),
            &executor,
            &operation(),
            options(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ObserveErrorKind::InvalidState);
        assert_eq!(error.last_observation.unwrap().raw_status, value);
    }
    let executor = executor([response(200, b"not-json", &[])]);
    let error = observe(
        &client(Arc::new(Credential::default())),
        &executor,
        &operation(),
        options(),
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ObserveErrorKind::InvalidState);
    assert!(error.last_observation.is_none());
    assert_eq!(error.requests, 1);
}

#[tokio::test]
async fn actual_http_status_is_never_synthesized_or_reconciled() {
    for status in [201, 202, 204, 301, 401, 403, 404, 429, 500] {
        let executor = executor([
            response(
                status,
                br#"{"status":"Succeeded","errorCode":"Denied"}"#,
                &[("Location", RESULT), ("Retry-After", "1")],
            ),
            state("Succeeded", &[]),
        ]);
        let error = observe(
            &client(Arc::new(Credential::default())),
            &executor,
            &operation(),
            ObserveOptions {
                wait: true,
                ..options()
            },
            pending(),
        )
        .await
        .unwrap_err();
        if matches!(status, 201 | 204) {
            assert_eq!(error.kind, ObserveErrorKind::UnexpectedStatus { status });
        } else {
            assert!(matches!(
                error.operation_error.kind,
                OperationErrorKind::Http { status: actual, .. } if actual == status
            ));
        }
        assert_eq!(error.requests, 1);
        assert!(error.last_observation.is_none());
        assert!(executor.waits.lock().unwrap().is_empty());
        assert_eq!(executor.replies.lock().unwrap().len(), 1);
        assert_requests(&executor, &[OPERATION]);
    }
}

#[tokio::test]
async fn result_endpoint_cannot_turn_accepted_or_no_content_into_a_result() {
    for status in [201, 202, 204] {
        let executor = executor([
            state("Succeeded", &[("Location", RESULT)]),
            response(status, b"", &[("Location", RESULT), ("Retry-After", "1")]),
        ]);
        let error = observe(
            &client(Arc::new(Credential::default())),
            &executor,
            &operation(),
            ObserveOptions {
                result: true,
                ..options()
            },
            pending(),
        )
        .await
        .unwrap_err();
        if status == 202 {
            assert!(matches!(
                error.operation_error.kind,
                OperationErrorKind::Http { status: 202, .. }
            ));
        } else {
            assert_eq!(error.kind, ObserveErrorKind::UnexpectedStatus { status });
        }
        assert_eq!(error.requests, 2);
        assert!(error.last_observation.unwrap().work_succeeded);
        assert!(executor.waits.lock().unwrap().is_empty());
        assert_requests(&executor, &[OPERATION, "/v1/operations/operation_1/result"]);
    }
}

#[tokio::test]
async fn result_advertisements_are_bound_to_exact_selected_host_handle_and_path() {
    for advertised in [
        "https://api.fabric.microsoft.com/v1/operations/operation_1/result",
        "https://evil.invalid/v1/operations/operation_1/result",
        "http://dailyapi.fabric.microsoft.com/v1/operations/operation_1/result",
        "https://dailyapi.fabric.microsoft.com/v1/operations/other/result",
        "https://dailyapi.fabric.microsoft.com/v1/operations/operation_1",
        "https://dailyapi.fabric.microsoft.com/v1/operations/operation_1/result?",
        "https://dailyapi.fabric.microsoft.com/v1/operations/operation_1/result#",
        "https://user@dailyapi.fabric.microsoft.com/v1/operations/operation_1/result",
        "https://dailyapi.fabric.microsoft.com/v1/operations/operation_1/../operation_1/result",
        "/v1/operations/operation_1/result",
        "",
    ] {
        let executor = executor([state("Succeeded", &[("Location", advertised)])]);
        let error = observe(
            &client(Arc::new(Credential::default())),
            &executor,
            &operation(),
            ObserveOptions {
                result: true,
                ..options()
            },
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.operation_error.kind, OperationErrorKind::InvalidLink);
        assert_eq!(error.requests, 1);
        assert!(error.last_observation.unwrap().work_succeeded);
        assert_requests(&executor, &[OPERATION]);
    }
}

#[tokio::test]
async fn response_bounds_cover_status_and_result_even_with_nonconforming_executor() {
    for result in [false, true] {
        let credential = Arc::new(Credential::default());
        let too_large = response(200, &vec![b'x'; 4097], &[]);
        let executor = executor(if result {
            vec![state("Succeeded", &[("Location", RESULT)]), too_large]
        } else {
            vec![too_large]
        });
        let error = observe(
            &client(credential.clone()),
            &executor,
            &operation(),
            ObserveOptions {
                result,
                ..options()
            },
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.operation_error.kind, OperationErrorKind::Limit);
        assert_eq!(error.requests, if result { 2 } else { 1 });
        assert_eq!(error.last_observation.is_some(), result);
        assert_scopes(&credential, error.requests);
        assert!(
            executor
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.bound == 4096)
        );
    }
}

#[tokio::test]
async fn executor_errors_and_retry_exhaustion_are_not_retried_by_observation() {
    for shared_error in [
        OperationExecutionError::Failed,
        OperationExecutionError::TimedOut,
        OperationExecutionError::Cancelled,
        OperationExecutionError::RetriesExhausted,
        OperationExecutionError::ResponseLimit,
    ] {
        let executor = Executor {
            replies: Mutex::new([Err(shared_error), Ok(state("Succeeded", &[]))].into()),
            ..Default::default()
        };
        let error = observe(
            &client(Arc::new(Credential::default())),
            &executor,
            &operation(),
            options(),
            pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.operation_error.kind,
            if shared_error == OperationExecutionError::ResponseLimit {
                OperationErrorKind::Limit
            } else {
                OperationErrorKind::Execution(shared_error)
            }
        );
        assert_eq!(
            error.operation_error.operation_id.as_deref(),
            Some("operation_1")
        );
        assert!(!error.operation_error.mutation_may_have_occurred);
        assert_eq!(error.requests, 1);
        assert_eq!(executor.replies.lock().unwrap().len(), 1);
        assert!(executor.waits.lock().unwrap().is_empty());
        assert_requests(&executor, &[OPERATION]);
    }
}

#[tokio::test]
async fn wait_executor_error_preserves_last_state_without_another_request() {
    let executor = Executor {
        replies: Mutex::new([Ok(state("Running", &[]))].into()),
        wait_error: Some(OperationExecutionError::TimedOut),
        ..Default::default()
    };
    let error = observe(
        &client(Arc::new(Credential::default())),
        &executor,
        &operation(),
        ObserveOptions {
            wait: true,
            ..options()
        },
        pending(),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.operation_error.kind,
        OperationErrorKind::Execution(OperationExecutionError::TimedOut)
    );
    assert_eq!(
        error.last_observation.unwrap().raw_status["status"],
        "Running"
    );
    assert_eq!(error.requests, 1);
    assert_requests(&executor, &[OPERATION]);
}

#[tokio::test]
async fn cancellation_before_work_acquires_no_credential_or_http() {
    let credential = Arc::new(Credential::default());
    let executor = Executor::default();
    let error = observe(
        &client(credential.clone()),
        &executor,
        &operation(),
        options(),
        ready(()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.operation_error.kind, OperationErrorKind::Cancelled);
    assert_eq!(error.requests, 0);
    assert!(error.last_observation.is_none());
    assert_scopes(&credential, 0);
    assert_requests(&executor, &[]);
}

#[tokio::test]
async fn cancellation_during_initial_poll_and_result_get_drops_local_http_only() {
    for (blocked, first, result) in [
        (1, "Running", false),
        (2, "Running", false),
        (2, "Succeeded", true),
    ] {
        let flag = Arc::new(AtomicBool::new(false));
        let credential = Arc::new(Credential::default());
        let executor = Executor {
            replies: Mutex::new([Ok(state(first, &[("Location", RESULT)]))].into()),
            block_request: Some((blocked, flag.clone())),
            ..Default::default()
        };
        let error = observe(
            &client(credential.clone()),
            &executor,
            &operation(),
            ObserveOptions {
                wait: true,
                result,
                ..options()
            },
            cancellation(flag),
        )
        .await
        .unwrap_err();
        assert_eq!(error.operation_error.kind, OperationErrorKind::Cancelled);
        assert_eq!(error.requests, blocked);
        assert_eq!(error.handle, operation());
        assert!(!error.operation_error.mutation_may_have_occurred);
        assert_eq!(executor.dropped_requests.load(Ordering::SeqCst), 1);
        assert_scopes(&credential, blocked);
        if blocked == 1 {
            assert!(error.last_observation.is_none());
        } else {
            assert_eq!(error.last_observation.unwrap().raw_status["status"], first);
        }
        let second = if result {
            "/v1/operations/operation_1/result"
        } else {
            OPERATION
        };
        let expected = if blocked == 1 {
            vec![OPERATION]
        } else {
            vec![OPERATION, second]
        };
        assert_requests(&executor, &expected);
    }
}

#[tokio::test]
async fn observation_deadline_during_sleep_never_cancels_remote_job() {
    let flag = Arc::new(AtomicBool::new(false));
    let executor = Executor {
        replies: Mutex::new([Ok(state("InProgress", &[("Retry-After", "2")]))].into()),
        block_wait: Some(flag.clone()),
        ..Default::default()
    };
    let error = observe(
        &client(Arc::new(Credential::default())),
        &executor,
        &job(),
        ObserveOptions {
            wait: true,
            ..options()
        },
        cancellation(flag),
    )
    .await
    .unwrap_err();
    assert_eq!(error.operation_error.kind, OperationErrorKind::Cancelled);
    assert_eq!(error.requests, 1);
    assert_eq!(error.handle, job());
    assert_eq!(executor.dropped_waits.load(Ordering::SeqCst), 1);
    assert_eq!(*executor.waits.lock().unwrap(), [Duration::from_secs(2)]);
    assert_eq!(
        error.last_observation.unwrap().raw_status["status"],
        "InProgress"
    );
    assert_requests(&executor, &[JOB]);
}

#[tokio::test]
async fn credentials_fail_or_cancel_without_counting_a_request_or_leaking_diagnostics() {
    for block in [false, true] {
        let flag = Arc::new(AtomicBool::new(false));
        let credential = Arc::new(Credential {
            fail: !block,
            block: block.then_some(flag.clone()),
            ..Default::default()
        });
        let executor = Executor::default();
        let error = observe(
            &client(credential.clone()),
            &executor,
            &operation(),
            options(),
            cancellation(flag),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.operation_error.kind,
            if block {
                OperationErrorKind::Cancelled
            } else {
                OperationErrorKind::Credential
            }
        );
        assert_eq!(error.requests, 0);
        assert_eq!(
            credential.dropped.load(Ordering::SeqCst),
            usize::from(block)
        );
        assert!(!format!("{error:?} {error}").contains("private credential diagnostic"));
        assert_scopes(&credential, 1);
        assert_requests(&executor, &[]);
    }
}

#[test]
fn debug_and_display_redact_handles_status_result_bytes_and_typed_service_evidence() {
    let handle = ReadHandle::Operation {
        operation_id: "private-handle".into(),
    };
    let observation = Observation {
        handle: handle.clone(),
        raw_status: json!({"status":"private-status","error":"private-body"}),
        terminal: false,
        work_succeeded: false,
        result: Some(fabric_sdk::observe::ObservedResult {
            bytes: b"private-result".to_vec(),
            media_type: Some("private-media".into()),
        }),
        requests: 1,
    };
    let error = fabric_sdk::observe::ObserveError {
        kind: ObserveErrorKind::Operation,
        operation_error: Box::new(fabric_sdk_operations::OperationError {
            kind: OperationErrorKind::Http {
                status: 403,
                code: Some("private-code".into()),
                error: None,
            },
            operation_id: Some("private-handle".into()),
            mutation_may_have_occurred: false,
            raw_service_error: Some(Box::new(json!({"error":"private-provider"}))),
        }),
        handle: handle.clone(),
        last_observation: Some(Box::new(observation.clone())),
        requests: 1,
    };
    let debug = format!("{handle:?} {observation:?} {error:?} {error}");
    for secret in [
        "private-handle",
        "private-status",
        "private-body",
        "private-result",
        "private-media",
        "private-code",
        "private-provider",
    ] {
        assert!(!debug.contains(secret), "{secret} was exposed");
    }
    let job = ReadHandle::Job {
        workspace_id: "private-workspace".into(),
        item_id: "private-item".into(),
        job_id: "private-job".into(),
    };
    let debug = format!("{job:?}");
    assert!(!debug.contains("private-"));
}
