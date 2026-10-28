// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use azure_core::http::Method;
use fabric_sdk_api::{OperationErrorKind, Request as ApiRequest};

#[derive(Default)]
struct Time {
    waits: Mutex<Vec<u64>>,
}
impl ExecutionTimer for Time {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay == Duration::from_secs(240) {
            Box::pin(pending())
        } else {
            self.waits.lock().unwrap().push(delay.as_secs());
            Box::pin(ready(()))
        }
    }
}

#[tokio::test]
async fn accepted_acl_label_response_uses_actual_shared_operation_engine_and_never_replays_initial_write()
 {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../../tests/compatibility/access/operation_valid_cases.json"
    ))
    .unwrap();
    for case in fixtures["cases"].as_array().unwrap() {
        let headers: Vec<_> = case["headers"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap()))
            .collect();
        let mut replies = vec![reply(202, r#"{"accepted": true}"#, &headers)];
        replies.extend(case["replies"].as_array().unwrap().iter().map(|r| {
            let headers: Vec<_> = r["headers"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str().unwrap()))
                .collect();
            let body = r["raw_body"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| r["body"].to_string());
            reply(r["status"].as_u64().unwrap() as u16, &body, &headers)
        }));
        let wire = Arc::new(Wire {
            requests: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into()),
        });
        let time = Arc::new(Time::default());
        let executor = Arc::new(
            HttpExecutor::new(wire.clone(), time.clone(), ExecutionOptions::default()).unwrap(),
        );
        let api = ApiClient::new(
            Arc::new(Credential::default()),
            CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
            Arc::new(fabric_cli_resources::ResourceHttpExecutor(executor)),
        );
        let request = ApiRequest::fabric(Method::Post, &["admin", "items", "bulkSetLabels"])
            .unwrap()
            .with_json(&json!({"items":[]}))
            .unwrap();
        let result = api
            .send_completed(&request, NonZeroUsize::new(4096).unwrap(), pending())
            .await;
        let expected = &case["expected"];
        if expected["error"].is_null() {
            let response = result.unwrap_or_else(|e| panic!("{}: {e}", case["name"]));
            assert_eq!(
                response.status,
                expected["status"].as_u64().unwrap() as u16,
                "{}",
                case["name"]
            );
            if expected["text"] == "" {
                assert!(response.body.is_empty());
            } else {
                assert_eq!(
                    serde_json::from_slice::<Value>(&response.body).unwrap(),
                    serde_json::from_str::<Value>(expected["text"].as_str().unwrap()).unwrap()
                );
            }
        } else {
            let ApiError::Operation(error) = result.unwrap_err() else {
                panic!("shared reconciliation evidence required");
            };
            assert!(error.mutation_may_have_occurred);
            assert_eq!(
                error.operation_id.as_deref(),
                Some("22222222-2222-2222-2222-222222222222")
            );
            assert!(matches!(
                error.kind,
                OperationErrorKind::OperationFailed { .. }
            ));
            let command = CommandError::from(ApiError::Operation(error));
            assert_eq!(
                command.code,
                expected["error"]["code"].as_str().unwrap(),
                "{}",
                case["name"]
            );
            assert_eq!(
                command.message,
                expected["error"]["message"].as_str().unwrap(),
                "{}",
                case["name"]
            );
        }

        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests[0].method(), Method::Post);
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.method() != Method::Get)
                .count(),
            1
        );
        let actual: Vec<_> = requests
            .iter()
            .skip(1)
            .map(|r| json!({"method":r.method().to_string(),"url":r.url().as_str()}))
            .collect();
        assert_eq!(json!(actual), expected["requests"], "{}", case["name"]);
        // The existing shared timer deliberately performs no I/O for sleep(0).
        // The independent source trace remains unchanged, including its zeros.
        let nonzero: Vec<_> = expected["waits"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v.as_u64() != Some(0))
            .cloned()
            .collect();
        assert_eq!(
            json!(*time.waits.lock().unwrap()),
            json!(nonzero),
            "{}",
            case["name"]
        );
    }
}

#[tokio::test]
async fn source_accepts_non_guid_operation_id_but_shared_safety_policy_rejects_before_polling() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../tests/compatibility/access/operation_cases.json"
    ))
    .unwrap();
    for case in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["name"] != "headerless")
    {
        let headers: Vec<_> = case["headers"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str().unwrap()))
            .collect();
        let (api, wire, _) = setup(vec![reply(202, r#"{"accepted":true}"#, &headers)]);
        let request = ApiRequest::fabric(Method::Post, &["admin", "items", "bulkSetLabels"])
            .unwrap()
            .with_json(&json!({"items":[]}))
            .unwrap();
        let error = api
            .send_completed(&request, NonZeroUsize::new(4096).unwrap(), pending())
            .await
            .unwrap_err();
        let ApiError::Operation(error) = error else {
            panic!("expected shared safety classification");
        };
        assert_eq!(error.kind, OperationErrorKind::InvalidResponse);
        assert!(error.mutation_may_have_occurred);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}
