// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Read-only prefix selection through parsed reads and the real HTTP executor.
#![deny(unnameable_test_items)]

#[allow(dead_code)]
mod folder_support;

use fabric_cli::{
    ReadConfig, ReadHost,
    parser::{Invocation, ReadCommand, parse_args},
    session::ReadSession,
};
use fabric_cli_context::{Catalog, Element, HomeContext, NodeKind, WorkspaceType};
use fabric_cli_output::Timestamp;
use fabric_sdk::{folders::FolderListLimits, items::ItemListLimits};
use folder_support::{Http, Reply, complete, executor, reply, sdk};
use serde_json::{Value, json};
use std::{future::pending, num::NonZeroUsize, sync::Arc};

fn parse(args: &[&str]) -> ReadCommand {
    let Invocation::Read(command) =
        parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
    else {
        panic!("read invocation");
    };
    command
}

fn workspaces() -> Reply {
    reply(json!({"value":[{"id":"w","displayName":"Demo","type":"Workspace"}]}))
}

fn run(
    args: &[&str],
    current: &str,
    home: HomeContext,
    folders: bool,
    replies: impl IntoIterator<Item = Reply>,
) -> (u8, Value, Vec<String>) {
    let http = Arc::new(Http::new(replies));
    let executor = executor(http.clone(), Arc::default());
    let sdk = sdk(Arc::new(folder_support::Credential::default()));
    let session = ReadSession {
        sdk: &sdk,
        executor: &executor,
        tenant: Element::tenant("Tenant", "offline-tenant"),
        current_path: current,
        home,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig {
            folder_listing_enabled: folders,
            ..ReadConfig::default()
        },
    };
    let command = parse(args);
    let before = command.clone();
    let dispatch = complete(session.dispatch_with_evidence(&command, pending())).unwrap();
    assert_eq!(command, before);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    dispatch.plan.write_to(&mut stdout, &mut stderr).unwrap();
    assert!(stderr.is_empty());
    http.assert_consumed();
    let requests = http.requests.lock().unwrap().clone();
    (
        dispatch.plan.exit_code,
        serde_json::from_slice(&stdout).unwrap(),
        requests,
    )
}

#[test]
fn root_prefix_finishes_all_pages_and_preserves_case_and_order_without_fanout() {
    let (exit, output, requests) = run(
        &["ls", "marc*"],
        "/",
        HomeContext::Unspecified,
        false,
        [
            reply(json!({"value":[
                {"id":"x","displayName":"Other","type":"Workspace"},
                {"id":"u","displayName":"Marc","type":"Workspace"}
            ],"continuationToken":"next +/="})),
            reply(json!({"value":[
                {"id":"p","displayName":"marcus","type":"Personal"},
                {"id":"w","displayName":"marc","type":"workspace"}
            ]})),
        ],
    );
    assert_eq!(exit, 0);
    assert_eq!(
        output["result"]["data"],
        json!([{"name":"marc.Workspace"},{"name":"marcus.Personal"}])
    );
    assert_eq!(
        requests,
        [
            "https://api.fabric.microsoft.com/v1/workspaces",
            "https://api.fabric.microsoft.com/v1/workspaces?continuationToken=next+%2B%2F%3D"
        ]
    );
}

#[test]
fn known_suffix_is_case_insensitive_but_display_name_prefix_is_not() {
    for (path, expected) in [
        ("marc.workspace*", json!([{"name":"marc.Workspace"}])),
        ("Marc.workspace*", json!([])),
        ("marc.Unknown*", json!([{"name":"marc.Unknown.Workspace"}])),
    ] {
        let (exit, output, requests) = run(
            &["dir", path],
            "/",
            HomeContext::Unspecified,
            false,
            [reply(json!({"value":[
                {"id":"w","displayName":"marc","type":"Workspace"},
                {"id":"p","displayName":"marc","type":"Personal"},
                {"id":"u","displayName":"marc.Unknown","type":"Workspace"}
            ]}))],
        );
        assert_eq!(exit, 0);
        assert_eq!(output["command"], "dir");
        assert_eq!(output["result"]["data"], expected);
        assert_eq!(requests.len(), 1);
    }
}

#[test]
fn empty_long_prefix_does_not_enrich_or_emit_other_children() {
    let (exit, output, requests) = run(
        &["ls", "missing*", "--long"],
        "/",
        HomeContext::Unspecified,
        false,
        [workspaces()],
    );
    assert_eq!(exit, 0);
    assert_eq!(output["result"], json!({"data":[]}));
    assert_eq!(requests.len(), 1);
}

#[test]
fn long_root_prefix_retains_capacity_details_for_matching_workspaces() {
    let (exit, output, requests) = run(
        &["ls", "Demo*", "--long"],
        "/",
        HomeContext::Unspecified,
        false,
        [
            reply(json!({"value":[
                {"id":"w","displayName":"Demo","type":"Workspace","capacityId":"c"},
                {"id":"o","displayName":"Other","type":"Workspace","capacityId":"d"}
            ]})),
            reply(json!({"value":[{"id":"c","displayName":"Capacity","region":"west"}]})),
        ],
    );
    assert_eq!(exit, 0);
    assert_eq!(
        output["result"]["data"],
        json!([{
            "name":"Demo.Workspace","id":"w","capacityName":"Capacity",
            "capacityId":"c","capacityRegion":"west"
        }])
    );
    assert_eq!(requests.len(), 2);
}

#[test]
fn prefix_filters_hidden_collections_only_when_they_are_enabled() {
    for (args, expected) in [
        (vec!["ls", ".cap*"], json!({"data":[]})),
        (
            vec!["ls", ".cap*", "--all"],
            json!({"data":[],"hidden_data":[".capacities"]}),
        ),
    ] {
        let (exit, output, _) = run(&args, "/", HomeContext::Unspecified, false, [workspaces()]);
        assert_eq!(exit, 0);
        assert_eq!(output["result"], expected);
    }
}

#[test]
fn matching_first_page_does_not_hide_a_later_service_failure() {
    let mut failure = reply(json!({"error":{"code":"InvalidRequest","message":"offline failure"}}));
    failure.status = 400;
    let (exit, output, requests) = run(
        &["ls", "marc*"],
        "/",
        HomeContext::Unspecified,
        false,
        [
            reply(
                json!({"value":[{"id":"w","displayName":"marc","type":"Workspace"}],
                "continuationToken":"more"}),
            ),
            failure,
        ],
    );
    assert_eq!(exit, 1);
    assert_eq!(output["status"], "Failure");
    assert!(output["result"].get("data").is_none());
    assert_eq!(requests.len(), 2);
}

#[test]
fn workspace_prefix_filters_before_long_query_and_never_resolves_each_match() {
    for path in ["marc*", "./marc*", "~/marc*", "/Demo.Workspace/marc*"] {
        let (exit, output, requests) = run(
            &["ls", path, "--long", "-q", "[].id"],
            "/Demo.Workspace",
            HomeContext::Workspace("Demo.Workspace".into()),
            false,
            [
                workspaces(),
                reply(json!({"value":[
                    {"id":"m","displayName":"marc","type":"notebook"},
                    {"id":"r","displayName":"marcia","type":"Report"},
                    {"id":"u","displayName":"Marc","type":"Notebook"},
                    {"id":"o","displayName":"Other","type":"Notebook"}
                ]})),
            ],
        );
        assert_eq!(exit, 0, "{path}: {output}");
        assert_eq!(output["result"]["data"], json!(["m", "r"]));
        assert_eq!(
            requests,
            [
                "https://api.fabric.microsoft.com/v1/workspaces",
                "https://api.fabric.microsoft.com/v1/workspaces/w/items"
            ]
        );
    }
}

#[test]
fn exact_workspace_and_no_prefix_listing_keep_their_original_routing() {
    for path in ["/Demo.Workspace", "/Demo.workspace/"] {
        let (exit, output, requests) = run(
            &["ls", path],
            "/",
            HomeContext::Unspecified,
            false,
            [
                workspaces(),
                reply(json!({"value":[{"id":"i","displayName":"Other","type":"Notebook"}]})),
            ],
        );
        assert_eq!(exit, 0);
        assert_eq!(output["result"]["data"], json!([{"name":"Other.Notebook"}]));
        assert_eq!(requests.len(), 2);
    }
}

#[test]
fn exact_workspace_names_keep_question_marks_and_brackets_literal() {
    for name in ["Demo?", "Demo[", "Demo]", "Demo[1]"] {
        let path = format!("/{name}.Workspace");
        let (exit, output, requests) = run(
            &["ls", &path],
            "/",
            HomeContext::Unspecified,
            false,
            [
                reply(json!({"value":[{"id":"w","displayName":name,"type":"Workspace"}]})),
                reply(json!({"value":[]})),
            ],
        );
        assert_eq!(exit, 0, "{path}: {output}");
        assert_eq!(output["result"]["data"], json!([]));
        assert_eq!(requests.len(), 2);
    }
}

#[test]
fn malformed_patterns_and_get_wildcards_fail_before_metadata_io() {
    for path in [
        "*",
        "mar*c",
        "mar**",
        "mar?*",
        "mar[ab]*",
        "/mar*/child",
        "/mar*/..",
        "/mar*/",
        "/mar*/child*",
        "mar*/../other",
    ] {
        let (exit, output, requests) = run(&["ls", path], "/", HomeContext::Unspecified, false, []);
        assert_eq!(exit, 1, "{path}: {output}");
        assert_eq!(output["result"]["error_code"], "InvalidPath");
        assert!(requests.is_empty());
    }
    let (exit, output, requests) = run(
        &["get", "/Demo.Workspace/marc*"],
        "/",
        HomeContext::Unspecified,
        false,
        [],
    );
    assert_eq!(exit, 1);
    assert_eq!(output["result"]["error_code"], "InvalidPath");
    assert!(requests.is_empty());
}

#[test]
fn folder_prefix_returns_only_direct_children_not_descendant_matches() {
    let folders = || {
        reply(json!({"value":[
            {"id":"f","displayName":"F"},
            {"id":"m","displayName":"marc-folder","parentFolderId":"f"},
            {"id":"d","displayName":"marc-descendant","parentFolderId":"m"}
        ]}))
    };
    let (exit, output, requests) = run(
        &["ls", "/Demo.Workspace/F.Folder/marc*", "--long"],
        "/",
        HomeContext::Unspecified,
        true,
        [
            workspaces(),
            folders(),
            reply(json!({"value":[
                {"id":"n","displayName":"marc","type":"Notebook","folderId":"f"},
                {"id":"d","displayName":"marc-deep","type":"Notebook","folderId":"m"}
            ]})),
            folders(),
        ],
    );
    assert_eq!(exit, 0, "{output}");
    assert_eq!(
        output["result"]["data"],
        json!([{"name":"marc.Notebook","id":"n"},{"name":"marc-folder.Folder","id":"m"}])
    );
    assert_eq!(requests.len(), 4);
    assert!(
        requests
            .iter()
            .all(|url| !url.contains('*') && !url.contains("marc"))
    );
}

#[test]
fn direct_host_prefix_keeps_escaped_slash_and_multi_token_path_semantics() {
    let root = Element::tenant("Tenant", "offline-tenant");
    let workspace = Element::child(
        "Demo Space",
        Some("w".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        root.clone(),
    )
    .unwrap();
    let catalog = Catalog::new(vec![workspace]);
    let http = Arc::new(Http::new([reply(json!({"value":[
        {"id":"n","displayName":"marc/one","type":"Notebook"},
        {"id":"x","displayName":"marc-two","type":"Notebook"}
    ]}))]));
    let exec = executor(http.clone(), Arc::default());
    let sdk = sdk(Arc::default());
    let host = ReadHost {
        sdk: &sdk,
        workspaces: &exec,
        items: &exec,
        capacities: &exec,
        lookup: &catalog,
        current: root,
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
        projection: &fabric_cli::PythonQuery,
    };
    let command = parse(&["ls", "'/Demo", "Space.Workspace/marc\\/o*'"]);
    let plan = complete(host.dispatch(&command, pending())).unwrap();
    let value: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(plan.exit_code, 0, "{value}");
    assert_eq!(
        value["result"]["data"],
        json!([{"name":"marc/one.Notebook"}])
    );
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    http.assert_consumed();
}

#[test]
fn prefix_never_turns_a_partial_bounded_folder_page_into_success() {
    let root = Element::tenant("Tenant", "offline-tenant");
    let workspace = Element::child(
        "Demo",
        Some("w".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        root,
    )
    .unwrap();
    let catalog = Catalog::new(vec![]);
    let http = Arc::new(Http::new([
        reply(json!({"value":[]})),
        reply(json!({"value":[{"id":"f","displayName":"marc"}],"continuationToken":"more"})),
    ]));
    let exec = executor(http.clone(), Arc::default());
    let sdk = sdk(Arc::default());
    let host = ReadHost {
        sdk: &sdk,
        workspaces: &exec,
        items: &exec,
        capacities: &exec,
        lookup: &catalog,
        current: workspace,
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig {
            folder_listing_enabled: true,
            ..ReadConfig::default()
        },
        projection: &fabric_cli::PythonQuery,
    };
    let limits = FolderListLimits {
        max_pages: NonZeroUsize::new(1).unwrap(),
        ..FolderListLimits::default()
    };
    let plan = complete(
        host.with_folders(&exec)
            .with_limits(ItemListLimits::default(), limits)
            .dispatch(&parse(&["ls", "marc*"]), pending()),
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(plan.exit_code, 1);
    assert_eq!(value["result"]["error_code"], "ReadFailed");
    assert!(value["result"].get("data").is_none());
    http.assert_consumed();
}

#[test]
fn cancelled_prefix_keeps_cancellation_priority_and_performs_no_requests() {
    let catalog = Catalog::new(vec![]);
    let http = Arc::new(Http::new([]));
    let exec = executor(http.clone(), Arc::default());
    let sdk = sdk(Arc::default());
    let host = ReadHost {
        sdk: &sdk,
        workspaces: &exec,
        items: &exec,
        capacities: &exec,
        lookup: &catalog,
        current: Element::tenant("Tenant", "offline-tenant"),
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
        projection: &fabric_cli::PythonQuery,
    };
    let plan = complete(host.dispatch(&parse(&["ls", "marc*"]), std::future::ready(()))).unwrap();
    let output: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(plan.exit_code, 1);
    assert_eq!(
        output["result"]["error_code"],
        "LongRunningOperationCancelled"
    );
    assert!(http.requests.lock().unwrap().is_empty());
}
