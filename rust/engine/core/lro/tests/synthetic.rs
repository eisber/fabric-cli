// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline protocol evidence; no credentials, sockets, or wall-clock sleeps.
#![deny(unnameable_test_items)]
#![forbid(unsafe_code)]
use async_trait::async_trait;
use fabric_core::*;
use fabric_sdk_operations::complete;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

struct FakeClock {
    start: Instant,
    elapsed: Mutex<Duration>,
    sleeps: Mutex<Vec<Duration>>,
    warnings: Mutex<Vec<&'static str>>,
}
impl FakeClock {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
            sleeps: Mutex::new(Vec::new()),
            warnings: Mutex::new(Vec::new()),
        }
    }
}
#[async_trait]
impl Clock for FakeClock {
    fn now(&self) -> Instant {
        self.start + *self.elapsed.lock().unwrap()
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + *self.elapsed.lock().unwrap()
    }
    fn diagnostic(&self, message: &'static str) {
        self.warnings.lock().unwrap().push(message);
    }
    async fn sleep(&self, delay: Duration, context: &OperationContext) -> Result<(), CoreError> {
        context.remaining_at(self.now())?;
        self.sleeps.lock().unwrap().push(delay);
        *self.elapsed.lock().unwrap() += delay;
        context.remaining_at(self.now())?;
        Ok(())
    }
}
struct Script {
    responses: Mutex<VecDeque<ApiResponse>>,
    calls: Mutex<Vec<ApiRequest>>,
}
struct ExpiringResult<'a> {
    clock: &'a FakeClock,
    count: Mutex<usize>,
}
#[async_trait]
impl HttpExecutor for ExpiringResult<'_> {
    async fn execute(
        &self,
        request: ApiRequest,
        _: Audience,
        _: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        assert_eq!(request.method, "GET");
        let mut count = self.count.lock().unwrap();
        *count += 1;
        if *count == 1 {
            Ok(response(
                200,
                r#"{"status":"Succeeded"}"#,
                &[("location", "/v1/operations/op-1/result")],
            ))
        } else {
            *self.clock.elapsed.lock().unwrap() += Duration::from_secs(600);
            Ok(response(200, r#"{"done":true}"#, &[]))
        }
    }
}
impl Script {
    fn new(responses: Vec<ApiResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            calls: Mutex::new(Vec::new()),
        }
    }
}
#[async_trait]
impl HttpExecutor for Script {
    async fn execute(
        &self,
        request: ApiRequest,
        _: Audience,
        _: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        assert_eq!(
            request.method, "GET",
            "reconciliation must never replay a mutation"
        );
        assert!(request.body.is_empty());
        assert!(
            request.headers.is_empty(),
            "mutation headers must not reach poll/result GET"
        );
        self.calls.lock().unwrap().push(request);
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP attempt"))
    }
}
fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> ApiResponse {
    ApiResponse {
        status,
        body: body.as_bytes().to_vec(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}
fn accepted() -> ApiResponse {
    response(202, "initial", &[("x-ms-operation-id", "op-1")])
}
fn success() -> ApiResponse {
    response(200, r#"{"status":"Succeeded"}"#, &[])
}

#[tokio::test]
async fn fabric_id_and_location_only_handles_poll_actual_initial() {
    for initial in [
        accepted(),
        response(
            202,
            "",
            &[(
                "Location",
                "https://api.fabric.microsoft.com/v1/operations/op-1",
            )],
        ),
    ] {
        let http = Script::new(vec![success()]);
        let clock = FakeClock::new();
        let result = complete(
            &http,
            &clock,
            Audience::Fabric,
            initial,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.body, success().body);
        assert_eq!(http.calls.lock().unwrap().len(), 1);
        assert_eq!(*clock.sleeps.lock().unwrap(), vec![Duration::from_secs(1)]);
    }
}
#[tokio::test]
async fn fixed_exponential_delays_have_no_sixty_second_cap() {
    let mut replies = vec![response(200, r#"{"status":"Running"}"#, &[]); 7];
    replies.push(success());
    let http = Script::new(replies);
    let clock = FakeClock::new();
    complete(
        &http,
        &clock,
        Audience::Fabric,
        accepted(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        *clock.sleeps.lock().unwrap(),
        [1, 2, 4, 8, 16, 32, 64, 128].map(Duration::from_secs)
    );
}
#[tokio::test]
async fn retry_after_delta_date_invalid_and_budget_are_not_before() {
    for value in ["7", "Thu, 01 Jan 1970 00:00:07 GMT"] {
        let clock = FakeClock::new();
        let http = Script::new(vec![success()]);
        let mut initial = accepted();
        initial.headers.insert("Retry-After".into(), value.into());
        complete(
            &http,
            &clock,
            Audience::Fabric,
            initial,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(*clock.sleeps.lock().unwrap(), vec![Duration::from_secs(7)]);
    }
    let clock = FakeClock::new();
    let http = Script::new(vec![success()]);
    let mut initial = accepted();
    initial
        .headers
        .insert("Retry-After".into(), "not a date".into());
    complete(
        &http,
        &clock,
        Audience::Fabric,
        initial,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(clock.warnings.lock().unwrap().len(), 1);
    assert_eq!(*clock.sleeps.lock().unwrap(), vec![Duration::from_secs(1)]);
    let clock = FakeClock::new();
    let http = Script::new(vec![]);
    let mut initial = accepted();
    initial.headers.insert("Retry-After".into(), "601".into());
    let error = complete(
        &http,
        &clock,
        Audience::Fabric,
        initial,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert_eq!(
        error.recovery.unwrap().operation_id.as_deref(),
        Some("op-1")
    );
    assert!(http.calls.lock().unwrap().is_empty());
    assert_eq!(
        *clock.sleeps.lock().unwrap(),
        vec![Duration::from_secs(600)]
    );
}
#[tokio::test]
async fn fabric_result_and_only_documented_no_result_condition() {
    for (status, body, expected_ok) in [
        (200, r#"{"answer":42}"#, true),
        (400, r#"{"errorCode":"OperationHasNoResult"}"#, true),
        (400, r#"{"errorCode":"Unrelated"}"#, false),
    ] {
        let terminal = response(
            200,
            r#"{"status":"Completed"}"#,
            &[("location", "/v1/operations/op-1/result")],
        );
        let http = Script::new(vec![terminal.clone(), response(status, body, &[])]);
        let result = complete(
            &http,
            &FakeClock::new(),
            Audience::Fabric,
            accepted(),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(result.is_ok(), expected_ok);
        if status == 400 && expected_ok {
            assert_eq!(result.unwrap().body, terminal.body);
        }
        assert_eq!(http.calls.lock().unwrap().len(), 2);
    }
}
#[tokio::test]
async fn unsafe_result_is_rejected_with_recovery_without_forwarding_bearer() {
    let http = Script::new(vec![response(
        200,
        r#"{"status":"Succeeded"}"#,
        &[("location", "https://evil.invalid/result?secret=value")],
    )]);
    let error = complete(
        &http,
        &FakeClock::new(),
        Audience::Fabric,
        accepted(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert_eq!(http.calls.lock().unwrap().len(), 1);
    assert!(!format!("{error:?}").contains("secret"));
    assert!(error.recovery.unwrap().outcome_uncertain);
}
#[tokio::test]
async fn failed_cancelled_malformed_unknown_and_redirect_are_terminal() {
    for reply in [
        response(200, r#"{"status":"Failed"}"#, &[]),
        response(200, r#"{"status":"Canceled"}"#, &[]),
        response(200, r#"{"status":"Cancelled"}"#, &[]),
        response(200, r#"{"status":"Surprise"}"#, &[]),
        response(200, "invalid", &[]),
        response(302, "", &[("location", "https://evil.invalid")]),
    ] {
        let http = Script::new(vec![reply]);
        let error = complete(
            &http,
            &FakeClock::new(),
            Audience::Fabric,
            accepted(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(error.recovery.is_some());
        assert_eq!(http.calls.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn cancellation_prevents_status_request_and_preserves_handle() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let http = Script::new(vec![]);
    let error = complete(
        &http,
        &FakeClock::new(),
        Audience::Fabric,
        accepted(),
        cancel,
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert!(error.recovery.is_some());
    assert!(http.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn arm_prefers_asyncoperation_and_preserves_initial_body_headers() {
    let initial = response(
        201,
        r#"{"original":"resource"}"#,
        &[
            (
                "Azure-AsyncOperation",
                "https://management.azure.com/operations/one",
            ),
            ("Location", "https://evil.invalid/ignored"),
        ],
    );
    let http = Script::new(vec![success()]);
    let result = complete(
        &http,
        &FakeClock::new(),
        Audience::Azure,
        initial.clone(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.status, 200);
    assert_eq!(result.body, initial.body);
    assert_eq!(result.headers, initial.headers);
    assert_eq!(http.calls.lock().unwrap()[0].url.path(), "/operations/one");
}
#[tokio::test]
async fn arm_location_pending_then_200_does_not_require_status_json() {
    let initial = response(
        202,
        "original",
        &[("location", "https://management.azure.com/status/one")],
    );
    let http = Script::new(vec![
        response(201, "", &[]),
        response(202, "", &[]),
        response(200, "done", &[]),
    ]);
    let result = complete(
        &http,
        &FakeClock::new(),
        Audience::Azure,
        initial,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.status, 200);
    assert_eq!(result.body, b"original");
}
#[tokio::test]
async fn raw_and_job_responses_do_not_invent_protocols() {
    for (audience, initial) in [
        (Audience::Fabric, response(202, "accepted", &[])),
        (
            Audience::Fabric,
            response(
                202,
                "job",
                &[("location", "/v1/workspaces/id/items/id/jobs/instances/job")],
            ),
        ),
        (
            Audience::Fabric,
            response(202, "resource", &[("location", "/v1/workspaces/id")]),
        ),
        (Audience::Storage, accepted()),
        (Audience::PowerBi, accepted()),
        (Audience::Fabric, response(500, "raw failure", &[])),
    ] {
        let http = Script::new(vec![]);
        let result = complete(
            &http,
            &FakeClock::new(),
            audience,
            initial.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.status, initial.status);
        assert_eq!(result.body, initial.body);
        assert!(http.calls.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn conflict_and_unsafe_poll_are_rejected_before_get() {
    for headers in [
        BTreeMap::from([
            ("x-ms-operation-id".into(), "different".into()),
            ("location".into(), "/v1/operations/op-1".into()),
        ]),
        BTreeMap::from([("location".into(), "https://evil.invalid/op".into())]),
    ] {
        let http = Script::new(vec![]);
        assert!(
            complete(
                &http,
                &FakeClock::new(),
                Audience::Fabric,
                ApiResponse {
                    status: 202,
                    headers,
                    body: vec![]
                },
                CancellationToken::new()
            )
            .await
            .is_err()
        );
        assert!(http.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn final_result_cannot_escape_polling_deadline() {
    let clock = FakeClock::new();
    let http = ExpiringResult {
        clock: &clock,
        count: Mutex::new(0),
    };
    let error = complete(
        &http,
        &clock,
        Audience::Fabric,
        accepted(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert_eq!(*http.count.lock().unwrap(), 2);
    assert!(error.recovery.unwrap().outcome_uncertain);
}
