// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Metadata GET flags through direct hosts and the production read session.

#![deny(unnameable_test_items)]

#[allow(dead_code)]
mod folder_support;

use fabric_cli::{
    PythonQuery, ReadConfig, ReadHost, WriteError,
    parser::{Invocation, ReadCommand, parse_args},
    session::{ReadSession, get_options::is_metadata_property_query},
};
use fabric_cli_context::{Catalog, Element, HomeContext, ItemType, NodeKind, WorkspaceType};
use fabric_cli_output::{Timestamp, text::TextPlan};
use folder_support::{Credential, Http, Reply, Timer, complete, executor, reply, sdk};
use serde_json::{Value, json};
use std::{
    future::{Future, pending, ready},
    io::{self, Write},
    sync::Arc,
};

fn command(args: &[&str]) -> ReadCommand {
    let Invocation::Read(command) =
        parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
    else {
        panic!("read command required")
    };
    command
}

fn workspaces() -> Reply {
    reply(json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}))
}

fn items() -> Reply {
    reply(json!({"value":[{"id":"n","displayName":"Book","type":"Notebook"}]}))
}

fn metadata() -> Reply {
    reply(json!({
        "id":"n", "displayName":"Book", "description":"",
        "properties":{"level":{"deeper":{"still":{"value":true}}}}
    }))
}

fn session(
    command: &ReadCommand,
    replies: Vec<Reply>,
    cancelled: impl Future<Output = ()> + Send,
) -> (TextPlan, Vec<String>, usize) {
    let http = Arc::new(Http::new(replies));
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
    let plan = complete(session.dispatch(command, cancelled)).unwrap();
    http.assert_consumed();
    let requests = http.requests.lock().unwrap().clone();
    let tokens = credential.scopes.lock().unwrap().len();
    (plan, requests, tokens)
}

fn streams(plan: &TextPlan) -> (Vec<u8>, Vec<u8>) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    plan.write_to(&mut out, &mut err).unwrap();
    (out, err)
}

fn failure_code(plan: &TextPlan) -> Value {
    let (out, err) = streams(plan);
    assert!(err.is_empty());
    let output: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(output["status"], "Failure");
    output["result"]["error_code"].clone()
}

#[test]
fn metadata_roots_match_the_exact_source_string_rule() {
    for root in [
        "id",
        "type",
        "displayName",
        "description",
        "workspaceId",
        "folderId",
        "properties",
    ] {
        assert!(is_metadata_property_query(root));
        assert!(is_metadata_property_query(&format!("{root}.nested[0]")));
    }
    for query in [
        "",
        ".",
        "Id",
        " id",
        "id ",
        "id[0]",
        "properties[0]",
        "[id]",
        "definition.parts",
        "connections",
        "schedules",
        "notproperties.x",
        "'properties.x'",
    ] {
        assert!(!is_metadata_property_query(query), "{query}");
    }
}

#[test]
fn metadata_flags_preserve_no_option_bytes_requests_and_empty_silence() {
    for query in ["id", "description", "properties", "properties.level.deeper"] {
        for format in ["json", "text"] {
            let baseline = command(&[
                "get",
                "Demo.Workspace/Book.Notebook",
                "-q",
                query,
                "--output_format",
                format,
            ]);
            let (expected, expected_requests, _) = session(
                &baseline,
                vec![workspaces(), items(), metadata()],
                pending(),
            );
            for (force, verbose) in [(true, false), (false, true), (true, true)] {
                let mut selected = baseline.clone();
                selected.force = force;
                selected.verbose = verbose;
                let (actual, requests, _) = session(
                    &selected,
                    vec![workspaces(), items(), metadata()],
                    pending(),
                );
                assert_eq!(actual.exit_code, expected.exit_code);
                assert_eq!(streams(&actual), streams(&expected));
                assert_eq!(requests, expected_requests);
                assert_eq!(
                    requests.last().unwrap(),
                    "https://api.fabric.microsoft.com/v1/workspaces/w/notebooks/n"
                );
            }
        }
    }
}

#[test]
fn workspace_flags_do_not_change_depth_projection_or_streams() {
    for query in [
        None,
        Some(""),
        Some("."),
        Some("sparkSettings.level.deep.more.value"),
    ] {
        let mut baseline = command(&["get", "Demo.Workspace"]);
        baseline.query = query.map(str::to_owned);
        let replies = || {
            vec![
                workspaces(),
                reply(json!({"id":"w","displayName":"Demo"})),
                reply(json!({"value":[]})),
                reply(json!({"level":{"deep":{"more":{"value":true}}}})),
                reply(json!({"value":[]})),
            ]
        };
        let (expected, expected_requests, _) = session(&baseline, replies(), pending());
        for (force, verbose) in [(true, false), (false, true), (true, true)] {
            let mut selected = baseline.clone();
            selected.force = force;
            selected.verbose = verbose;
            let (actual, requests, _) = session(&selected, replies(), pending());
            assert_eq!(actual.exit_code, expected.exit_code);
            assert_eq!(streams(&actual), streams(&expected));
            assert_eq!(requests, expected_requests);
        }
    }
}

#[test]
fn definition_queries_stay_explicitly_unsupported_after_target_resolution() {
    for query in [
        None,
        Some(""),
        Some("."),
        Some("definition.parts"),
        Some("properties[0]"),
    ] {
        for (force, verbose) in [(true, false), (false, true), (true, true)] {
            let mut selected = command(&["get", "Demo.Workspace/Book.Notebook"]);
            selected.query = query.map(str::to_owned);
            selected.force = force;
            selected.verbose = verbose;
            let (plan, requests, _) = session(&selected, vec![workspaces(), items()], pending());
            assert_eq!(plan.exit_code, 1);
            assert_eq!(failure_code(&plan), "NotSupported");
            assert_eq!(requests.len(), 2);
            assert!(
                !requests
                    .iter()
                    .any(|request| request.contains("getDefinition"))
            );
        }
    }
}

#[test]
fn output_combinations_never_fall_through_to_success_or_file_io() {
    for output in ["", "not-written.json"] {
        for (force, verbose) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut selected = command(&["get", "Demo.Workspace", "-q", "id"]);
            selected.output = Some(output.into());
            selected.force = force;
            selected.verbose = verbose;
            let (plan, requests, tokens) = session(&selected, vec![], pending());
            assert_eq!(failure_code(&plan), "NotSupported");
            assert!(requests.is_empty());
            assert_eq!(tokens, 0);
        }
    }
}

#[test]
fn invalid_format_cancellation_and_missing_target_precede_metadata_reads() {
    let mut selected = command(&[
        "get",
        "Demo.Workspace/Book.Notebook",
        "-q",
        "id",
        "-f",
        "-v",
    ]);
    selected.output_format = Some("invalid".into());
    let http = Arc::new(Http::new([]));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential.clone());
    let reads = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: "/",
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
    };
    assert!(complete(reads.dispatch(&selected, pending())).is_err());
    assert!(http.requests.lock().unwrap().is_empty());
    assert!(credential.scopes.lock().unwrap().is_empty());
    selected.output_format = None;
    let (plan, requests, tokens) = session(&selected, vec![], ready(()));
    assert_eq!(failure_code(&plan), "LongRunningOperationCancelled");
    assert!(requests.is_empty());
    assert_eq!(tokens, 0);
    selected.path = vec!["Missing.Workspace/Book.Notebook".into()];
    let (plan, requests, _) = session(&selected, vec![workspaces()], pending());
    assert_eq!(failure_code(&plan), "NotFound");
    assert_eq!(requests.len(), 1);
}

#[test]
fn service_failure_remains_failure_with_its_existing_exit_and_stream_contract() {
    for status in [401, 403, 404] {
        let code = match status {
            401 => "Unauthorized",
            403 => "Forbidden",
            _ => "NotFound",
        };
        let mut denied = reply(json!({"errorCode":code, "message":"private-service-detail"}));
        denied.status = status;
        let selected = command(&[
            "get",
            "Demo.Workspace/Book.Notebook",
            "-q",
            "id",
            "-f",
            "-v",
        ]);
        let (plan, requests, _) =
            session(&selected, vec![workspaces(), items(), denied], pending());
        assert_eq!(plan.exit_code, if status == 401 { 4 } else { 1 });
        assert_eq!(failure_code(&plan), code);
        assert!(!String::from_utf8_lossy(&streams(&plan).0).contains("private-service-detail"));
        assert_eq!(requests.len(), 3);
    }
}

struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "synthetic writer failure",
        ))
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("the caller's writer must not be flushed")
    }
}

#[test]
fn failed_output_delivery_remains_an_error_not_a_successful_write() {
    let selected = command(&[
        "get",
        "Demo.Workspace/Book.Notebook",
        "-q",
        "id",
        "-f",
        "-v",
    ]);
    let (plan, _, _) = session(
        &selected,
        vec![workspaces(), items(), metadata()],
        pending(),
    );
    assert_eq!(
        plan.write_to(&mut BrokenWriter, &mut Vec::new())
            .unwrap_err()
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(plan.exit_code, 0);
}

#[test]
fn direct_read_host_uses_the_same_option_gate_without_a_session_wrapper() {
    let credential = Arc::new(Credential::default());
    let sdk = sdk(credential);
    let http = Arc::new(Http::new([metadata(), metadata()]));
    let executor = executor(http.clone(), Arc::new(Timer::default()));
    let tenant = Element::tenant("Tenant", "offline-tenant");
    let workspace = Element::child(
        "Demo",
        Some("w".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        tenant.clone(),
    )
    .unwrap();
    let item = Element::child(
        "Book",
        Some("n".into()),
        NodeKind::Item(ItemType::parse("Notebook").unwrap()),
        workspace.clone(),
    )
    .unwrap();
    let lookup = Catalog::new(vec![workspace.clone(), item]);
    let host = ReadHost {
        sdk: &sdk,
        workspaces: &executor,
        items: &executor,
        capacities: &executor,
        lookup: &lookup,
        current: workspace,
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
        projection: &PythonQuery,
    };
    let selected = command(&["get", "Book.Notebook", "-q", "id", "-f", "-v"]);
    let plan = complete(host.dispatch(&selected, pending())).unwrap();
    assert_eq!(plan.exit_code, 0);
    let mut rejected = selected.clone();
    rejected.query = Some("definition.parts".into());
    assert_eq!(
        complete(host.dispatch(&rejected, pending()))
            .unwrap()
            .exit_code,
        1
    );
    rejected = selected.clone();
    rejected.kind = fabric_cli::parser::ReadKind::List;
    assert_eq!(
        complete(host.dispatch(&rejected, pending()))
            .unwrap()
            .exit_code,
        1
    );
    let error =
        complete(host.run(&selected, pending(), &mut BrokenWriter, &mut Vec::new())).unwrap_err();
    assert!(matches!(error, WriteError::Io(error) if error.kind() == io::ErrorKind::BrokenPipe));
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    http.assert_consumed();
}
