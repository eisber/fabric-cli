// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline terminal-status not-before constraints, sharing the initial budget.
#![forbid(unsafe_code)]
#![deny(unnameable_test_items)]

use async_trait::async_trait;
use fabric_core::*;
use fabric_sdk_operations::complete;
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

struct TestClock {
    start: Instant,
    elapsed: Mutex<Duration>,
    sleeps: Mutex<Vec<Duration>>,
    warnings: Mutex<Vec<&'static str>>,
    cancel_result_wait: bool,
}
impl TestClock {
    fn new(cancel_result_wait: bool) -> Self {
        Self {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
            sleeps: Mutex::new(vec![]),
            warnings: Mutex::new(vec![]),
            cancel_result_wait,
        }
    }
}
#[async_trait]
impl Clock for TestClock {
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
        let count = {
            let mut sleeps = self.sleeps.lock().unwrap();
            sleeps.push(delay);
            sleeps.len()
        };
        if self.cancel_result_wait && count == 2 {
            context.cancellation.cancel();
        }
        context.remaining_at(self.now())?;
        *self.elapsed.lock().unwrap() += delay;
        context.remaining_at(self.now())?;
        Ok(())
    }
}
struct Script<'a> {
    clock: &'a TestClock,
    replies: Mutex<VecDeque<ApiResponse>>,
    calls: Mutex<Vec<(Duration, String)>>,
}
#[async_trait]
impl HttpExecutor for Script<'_> {
    async fn execute(
        &self,
        request: ApiRequest,
        _: Audience,
        _: &OperationContext,
    ) -> Result<ApiResponse, CoreError> {
        assert_eq!(request.method, "GET");
        assert!(request.headers.is_empty());
        assert!(request.body.is_empty());
        self.calls.lock().unwrap().push((
            *self.clock.elapsed.lock().unwrap(),
            request.url.path().to_owned(),
        ));
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("premature or extra GET"))
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
fn initial() -> ApiResponse {
    response(202, "", &[("x-ms-operation-id", "op-1")])
}
fn terminal(hint: &str, json_location: bool) -> ApiResponse {
    if json_location {
        response(
            200,
            r#"{"status":"Succeeded","resourceLocation":"/v1/operations/op-1/result"}"#,
            &[("retry-after", hint)],
        )
    } else {
        response(
            200,
            r#"{"status":"Succeeded"}"#,
            &[
                ("location", "/v1/operations/op-1/result"),
                ("retry-after", hint),
            ],
        )
    }
}

#[tokio::test]
async fn terminal_delta_date_past_zero_and_invalid_hint_never_fetch_early() {
    for json_location in [false, true] {
        for (hint, result_second, warnings) in [
            ("7", 8, 0),
            ("Thu, 01 Jan 1970 00:00:08 GMT", 8, 0),
            ("Thu, 01 Jan 1970 00:00:00 GMT", 1, 0),
            ("0", 1, 0),
            ("invalid synthetic hint", 2, 1),
        ] {
            let clock = TestClock::new(false);
            let script = Script {
                clock: &clock,
                calls: Mutex::new(vec![]),
                replies: Mutex::new(VecDeque::from([
                    terminal(hint, json_location),
                    response(200, "result", &[]),
                ])),
            };
            let result = complete(
                &script,
                &clock,
                Audience::Fabric,
                initial(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(result.body, b"result");
            let calls = script.calls.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].0, Duration::from_secs(1));
            assert_eq!(calls[1].0, Duration::from_secs(result_second));
            assert_eq!(calls[1].1, "/v1/operations/op-1/result");
            assert_eq!(clock.warnings.lock().unwrap().len(), warnings);
        }
    }
}

#[tokio::test]
async fn terminal_not_before_at_or_past_budget_never_sends_result_get() {
    for hint in [
        "599",
        "600",
        "999999999999999999999999999999999",
        "Thu, 01 Jan 1970 00:10:00 GMT",
        "Thu, 01 Jan 1970 00:10:01 GMT",
    ] {
        let clock = TestClock::new(false);
        let script = Script {
            clock: &clock,
            calls: Mutex::new(vec![]),
            replies: Mutex::new(VecDeque::from([terminal(hint, false)])),
        };
        let error = complete(
            &script,
            &clock,
            Audience::Fabric,
            initial(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Timeout);
        assert_eq!(script.calls.lock().unwrap().len(), 1);
        assert_eq!(*clock.elapsed.lock().unwrap(), Duration::from_secs(600));
        assert_eq!(
            *clock.sleeps.lock().unwrap(),
            [1, 599].map(Duration::from_secs)
        );
        assert_eq!(
            error.recovery.unwrap().operation_id.as_deref(),
            Some("op-1")
        );
    }
}

#[tokio::test]
async fn cancellation_during_terminal_not_before_wait_preserves_handle_and_skips_result() {
    let clock = TestClock::new(true);
    let script = Script {
        clock: &clock,
        calls: Mutex::new(vec![]),
        replies: Mutex::new(VecDeque::from([terminal("7", false)])),
    };
    let error = complete(
        &script,
        &clock,
        Audience::Fabric,
        initial(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(script.calls.lock().unwrap().len(), 1);
    assert_eq!(
        *clock.sleeps.lock().unwrap(),
        [1, 7].map(Duration::from_secs)
    );
    assert!(error.recovery.unwrap().outcome_uncertain);
}

#[tokio::test]
async fn unsafe_result_is_validated_before_the_terminal_wait() {
    let clock = TestClock::new(false);
    let script = Script {
        clock: &clock,
        calls: Mutex::new(vec![]),
        replies: Mutex::new(VecDeque::from([response(
            200,
            r#"{"status":"Succeeded"}"#,
            &[
                ("location", "https://evil.invalid/result?secret=synthetic"),
                ("retry-after", "700"),
            ],
        )])),
    };
    let error = complete(
        &script,
        &clock,
        Audience::Fabric,
        initial(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert_eq!(script.calls.lock().unwrap().len(), 1);
    assert_eq!(*clock.sleeps.lock().unwrap(), [Duration::from_secs(1)]);
    assert!(!format!("{error:?}").contains("synthetic"));
}
