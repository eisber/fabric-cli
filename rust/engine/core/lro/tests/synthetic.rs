// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline protocol evidence; no credentials, sockets, or wall-clock sleeps.
#![deny(unnameable_test_items)]
#![forbid(unsafe_code)]
use async_trait::async_trait;
use fabric_core::*;
use fabric_sdk_operations::{LroConfig, complete, complete_with_config};
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
        assert_eq!(*clock.sleeps.lock().unwrap(), vec![Duration::from_secs(2)]);
    }
}
#[tokio::test]
async fn fabric_terminal_status_preserves_ordinary_reserved_marker_objects() {
    for body in [
        r#"{"status":"Succeeded","metadata":{"$serde_json::private::Number":"not a number"}}"#,
        r#"{"$serde_json::private::Number":"not a number","status":"Completed"}"#,
        r#"{"status":"Succeeded","metadata":[{"$serde_json::private::Number":"not a number"}]}"#,
    ] {
        let terminal = response(200, body, &[("x-test-terminal", "retained")]);
        let http = Script::new(vec![terminal.clone()]);
        let result = complete(
            &http,
            &FakeClock::new(),
            Audience::Fabric,
            accepted(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.body, terminal.body);
        assert_eq!(result.headers, terminal.headers);
        assert_eq!(result.status, terminal.status);
        assert_eq!(http.calls.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn fabric_non_job_default_schedule_stays_at_twenty_seconds() {
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
        [2, 7, 20, 20, 20, 20, 20, 20].map(Duration::from_secs)
    );
}

#[tokio::test]
async fn configured_intervals_do_not_shorten_server_hints_for_any_protocol() {
    for audience in [Audience::Fabric, Audience::Azure] {
        for job in [false, true] {
            if job && audience == Audience::Azure {
                continue;
            }
            let location = if job {
                "/v1/workspaces/workspace/items/item/jobs/instances/job"
            } else if audience == Audience::Azure {
                "/status"
            } else {
                "/v1/operations/op-1"
            };
            let handle_header = if audience == Audience::Azure {
                "azure-asyncoperation"
            } else {
                "location"
            };
            let pending = if job {
                r#"{"status":"InProgress"}"#
            } else {
                r#"{"status":"Running"}"#
            };
            let terminal = if job {
                r#"{"status":"Completed"}"#
            } else {
                r#"{"status":"Succeeded"}"#
            };
            let http = Script::new(vec![
                response(200, pending, &[("retry-after", "12")]),
                response(200, pending, &[("retry-after", "invalid")]),
                response(200, terminal, &[]),
            ]);
            let clock = FakeClock::new();
            complete_with_config(
                &http,
                &clock,
                audience,
                response(202, "", &[(handle_header, location), ("retry-after", "2")]),
                CancellationToken::new(),
                LroConfig::new(Some(Duration::from_secs(5)), None).unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(
                *clock.sleeps.lock().unwrap(),
                [5, 12, 5].map(Duration::from_secs)
            );
            assert_eq!(clock.warnings.lock().unwrap().len(), 1);
        }
    }
}

struct InspectingPoll<'a> {
    clock: &'a FakeClock,
    replies: Mutex<VecDeque<(Duration, ApiResponse)>>,
    budgets: Mutex<Vec<Duration>>,
}
#[async_trait]
impl HttpExecutor for InspectingPoll<'_> {
    async fn execute(
        &self,
        request: ApiRequest,
        _: Audience,
        context: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        assert_eq!(request.method, "GET");
        assert!(request.body.is_empty());
        assert!(request.headers.is_empty());
        self.budgets
            .lock()
            .unwrap()
            .push(context.remaining_at(self.clock.now())?);
        let (elapsed, response) = self.replies.lock().unwrap().pop_front().unwrap();
        *self.clock.elapsed.lock().unwrap() += elapsed;
        Ok(response)
    }
}

#[tokio::test]
async fn no_default_timeout_but_each_poll_and_result_request_is_bounded() {
    for elapsed in [239, 241] {
        let clock = FakeClock::new();
        let http = InspectingPoll {
            clock: &clock,
            budgets: Mutex::new(vec![]),
            replies: Mutex::new(VecDeque::from([
                (
                    Duration::ZERO,
                    response(200, r#"{"status":"Running"}"#, &[]),
                ),
                (
                    Duration::ZERO,
                    response(
                        200,
                        r#"{"status":"Succeeded"}"#,
                        &[
                            ("location", "/v1/operations/op-1/result"),
                            ("retry-after", "700"),
                        ],
                    ),
                ),
                (
                    Duration::from_secs(elapsed),
                    response(200, r#"{"done":true}"#, &[]),
                ),
            ])),
        };
        let result = complete(
            &http,
            &clock,
            Audience::Fabric,
            response(
                202,
                "",
                &[("x-ms-operation-id", "op-1"), ("retry-after", "700")],
            ),
            CancellationToken::new(),
        )
        .await;
        assert!(*clock.elapsed.lock().unwrap() > Duration::from_secs(600));
        assert_eq!(*http.budgets.lock().unwrap(), [Duration::from_secs(240); 3]);
        if elapsed == 239 {
            assert_eq!(result.unwrap().body, br#"{"done":true}"#);
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.kind, ErrorKind::Timeout);
            assert!(error.recovery.is_some());
        }
    }
}

#[tokio::test]
async fn configured_timeout_spans_polls_and_skips_get_at_deadline() {
    let clock = FakeClock::new();
    let http = InspectingPoll {
        clock: &clock,
        budgets: Mutex::new(vec![]),
        replies: Mutex::new(VecDeque::from([(
            Duration::from_secs(3),
            response(200, r#"{"status":"Running"}"#, &[]),
        )])),
    };
    let error = complete_with_config(
        &http,
        &clock,
        Audience::Fabric,
        accepted(),
        CancellationToken::new(),
        LroConfig::new(None, Some(Duration::from_secs(10))).unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert_eq!(*http.budgets.lock().unwrap(), [Duration::from_secs(8)]);
    assert_eq!(
        *clock.sleeps.lock().unwrap(),
        [2, 5].map(Duration::from_secs)
    );
    assert_eq!(*clock.elapsed.lock().unwrap(), Duration::from_secs(10));
    assert!(error.recovery.is_some());
}

#[tokio::test]
async fn unrepresentable_server_delay_fails_safely_without_polling_or_timer_overflow() {
    let http = Script::new(vec![]);
    let error = complete(
        &http,
        &FakeClock::new(),
        Audience::Fabric,
        response(
            202,
            "",
            &[
                ("x-ms-operation-id", "op-1"),
                ("retry-after", "999999999999999999999999999999999"),
            ],
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert!(error.recovery.is_some());
    assert!(http.calls.lock().unwrap().is_empty());
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
    assert_eq!(*clock.sleeps.lock().unwrap(), vec![Duration::from_secs(2)]);
    let clock = FakeClock::new();
    let http = Script::new(vec![]);
    let mut initial = accepted();
    initial.headers.insert("Retry-After".into(), "601".into());
    let error = complete_with_config(
        &http,
        &clock,
        Audience::Fabric,
        initial,
        CancellationToken::new(),
        LroConfig::new(None, Some(Duration::from_secs(600))).unwrap(),
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
        (
            400,
            r#"{"error":{"code":"OperationHasNoResult"},"metadata":{"$serde_json::private::Number":"not a number"}}"#,
            true,
        ),
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
    let clock = FakeClock::new();
    let result = complete(
        &http,
        &clock,
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
    assert_eq!(*clock.sleeps.lock().unwrap(), [Duration::from_secs(10)]);
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
    let clock = FakeClock::new();
    let result = complete(
        &http,
        &clock,
        Audience::Azure,
        initial,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.status, 200);
    assert_eq!(result.body, b"original");
    assert_eq!(*clock.sleeps.lock().unwrap(), [Duration::from_secs(10); 3]);
}

#[tokio::test]
async fn arm_status_and_location_use_each_hint_or_ten_seconds_without_staged_backoff() {
    for status_protocol in [false, true] {
        let header = if status_protocol {
            "azure-asyncoperation"
        } else {
            "location"
        };
        let initial = response(
            202,
            "original",
            &[(header, "/status"), ("retry-after", "7")],
        );
        let pending_status = if status_protocol { 200 } else { 202 };
        let http = Script::new(vec![
            response(
                pending_status,
                r#"{"status":"Running"}"#,
                &[("retry-after", "3")],
            ),
            response(pending_status, r#"{"status":"Running"}"#, &[]),
            response(
                pending_status,
                r#"{"status":"Running"}"#,
                &[("retry-after", "invalid")],
            ),
            response(pending_status, r#"{"status":"Running"}"#, &[]),
            success(),
        ]);
        let clock = FakeClock::new();
        let result = complete(
            &http,
            &clock,
            Audience::Azure,
            initial.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            *clock.sleeps.lock().unwrap(),
            [7, 3, 10, 10, 10].map(Duration::from_secs)
        );
        assert_eq!(clock.warnings.lock().unwrap().len(), 1);
        assert_eq!(http.calls.lock().unwrap().len(), 5);
        assert_eq!(result.status, 200);
        assert_eq!(result.body, initial.body);
        assert_eq!(result.headers, initial.headers);
    }
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
                &[("location", "/v1/items/id/jobs/instances/job")],
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
        let clock = FakeClock::new();
        let result = complete_with_config(
            &http,
            &clock,
            audience,
            initial.clone(),
            CancellationToken::new(),
            LroConfig::new(
                Some(Duration::from_millis(500)),
                Some(Duration::from_secs(1)),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(result.status, initial.status);
        assert_eq!(result.body, initial.body);
        assert!(http.calls.lock().unwrap().is_empty());
        assert!(clock.sleeps.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn job_polling_uses_each_response_hint_or_ten_seconds_without_backoff() {
    let location = "/v1/workspaces/workspace/items/item/jobs/instances/job?token=CANARY";
    let initial = response(202, "", &[("location", location), ("retry-after", "7")]);
    let terminal = response(
        200,
        r#"{"status":"Completed","metadata":{"$serde_json::private::Number":"ordinary"},"failureReason":null}"#,
        &[
            ("x-terminal", "retained"),
            ("location", "https://evil.invalid/ignored"),
        ],
    );
    let http = Script::new(vec![
        response(200, r#"{"status":"NotStarted"}"#, &[("retry-after", "3")]),
        response(200, r#"{"status":"InProgress"}"#, &[]),
        response(
            200,
            r#"{"status":"InProgress"}"#,
            &[("retry-after", "invalid")],
        ),
        terminal.clone(),
    ]);
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
    assert_eq!(result.status, terminal.status);
    assert_eq!(result.body, terminal.body);
    assert_eq!(result.headers, terminal.headers);
    assert_eq!(
        *clock.sleeps.lock().unwrap(),
        [7, 3, 10, 10].map(Duration::from_secs)
    );
    assert_eq!(
        *clock.warnings.lock().unwrap(),
        vec!["Invalid Retry-After header; using 10-second job polling interval"]
    );
    let requests = http.calls.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(
        requests.iter().all(|request| request.url.path()
            == "/v1/workspaces/workspace/items/item/jobs/instances/job")
    );
    assert!(
        requests
            .iter()
            .all(|request| request.url.query() == Some("token=CANARY"))
    );
}

#[tokio::test]
async fn job_initial_delay_and_terminal_statuses_follow_job_protocol() {
    let location = "/v1/workspaces/workspace/items/item/jobs/instances/job";
    for (hint, seconds) in [
        (None, 10),
        (Some("60"), 60),
        (Some("Thu, 01 Jan 1970 00:00:07 GMT"), 7),
        (Some("invalid"), 10),
        (Some("0"), 0),
    ] {
        for (status, expected) in [
            ("Completed", None),
            ("Deduped", None),
            ("Failed", Some(ErrorKind::Service)),
            ("Cancelled", Some(ErrorKind::Service)),
            ("Unknown", Some(ErrorKind::Protocol)),
        ] {
            let mut headers = vec![("location", location)];
            if let Some(hint) = hint {
                headers.push(("retry-after", hint));
            }
            let terminal = response(200, &format!(r#"{{"status":"{status}"}}"#), &[]);
            let http = Script::new(vec![terminal.clone()]);
            let clock = FakeClock::new();
            let result = complete(
                &http,
                &clock,
                Audience::Fabric,
                response(202, "", &headers),
                CancellationToken::new(),
            )
            .await;
            if let Some(kind) = expected {
                let error = result.unwrap_err();
                assert_eq!(error.kind, kind);
                let recovery = error.recovery.unwrap();
                assert_eq!(recovery.operation_id, None);
                assert_eq!(
                    recovery.status_url,
                    format!("https://api.fabric.microsoft.com{location}")
                );
            } else {
                assert_eq!(result.unwrap().body, terminal.body);
            }
            assert_eq!(
                *clock.sleeps.lock().unwrap(),
                vec![Duration::from_secs(seconds)]
            );
            assert_eq!(http.calls.lock().unwrap().len(), 1);
        }
    }
    for terminal in [
        response(200, "", &[]),
        response(200, "invalid", &[]),
        response(200, "{}", &[]),
        response(202, r#"{"status":"Completed"}"#, &[]),
        response(302, "", &[("location", "https://evil.invalid")]),
    ] {
        let http = Script::new(vec![terminal]);
        let error = complete(
            &http,
            &FakeClock::new(),
            Audience::Fabric,
            response(202, "", &[("location", location)]),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(error.recovery.is_some());
        assert_eq!(http.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn job_budget_and_local_cancellation_preserve_query_redacted_recovery() {
    let location = "/v1/workspaces/workspace/items/item/jobs/instances/job?secret=CANARY";
    for cancel in [false, true] {
        let cancellation = CancellationToken::new();
        if cancel {
            cancellation.cancel();
        }
        let http = Script::new(vec![]);
        let clock = FakeClock::new();
        let error = complete_with_config(
            &http,
            &clock,
            Audience::Fabric,
            response(202, "", &[("location", location), ("retry-after", "601")]),
            cancellation,
            LroConfig::new(None, Some(Duration::from_secs(600))).unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.kind,
            if cancel {
                ErrorKind::Cancelled
            } else {
                ErrorKind::Timeout
            }
        );
        let recovery = error.recovery.unwrap();
        assert!(recovery.outcome_uncertain);
        assert_eq!(recovery.operation_id, None);
        assert!(!recovery.status_url.contains("CANARY"));
        assert!(http.calls.lock().unwrap().is_empty());
        assert_eq!(
            *clock.sleeps.lock().unwrap(),
            if cancel {
                vec![]
            } else {
                vec![Duration::from_secs(600)]
            }
        );
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
