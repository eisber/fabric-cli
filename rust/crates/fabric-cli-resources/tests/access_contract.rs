// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real shared-HTTP and command consumers of independent pinned Python fixtures.

mod boundary_contract;
mod catalog_contract;
mod lifecycle_contract;
mod raw_api_contract;
mod render_contract;

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli_output::Timestamp;
use fabric_cli_resources::{AclRead, labels_from_bytes, list_local_labels, read_acls, write_error};
use fabric_sdk_access::{AccessClient, Resource};
use fabric_sdk_api::{ApiClient, ApiError, Audience, CredentialSelection, Limits};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{pending, ready},
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Debug, Default)]
struct Credential {
    calls: AtomicUsize,
    audience: Option<Audience>,
}

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(scopes, [self.audience.unwrap_or(Audience::Fabric).scope()]);
        Ok(AccessToken {
            token: "offline-access-contract-only".into(),
            expires_on: OffsetDateTime::from_unix_timestamp(4_000_000_000).unwrap(),
        })
    }
}

struct Timer;
impl ExecutionTimer for Timer {
    fn sleep(&self, _: Duration) -> ExecutionFuture<'_, ()> {
        Box::pin(pending())
    }
}

#[derive(Debug)]
struct Wire {
    requests: Mutex<Vec<Request>>,
    replies: Mutex<VecDeque<AsyncRawResponse>>,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unscripted request or mutation replay"))
    }
}

fn reply(status: u16, body: &str, headers: &[(&str, &str)]) -> AsyncRawResponse {
    let mut map = Headers::new();
    for (name, value) in headers {
        map.insert((*name).to_owned(), (*value).to_owned());
    }
    AsyncRawResponse::from_bytes(StatusCode::from(status), map, body.as_bytes().to_vec())
}

fn setup(replies: Vec<AsyncRawResponse>) -> (ApiClient, Arc<Wire>, Arc<Credential>) {
    setup_audience(replies, Audience::Fabric)
}

fn setup_audience(
    replies: Vec<AsyncRawResponse>,
    audience: Audience,
) -> (ApiClient, Arc<Wire>, Arc<Credential>) {
    let wire = Arc::new(Wire {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(replies.into()),
    });
    let credential = Arc::new(Credential {
        calls: AtomicUsize::new(0),
        audience: Some(audience),
    });
    let executor = Arc::new(
        HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default()).unwrap(),
    );
    let api = ApiClient::new(
        credential.clone(),
        CredentialSelection::new("offline", "tenant", audience).unwrap(),
        Arc::new(fabric_cli_resources::ResourceHttpExecutor(executor)),
    );
    (api, wire, credential)
}

fn resource(name: &str) -> Resource {
    match name {
        "workspace" => Resource::Workspace("resource".into()),
        "gateway" => Resource::Gateway("resource".into()),
        "connection" => Resource::Connection("resource".into()),
        "item" => Resource::Item {
            workspace: "workspace".into(),
            item: "resource".into(),
        },
        "onelake" => Resource::LakehouseDataRoles {
            workspace: "workspace".into(),
            item: "resource".into(),
        },
        _ => panic!("unknown fixture resource"),
    }
}

#[tokio::test]
async fn independent_python_handlers_match_all_five_routes_projection_queries_and_empty_output() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/access/cases.json"
    ))
    .unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    let streams: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/access/streams.json"
    ))
    .unwrap();
    assert_eq!(cases.len(), 39);
    for case in cases {
        let (api, wire, credential) = setup(vec![reply(200, &case["body"].to_string(), &[])]);
        let target = resource(case["resource"].as_str().unwrap());
        let query = case["query"].as_str().map(str::to_owned);
        let options = if case["command"] == "ls" {
            AclRead::List {
                long: case["long"].as_bool().unwrap_or(false),
                query,
            }
        } else {
            AclRead::Get { query }
        };
        let value = read_acls(
            &AccessClient::new(api),
            &target,
            options,
            Limits::default(),
            pending(),
        )
        .await
        .unwrap();
        assert_eq!(
            value.data.as_ref().unwrap_or(&Value::Null),
            &case["expected"]["data"],
            "{}",
            case["name"]
        );
        assert_eq!(credential.calls.load(Ordering::SeqCst), 1);
        let requests = wire.requests.lock().unwrap();
        let actual: Vec<_> = requests
            .iter()
            .map(|r| json!({"method":r.method().to_string(),"url":r.url().as_str()}))
            .collect();
        assert_eq!(
            json!(actual),
            case["expected"]["requests"],
            "{}",
            case["name"]
        );
        assert_eq!(
            requests[0]
                .headers()
                .get_str(&"content-type".into())
                .unwrap(),
            "application/json"
        );
        assert!(
            requests[0]
                .headers()
                .get_str(&"authorization".into())
                .unwrap()
                .ends_with("offline-access-contract-only")
        );
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            value
                .write_json(
                    "acl",
                    Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
                    &mut out,
                    &mut err
                )
                .unwrap(),
            0
        );
        assert!(err.is_empty());
        let stream = streams
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == case["name"])
            .unwrap();
        assert_eq!(
            String::from_utf8(out.clone()).unwrap(),
            stream["stdout"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        if let Some(data) = &value.data {
            let document: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(document["command"], "acl");
            let expected = if data.is_array() {
                data.clone()
            } else {
                json!([data])
            };
            assert_eq!(document["result"]["data"], expected);
            assert_eq!(out.last(), Some(&b'\n'));
        } else {
            assert!(out.is_empty());
        }
    }
}

#[tokio::test]
async fn shared_pager_accumulates_item_access_details_and_preserves_partial_failure() {
    let (api, wire, _) = setup(vec![
        reply(
            200,
            r#"{"accessDetails":[{"id":"a"}],"continuationToken":"next /+"}"#,
            &[],
        ),
        reply(
            403,
            r#"{"errorCode":"body-ignored"}"#,
            &[("x-ms-public-api-error-code", "Denied")],
        ),
    ]);
    let error = AccessClient::new(api)
        .list(&resource("item"), Limits::default(), pending())
        .await
        .unwrap_err();
    assert_eq!(
        error.error,
        ApiError::Http {
            status: 403,
            code: "Denied".into()
        }
    );
    assert_eq!(
        error.partial.rows,
        json!([{"id":"a"}]).as_array().unwrap().clone()
    );
    assert_eq!(error.partial.pages, 1);
    assert_eq!(error.partial.next_token.as_deref(), Some("next /+"));
    assert_eq!(
        wire.requests.lock().unwrap()[1].url().query(),
        Some("continuationToken=next+%2F%2B")
    );
}

#[tokio::test]
async fn malformed_redirect_accepted_and_empty_statuses_are_not_success() {
    for (status, body, expected) in [
        (200, "{}", ApiError::InvalidResponse),
        (200, r#"{"value":[null]}"#, ApiError::InvalidResponse),
        (
            200,
            r#"{"value":[],"continuationToken":true}"#,
            ApiError::Continuation,
        ),
        (202, "", ApiError::UnsupportedStatus(202)),
        (204, "", ApiError::UnsupportedStatus(204)),
        (
            302,
            "",
            ApiError::Http {
                status: 302,
                code: "UnknownError".into(),
            },
        ),
        (
            404,
            r#"{"errorCode":"Ignored"}"#,
            ApiError::Http {
                status: 404,
                code: "NotFound".into(),
            },
        ),
    ] {
        let (api, wire, _) = setup(vec![reply(status, body, &[])]);
        let e = AccessClient::new(api)
            .list(&resource("workspace"), Limits::default(), pending())
            .await
            .unwrap_err();
        assert_eq!(e.error, expected);
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn exact_bounds_origin_cycles_and_cancellation_never_return_partial_success() {
    let (api, wire, _) = setup(vec![reply(
        200,
        r#"{"value":[{"id":"a"}],"continuationToken":"next"}"#,
        &[],
    )]);
    let e = AccessClient::new(api)
        .list(
            &resource("workspace"),
            Limits {
                max_pages: NonZeroUsize::new(1).unwrap(),
                ..Limits::default()
            },
            pending(),
        )
        .await
        .unwrap_err();
    assert_eq!(e.error, ApiError::Limit);
    assert_eq!(e.partial.rows.len(), 1);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
    for uri in [
        "https://foreign.invalid/v1/workspaces/resource/roleAssignments",
        "https://api.fabric.microsoft.com/v1/connections/resource/roleAssignments",
        "https://user@api.fabric.microsoft.com/v1/workspaces/resource/roleAssignments",
    ] {
        let (api, wire, _) = setup(vec![reply(
            200,
            &json!({"value":[],"continuationToken":"next","continuationUri":uri}).to_string(),
            &[],
        )]);
        assert_eq!(
            AccessClient::new(api)
                .list(&resource("workspace"), Limits::default(), pending())
                .await
                .unwrap_err()
                .error,
            ApiError::Continuation
        );
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
    let (api, wire, credential) = setup(vec![]);
    assert_eq!(
        AccessClient::new(api)
            .list(&resource("workspace"), Limits::default(), ready(()))
            .await
            .unwrap_err()
            .error,
        ApiError::Cancelled
    );
    assert!(wire.requests.lock().unwrap().is_empty());
    assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn service_errors_use_existing_json_output_and_authorization_exit_contract() {
    let (api, _, _) = setup(vec![reply(401, r#"{"errorCode":"body"}"#, &[])]);
    let error = read_acls(
        &AccessClient::new(api),
        &resource("workspace"),
        AclRead::Get {
            query: Some(".".into()),
        },
        Limits::default(),
        pending(),
    )
    .await
    .unwrap_err();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        write_error(
            &error,
            "acl get",
            Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            &mut out,
            &mut err
        )
        .unwrap(),
        4
    );
    assert!(err.is_empty());
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["status"], "Failure");
    assert_eq!(value["result"]["error_code"], "Unauthorized");
}

#[test]
fn label_local_configuration_and_file_shape_do_not_read_ambient_state() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/access/label_local.json"
    ))
    .unwrap();
    for case in fixtures["cases"].as_array().unwrap() {
        let actual = if let Some(input) = case["input"].as_str() {
            labels_from_bytes(input.as_bytes())
        } else {
            list_local_labels(Some(std::path::Path::new(
                "nonexistent-access-contract-labels.json",
            )))
        };
        if case["error"].is_null() {
            assert_eq!(
                actual.unwrap().data,
                Some(case["data"].clone()),
                "{}",
                case["name"]
            );
        } else {
            let error = actual.unwrap_err();
            assert_eq!(error.code, case["error"]["code"].as_str().unwrap());
            assert_eq!(error.message, case["error"]["message"].as_str().unwrap());
        }
    }
    let unset = list_local_labels(None).unwrap();
    assert!(unset.data.is_none());
    assert_eq!(unset.warnings, ["Label definitions for CLI not set"]);
    assert_eq!(
        labels_from_bytes(br#"{"labels":[{"name":"Public","id":"label"}]}"#)
            .unwrap()
            .data,
        Some(json!([{"name":"Public","id":"label"}]))
    );
    assert_eq!(
        labels_from_bytes(b"not-json").unwrap_err().code,
        "InvalidJson"
    );
    assert_eq!(
        labels_from_bytes(b"{}").unwrap_err().code,
        "InvalidEntriesFormat"
    );
    assert_eq!(
        labels_from_bytes(br#"{"labels":[]}"#).unwrap().data,
        Some(json!([]))
    );
    assert_eq!(
        list_local_labels(Some(std::path::Path::new(
            "nonexistent-access-contract-labels.json"
        )))
        .unwrap_err()
        .code,
        "InvalidPath"
    );
}

mod mutation_contract {
    use super::*;

    struct Prompts {
        answers: VecDeque<bool>,
        questions: Vec<String>,
    }

    impl fabric_cli_resources::Confirmation for Prompts {
        fn confirm<'a>(
            &'a mut self,
            question: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<bool, fabric_cli_resources::CommandError>>
                    + Send
                    + 'a,
            >,
        > {
            self.questions.push(question.into());
            let answer = self.answers.pop_front().expect("unscripted confirmation");
            Box::pin(ready(Ok(answer)))
        }
    }

    #[tokio::test]
    async fn independent_mutation_source_matches_routes_payloads_roles_principal_order_prompts_and_outputs()
     {
        use fabric_cli_resources::{
            AclChange, LabelChange, MutationOptions, NamedResource, mutate_acl, mutate_label,
        };
        use fabric_sdk_access::LabelItem;
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/compatibility/access/mutation_cases.json"
        ))
        .unwrap();
        for (index, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
            let replies = case["responses"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    reply(
                        r["status"].as_u64().unwrap() as u16,
                        &r["body"].to_string(),
                        &[],
                    )
                })
                .collect();
            let (api, wire, _) = setup(replies);
            let client = AccessClient::new(api);
            let options = MutationOptions {
                force: case["force"].as_bool().unwrap(),
                ..MutationOptions::default()
            };
            let mut prompt = Prompts {
                answers: case["confirmations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_bool().unwrap())
                    .collect(),
                questions: Vec::new(),
            };
            let actual = if case["resource"] == "label" {
                let path = std::env::temp_dir().join(format!(
                    "fabric-label-7196-{}-{index}.json",
                    std::process::id()
                ));
                std::fs::write(&path, case["definitions"].to_string()).unwrap();
                let change = if case["command"] == "set" {
                    LabelChange::Set(case["label_name"].as_str().unwrap().into())
                } else {
                    LabelChange::Remove
                };
                let result = mutate_label(
                    &client,
                    &LabelItem {
                        id: "resource".into(),
                        item_type: "Notebook".into(),
                    },
                    &change,
                    case["configured"]
                        .as_bool()
                        .unwrap()
                        .then_some(path.as_path()),
                    options,
                    &mut prompt,
                    pending(),
                )
                .await;
                std::fs::remove_file(path).unwrap();
                result
            } else {
                let target = NamedResource {
                    resource: resource(case["resource"].as_str().unwrap()),
                    name: "Resource".into(),
                };
                let identity = case["identity"].as_str().unwrap().into();
                let change = if case["command"] == "set" {
                    AclChange::Set {
                        identity,
                        role: case["role"].as_str().unwrap().into(),
                    }
                } else {
                    AclChange::Remove { identity }
                };
                mutate_acl(&client, &target, &change, options, &mut prompt, pending()).await
            };
            let expected = &case["expected"];
            assert_eq!(
                json!(prompt.questions),
                expected["questions"],
                "{}",
                case["name"]
            );
            if expected["error"].is_null() {
                let actual = actual.unwrap_or_else(|e| panic!("{}: {e}", case["name"]));
                assert_eq!(
                    json!(actual.message),
                    expected["message"],
                    "{}",
                    case["name"]
                );
                assert_eq!(
                    json!(actual.diagnostics),
                    expected["diagnostics"],
                    "{}",
                    case["name"]
                );
                assert_eq!(
                    json!(actual.warnings),
                    expected["warnings"],
                    "{}",
                    case["name"]
                );
            } else {
                let actual = actual.unwrap_err();
                assert_eq!(
                    actual.code,
                    expected["error"]["code"].as_str().unwrap(),
                    "{}",
                    case["name"]
                );
                assert_eq!(
                    actual.message,
                    expected["error"]["message"].as_str().unwrap(),
                    "{}",
                    case["name"]
                );
                assert_eq!(
                    json!(actual.diagnostics),
                    expected["diagnostics"],
                    "{}",
                    case["name"]
                );
                assert_eq!(
                    json!(actual.warnings),
                    expected["warnings"],
                    "{}",
                    case["name"]
                );
            }
            let requests: Vec<_> = wire.requests.lock().unwrap().iter().map(|r|{
                    let bytes = azure_core::Bytes::from(r.body());
                    json!({"method":r.method().to_string(),"url":r.url().as_str(),"body":if bytes.is_empty(){Value::Null}else{serde_json::from_slice::<Value>(&bytes).unwrap()}})
                }).collect();
            assert_eq!(json!(requests), expected["requests"], "{}", case["name"]);
        }
    }

    #[tokio::test]
    async fn mutation_auth_failure_accepted_status_and_cancel_are_not_principal_not_found_or_success()
     {
        use fabric_cli_resources::{AclChange, MutationOptions, NamedResource, mutate_acl};
        let target = NamedResource {
            resource: resource("workspace"),
            name: "Resource".into(),
        };
        for (status, code) in [
            (403, "Forbidden"),
            (202, "UnexpectedError"),
            (500, "UnknownError"),
        ] {
            let (api, wire, _) = setup(vec![
                reply(200, r#"{"value":[]}"#, &[]),
                reply(status, "{}", &[]),
            ]);
            let mut prompt = Prompts {
                answers: VecDeque::new(),
                questions: Vec::new(),
            };
            let e = mutate_acl(
                &AccessClient::new(api),
                &target,
                &AclChange::Set {
                    identity: "new".into(),
                    role: "Viewer".into(),
                },
                MutationOptions {
                    force: true,
                    ..MutationOptions::default()
                },
                &mut prompt,
                pending(),
            )
            .await
            .unwrap_err();
            assert_eq!(e.code, code);
            assert_eq!(wire.requests.lock().unwrap().len(), 2);
        }
        let (api, wire, credential) = setup(vec![]);
        let mut prompt = Prompts {
            answers: VecDeque::new(),
            questions: Vec::new(),
        };
        let e = mutate_acl(
            &AccessClient::new(api),
            &target,
            &AclChange::Remove {
                identity: "object".into(),
            },
            MutationOptions::default(),
            &mut prompt,
            ready(()),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "Cancelled");
        assert!(wire.requests.lock().unwrap().is_empty());
        assert_eq!(credential.calls.load(Ordering::SeqCst), 0);
    }
}
