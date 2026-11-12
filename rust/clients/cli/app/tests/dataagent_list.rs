// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public parser -> adapter -> SDK -> shared HTTP fixture; no live identity.

#![deny(unnameable_test_items)]

use async_trait::async_trait;
use azure_core::{
    credentials::{AccessToken, TokenCredential, TokenRequestOptions},
    http::{AsyncRawResponse, HttpClient, Request, StatusCode, headers::Headers},
    time::OffsetDateTime,
};
use fabric_cli::v2_dataagent_list_host::{
    ListCommand, ListDelivery, ListError, ListHost, ListInterruption, parse_list,
};
use fabric_sdk::{
    Audience, Client, CredentialSelection, EndpointBinding, RequestContext, ServiceOrigin,
};
use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    ffi::OsString,
    future::{pending, poll_fn, ready},
    io::{self, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

const WS: &str = "11111111-1111-1111-1111-111111111111";
const AGENT: &str = "22222222-2222-2222-2222-222222222222";
const SOURCE: &str = "33333333-3333-3333-3333-333333333333";
const OBSERVED: &str = "2026-11-11T23:00:00Z";

#[derive(Debug, Default)]
struct Credential(AtomicUsize);

#[async_trait]
impl TokenCredential for Credential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _: Option<TokenRequestOptions<'_>>,
    ) -> azure_core::Result<AccessToken> {
        assert_eq!(scopes, [Audience::Fabric.scope()]);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken::new(
            "offline-list-fixture",
            OffsetDateTime::from_unix_timestamp(4_000_000_000).unwrap(),
        ))
    }
}

struct Timer;

impl ExecutionTimer for Timer {
    fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
        if delay <= Duration::from_secs(4) {
            Box::pin(ready(()))
        } else {
            Box::pin(pending())
        }
    }
}

#[derive(Debug)]
struct Wire {
    requests: Mutex<Vec<Request>>,
    replies: Mutex<VecDeque<AsyncRawResponse>>,
    replied: AtomicBool,
}

#[async_trait]
impl HttpClient for Wire {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
        self.requests.lock().unwrap().push(request.clone());
        self.replied.store(true, Ordering::SeqCst);
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected page traversal or fallback"))
    }
}

fn setup(
    replies: Vec<(u16, Value)>,
    host: Option<&str>,
) -> (Client, HttpExecutor, Arc<Wire>, Arc<Credential>) {
    let wire = Arc::new(Wire {
        requests: Mutex::default(),
        replies: Mutex::new(
            replies
                .into_iter()
                .map(|(status, body)| {
                    AsyncRawResponse::from_bytes(
                        StatusCode::from(status),
                        Headers::new(),
                        serde_json::to_vec(&body).unwrap(),
                    )
                })
                .collect(),
        ),
        replied: AtomicBool::new(false),
    });
    let credential = Arc::new(Credential::default());
    let endpoint = host
        .map(|host| EndpointBinding::with_host(ServiceOrigin::Fabric, host).unwrap())
        .unwrap_or_else(|| EndpointBinding::public(ServiceOrigin::Fabric));
    let client = Client::with_context(RequestContext::with_endpoint(
        credential.clone(),
        CredentialSelection::new("offline", "tenant", Audience::Fabric).unwrap(),
        endpoint.clone(),
    ));
    let executor = HttpExecutor::new(wire.clone(), Arc::new(Timer), ExecutionOptions::default())
        .unwrap()
        .with_endpoint(endpoint);
    (client, executor, wire, credential)
}

fn host<'a>(client: &'a Client, executor: &'a HttpExecutor) -> ListHost<'a> {
    ListHost {
        client,
        items: executor,
        workspaces: executor,
        operations: executor,
    }
}

fn path(stage: &str) -> String {
    format!("/{WS}.Workspace/{AGENT}.DataAgent/settings/{stage}/datasources")
}

fn args(stage: &str, facade: bool, extra: &[&str]) -> Vec<OsString> {
    let prefix = if facade {
        vec!["artifact", "data-agent", "list"]
    } else {
        vec!["list"]
    };
    let mut args: Vec<OsString> = prefix.into_iter().map(Into::into).collect();
    args.extend([
        OsString::from(path(if stage == "draft" { "staging" } else { stage })),
        OsString::from("--stage"),
        OsString::from(stage),
    ]);
    args.extend(extra.iter().map(|value| OsString::from(*value)));
    args
}

fn parse(stage: &str, facade: bool, extra: &[&str]) -> ListCommand {
    parse_list(args(stage, facade, extra)).unwrap()
}

fn datasource() -> Value {
    json!({"id":SOURCE,"displayName":"Inert source","type":"FabricItem",
        "itemReference":{"itemId":AGENT,"workspaceId":WS},"future":{"instruction":"never execute"}})
}

fn read(command: &ListCommand, body: Value) -> (Value, u8, Arc<Wire>) {
    let (client, executor, wire, _) = setup(vec![(200, body)], None);
    let result = host(&client, &executor)
        .execute(command, OBSERVED, pending())
        .now_or_never()
        .unwrap()
        .unwrap();
    (
        serde_json::from_slice(result.bytes()).unwrap(),
        result.exit_code(),
        wire,
    )
}

#[test]
fn selected_collection_requires_matching_explicit_stage_and_exact_owner() {
    for (stage, path_stage) in [("draft", "published"), ("published", "staging")] {
        assert!(
            parse_list(vec![
                "list".into(),
                path(path_stage).into(),
                "--stage".into(),
                stage.into()
            ])
            .is_err()
        );
    }
    for invalid in [
        path("draft"),
        format!("/{WS}.Workspace/{AGENT}.DataAgent"),
        format!("/{WS}.Workspace/{AGENT}.DataAgent/settings/staging"),
        path("staging").replace(".DataAgent", ".Lakehouse"),
        path("staging").replace(".DataAgent", ".AISkill"),
        format!("/workspaces/{WS}/items/{AGENT}/settings/staging/datasources"),
        path("staging").replace(WS, "*"),
        format!("{}/source", path("staging")),
    ] {
        assert!(
            parse_list(vec![
                "list".into(),
                invalid.into(),
                "--stage".into(),
                "draft".into()
            ])
            .is_err()
        );
    }
    assert!(parse_list(vec!["list".into(), path("staging").into()]).is_err());
    for extra in [
        vec!["--all"],
        vec!["--limit", "0"],
        vec!["--max-bytes", "0"],
        vec!["--max-bytes", "4194305"],
        vec!["--timeout", "0s"],
        vec!["--timeout", "25h"],
        vec!["--format", "yaml"],
        vec!["--project", "["],
        vec!["--continuation", ""],
        vec!["--continuation", "secret\nvalue"],
        vec!["--stage", "published"],
    ] {
        assert!(
            parse_list(args("draft", false, &extra)).is_err(),
            "{extra:?}"
        );
    }
}

#[test]
fn explicit_stages_and_facade_use_identical_sdk_routes_and_results() {
    for (stage, native_suffix) in [
        ("draft", "/staging/datasources"),
        ("published", "/datasources"),
    ] {
        let body = json!({"value":[datasource()]});
        let (root, root_exit, wire) = read(&parse(stage, false, &[]), body.clone());
        let (facade, facade_exit, facade_wire) = read(&parse(stage, true, &[]), body);
        assert_eq!(root, facade);
        assert_eq!(root_exit, 0);
        assert_eq!(facade_exit, 0);
        assert_eq!(root["data"], json!([datasource()]));
        assert_eq!(root["data"][0]["id"], SOURCE);
        assert_ne!(
            root["data"][0]["id"],
            root["data"][0]["itemReference"]["itemId"]
        );
        assert_eq!(root["meta"]["target"]["stage"], stage);
        for wire in [wire, facade_wire] {
            let requests = wire.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].method().as_str(), "GET");
            assert_eq!(
                requests[0].url().as_str(),
                format!(
                    "https://api.fabric.microsoft.com/v1/workspaces/{WS}/dataAgents/{AGENT}{native_suffix}"
                )
            );
        }
    }
}

#[test]
fn arrays_empty_singleton_and_data_only_scalar_projection_preserve_metadata() {
    let (empty, exit, _) = read(
        &parse("draft", false, &["--max-bytes", "2"]),
        json!({"value":[]}),
    );
    assert_eq!(empty["data"], json!([]));
    assert_eq!(empty["meta"]["coverage"]["complete"], true);
    assert_eq!(exit, 0);
    let body = json!({"value":[datasource()],"continuationToken":"next","continuationUri":"https://untrusted.invalid/never-follow"});
    let (unprojected, _, _) = read(&parse("draft", false, &[]), body.clone());
    for (expression, expected) in [
        ("length(@)", json!(1)),
        ("[0].id", json!(SOURCE)),
        ("`null`", Value::Null),
    ] {
        let (projected, exit, _) = read(
            &parse("draft", false, &["--project", expression]),
            body.clone(),
        );
        assert_eq!(projected["data"], expected);
        assert_eq!(projected["meta"], unprojected["meta"]);
        assert_eq!(exit, 1);
    }
}

#[test]
fn opaque_token_encodes_once_and_foreign_uri_is_inert_incomplete_data() {
    let token = "a%2Fb +/=&?";
    let (client, executor, wire, credential) = setup(
        vec![(
            200,
            json!({
                "value":[], "continuationToken":token, "continuationUri":"https://foreign.invalid/credential-trap"
            }),
        )],
        Some("msitapi.fabric.microsoft.com"),
    );
    let command = parse("draft", false, &["--continuation", token]);
    let result = host(&client, &executor)
        .execute(&command, OBSERVED, pending())
        .now_or_never()
        .unwrap()
        .unwrap();
    let envelope: Value = serde_json::from_slice(result.bytes()).unwrap();
    assert_eq!(result.exit_code(), 1);
    assert_eq!(envelope["status"], "partial");
    assert_eq!(envelope["meta"]["continuation"]["token"], token);
    assert_eq!(
        envelope["meta"]["continuation"]["uri"],
        "https://foreign.invalid/credential-trap"
    );
    assert_eq!(envelope["meta"]["continuation"]["uriFollowed"], false);
    assert_eq!(credential.0.load(Ordering::SeqCst), 1);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url().query_pairs().collect::<Vec<_>>(),
        vec![("continuationToken".into(), token.into())]
    );
    assert!(
        requests[0]
            .url()
            .as_str()
            .ends_with("?continuationToken=a%252Fb+%2B%2F%3D%26%3F")
    );
    assert_eq!(
        requests[0].url().host_str(),
        Some("msitapi.fabric.microsoft.com")
    );
    let (uri_only, exit, wire) = read(
        &parse("published", false, &[]),
        json!({"value":[],"continuationUri":"https://foreign.invalid/no-token"}),
    );
    assert_eq!(exit, 1);
    assert_eq!(uri_only["meta"]["continuation"]["token"], Value::Null);
    assert_eq!(uri_only["meta"]["continuation"]["resumable"], false);
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[test]
fn whole_page_budgets_are_measured_before_projection_without_skipped_rows() {
    let body = json!({"value":[datasource()],"continuationToken":"after-whole-page"});
    let exact = serde_json::to_vec(&json!([datasource()])).unwrap().len();
    for (bound, succeeds) in [(exact, true), (exact - 1, false)] {
        let command = parse(
            "draft",
            false,
            &["--max-bytes", &bound.to_string(), "--project", "`null`"],
        );
        let (client, executor, wire, _) = setup(vec![(200, body.clone())], None);
        let result = host(&client, &executor)
            .execute(&command, OBSERVED, pending())
            .now_or_never()
            .unwrap();
        assert_eq!(result.is_ok(), succeeds);
        if !succeeds {
            assert!(matches!(result, Err(ListError::BudgetExceeded)));
        }
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
    let (client, executor, wire, _) = setup(
        vec![(
            200,
            json!({"value":[datasource(),datasource()],"continuationToken":"past-both"}),
        )],
        None,
    );
    assert!(matches!(
        host(&client, &executor)
            .execute(
                &parse("draft", false, &["--limit", "1"]),
                OBSERVED,
                pending()
            )
            .now_or_never()
            .unwrap(),
        Err(ListError::BudgetExceeded)
    ));
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[test]
fn malformed_denied_and_accepted_pages_fail_without_empty_or_stage_fallback() {
    for (status, body) in [
        (200, json!([])),
        (200, json!([[]])),
        (200, json!({})),
        (200, json!({"value":{}})),
        (200, json!({"value":[{}]})),
        (200, json!({"value":[], "continuationToken":""})),
        (202, json!({"value":[]})),
        (
            403,
            json!({"error":{"code":"Forbidden","message":"secret"}}),
        ),
    ] {
        let (client, executor, wire, _) = setup(vec![(status, body)], None);
        let result = host(&client, &executor)
            .execute(&parse("draft", false, &[]), OBSERVED, pending())
            .now_or_never()
            .unwrap();
        assert!(matches!(result, Err(ListError::Read(_))), "status={status}");
        if status == 403 {
            assert_eq!(result.err().unwrap().code(), "PermissionDenied");
        }
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn ready_cancel_or_deadline_prevents_credentials_and_late_cancel_prevents_delivery() {
    for signal in [ListInterruption::Cancelled, ListInterruption::TimedOut] {
        let (client, executor, wire, credential) = setup(vec![(200, json!({"value":[]}))], None);
        let command = parse("draft", false, &[]);
        let result = host(&client, &executor)
            .execute(&command, OBSERVED, ready(signal))
            .now_or_never()
            .unwrap();
        assert!(matches!(result, Err(ListError::Interrupted(value)) if value == signal));
        assert_eq!(credential.0.load(Ordering::SeqCst), 0);
        assert!(wire.requests.lock().unwrap().is_empty());
    }
    let (client, executor, wire, _) = setup(vec![(200, json!({"value":[]}))], None);
    let signal = poll_fn(|_| {
        if wire.replied.load(Ordering::SeqCst) {
            Poll::Ready(ListInterruption::Cancelled)
        } else {
            Poll::Pending
        }
    });
    let result = host(&client, &executor)
        .execute(&parse("draft", false, &[]), OBSERVED, signal)
        .now_or_never()
        .unwrap();
    assert!(matches!(
        result,
        Err(ListError::Interrupted(ListInterruption::Cancelled))
    ));
    assert_eq!(wire.requests.lock().unwrap().len(), 1);
}

#[test]
fn bounded_name_resolution_precedes_one_datasource_page_and_preserves_owner() {
    let (client, executor, wire, _) = setup(
        vec![
            (
                200,
                json!({"value":[{"id":WS,"displayName":"Workspace","type":"Workspace"}]}),
            ),
            (
                200,
                json!({"value":[{"id":AGENT,"displayName":"Agent","type":"DataAgent","workspaceId":WS}]}),
            ),
            (200, json!({"value":[datasource()]})),
        ],
        None,
    );
    let command = parse_list(vec![
        "list".into(),
        "/Workspace.Workspace/Agent.DataAgent/settings/staging/datasources".into(),
        "--stage".into(),
        "draft".into(),
    ])
    .unwrap();
    let result = host(&client, &executor)
        .execute(&command, OBSERVED, pending())
        .now_or_never()
        .unwrap()
        .unwrap();
    let value: Value = serde_json::from_slice(result.bytes()).unwrap();
    assert_eq!(value["meta"]["target"]["itemId"], AGENT);
    assert_eq!(wire.requests.lock().unwrap().len(), 3);
}

struct BrokenWriter {
    flush_only: bool,
}

impl Write for BrokenWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.flush_only {
            Ok(bytes.len())
        } else {
            Err(io::Error::other("write failure"))
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("flush failure"))
    }
}

#[test]
fn output_is_exclusive_create_and_stream_errors_are_not_success() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("page.json");
    let command = parse("draft", false, &["--output", path.to_str().unwrap()]);
    let delivery = ListDelivery::preflight(&command, directory.path()).unwrap();
    assert!(!path.exists());
    let (client, executor, _, _) = setup(vec![(200, json!({"value":[]}))], None);
    let result = host(&client, &executor)
        .execute(&command, OBSERVED, pending())
        .now_or_never()
        .unwrap()
        .unwrap();
    let mut stdout = Vec::new();
    assert_eq!(delivery.deliver(&result, &mut stdout).unwrap(), 0);
    assert!(stdout.is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), result.bytes());
    assert!(matches!(
        ListDelivery::preflight(&command, directory.path()),
        Err(ListError::File(_))
    ));
    let other_path = directory.path().join("raced.json");
    let command = parse("draft", false, &["--output", other_path.to_str().unwrap()]);
    let delivery = ListDelivery::preflight(&command, directory.path()).unwrap();
    std::fs::write(&other_path, b"preserve").unwrap();
    assert!(matches!(
        delivery.deliver(&result, &mut stdout),
        Err(ListError::File(_))
    ));
    assert_eq!(std::fs::read(&other_path).unwrap(), b"preserve");
    for flush_only in [true, false] {
        let delivery =
            ListDelivery::preflight(&parse("draft", false, &[]), directory.path()).unwrap();
        assert!(matches!(
            delivery.deliver(&result, &mut BrokenWriter { flush_only }),
            Err(ListError::Output)
        ));
    }
}

#[test]
fn shared_http_retries_are_finite_and_stay_on_the_same_native_page() {
    let (client, executor, wire, _) = setup(
        vec![
            (503, json!({"error":{"code":"ServiceUnavailable"}})),
            (200, json!({"value":[]})),
        ],
        None,
    );
    let command = parse("draft", false, &[]);
    let result = host(&client, &executor)
        .execute(&command, OBSERVED, pending())
        .now_or_never()
        .unwrap()
        .unwrap();
    let envelope: Value = serde_json::from_slice(result.bytes()).unwrap();
    assert_eq!(envelope["meta"]["provenance"]["datasourcePages"], 1);
    let sent = wire.requests.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].url(), sent[1].url());
    drop(sent);
    let (client, executor, wire, _) = setup(
        (0..4)
            .map(|_| (503, json!({"error":{"code":"ServiceUnavailable"}})))
            .collect(),
        None,
    );
    assert!(matches!(
        host(&client, &executor)
            .execute(&command, OBSERVED, pending())
            .now_or_never()
            .unwrap(),
        Err(ListError::Read(_))
    ));
    assert_eq!(wire.requests.lock().unwrap().len(), 4);
}

#[cfg(feature = "native-host")]
struct BlockedWriter {
    entered: Arc<tokio::sync::Notify>,
    release: std::sync::mpsc::Receiver<()>,
}

#[cfg(feature = "native-host")]
impl Write for BlockedWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        self.entered.notify_one();
        self.release.recv().unwrap();
        Err(io::Error::other(
            "blocked write released after supervision returned",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(feature = "native-host")]
#[tokio::test]
async fn cancellation_and_deadline_supervise_blocked_output_without_claiming_rollback() {
    for signal in [ListInterruption::Cancelled, ListInterruption::TimedOut] {
        let command = parse("draft", false, &[]);
        let (client, executor, _, _) = setup(vec![(200, json!({"value":[]}))], None);
        let result = host(&client, &executor)
            .execute(&command, OBSERVED, pending())
            .await
            .unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, receiver) = std::sync::mpsc::channel();
        let delivery = ListDelivery::preflight(&command, &std::env::temp_dir()).unwrap();
        let supervise = delivery.deliver_supervised(
            result,
            BlockedWriter {
                entered: entered.clone(),
                release: receiver,
            },
            async {
                entered.notified().await;
                signal
            },
        );
        let outcome = tokio::time::timeout(Duration::from_secs(2), supervise).await;
        // Release only our fixture's blocked thread, including on an assertion failure.
        release.send(()).unwrap();
        assert!(
            matches!(outcome.unwrap(), Err(ListError::DeliveryInterrupted(value)) if value == signal)
        );
    }
}
