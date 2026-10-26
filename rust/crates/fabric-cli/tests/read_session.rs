// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual production invocation path; only credentials and HTTP/time are synthetic.

#[allow(dead_code)]
mod folder_support;

use fabric_cli::{
    ReadConfig,
    parser::{Invocation, parse_args},
    session::ReadSession,
};
use fabric_cli_context::{Element, HomeContext};
use fabric_cli_output::Timestamp;
use folder_support::{Credential, Http, Reply, Timer, complete, executor, reply, sdk};
use serde_json::{Value, json};
use std::{
    future::{pending, ready},
    sync::Arc,
};

fn workspaces() -> Reply {
    reply(json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}))
}

fn run(args: &[&str], replies: Vec<Reply>) -> (u8, Vec<u8>, Vec<u8>, Vec<String>) {
    run_context(args, replies, "/", HomeContext::Unspecified)
}

fn run_context(
    args: &[&str],
    replies: Vec<Reply>,
    current: &str,
    home: HomeContext,
) -> (u8, Vec<u8>, Vec<u8>, Vec<String>) {
    let Invocation::Read(command) =
        parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
    else {
        panic!("read required")
    };
    let http = Arc::new(Http::new(replies));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential);
    let session = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: current,
        home,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let plan = complete(session.dispatch(&command, pending())).unwrap();
    let (mut stdout, mut stderr) = (vec![], vec![]);
    plan.write_to(&mut stdout, &mut stderr).unwrap();
    http.assert_consumed();
    let requests = http.requests.lock().unwrap().clone();
    (plan.exit_code, stdout, stderr, requests)
}

#[test]
fn root_ls_paginates_sorts_queries_and_writes_real_envelope() {
    let (exit, out, err, requests) = run(
        &["ls", "/", "-q", "[].name"],
        vec![
            reply(
                json!({"value":[{"id":"z","displayName":"Zulu","type":"Workspace"}],"continuationToken":"a +/="}),
            ),
            workspaces(),
        ],
    );
    assert_eq!(exit, 0);
    assert!(err.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap(),
        json!({
            "timestamp":"2000-01-01T00:00:00Z", "status":"Success", "command":"ls",
            "result":{"data":["Demo.Workspace","Zulu.Workspace"]}
        })
    );
    assert_eq!(
        requests,
        [
            "https://api.fabric.microsoft.com/v1/workspaces",
            "https://api.fabric.microsoft.com/v1/workspaces?continuationToken=a+%2B%2F%3D",
        ]
    );
}

#[test]
fn named_workspace_ls_resolves_real_metadata_then_lists_items() {
    let (exit, out, err, requests) = run(
        &["dir", "Demo.Workspace", "-l"],
        vec![
            workspaces(),
            reply(json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}]})),
        ],
    );
    assert_eq!(exit, 0);
    assert!(err.is_empty());
    let result: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(
        result["result"]["data"],
        json!([{"name":"Book.Notebook","id":"n"}])
    );
    assert_eq!(
        requests,
        [
            "https://api.fabric.microsoft.com/v1/workspaces",
            "https://api.fabric.microsoft.com/v1/workspaces/w/items",
        ]
    );
}

#[test]
fn get_resolves_workspace_and_item_then_uses_specialized_endpoint() {
    let (exit, out, err, requests) = run(
        &["get", "Demo.Workspace/Book.Notebook", "-q", "description"],
        vec![
            workspaces(),
            reply(json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}]})),
            reply(json!({"id":"n","displayName":"Book","description":"actual SDK result"})),
        ],
    );
    assert_eq!(exit, 0);
    assert!(err.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap()["result"]["data"],
        json!(["actual SDK result"])
    );
    assert_eq!(
        requests.last().unwrap(),
        "https://api.fabric.microsoft.com/v1/workspaces/w/notebooks/n"
    );
}

#[test]
fn workspace_get_enriches_before_query_in_the_actual_session() {
    let (exit, out, err, requests) = run(
        &["get", "Demo.Workspace", "-q", "sparkSettings.enabled"],
        vec![
            workspaces(),
            reply(json!({"id":"w","displayName":"Demo"})),
            reply(json!({"value":[]})),
            reply(json!({"enabled":false})),
            reply(json!({"value":[]})),
        ],
    );
    assert_eq!(exit, 0);
    assert!(err.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap()["result"]["data"],
        json!(["False"])
    );
    assert_eq!(requests.len(), 5);
}

#[test]
fn nested_folder_lookup_uses_one_complete_source_ordered_graph() {
    let graph = reply(json!({"value":[
        {"id":"child","displayName":"Child","parentFolderId":"parent"},
        {"id":"parent","displayName":"Parent"}
    ]}));
    let (exit, out, err, requests) = run(
        &["ls", "Demo.Workspace/Parent.Folder/Child.Folder", "-l"],
        vec![
            workspaces(),
            graph.clone(),
            reply(
                json!({"value":[{"id":"n","displayName":"Book","type":"Notebook","folderId":"child"}]}),
            ),
            graph,
        ],
    );
    assert_eq!(exit, 0);
    assert!(err.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap()["result"]["data"],
        json!([{"name":"Book.Notebook","id":"n"}])
    );
    assert_eq!(
        requests
            .iter()
            .filter(|s| s.ends_with("/folders?recursive=True"))
            .count(),
        2
    );
}

#[test]
fn missing_path_or_unauthorized_discovery_is_failure_not_success_or_notconfigured() {
    let (exit, out, err, requests) = run(&["ls", "Missing.Workspace"], vec![workspaces()]);
    assert_eq!(exit, 1);
    assert!(err.is_empty());
    let result: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(result["status"], "Failure");
    assert_eq!(result["result"]["error_code"], "NotFound");
    assert_eq!(requests.len(), 1);
    let mut denied = reply(json!({"errorCode":"Unauthorized","message":"sensitive"}));
    denied.status = 401;
    let (exit, out, err, _) = run(&["get", "Demo.Workspace"], vec![denied]);
    assert_eq!(exit, 4);
    assert!(err.is_empty());
    assert!(!String::from_utf8_lossy(&out).contains("sensitive"));
}

#[test]
fn cancellation_precedes_discovery_and_never_acquires_credentials() {
    let Invocation::Read(command) = parse_args(["fab-rs", "get", "Demo.Workspace"]).unwrap() else {
        panic!()
    };
    let http = Arc::new(Http::new([]));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential.clone());
    let session = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let plan = complete(session.dispatch(&command, ready(()))).unwrap();
    assert_eq!(plan.exit_code, 1);
    assert!(credential.scopes.lock().unwrap().is_empty());
    assert!(http.requests.lock().unwrap().is_empty());
}

#[test]
fn explicit_text_is_rendered_by_the_shared_output_consumer() {
    let (exit, out, err, _) = run(&["ls", "/", "--output_format", "text"], vec![workspaces()]);
    assert_eq!(exit, 0);
    assert!(err.is_empty());
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Demo.Workspace"));
    assert!(!text.contains("\"timestamp\""));
}

#[test]
fn current_and_home_are_explicit_and_resolve_before_relative_reads() {
    for (path, current, home) in [
        ("", "/Demo.Workspace", HomeContext::Unspecified),
        ("~", "/", HomeContext::Workspace("Demo.Workspace".into())),
    ] {
        let (exit, out, err, requests) = run_context(
            &["ls", path, "-a"],
            vec![workspaces(), reply(json!({"value": []}))],
            current,
            home,
        );
        assert_eq!(exit, 0);
        assert!(err.is_empty());
        let result: Value = serde_json::from_slice(&out).unwrap();
        assert!(result["result"]["hidden_data"].is_array(), "{result}");
        assert_eq!(requests.len(), 2);
    }
    let (exit, out, _, requests) = run(&["ls", "~"], vec![]);
    assert_eq!(exit, 1);
    assert!(String::from_utf8(out).unwrap().contains("NotSupported"));
    assert!(requests.is_empty());
}

#[test]
fn partial_paging_never_escapes_as_successful_discovery_or_listing() {
    for path in ["/", "Demo.Workspace"] {
        let (exit, out, err, requests) = run(
            &["ls", path],
            vec![
                reply(json!({
                    "value":[{"id":"w","displayName":"Demo","type":"Workspace"}],
                    "continuationToken":"next"
                })),
                reply(json!({"not_a_page": []})),
            ],
        );
        assert_eq!(exit, 1);
        assert!(err.is_empty());
        let result: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(result["status"], "Failure");
        assert!(result["result"].get("data").is_none(), "{result}");
        assert_eq!(requests.len(), 2);
    }
}

#[test]
fn cancellation_during_discovery_drops_inflight_http_without_partial_success() {
    let Invocation::Read(command) = parse_args(["fab-rs", "get", "Demo.Workspace"]).unwrap() else {
        panic!()
    };
    let mut backend = Http::new([]);
    backend.hold_after = Some(1);
    let http = Arc::new(backend);
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let sdk = sdk(Arc::new(Credential::default()));
    let session = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let cancel = std::future::poll_fn(|cx| {
        if http.requests.lock().unwrap().is_empty() {
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    });
    let plan = complete(session.dispatch(&command, cancel)).unwrap();
    assert_eq!(plan.exit_code, 1);
    let (mut out, mut err) = (vec![], vec![]);
    plan.write_to(&mut out, &mut err).unwrap();
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("LongRunningOperationCancelled")
    );
    assert!(err.is_empty());
    assert_eq!(http.requests.lock().unwrap().len(), 1);
}

#[test]
fn root_long_enrichment_and_hidden_collections_use_existing_sdk_and_output() {
    let (exit, out, err, requests) = run(
        &["ls", "/", "-la"],
        vec![
            reply(json!({"value":[
                {"id":"w","displayName":"Demo","type":"Workspace","capacityId":"c"}
            ]})),
            reply(json!({"value":[{"id":"c","displayName":"Capacity","region":"West"}]})),
        ],
    );
    assert_eq!(exit, 0);
    assert!(err.is_empty());
    let result: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(result["result"]["data"][0]["capacityName"], "Capacity");
    assert!(String::from_utf8(out).unwrap().contains(".capacities"));
    assert_eq!(
        requests.last().unwrap(),
        "https://api.fabric.microsoft.com/v1/capacities"
    );
}

#[test]
fn unsupported_resources_and_options_fail_explicitly_without_http() {
    for args in [
        vec!["get", "/"],
        vec!["ls", "/.capacities"],
        vec!["get", "Demo.Workspace", "--output", "not-written.json"],
    ] {
        let (exit, out, err, requests) = run(&args, vec![]);
        assert_eq!(exit, 1);
        assert!(err.is_empty());
        let result: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(result["status"], "Failure");
        assert!(requests.is_empty());
    }
}

#[derive(Debug)]
struct UnavailableCredential;

#[async_trait::async_trait]
impl azure_core::credentials::TokenCredential for UnavailableCredential {
    async fn get_token(
        &self,
        _: &[&str],
        _: Option<azure_core::credentials::TokenRequestOptions<'_>>,
    ) -> azure_core::Result<azure_core::credentials::AccessToken> {
        Err(azure_core::Error::with_message(
            azure_core::error::ErrorKind::Credential,
            "synthetic unavailable credential detail must not escape",
        ))
    }
}

#[test]
fn unavailable_injected_credentials_fail_without_http_or_provider_fallback() {
    let Invocation::Read(command) = parse_args(["fab-rs", "ls", "Demo.Workspace"]).unwrap() else {
        panic!()
    };
    let sdk = fabric_sdk::Client::new(
        Arc::new(UnavailableCredential),
        fabric_sdk::CredentialSelection::new(
            "offline-account",
            "offline-tenant",
            fabric_sdk::Audience::Fabric,
        )
        .unwrap(),
    );
    let http = Arc::new(Http::new([]));
    let executor = executor(http.clone(), Arc::default());
    let session = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let plan = complete(session.dispatch(&command, pending())).unwrap();
    assert_eq!(plan.exit_code, 4);
    let (mut out, mut err) = (vec![], vec![]);
    plan.write_to(&mut out, &mut err).unwrap();
    assert!(err.is_empty());
    assert!(!String::from_utf8_lossy(&out).contains("synthetic"));
    assert!(http.requests.lock().unwrap().is_empty());
}

#[test]
fn actual_session_plan_propagates_writer_failure() {
    struct BrokenWriter;
    impl std::io::Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "closed",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            panic!("writers must not be flushed by rendering")
        }
    }
    let Invocation::Read(command) = parse_args(["fab-rs", "ls", "/"]).unwrap() else {
        panic!()
    };
    let sdk = sdk(Arc::new(Credential::default()));
    let http = Arc::new(Http::new([workspaces()]));
    let executor = executor(http, Arc::default());
    let session = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    let plan = complete(session.dispatch(&command, pending())).unwrap();
    assert_eq!(
        plan.write_to(&mut BrokenWriter, &mut Vec::new())
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::BrokenPipe
    );
}
