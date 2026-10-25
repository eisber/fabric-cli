// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public caller compositions through the SDK facade and real shared HTTP policy.

#[allow(dead_code)]
mod folder_support;

use fabric_cli::{
    ItemSort, NoQuery, ReadConfig, ReadHost,
    parser::{Invocation, ReadCommand, parse_args},
};
use fabric_cli_context::{Catalog, ContextError, Element, HomeContext, NodeKind, WorkspaceType};
use fabric_cli_output::Timestamp;
use fabric_sdk::{folders::FolderListLimits, items::ItemListLimits};
use folder_support::{Http, Reply, Timer, complete, executor, reply, sdk};
use serde_json::{Value, json};
use std::{
    future::{Future, pending, poll_fn, ready},
    io,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

fn parse(arguments: &[&str]) -> ReadCommand {
    let Invocation::Read(command) =
        parse_args(std::iter::once("fab-rs").chain(arguments.iter().copied())).unwrap()
    else {
        panic!("read command required");
    };
    command
}

fn workspace() -> Arc<Element> {
    Element::child(
        "W",
        Some("w".into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        Element::tenant("T", "t"),
    )
    .unwrap()
}

fn folder(name: &str, id: &str, parent: Arc<Element>) -> Arc<Element> {
    Element::child(name, Some(id.into()), NodeKind::Folder, parent).unwrap()
}

fn host<'a>(
    sdk: &'a fabric_sdk::Client,
    exec: &'a fabric_sdk_http::HttpExecutor,
    catalog: &'a Catalog,
    current: Arc<Element>,
) -> ReadHost<'a> {
    ReadHost {
        sdk,
        workspaces: exec,
        items: exec,
        capacities: exec,
        lookup: catalog,
        current,
        home: HomeContext::Unspecified,
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig {
            folder_listing_enabled: true,
            ..ReadConfig::default()
        },
        projection: &NoQuery,
    }
    .with_default_projection()
}

fn output(host: &ReadHost<'_>, exec: &fabric_sdk_http::HttpExecutor, args: &[&str]) -> (Value, u8) {
    let (mut stdout, mut stderr) = (vec![], vec![]);
    let exit =
        complete(
            host.with_folders(exec)
                .run(&parse(args), pending(), &mut stdout, &mut stderr),
        )
        .unwrap();
    assert!(stderr.is_empty());
    assert!(stdout.ends_with(b"\n"));
    (serde_json::from_slice(&stdout).unwrap(), exit)
}

#[test]
fn workspace_selects_direct_children_and_preserves_source_names_types_ids_and_hidden() {
    let current = workspace();
    let credential = Arc::new(folder_support::Credential::default());
    let sdk = sdk(credential.clone());
    let http = Arc::new(Http::new([
        reply(json!({"value":[
            {"id":"root+/= id","displayName":"alpha","type":"notebook","unknown":true},
            {"id":"nested","displayName":"NOT ROOT","type":"Notebook","folderId":"b"},
            {"id":"r","displayName":"alpha","type":"Report"},
            {"id":"u","displayName":"Équipe","type":"Notebook"},
            {"id":"future","displayName":"Future","type":"FutureType"}
        ]})),
        reply(json!({"value":[
            {"id":"b","displayName":"Nested","parentFolderId":"a"},
            {"id":"a","displayName":"alpha"},
            {"id":"s","displayName":"Already.FoLdEr"},
            {"id":"u","displayName":"équipe","future":null}
        ]})),
    ]));
    let exec = executor(http.clone(), Arc::default());
    let catalog = Catalog::new(vec![]);
    let host = host(&sdk, &exec, &catalog, current.clone());
    let (value, exit) = output(&host, &exec, &["ls", "-l", "-a"]);
    assert_eq!(exit, 0);
    assert_eq!(
        value,
        json!({
            "timestamp":"2000-01-01T00:00:00Z","status":"Success","command":"ls",
            "result":{"data":[
                {"name":"alpha.Folder","id":"a"},
                {"name":"alpha.Notebook","id":"root+/= id"},
                {"name":"alpha.Report","id":"r"},
                {"name":"Already.FoLdEr.Folder","id":"s"},
                {"name":"équipe.Folder","id":"u"},
                {"name":"Équipe.Notebook","id":"u"}
            ],"hidden_data":[".externaldatashares",".managedidentities",".managedprivateendpoints",".sparkpools"]}
        })
    );
    assert!(Arc::ptr_eq(&host.current, &current));
    assert_eq!(
        *http.requests.lock().unwrap(),
        [
            "https://api.fabric.microsoft.com/v1/workspaces/w/items",
            "https://api.fabric.microsoft.com/v1/workspaces/w/folders?recursive=True",
        ]
    );
    assert_eq!(credential.scopes.lock().unwrap().len(), 2);
    http.assert_consumed();
}

#[test]
fn nested_folder_forces_presentation_and_ignores_query_all_and_hidden_like_source() {
    let current = folder("Parent", "p", folder("Root", "r", workspace()));
    let sdk = sdk(Arc::default());
    let http = Arc::new(Http::new([
        reply(json!({"value":[
            {"id":"i","displayName":"alpha","type":"Notebook","folderId":"p"},
            {"id":"deep","displayName":"Deep","type":"Report","folderId":"n3"},
            {"id":"flat","displayName":"Flat","type":"Report"}
        ]})),
        reply(json!({"value":[
            {"id":"n1","displayName":"same","parentFolderId":"p"},
            {"id":"n2","displayName":"SAME","parentFolderId":"p"},
            {"id":"p","displayName":"Parent","parentFolderId":"r"},
            {"id":"r","displayName":"Root"},
            {"id":"n3","displayName":"same","parentFolderId":"p"}
        ]})),
    ]));
    let exec = executor(http.clone(), Arc::default());
    let catalog = Catalog::new(vec![]);
    let mut host = host(&sdk, &exec, &catalog, current.clone());
    host.config.folder_listing_enabled = false;
    host.config.show_hidden = true;
    host.projection = &NoQuery;
    let (value, exit) = output(&host, &exec, &["ls", "-la", "-q", "invalid("]);
    assert_eq!(exit, 0);
    assert_eq!(
        value["result"],
        json!({"data":[
            {"name":"alpha.Notebook","id":"i"},
            {"name":"same.Folder","id":"n3"},
            {"name":"same.Folder","id":"n1"},
            {"name":"SAME.Folder","id":"n2"}
        ]})
    );
    assert!(Arc::ptr_eq(&host.current, &current));
    http.assert_consumed();
}

#[test]
fn duplicate_id_uses_first_assembled_parent_and_structural_folder_equality() {
    for (name, expected) in [
        ("First", json!([{"name":"Child.Folder"}])),
        ("Second", json!([])),
    ] {
        let sdk = sdk(Arc::default());
        let http = Arc::new(Http::new([
            reply(json!({"value":[]})),
            reply(json!({"value":[
                {"id":"c","displayName":"Child","parentFolderId":"d"},
                {"id":"d","displayName":"First"},
                {"id":"d","displayName":"Second"},
                {"id":"d","displayName":"First"}
            ]})),
        ]));
        let exec = executor(http.clone(), Arc::default());
        let catalog = Catalog::new(vec![]);
        let host = host(&sdk, &exec, &catalog, folder(name, "d", workspace()));
        let (value, exit) = output(&host, &exec, &["ls"]);
        assert_eq!(exit, 0);
        assert_eq!(value["result"]["data"], expected);
    }
}

#[test]
fn caller_preloaded_folder_path_preserves_case_first_lookup_and_current_context() {
    let current = workspace();
    let first = folder("Dup", "first", current.clone());
    let second = folder("Dup", "second", current.clone());
    let catalog = Catalog::new(vec![first, second]);
    let sdk = sdk(Arc::default());
    let http = Arc::new(Http::new([
        reply(json!({"value":[
            {"id":"i1","displayName":"First","type":"Notebook","folderId":"first"},
            {"id":"i2","displayName":"Second","type":"Notebook","folderId":"second"}
        ]})),
        reply(json!({"value":[
            {"id":"first","displayName":"Dup"},
            {"id":"second","displayName":"Dup"}
        ]})),
    ]));
    let exec = executor(http.clone(), Arc::default());
    let host = host(&sdk, &exec, &catalog, current.clone());
    let (value, exit) = output(&host, &exec, &["ls", "Dup.folder", "-l"]);
    assert_eq!(exit, 0);
    assert_eq!(
        value["result"]["data"],
        json!([{"name":"First.Notebook","id":"i1"}])
    );
    let (value, exit) = output(&host, &exec, &["ls", "dup.Folder"]);
    assert_eq!(exit, 1);
    assert_eq!(value["result"]["error_code"], "NotFound");
    assert!(Arc::ptr_eq(&host.current, &current));
    http.assert_consumed();
}

#[test]
fn bytype_uses_source_enum_member_names_and_mixed_case_folder_type() {
    let sdk = sdk(Arc::default());
    let http = Arc::new(Http::new([
        reply(json!({"value":[
            {"id":"co","displayName":"B","type":"CosmosDBDatabase"},
            {"id":"cp","displayName":"Z","type":"CopyJob"},
            {"id":"n","displayName":"A","type":"Notebook"},
            {"id":"g","displayName":"A","type":"GraphModel"}
        ]})),
        reply(json!({"value":[{"id":"f","displayName":"Z"}]})),
    ]));
    let exec = executor(http, Arc::default());
    let catalog = Catalog::new(vec![]);
    let mut host = host(&sdk, &exec, &catalog, workspace());
    host.config.item_sort = ItemSort::ByType;
    let (value, exit) = output(&host, &exec, &["ls"]);
    assert_eq!(exit, 0);
    assert_eq!(
        value["result"]["data"],
        json!([
            {"name":"Z.CopyJob"},{"name":"B.CosmosDBDatabase"},{"name":"Z.Folder"},
            {"name":"A.GraphModel"},{"name":"A.Notebook"}
        ])
    );
}

#[test]
fn workspace_queries_use_real_native_projection_after_source_column_selection() {
    for (query, expected) in [
        ("[?contains(name, 'Folder')].name", json!(["A.Folder"])),
        ("[].id", json!([])),
        ("`false`", json!("False")),
        ("length(@)", json!("2")),
        ("[0].name", json!("A.Folder")),
        ("", json!(["[0].name", "[1].name"])),
    ] {
        let sdk = sdk(Arc::default());
        let http = Arc::new(Http::new([
            reply(json!({"value":[{"id":"i","displayName":"B","type":"Notebook"}]})),
            reply(json!({"value":[{"id":"f","displayName":"A"}]})),
        ]));
        let exec = executor(http, Arc::default());
        let catalog = Catalog::new(vec![]);
        let host = host(&sdk, &exec, &catalog, workspace());
        let (value, exit) = output(&host, &exec, &["ls", "-q", query]);
        assert_eq!(exit, 0);
        let expected = if expected.is_string() {
            json!([expected])
        } else {
            expected
        };
        assert_eq!(value["result"]["data"], expected, "{query}");
    }
}

#[test]
fn query_errors_and_explicit_provider_opt_out_do_not_become_plain_success() {
    for (query, code, no_query) in [
        ("invalid(", "InvalidInput", false),
        ("abs(name)", "InvalidInput", false),
        ("sum(`[9007199254740993]`)", "InvalidInput", false),
        (".", "NotSupported", true),
    ] {
        let sdk = sdk(Arc::default());
        let http = Arc::new(Http::new([
            reply(json!({"value":[]})),
            reply(json!({"value":[]})),
        ]));
        let exec = executor(http, Arc::default());
        let catalog = Catalog::new(vec![]);
        let mut host = host(&sdk, &exec, &catalog, workspace());
        if no_query {
            host.projection = &NoQuery;
        }
        let (value, exit) = output(&host, &exec, &["ls", "-q", query]);
        assert_eq!(exit, 1, "{query}");
        assert_eq!(value["result"]["error_code"], code, "{query}");
        assert!(value["result"].get("data").is_none());
    }
}

#[test]
fn source_order_loads_folders_before_unknown_or_missing_type_and_only_once() {
    for (item_type, expected_code) in [
        (Some(json!("FutureType")), None),
        (None, Some("InvalidResponse")),
    ] {
        let sdk = sdk(Arc::default());
        let mut item = json!({"id":"i","displayName":"I","folderId":"f"});
        if let Some(item_type) = item_type {
            item["type"] = item_type;
        }
        let http = Arc::new(Http::new([
            reply(json!({"value":[item]})),
            reply(json!({"value":[{"id":"f","displayName":"F"}]})),
        ]));
        let exec = executor(http.clone(), Arc::default());
        let catalog = Catalog::new(vec![]);
        let host = host(&sdk, &exec, &catalog, workspace());
        let (value, exit) = output(&host, &exec, &["ls"]);
        if let Some(code) = expected_code {
            assert_eq!(exit, 1);
            assert_eq!(value["result"]["error_code"], code);
        } else {
            assert_eq!(exit, 0);
            assert_eq!(value["result"]["data"], json!([{"name":"F.Folder"}]));
        }
        assert_eq!(http.requests.lock().unwrap().len(), 2);
        http.assert_consumed();
    }
}

#[test]
fn malformed_graphs_and_item_parent_never_hang_reparent_or_emit_partial_data() {
    for (folders, item, category) in [
        (
            json!([{"id":"f","displayName":"F","parentFolderId":null}]),
            None,
            "NullParent",
        ),
        (
            json!([{"id":"f","displayName":"F","parentFolderId":1}]),
            None,
            "NonStringParent",
        ),
        (
            json!([{"id":"f","displayName":"F","parentFolderId":"missing"}]),
            None,
            "UnresolvedParent",
        ),
        (
            json!([
                {"id":"a","displayName":"A","parentFolderId":"b"},
                {"id":"b","displayName":"B","parentFolderId":"a"}
            ]),
            None,
            "Cycle",
        ),
        (
            json!([]),
            Some(json!({"id":"i","displayName":"I","type":"Notebook","folderId":null})),
            "ItemParent",
        ),
    ] {
        let sdk = sdk(Arc::default());
        let http = Arc::new(Http::new([
            reply(json!({"value":item.into_iter().collect::<Vec<_>>()})),
            reply(json!({"value":folders})),
        ]));
        let exec = executor(http, Arc::default());
        let catalog = Catalog::new(vec![]);
        let host = host(&sdk, &exec, &catalog, workspace());
        let (value, exit) = output(&host, &exec, &["ls"]);
        assert_eq!(exit, 1);
        assert_eq!(value["result"]["error_code"], "NotSupported");
        assert!(
            value["result"]["message"]
                .as_str()
                .unwrap()
                .contains(category)
        );
        assert!(value["result"].get("data").is_none());
    }
}

#[test]
fn real_shared_http_enforces_retry_paging_headers_and_complete_folder_snapshot() {
    let sdk = sdk(Arc::default());
    let mut page = reply(json!({
        "value":[{"id":"nested","displayName":"Nested","parentFolderId":"root"}],
        "continuationToken":"ignored-body"
    }));
    page.headers
        .insert("x-ms-continuation".into(), "a+/= ~*".into());
    let http = Arc::new(Http::new([
        reply(json!({"value":[]})),
        Reply {
            status: 503,
            body: json!(""),
            headers: Default::default(),
        },
        Reply {
            status: 429,
            body: json!(""),
            headers: Default::default(),
        },
        page,
        reply(json!({"value":[{"id":"root","displayName":"Root"}]})),
    ]));
    let timer = Arc::new(Timer::default());
    let exec = executor(http.clone(), timer.clone());
    let catalog = Catalog::new(vec![]);
    let host = host(&sdk, &exec, &catalog, workspace());
    let (value, exit) = output(&host, &exec, &["ls"]);
    assert_eq!(exit, 0);
    assert_eq!(value["result"]["data"], json!([{"name":"Root.Folder"}]));
    assert_eq!(
        http.requests.lock().unwrap()[4],
        "https://api.fabric.microsoft.com/v1/workspaces/w/folders?recursive=True&continuationToken=a%2B%2F%3D+~%2A"
    );
    assert!(timer.0.lock().unwrap().contains(&Duration::from_secs(10)));
    http.assert_consumed();
}

#[test]
fn service_failures_and_malformed_responses_do_not_emit_completed_partial_rows() {
    for (status, body, code, exit) in [
        (401, json!({}), "Unauthorized", 4),
        (403, json!({}), "Forbidden", 1),
        (404, json!({}), "NotFound", 1),
        (
            200,
            json!({"value":[{"id":"missing-name"}]}),
            "InvalidResponse",
            1,
        ),
    ] {
        let sdk = sdk(Arc::default());
        let http = Arc::new(Http::new([
            reply(json!({"value":[]})),
            Reply {
                status,
                body,
                headers: Default::default(),
            },
        ]));
        let exec = executor(http, Arc::default());
        let catalog = Catalog::new(vec![]);
        let host = host(&sdk, &exec, &catalog, workspace());
        let (value, actual_exit) = output(&host, &exec, &["ls"]);
        assert_eq!(actual_exit, exit);
        assert_eq!(value["result"]["error_code"], code);
        assert!(value["result"].get("data").is_none());
    }
}

#[test]
fn traversal_body_and_continuation_boundaries_are_explicit_failures() {
    for (body, limit_kind) in [
        (
            json!({"value":[{"id":"f","displayName":"F"}],"continuationToken":"next"}),
            "pages",
        ),
        (
            json!({"value":[{"id":"f","displayName":"F"},{"id":"g","displayName":"G"}]}),
            "items",
        ),
        (
            json!({"value":[],"continuationToken":"x","continuationUri":"https://foreign.invalid/folders"}),
            "continuation",
        ),
        (json!({"value":[{"id":"f","displayName":"F"}]}), "bytes"),
    ] {
        let sdk = sdk(Arc::default());
        let http = Arc::new(Http::new([reply(json!({"value":[]})), reply(body)]));
        let exec = executor(http.clone(), Arc::default());
        let catalog = Catalog::new(vec![]);
        let host = host(&sdk, &exec, &catalog, workspace());
        let mut folders = FolderListLimits::default();
        match limit_kind {
            "pages" => folders.max_pages = NonZeroUsize::new(1).unwrap(),
            "items" => folders.max_items = NonZeroUsize::new(1).unwrap(),
            "bytes" => folders.max_response_bytes = NonZeroUsize::new(10).unwrap(),
            _ => {}
        }
        let plan = complete(
            host.with_folders(&exec)
                .with_limits(ItemListLimits::default(), folders)
                .dispatch(&parse(&["ls"]), pending()),
        )
        .unwrap();
        assert_eq!(plan.exit_code, 1);
        let output: Value = serde_json::from_slice(&plan.payload).unwrap();
        assert_eq!(output["result"]["error_code"], "ReadFailed");
        assert!(output["result"].get("data").is_none());
        http.assert_consumed();
    }
}

#[test]
fn old_workspace_and_root_reads_delegate_without_folder_requests_or_byte_changes() {
    for (path, body, folder_config) in [
        (
            ".",
            json!({"value":[{"id":"i","displayName":"I","type":"Notebook","folderId":"unresolved"}]}),
            false,
        ),
        (
            "/",
            json!({"value":[{"id":"w","displayName":"W","type":"Workspace"}]}),
            true,
        ),
    ] {
        let sdk = sdk(Arc::default());
        let http = Arc::new(Http::new([reply(body.clone()), reply(body)]));
        let exec = executor(http.clone(), Arc::default());
        let catalog = Catalog::new(vec![]);
        let mut host = host(&sdk, &exec, &catalog, workspace());
        host.config.folder_listing_enabled = folder_config;
        let command = parse(&["ls", path, if path == "/" { "-a" } else { "-la" }]);
        let base = complete(host.dispatch(&command, pending())).unwrap();
        let wrapped = complete(host.with_folders(&exec).dispatch(&command, pending())).unwrap();
        assert_eq!(base.payload, wrapped.payload);
        assert_eq!(base.exit_code, wrapped.exit_code);
        assert_eq!(http.requests.lock().unwrap().len(), 2);
        assert!(
            !http
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|url| url.contains("/folders"))
        );
    }
}

#[test]
fn get_delegates_to_the_base_host_with_no_folder_graph_or_metadata_rewrite() {
    let item = Element::child(
        "I",
        Some("i".into()),
        NodeKind::Item(fabric_cli_context::ItemType::parse("Notebook").unwrap()),
        workspace(),
    )
    .unwrap();
    let sdk = sdk(Arc::default());
    let metadata =
        json!({"future":{"b":[1,1.0,null],"a":false},"displayName":"I","id":"i","type":null});
    let http = Arc::new(Http::new([reply(metadata.clone()), reply(metadata)]));
    let exec = executor(http.clone(), Arc::default());
    let catalog = Catalog::new(vec![]);
    let host = host(&sdk, &exec, &catalog, item);
    let command = parse(&["get", ".", "-q", "."]);
    let base = complete(host.dispatch(&command, pending())).unwrap();
    let wrapped = complete(host.with_folders(&exec).dispatch(&command, pending())).unwrap();
    assert_eq!(base.payload, wrapped.payload);
    assert_eq!(base.exit_code, wrapped.exit_code);
    assert_eq!(base.exit_code, 0);
    assert!(
        http.requests
            .lock()
            .unwrap()
            .iter()
            .all(|url| !url.contains("/folders"))
    );
    http.assert_consumed();
}

#[test]
fn cancellation_and_lookup_snapshot_failures_preserve_old_priority_and_boundary() {
    let sdk = sdk(Arc::default());
    let http = Arc::new(Http::new([]));
    let exec = executor(http.clone(), Arc::default());
    let lookups = AtomicUsize::new(0);
    let lookup = |_: fabric_cli_context::LookupRequest<'_>| {
        lookups.fetch_add(1, Ordering::SeqCst);
        Err(ContextError::lookup(
            "NotSupported",
            "Folder path snapshot is not preloaded",
        ))
    };
    let catalog = Catalog::new(vec![]);
    let mut host = host(&sdk, &exec, &catalog, workspace());
    host.lookup = &lookup;
    let command = parse(&["ls", "A.Folder"]);
    let plan = complete(host.with_folders(&exec).dispatch(&command, ready(()))).unwrap();
    assert_eq!(plan.exit_code, 1);
    assert_eq!(lookups.load(Ordering::SeqCst), 0);
    let plan = complete(host.with_folders(&exec).dispatch(&command, pending())).unwrap();
    assert_eq!(plan.exit_code, 1);
    let output: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(output["result"]["error_code"], "NotSupported");
    assert_eq!(
        output["result"]["message"],
        "Folder path snapshot is not preloaded"
    );
    assert!(http.requests.lock().unwrap().is_empty());
}

#[test]
fn cancellation_during_real_folder_http_discards_the_incomplete_command() {
    let sdk = sdk(Arc::default());
    let mut http = Http::new([reply(json!({"value":[]}))]);
    http.hold_after = Some(2);
    let http = Arc::new(http);
    let exec = executor(http.clone(), Arc::default());
    let catalog = Catalog::new(vec![]);
    let host = host(&sdk, &exec, &catalog, workspace());
    let wrapper = host.with_folders(&exec);
    let cancelled = AtomicBool::new(false);
    let command = parse(&["ls"]);
    let mut future = Box::pin(wrapper.dispatch(
        &command,
        poll_fn(|_| {
            if cancelled.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }),
    ));
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    cancelled.store(true, Ordering::SeqCst);
    let plan = complete(future).unwrap();
    assert_eq!(plan.exit_code, 1);
    let value: Value = serde_json::from_slice(&plan.payload).unwrap();
    assert_eq!(
        value["result"]["error_code"],
        "LongRunningOperationCancelled"
    );
    assert!(value["result"].get("data").is_none());
}

#[test]
fn writer_failures_retain_the_original_io_error() {
    struct Broken;
    impl io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "folder writer"))
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("must not flush");
        }
    }
    let sdk = sdk(Arc::default());
    let http = Arc::new(Http::new([
        reply(json!({"value":[]})),
        reply(json!({"value":[]})),
    ]));
    let exec = executor(http, Arc::default());
    let catalog = Catalog::new(vec![]);
    let host = host(&sdk, &exec, &catalog, workspace());
    let error = complete(host.with_folders(&exec).run(
        &parse(&["ls"]),
        pending(),
        &mut Broken,
        &mut vec![],
    ))
    .unwrap_err();
    assert!(
        matches!(error, fabric_cli::WriteError::Io(ref error) if error.kind() == io::ErrorKind::BrokenPipe)
    );
}
