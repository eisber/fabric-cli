// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Raw parser -> immutable context -> real shared HTTP/SDK -> query/output.

mod folder_support;

use fabric_cli::{
    PythonQuery, ReadConfig, ReadHost, WriteError,
    parser::{Invocation, ReadCommand, parse_args},
};
use fabric_cli_context::{Catalog, Element, HomeContext, NodeKind, WorkspaceType};
use fabric_cli_output::Timestamp;
use folder_support::{Credential, Http, Reply, Timer, complete, executor, reply, sdk};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{pending, poll_fn, ready},
    io,
    sync::Arc,
    task::Poll,
    time::Duration,
};

fn parse(args: &[&str]) -> ReadCommand {
    let Invocation::Read(command) =
        parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
    else {
        panic!("read command required")
    };
    command
}

fn replies() -> Vec<Reply> {
    vec![
        reply(serde_json::from_str(r#"{"unknown":{"z":null,"a":1.0},"id":"w","displayName":"\u65e5","sparkSettings":"old","n":1.0}"#).unwrap()),
        reply(json!({"value":[{"id":"m1","future":false}],"continuationToken":"next+/="})),
        reply(json!({"value":[{"id":"m2"}]})),
        reply(serde_json::from_str(r#"{"enabled":false,"z":null,"a":[1,1.0]}"#).unwrap()),
        reply(json!({"value":[]})),
    ]
}

#[test]
fn both_hosts_resolve_snapshot_and_enrich_before_native_query_without_context_changes() {
    for folders in [false, true] {
        for (query, expected) in [
            (
                ".",
                json!([{
                    "unknown":{"z":null,"a":1.0},"id":"w","displayName":"\u{65e5}",
                    "sparkSettings":{"enabled":false,"z":null,"a":[1,1.0]},"n":1.0,
                    "managedPrivateEndpoints":[{"id":"m1","future":false},{"id":"m2"}],
                    "roleAssignments":[]
                }]),
            ),
            ("sparkSettings.enabled", json!(["False"])),
            ("n", json!(["1.0"])),
            ("managedPrivateEndpoints[].id", json!(["m1", "m2"])),
            (
                "keys(@)",
                json!([
                    "unknown",
                    "id",
                    "displayName",
                    "sparkSettings",
                    "n",
                    "managedPrivateEndpoints",
                    "roleAssignments"
                ]),
            ),
            ("(\"sparkSettings\").enabled", json!(["False"])),
        ] {
            let http = Arc::new(Http::new(replies()));
            let timer = Arc::new(Timer::default());
            let executor = executor(http.clone(), timer.clone());
            let credential = Arc::new(Credential::default());
            let sdk = sdk(credential.clone());
            let root = Element::tenant("Tenant", "t");
            let workspace = Element::child(
                "W",
                Some("w".into()),
                NodeKind::Workspace(WorkspaceType::Workspace),
                root.clone(),
            )
            .unwrap();
            let catalog = Catalog::new(vec![workspace]);
            let host = ReadHost {
                sdk: &sdk,
                workspaces: &executor,
                items: &executor,
                capacities: &executor,
                lookup: &catalog,
                current: root.clone(),
                home: HomeContext::Unspecified,
                timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
                config: ReadConfig {
                    folder_listing_enabled: true,
                    ..ReadConfig::default()
                },
                projection: &PythonQuery,
            };
            let command = parse(&[
                "get",
                "W.Workspace",
                "--query",
                query,
                "--output_format",
                "json",
            ]);
            let mut stdout = vec![];
            let mut stderr = vec![];
            let exit = if folders {
                complete(host.with_folders(&executor).run(
                    &command,
                    pending(),
                    &mut stdout,
                    &mut stderr,
                ))
            } else {
                complete(host.run(&command, pending(), &mut stdout, &mut stderr))
            }
            .unwrap();
            assert_eq!(exit, 0, "query={query}, folders={folders}");
            assert!(stderr.is_empty());
            assert_eq!(
                serde_json::from_slice::<Value>(&stdout).unwrap(),
                json!({
                    "timestamp":"2000-01-01T00:00:00Z","status":"Success","command":"get",
                    "result":{"data":expected}
                })
            );
            assert!(Arc::ptr_eq(&host.current, &root));
            assert_eq!(
                *http.requests.lock().unwrap(),
                [
                    "https://api.fabric.microsoft.com/v1/workspaces/w",
                    "https://api.fabric.microsoft.com/v1/workspaces/w/managedPrivateEndpoints",
                    "https://api.fabric.microsoft.com/v1/workspaces/w/managedPrivateEndpoints?continuationToken=next%2B%2F%3D",
                    "https://api.fabric.microsoft.com/v1/workspaces/w/spark/settings",
                    "https://api.fabric.microsoft.com/v1/workspaces/w/roleAssignments",
                ]
            );
            assert_eq!(credential.scopes.lock().unwrap().len(), 5);
            assert_eq!(
                timer
                    .0
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|&&v| v == Duration::from_secs(240))
                    .count(),
                5
            );
            http.assert_consumed();
        }
    }
}

fn basic() -> Vec<Reply> {
    vec![
        reply(json!({"id":"w","displayName":"W"})),
        reply(json!({"value":[]})),
        reply(json!({})),
        reply(json!({"value":[]})),
    ]
}

fn observe(http: Arc<Http>, args: &[&str]) -> (Value, u8) {
    let executor = executor(http, Arc::new(Timer::default()));
    let sdk = sdk(Arc::default());
    let root = Element::tenant("Tenant", "t");
    let workspace = Element::child(
        "W",
        Some("w".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        root,
    )
    .unwrap();
    let catalog = Catalog::new(vec![]);
    let host = ReadHost {
        sdk: &sdk,
        workspaces: &executor,
        items: &executor,
        capacities: &executor,
        lookup: &catalog,
        current: workspace,
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
        projection: &PythonQuery,
    };
    let plan = complete(host.dispatch(&parse(args), pending())).unwrap();
    (
        serde_json::from_slice(&plan.payload).unwrap(),
        plan.exit_code,
    )
}

#[test]
fn absent_and_empty_query_use_ordered_inventory_and_query_errors_still_read_all_enrichments() {
    for args in [vec!["get", "."], vec!["get", ".", "-q", ""]] {
        let http = Arc::new(Http::new(basic()));
        let (output, exit) = observe(http.clone(), &args);
        assert_eq!(exit, 0);
        assert_eq!(
            output["result"]["data"],
            json!([
                "id",
                "displayName",
                "managedPrivateEndpoints",
                "roleAssignments"
            ])
        );
        http.assert_consumed();
    }
    for query in ["[", "sum(id)", "unknown_function(@)"] {
        let http = Arc::new(Http::new(basic()));
        let (output, exit) = observe(http.clone(), &["get", ".", "-q", query]);
        assert_eq!(exit, 1);
        assert_eq!(output["result"]["error_code"], "InvalidInput");
        assert!(output["result"].get("data").is_none());
        http.assert_consumed();
    }
}

#[test]
fn optional_omission_preserves_protected_json_without_printed_diagnostics() {
    let mut responses = basic();
    responses[1] = Reply {
        status: 403,
        body: json!({"errorCode":"private-code","message":"private-provider"}),
        headers: BTreeMap::new(),
    };
    responses[2].body = json!("not JSON");
    let http = Arc::new(Http::new(responses));
    let (output, exit) = observe(http.clone(), &["get", ".", "-q", "."]);
    assert_eq!(exit, 0);
    assert_eq!(
        output["result"]["data"],
        json!([{"id":"w","displayName":"W","roleAssignments":[]}])
    );
    assert!(!output.to_string().contains("private"));
    http.assert_consumed();
}

#[test]
fn primary_error_and_explicit_optional_safety_errors_do_not_emit_complete_metadata() {
    for status in [400, 401, 403, 404] {
        let http = Arc::new(Http::new([Reply {
            status,
            body: json!({"errorCode":"Synthetic","message":"synthetic service error."}),
            headers: BTreeMap::new(),
        }]));
        let (output, exit) = observe(http.clone(), &["get", ".", "-q", "."]);
        assert_eq!(exit, if status == 401 { 4 } else { 1 });
        assert!(output["result"].get("data").is_none());
        assert_eq!(http.requests.lock().unwrap().len(), 1);
    }
    let mut responses = basic();
    responses[1].body = json!({"value":[1],"continuationToken":"next","continuationUri":"https://foreign.invalid/private"});
    let http = Arc::new(Http::new(responses));
    let (output, exit) = observe(http.clone(), &["get", ".", "-q", "."]);
    assert_eq!(exit, 1);
    assert_eq!(output["result"]["error_code"], "ReadFailed");
    assert!(output["result"].get("data").is_none());
    assert!(!output.to_string().contains("private"));
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    let mut responses = basic();
    responses[2].body = json!("x".repeat(4 * 1024 * 1024 + 1));
    let http = Arc::new(Http::new(responses));
    let (output, exit) = observe(http.clone(), &["get", ".", "-q", "."]);
    assert_eq!(exit, 1);
    assert_eq!(output["result"]["error_code"], "ReadFailed");
    assert!(output["result"].get("data").is_none());
    assert_eq!(http.requests.lock().unwrap().len(), 3);
}

#[test]
fn cancellation_has_priority_before_primary_and_during_each_enrichment() {
    for hold in 0..=4 {
        let mut http = Http::new(basic());
        http.hold_after = Some(hold);
        let http = Arc::new(http);
        let executor = executor(http.clone(), Arc::new(Timer::default()));
        let credential = Arc::new(Credential::default());
        let sdk = sdk(credential.clone());
        let root = Element::tenant("T", "t");
        let workspace = Element::child(
            "W",
            Some("w".into()),
            NodeKind::Workspace(WorkspaceType::Workspace),
            root,
        )
        .unwrap();
        let catalog = Catalog::new(vec![]);
        let host = ReadHost {
            sdk: &sdk,
            workspaces: &executor,
            items: &executor,
            capacities: &executor,
            lookup: &catalog,
            current: workspace,
            home: HomeContext::Unspecified,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            config: ReadConfig::default(),
            projection: &PythonQuery,
        };
        let cancellation = poll_fn(|_| {
            if http.requests.lock().unwrap().len() >= hold {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let command = parse(&["get", ".", "-q", "."]);
        let mut future = Box::pin(host.dispatch(&command, cancellation));
        if hold > 0 {
            complete(poll_fn(|cx| {
                assert!(future.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            }));
        }
        let plan = complete(future).unwrap();
        assert_eq!(plan.exit_code, 1);
        let output: Value = serde_json::from_slice(&plan.payload).unwrap();
        assert_eq!(
            output["result"]["error_code"],
            "LongRunningOperationCancelled"
        );
        assert!(output["result"].get("data").is_none());
        assert_eq!(http.requests.lock().unwrap().len(), hold);
        assert_eq!(credential.scopes.lock().unwrap().len(), hold);
    }
}

struct BrokenWriter;
impl io::Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("caller owns flushing")
    }
}

#[test]
fn completed_workspace_metadata_propagates_writer_failure_and_keeps_unsupported_flags_explicit() {
    let http = Arc::new(Http::new(basic()));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let sdk = sdk(Arc::default());
    let root = Element::tenant("T", "t");
    let workspace = Element::child(
        "W",
        Some("w".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        root,
    )
    .unwrap();
    let catalog = Catalog::new(vec![]);
    let host = ReadHost {
        sdk: &sdk,
        workspaces: &executor,
        items: &executor,
        capacities: &executor,
        lookup: &catalog,
        current: workspace,
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
        projection: &PythonQuery,
    };
    let mut stderr = vec![];
    assert!(
        matches!(complete(host.run(&parse(&["get",".","-q","."]),pending(),&mut BrokenWriter,&mut stderr)),Err(WriteError::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe)
    );
    assert!(stderr.is_empty());
    http.assert_consumed();
    for option in ["--force", "--verbose", "--output"] {
        let mut args = vec!["get", "."];
        args.push(option);
        if option == "--output" {
            args.push("ignored");
        }
        let plan = complete(host.dispatch(&parse(&args), pending())).unwrap();
        assert_eq!(plan.exit_code, 1);
        assert!(
            String::from_utf8(plan.payload)
                .unwrap()
                .contains("NotSupported")
        );
    }
    let plan = complete(host.dispatch(&parse(&["get", "."]), ready(()))).unwrap();
    assert_eq!(plan.exit_code, 1);
}

#[test]
fn cancellation_after_the_metadata_snapshot_and_projection_prevents_output_commit() {
    use fabric_cli::Projection;
    use fabric_cli_output::CommandError;
    use std::sync::atomic::{AtomicBool, Ordering};
    struct SnapshotProjection(AtomicBool);
    impl Projection for SnapshotProjection {
        fn project(&self, data: Value, _: Option<&str>, _: bool) -> Result<Value, CommandError> {
            self.0.store(true, Ordering::SeqCst);
            Ok(data)
        }
    }
    let http = Arc::new(Http::new(basic()));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let sdk = sdk(Arc::default());
    let root = Element::tenant("T", "t");
    let workspace = Element::child(
        "W",
        Some("w".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        root,
    )
    .unwrap();
    let catalog = Catalog::new(vec![]);
    let projection = SnapshotProjection(AtomicBool::new(false));
    let host = ReadHost {
        sdk: &sdk,
        workspaces: &executor,
        items: &executor,
        capacities: &executor,
        lookup: &catalog,
        current: workspace,
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
        projection: &projection,
    };
    let cancelled = poll_fn(|_| {
        if projection.0.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let plan = complete(host.dispatch(&parse(&["get", ".", "-q", "."]), cancelled)).unwrap();
    assert_eq!(plan.exit_code, 1);
    assert_eq!(
        serde_json::from_slice::<Value>(&plan.payload).unwrap()["result"]["error_code"],
        "LongRunningOperationCancelled"
    );
    http.assert_consumed();
}

#[test]
fn empty_workspace_projections_produce_no_envelope_or_writer_calls_in_either_host() {
    for folders in [false, true] {
        for query in ["roleAssignments", "sparkSettings", "`\"\"`"] {
            let http = Arc::new(Http::new(basic()));
            let executor = executor(http.clone(), Arc::new(Timer::default()));
            let sdk = sdk(Arc::default());
            let root = Element::tenant("T", "t");
            let workspace = Element::child(
                "W",
                Some("w".into()),
                NodeKind::Workspace(WorkspaceType::Workspace),
                root,
            )
            .unwrap();
            let catalog = Catalog::new(vec![]);
            let host = ReadHost {
                sdk: &sdk,
                workspaces: &executor,
                items: &executor,
                capacities: &executor,
                lookup: &catalog,
                current: workspace,
                home: HomeContext::Unspecified,
                timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
                config: ReadConfig::default(),
                projection: &PythonQuery,
            };
            let command = parse(&["get", ".", "-q", query]);
            let mut stderr = vec![];
            let exit = if folders {
                complete(host.with_folders(&executor).run(
                    &command,
                    pending(),
                    &mut BrokenWriter,
                    &mut stderr,
                ))
            } else {
                complete(host.run(&command, pending(), &mut BrokenWriter, &mut stderr))
            }
            .expect("zero-byte source output must not call the writer");
            assert_eq!(exit, 0);
            assert!(stderr.is_empty());
            http.assert_consumed();
        }
    }
}
