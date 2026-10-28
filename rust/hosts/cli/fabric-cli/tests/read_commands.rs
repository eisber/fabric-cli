// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real facade + stdlib-only fake executor command compositions.

mod support;

use fabric_cli::{
    ItemSort, NoQuery, Projection, ReadConfig, ReadHost,
    parser::{Invocation, ReadCommand, ReadKind, parse_args},
};
use fabric_cli_context::{Catalog, Element, HomeContext, ItemType, NodeKind, WorkspaceType};
use fabric_cli_output::{CommandError, Timestamp};
use serde_json::{Value, json};
use std::{
    future::{pending, ready},
    io,
    sync::{Arc, Mutex},
};
use support::{Backend, Credential, complete, reply, sdk};

fn parse(args: &[&str]) -> ReadCommand {
    let Invocation::Read(command) =
        parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
    else {
        panic!("expected a read command");
    };
    command
}

fn workspace(root: &Arc<Element>, name: &str, id: &str) -> Arc<Element> {
    Element::child(
        name,
        Some(id.into()),
        NodeKind::Workspace(WorkspaceType::Workspace),
        root.clone(),
    )
    .unwrap()
}

fn host<'a, E>(
    client: &'a fabric_sdk::Client,
    backend: &'a E,
    lookup: &'a Catalog,
    current: Arc<Element>,
    projection: &'a dyn Projection,
) -> ReadHost<'a>
where
    E: fabric_sdk::workspaces::WorkspaceExecutor
        + fabric_sdk::items::ItemExecutor
        + fabric_sdk::capacities::CapacityExecutor,
{
    ReadHost {
        sdk: client,
        workspaces: backend,
        items: backend,
        capacities: backend,
        lookup,
        current,
        home: HomeContext::Workspace("Home.Personal".into()),
        timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        config: ReadConfig::default(),
        projection,
    }
}

fn output(host: &ReadHost<'_>, args: &[&str]) -> (Value, u8) {
    let mut stdout = vec![];
    let mut stderr = vec![];
    let exit = complete(host.run(&parse(args), pending(), &mut stdout, &mut stderr)).unwrap();
    assert!(stderr.is_empty());
    assert!(stdout.ends_with(b"\n"));
    (serde_json::from_slice(&stdout).unwrap(), exit)
}

#[test]
fn root_list_uses_sdk_stable_lowercase_order_types_and_exact_envelope() {
    let root = Element::tenant("Tenant", "tenant");
    let current = workspace(&root, "Current", "current");
    let credential = Arc::new(Credential::default());
    let client = sdk(credential.clone());
    let backend = Backend::new([reply(json!({"value":[
        {"id":"z","displayName":"Zulu","type":"Workspace"},
        {"id":"a1","displayName":"alpha","type":"workspace"},
        {"id":"a2","displayName":"ALPHA","type":"Workspace"},
        {"id":"p","displayName":"Mine","type":"Personal"},
        {"id":"x","displayName":"Hidden","type":"FutureType"},
        {"id":"s","displayName":"A/B","type":"Workspace"},
        {"id":"u","displayName":"\u{e9}quipe","type":"Workspace"}
    ]}))]);
    let catalog = Catalog::new(vec![]);
    let host = host(&client, &backend, &catalog, current.clone(), &NoQuery);
    let (value, exit) = output(&host, &["ls", "/", "-a"]);
    assert_eq!(exit, 0);
    assert_eq!(
        value,
        json!({
            "timestamp":"2000-01-01T00:00:00Z","status":"Success","command":"ls",
            "result":{"data":[
                {"name":"A/B.Workspace"},{"name":"alpha.Workspace"},{"name":"ALPHA.Workspace"},
                {"name":"Mine.Personal"},{"name":"Zulu.Workspace"},{"name":"\u{e9}quipe.Workspace"}
            ],"hidden_data":[".capacities",".connections",".domains",".gateways"]}
        })
    );
    assert!(Arc::ptr_eq(&host.current, &current));
    assert_eq!(
        *backend.requests.lock().unwrap(),
        ["https://api.fabric.microsoft.com/v1/workspaces"]
    );
    assert_eq!(
        *credential.scopes.lock().unwrap(),
        [vec!["https://api.fabric.microsoft.com/.default"]]
    );
}

#[test]
fn long_root_enrichment_uses_real_capacity_sdk_last_id_wins_and_default_columns() {
    let root = Element::tenant("Tenant", "tenant");
    let client = sdk(Arc::default());
    let backend = Backend::new([
        reply(json!({"value":[
            {"id":"a","displayName":"Alpha","type":"Workspace","capacityId":"c"},
            {"id":"b","displayName":"Beta","type":"Personal"}
        ]})),
        reply(json!({"value":[
            {"id":"c","displayName":"Old","region":"old"},
            {"id":"c","displayName":"Capacity","region":null}
        ]})),
    ]);
    let catalog = Catalog::new(vec![]);
    let host = host(&client, &backend, &catalog, root, &NoQuery);
    let (value, exit) = output(&host, &["ls", "-l"]);
    assert_eq!(exit, 0);
    assert_eq!(
        value["result"]["data"],
        json!([
            {"name":"Alpha.Workspace","id":"a","capacityName":"Capacity","capacityId":"c","capacityRegion":null},
            {"name":"Beta.Personal","id":"b","capacityName":"N/A","capacityId":null,"capacityRegion":"Unknown"}
        ])
    );
    assert_eq!(
        *backend.requests.lock().unwrap(),
        [
            "https://api.fabric.microsoft.com/v1/workspaces",
            "https://api.fabric.microsoft.com/v1/capacities",
        ]
    );
    let bytes = complete(host.dispatch(&parse(&["ls"]), ready(())))
        .unwrap()
        .payload;
    assert!(
        String::from_utf8(bytes)
            .unwrap()
            .contains("LongRunningOperationCancelled")
    );
}

#[test]
fn empty_long_listing_never_fetches_capacities_and_retains_empty_data() {
    let client = sdk(Arc::default());
    let backend = Backend::new([reply(json!({"value":[]}))]);
    let catalog = Catalog::new(vec![]);
    let host = host(
        &client,
        &backend,
        &catalog,
        Element::tenant("T", "t"),
        &NoQuery,
    );
    let (value, exit) = output(&host, &["ls", "-l"]);
    assert_eq!(exit, 0);
    assert_eq!(value["result"], json!({"data":[]}));
    assert_eq!(backend.requests.lock().unwrap().len(), 1);
}

#[test]
fn workspace_item_list_resolves_snapshot_path_sort_type_suffixes_and_config_hidden() {
    let root = Element::tenant("Tenant", "tenant");
    let ws = workspace(&root, "Space Name", "w");
    let catalog = Catalog::new(vec![ws]);
    let backend = Backend::new([reply(json!({"value":[
        {"id":"z","displayName":"Zulu","type":"Notebook"},
        {"id":"r","displayName":"alpha","type":"Report"},
        {"id":"n1","displayName":"ALPHA","type":"notebook"},
        {"id":"n2","displayName":"alpha","type":"Notebook","folderId":"nested"},
        {"id":"x","displayName":"Future","type":"FutureItem"},
        {"id":"null-type","displayName":"Null type","type":null}
    ]}))]);
    let client = sdk(Arc::default());
    let mut host = host(&client, &backend, &catalog, root.clone(), &NoQuery);
    host.config.show_hidden = true;
    let (value, exit) = output(&host, &["ls", "/Space", "Name.Workspace", "-l"]);
    assert_eq!(exit, 0);
    assert_eq!(
        value["result"]["data"],
        json!([
            {"name":"ALPHA.Notebook","id":"n1"},{"name":"alpha.Notebook","id":"n2"},
            {"name":"alpha.Report","id":"r"},{"name":"Zulu.Notebook","id":"z"}
        ])
    );
    assert_eq!(
        value["result"]["hidden_data"],
        json!([
            ".externaldatashares",
            ".managedidentities",
            ".managedprivateendpoints",
            ".sparkpools"
        ])
    );
    assert!(Arc::ptr_eq(&host.current, &root));
    assert_eq!(
        *backend.requests.lock().unwrap(),
        ["https://api.fabric.microsoft.com/v1/workspaces/w/items"]
    );
}

#[test]
fn bytype_uses_python_enum_names_not_rest_type_names() {
    let root = Element::tenant("Tenant", "tenant");
    let ws = workspace(&root, "W", "w");
    let catalog = Catalog::new(vec![]);
    let backend = Backend::new([reply(json!({"value":[
        {"id":"c","displayName":"Z","type":"CopyJob"},
        {"id":"co","displayName":"A","type":"CosmosDBDatabase"},
        {"id":"dq","displayName":"A","type":"DataPipeline"},
        {"id":"df","displayName":"Z","type":"Dataflow"}
    ]}))]);
    let client = sdk(Arc::default());
    let mut host = host(&client, &backend, &catalog, ws, &NoQuery);
    host.config.item_sort = ItemSort::ByType;
    let (value, _) = output(&host, &["dir"]);
    assert_eq!(value["command"], "dir");
    assert_eq!(
        value["result"]["data"],
        json!([
            {"name":"Z.CopyJob"},{"name":"A.CosmosDBDatabase"},{"name":"Z.Dataflow"},{"name":"A.DataPipeline"}
        ])
    );
}

#[test]
fn missing_item_type_is_malformed_not_an_unknown_future_type() {
    let root = Element::tenant("Tenant", "tenant");
    let ws = workspace(&root, "W", "w");
    let catalog = Catalog::new(vec![]);
    let backend = Backend::new([reply(json!({"value":[
        {"id":"bad","displayName":"Missing type"}
    ]}))]);
    let client = sdk(Arc::default());
    let host = host(&client, &backend, &catalog, ws, &NoQuery);
    let (value, exit) = output(&host, &["ls"]);
    assert_eq!(exit, 1);
    assert_eq!(value["result"]["error_code"], "InvalidResponse");
    assert!(value["result"].get("data").is_none());
}

struct InspectProjection {
    seen: Mutex<Vec<Value>>,
}
impl Projection for InspectProjection {
    fn project(
        &self,
        data: Value,
        expression: Option<&str>,
        deep: bool,
    ) -> Result<Value, CommandError> {
        assert_eq!(expression, Some("host-expression"));
        assert!(!deep);
        self.seen.lock().unwrap().push(data.clone());
        // This test callback is an injected host boundary, not a query interpreter.
        Ok(data)
    }
}

#[test]
fn get_path_runs_real_sdk_before_injected_projection_and_preserves_context() {
    let root = Element::tenant("Tenant", "tenant");
    let ws = workspace(&root, "W", "w");
    let item = Element::child(
        "N",
        Some("i".into()),
        NodeKind::Item(ItemType::parse("Notebook").unwrap()),
        ws.clone(),
    )
    .unwrap();
    let catalog = Catalog::new(vec![ws, item]);
    let backend = Backend::new([reply(json!({
        "id":"i","displayName":"N","type":"FutureType","description":null,"properties":{"nested":[1,2]}
    }))]);
    let client = sdk(Arc::default());
    let projection = InspectProjection {
        seen: Mutex::new(vec![]),
    };
    let host = host(&client, &backend, &catalog, root.clone(), &projection);
    let (value, exit) = output(
        &host,
        &["get", "/W.Workspace/N.Notebook", "-q", "host-expression"],
    );
    assert_eq!(exit, 0);
    assert_eq!(
        value["result"]["data"],
        json!([{
            "id":"i","displayName":"N","type":"FutureType","description":null,"properties":{"nested":[1,2]}
        }])
    );
    assert_eq!(projection.seen.lock().unwrap().len(), 1);
    assert_eq!(
        *backend.requests.lock().unwrap(),
        ["https://api.fabric.microsoft.com/v1/workspaces/w/notebooks/i"]
    );
    assert!(Arc::ptr_eq(&host.current, &root));
}

#[test]
fn every_source_collection_uses_context_route_and_keeps_get_command_boundaries() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/compatibility/commands/specialized-routes.json"
    ))
    .unwrap();
    let routes = fixture["routes"].as_array().unwrap();
    assert_eq!(routes.len(), 44);
    assert_eq!(routes.iter().filter(|route| route[2] == true).count(), 31);
    let root = Element::tenant("T", "t");
    let ws = workspace(&root, "W", "w");
    let catalog = Catalog::new(vec![]);
    for route in routes {
        let kind = route[0].as_str().unwrap();
        let current = Element::child(
            "N",
            Some("i".into()),
            NodeKind::Item(ItemType::parse(kind).unwrap()),
            ws.clone(),
        )
        .unwrap();
        let credential = Arc::new(Credential::default());
        let client = sdk(credential.clone());
        let allowed = route[2].as_bool().unwrap();
        let backend = Backend::new(if allowed {
            vec![reply(
                json!({"before":null,"displayName":"N","id":"i","type":"Future"}),
            )]
        } else {
            vec![]
        });
        let host =
            host(&client, &backend, &catalog, current.clone(), &NoQuery).with_default_projection();
        let (value, exit) = output(&host, &["get", "."]);
        if allowed {
            assert_eq!(exit, 0, "{kind}");
            assert_eq!(
                value["result"]["data"],
                json!(["before", "displayName", "id", "type"])
            );
            assert_eq!(
                *backend.requests.lock().unwrap(),
                [format!(
                    "https://api.fabric.microsoft.com/v1/workspaces/w/{}/i",
                    route[1].as_str().unwrap()
                )]
            );
            assert_eq!(
                *credential.scopes.lock().unwrap(),
                [vec!["https://api.fabric.microsoft.com/.default"]]
            );
        } else {
            assert_eq!(exit, 1, "{kind}");
            assert_eq!(
                value["result"],
                json!({
                    "error_code":"UnsupportedCommand","message":"not supported for command 'get'"
                })
            );
            assert!(backend.requests.lock().unwrap().is_empty());
            assert!(credential.scopes.lock().unwrap().is_empty());
        }
        assert!(Arc::ptr_eq(&host.current, &current));
    }
    let client = sdk(Arc::default());
    let backend = Backend::new([]);
    let host = host(&client, &backend, &catalog, ws, &NoQuery).with_default_projection();
    let (value, exit) = output(&host, &["get", "Unknown.Future"]);
    assert_eq!(exit, 1);
    assert_eq!(value["result"]["error_code"], "InvalidPath");
    assert!(backend.requests.lock().unwrap().is_empty());
}

#[test]
fn queries_receive_source_rows_before_default_column_projection() {
    let backend = Backend::new([reply(json!({"value":[
        {"id":"b","displayName":"Beta","type":"Workspace"},
        {"id":"a","displayName":"Alpha","type":"Workspace"}
    ]}))]);
    let client = sdk(Arc::default());
    let catalog = Catalog::new(vec![]);
    let projection = InspectProjection {
        seen: Mutex::new(vec![]),
    };
    let host = host(
        &client,
        &backend,
        &catalog,
        Element::tenant("T", "t"),
        &projection,
    );
    let (value, _) = output(&host, &["ls", "-q", "host-expression"]);
    assert_eq!(
        value["result"]["data"],
        json!([
            {"name":"Alpha.Workspace","id":"a"},{"name":"Beta.Workspace","id":"b"}
        ])
    );
}

#[test]
fn builtin_query_projects_real_sdk_rows_before_default_columns() {
    let backend = Backend::new([reply(json!({"value":[
        {"id":"b","displayName":"Beta","type":"Workspace"},
        {"id":"a","displayName":"Alpha","type":"Workspace"}
    ]}))]);
    let client = sdk(Arc::default());
    let catalog = Catalog::new(vec![]);
    let host = host(
        &client,
        &backend,
        &catalog,
        Element::tenant("T", "t"),
        &NoQuery,
    )
    .with_default_projection();
    let (value, exit) = output(&host, &["ls", "-q", "[?id == 'a'].{label:name,key:id}"]);
    assert_eq!(exit, 0);
    assert_eq!(
        value["result"]["data"],
        json!([{"label":"Alpha.Workspace","key":"a"}])
    );
    assert_eq!(backend.requests.lock().unwrap().len(), 1);
}

#[test]
fn builtin_get_discovers_paths_or_executes_actual_query_with_sdk_metadata() {
    for (query, expected) in [
        (
            None,
            json!([
                "id",
                "displayName",
                "type",
                "properties.enabled",
                "properties.rows[0].n",
            ]),
        ),
        (
            Some("."),
            json!([{"id":"i","displayName":"N","properties":{"enabled":false,"rows":[{"n":2}]},"type":"Notebook"}]),
        ),
        (Some("properties.enabled"), json!(["False"])),
        (Some("properties.rows[?n > `1`].n"), json!([2])),
    ] {
        let root = Element::tenant("T", "t");
        let ws = workspace(&root, "W", "w");
        let item = Element::child(
            "N",
            Some("i".into()),
            NodeKind::Item(ItemType::parse("Notebook").unwrap()),
            ws.clone(),
        )
        .unwrap();
        let catalog = Catalog::new(vec![ws, item.clone()]);
        let backend = Backend::new([reply(json!({
            "id":"i","displayName":"N","type":"Notebook",
            "properties":{"enabled":false,"rows":[{"n":2}]}
        }))]);
        let client = sdk(Arc::default());
        let host =
            host(&client, &backend, &catalog, root.clone(), &NoQuery).with_default_projection();
        let mut args = vec!["get", "/W.Workspace/N.Notebook"];
        if let Some(query) = query {
            args.extend(["-q", query]);
        }
        let (value, exit) = output(&host, &args);
        assert_eq!(exit, 0, "{query:?}");
        assert_eq!(value["result"]["data"], expected, "{query:?}");
        assert_eq!(
            *backend.requests.lock().unwrap(),
            ["https://api.fabric.microsoft.com/v1/workspaces/w/notebooks/i"]
        );
        assert!(Arc::ptr_eq(&host.current, &root));
    }
}

#[test]
fn builtin_query_failures_are_source_shaped_errors_not_empty_success() {
    for query in ["[?", "length(`1`)", "sum(`[9007199254740993]`)"] {
        let backend = Backend::new([reply(json!({"value":[]}))]);
        let client = sdk(Arc::default());
        let catalog = Catalog::new(vec![]);
        let host = host(
            &client,
            &backend,
            &catalog,
            Element::tenant("T", "t"),
            &NoQuery,
        )
        .with_default_projection();
        let (value, exit) = output(&host, &["ls", "-q", query]);
        assert_eq!(exit, 1, "{query}");
        assert_eq!(
            value["result"],
            json!({
                "error_code":"InvalidInput",
                "message":"Invalid jmespath query (https://jmespath.org)"
            })
        );
        assert!(value["result"].get("data").is_none());
    }
}

#[test]
fn service_errors_preserve_source_code_status_message_streams_and_exit() {
    for (status, body, headers, code, message, exit) in [
        (
            401,
            json!({"errorCode":"Ignored","message":"private"}),
            vec![],
            "Unauthorized",
            "Access is unauthorized",
            4,
        ),
        (
            403,
            json!({"errorCode":"Ignored","message":"private"}),
            vec![("x-ms-public-api-error-code", "WorkspaceAccessDenied")],
            "WorkspaceAccessDenied",
            "Access is forbidden. You do not have permission to access this resource",
            1,
        ),
        (
            404,
            json!({"errorCode":"Ignored","message":"private"}),
            vec![],
            "NotFound",
            "The requested resource could not be found",
            1,
        ),
        (
            400,
            json!({"errorCode":"InvalidWorkspaceRequest","message":"Synthetic workspace request rejected.","requestId":"private"}),
            vec![],
            "InvalidWorkspaceRequest",
            "Synthetic workspace request rejected",
            1,
        ),
    ] {
        let client = sdk(Arc::default());
        let backend = Backend::new([support::Reply {
            status,
            body,
            headers: headers
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }]);
        let catalog = Catalog::new(vec![]);
        let host = host(
            &client,
            &backend,
            &catalog,
            Element::tenant("T", "t"),
            &NoQuery,
        );
        let (value, actual_exit) = output(&host, &["ls"]);
        assert_eq!(actual_exit, exit);
        assert_eq!(value["status"], "Failure");
        assert_eq!(value["command"], "ls");
        assert_eq!(
            value["result"],
            json!({"error_code":code,"message":message})
        );
        assert!(!value.to_string().contains("private"));
    }
}

#[test]
fn malformed_or_partial_page_cannot_be_success() {
    for second in [json!({}), json!({"value":null}), json!("not-json")] {
        let client = sdk(Arc::default());
        let backend = Backend::new([
            reply(
                json!({"value":[{"id":"a","displayName":"A","type":"Workspace"}],"continuationToken":"next"}),
            ),
            reply(second),
        ]);
        let catalog = Catalog::new(vec![]);
        let host = host(
            &client,
            &backend,
            &catalog,
            Element::tenant("T", "t"),
            &NoQuery,
        );
        let (value, exit) = output(&host, &["ls"]);
        assert_eq!(exit, 1);
        assert_eq!(value["result"]["error_code"], "InvalidResponse");
        assert!(value["result"].get("data").is_none());
        assert_eq!(backend.requests.lock().unwrap().len(), 2);
    }
}

#[test]
fn unsupported_branches_options_and_missing_snapshot_never_succeed_or_mutate() {
    let root = Element::tenant("T", "t");
    let ws = workspace(&root, "W", "w");
    let client = sdk(Arc::default());
    let catalog = Catalog::new(vec![]);
    for args in [
        vec!["get", "."],
        vec!["ls", "/.capacities"],
        vec!["ls", "Missing.Workspace"],
        vec!["get", ".", "-o", "output"],
        vec!["get", ".", "-f"],
        vec!["get", ".", "-v"],
        vec!["ls", "--output_format", "text"],
    ] {
        let backend = Backend::new([]);
        let host = host(&client, &backend, &catalog, root.clone(), &NoQuery);
        let (_, exit) = output(&host, &args);
        assert_eq!(exit, 1, "{args:?}");
        assert!(backend.requests.lock().unwrap().is_empty());
        assert!(Arc::ptr_eq(&host.current, &root));
    }
    let backend = Backend::new([]);
    let mut host = host(&client, &backend, &catalog, ws, &NoQuery);
    host.config.folder_listing_enabled = true;
    assert_eq!(output(&host, &["ls"]).1, 1);
    assert!(backend.requests.lock().unwrap().is_empty());
}

#[test]
fn default_noquery_is_explicitly_unsupported_for_queries_not_fake_identity() {
    let client = sdk(Arc::default());
    let backend = Backend::new([reply(json!({"value":[]}))]);
    let catalog = Catalog::new(vec![]);
    let host = host(
        &client,
        &backend,
        &catalog,
        Element::tenant("T", "t"),
        &NoQuery,
    );
    let (value, exit) = output(&host, &["ls", "-q", "[].name"]);
    assert_eq!(exit, 1);
    assert_eq!(value["result"]["error_code"], "NotSupported");
}

#[test]
fn source_unsupported_get_types_and_root_keep_exact_command_support_error() {
    let root = Element::tenant("T", "t");
    let ws = workspace(&root, "W", "w");
    let client = sdk(Arc::default());
    let catalog = Catalog::new(vec![]);
    let mut targets = vec![root];
    for kind in [
        "Dashboard",
        "PaginatedReport",
        "SQLEndpoint",
        "MirroredWarehouse",
        "Datamart",
        "OrgApp",
        "MetricSet",
        "ApacheAirflowJob",
        "Exploration",
        "RetailDataManager",
        "Healthcaredatasolution",
        "SustainabilityDataSolution",
        "AISkill",
    ] {
        targets.push(
            Element::child(
                "Item",
                Some("i".into()),
                NodeKind::Item(ItemType::parse(kind).unwrap()),
                ws.clone(),
            )
            .unwrap(),
        );
    }
    for target in targets {
        let backend = Backend::new([]);
        let host = host(&client, &backend, &catalog, target, &NoQuery);
        let (value, exit) = output(&host, &["get", "."]);
        assert_eq!(exit, 1);
        assert_eq!(
            value["result"],
            json!({
                "error_code": "UnsupportedCommand", "message": "not supported for command 'get'"
            })
        );
        assert!(backend.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn early_cancellation_never_acquires_credentials_or_sends_http() {
    let credential = Arc::new(Credential::default());
    let client = sdk(credential.clone());
    let backend = Backend::new([]);
    let catalog = Catalog::new(vec![]);
    let host = host(
        &client,
        &backend,
        &catalog,
        Element::tenant("T", "t"),
        &NoQuery,
    );
    let plan = complete(host.dispatch(&parse(&["ls"]), ready(()))).unwrap();
    assert_eq!(plan.exit_code, 1);
    assert!(backend.requests.lock().unwrap().is_empty());
    assert!(credential.scopes.lock().unwrap().is_empty());
}

struct BrokenWriter;
impl io::Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("not owned by dispatcher")
    }
}

#[test]
fn output_write_failure_is_propagated_without_success_or_fallback() {
    let client = sdk(Arc::default());
    let backend = Backend::new([reply(json!({"value":[]}))]);
    let catalog = Catalog::new(vec![]);
    let host = host(
        &client,
        &backend,
        &catalog,
        Element::tenant("T", "t"),
        &NoQuery,
    );
    let mut stderr = vec![];
    assert!(
        matches!(complete(host.run(&parse(&["ls"]),pending(),&mut BrokenWriter,&mut stderr)),
        Err(fabric_cli::WriteError::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe)
    );
    assert!(stderr.is_empty());
}

#[test]
fn parser_preserves_python_tokens_flags_alias_and_rejects_invented_namespaces() {
    let command = parse(&[
        "get",
        "/Space",
        "Name.Workspace/N.Notebook",
        "-q",
        "properties",
        "|",
        "keys(@)",
        "--output_format",
        "json",
    ]);
    assert_eq!(command.kind, ReadKind::Get);
    assert_eq!(command.path, ["/Space", "Name.Workspace/N.Notebook"]);
    assert_eq!(command.query.as_deref(), Some("properties | keys(@)"));
    assert_eq!(command.output_format.as_deref(), Some("json"));
    assert_eq!(parse(&["ls", "dir"]).name, "ls");
    assert_eq!(parse(&["dir", "-la"]).name, "dir");
    assert!(parse_args(["fab-rs", "workspace", "list"]).is_err());
}

mod shared_http {
    use super::*;
    use async_trait::async_trait;
    use azure_core::http::{AsyncRawResponse, HttpClient, Request, StatusCode, headers::Headers};
    use fabric_sdk_http::{ExecutionFuture, ExecutionOptions, ExecutionTimer, HttpExecutor};
    use std::{collections::VecDeque, future::poll_fn, task::Poll, time::Duration};

    #[derive(Debug)]
    struct Http {
        responses: Mutex<VecDeque<AsyncRawResponse>>,
        requests: Mutex<Vec<Request>>,
    }

    #[async_trait]
    impl HttpClient for Http {
        async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
            self.requests.lock().unwrap().push(request.clone());
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected HTTP retry"))
        }
    }

    #[derive(Default)]
    struct Timer {
        waits: Mutex<Vec<Duration>>,
    }
    impl ExecutionTimer for Timer {
        fn sleep(&self, delay: Duration) -> ExecutionFuture<'_, ()> {
            self.waits.lock().unwrap().push(delay);
            Box::pin(poll_fn(move |_| {
                if delay == Duration::from_secs(240) {
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            }))
        }
    }

    fn response(status: u16, body: &str) -> AsyncRawResponse {
        AsyncRawResponse::from_bytes(
            StatusCode::from(status),
            Headers::new(),
            body.as_bytes().to_vec(),
        )
    }

    fn http(responses: impl IntoIterator<Item = AsyncRawResponse>) -> Arc<Http> {
        Arc::new(Http {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(vec![]),
        })
    }

    #[tokio::test]
    async fn real_shared_http_adapter_connects_parser_sdk_query_boundary_and_output() {
        let http = http([
            response(503, ""),
            response(
                200,
                r#"{"value":[{"id":"n","displayName":"Native","type":"Notebook"}],"continuationToken":"next+/="}"#,
            ),
            response(200, r#"{"value":[]}"#),
            response(
                200,
                r#"{"id":"n","displayName":"Native","type":"Notebook","description":null}"#,
            ),
        ]);
        let timer = Arc::new(Timer::default());
        let executor =
            HttpExecutor::new(http.clone(), timer.clone(), ExecutionOptions::default()).unwrap();
        let credential = Arc::new(Credential::default());
        let client = sdk(credential.clone());
        let root = Element::tenant("T", "t");
        let ws = workspace(&root, "W", "w");
        let item = Element::child(
            "Native",
            Some("n".into()),
            NodeKind::Item(ItemType::parse("Notebook").unwrap()),
            ws.clone(),
        )
        .unwrap();
        let catalog = Catalog::new(vec![ws.clone(), item]);
        let projection = InspectProjection {
            seen: Mutex::new(vec![]),
        };
        let host = host(&client, &executor, &catalog, ws.clone(), &projection);
        let mut stdout = vec![];
        let mut stderr = vec![];
        let exit = host
            .run(&parse(&["ls", "-l"]), pending(), &mut stdout, &mut stderr)
            .await
            .unwrap();
        assert_eq!(exit, 0);
        assert!(stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&stdout).unwrap()["result"]["data"],
            json!([{"name":"Native.Notebook","id":"n"}])
        );
        let plan = host
            .dispatch(
                &parse(&["get", "Native.Notebook", "-q", "host-expression"]),
                pending(),
            )
            .await
            .unwrap();
        assert_eq!(plan.exit_code, 0);
        assert_eq!(
            projection.seen.lock().unwrap()[0]["description"],
            Value::Null
        );
        assert!(Arc::ptr_eq(&host.current, &ws));
        let requests = http.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|r| r.url().as_str())
                .collect::<Vec<_>>(),
            [
                "https://api.fabric.microsoft.com/v1/workspaces/w/items",
                "https://api.fabric.microsoft.com/v1/workspaces/w/items",
                "https://api.fabric.microsoft.com/v1/workspaces/w/items?continuationToken=next%2B%2F%3D",
                "https://api.fabric.microsoft.com/v1/workspaces/w/notebooks/n",
            ]
        );
        for request in requests.iter() {
            assert_eq!(request.method(), azure_core::http::Method::Get);
            assert_eq!(
                request.headers().get_str(&"content-type".into()).unwrap(),
                "application/json"
            );
            assert!(request.headers().get_str(&"authorization".into()).is_ok());
        }
        assert_eq!(credential.scopes.lock().unwrap().len(), 3);
        assert_eq!(
            timer
                .waits
                .lock()
                .unwrap()
                .iter()
                .filter(|&&wait| wait == Duration::from_secs(240))
                .count(),
            3
        );
    }

    #[tokio::test]
    async fn http_failures_empty_and_malformed_response_reach_real_command_envelopes() {
        for (status, body, expected) in [
            (
                403,
                r#"{"errorCode":"Ignored","message":"private"}"#,
                "Forbidden",
            ),
            (404, "private", "NotFound"),
            (200, r#"{"value":null}"#, "InvalidResponse"),
            (200, r#"{"value":[]}"#, ""),
        ] {
            let http = http([response(status, body)]);
            let executor = HttpExecutor::new(
                http.clone(),
                Arc::new(Timer::default()),
                ExecutionOptions::default(),
            )
            .unwrap();
            let client = sdk(Arc::default());
            let catalog = Catalog::new(vec![]);
            let host = host(
                &client,
                &executor,
                &catalog,
                Element::tenant("T", "t"),
                &NoQuery,
            );
            let plan = host.dispatch(&parse(&["ls"]), pending()).await.unwrap();
            let value: Value = serde_json::from_slice(&plan.payload).unwrap();
            if expected.is_empty() {
                assert_eq!(plan.exit_code, 0);
                assert_eq!(value["result"]["data"], json!([]));
            } else {
                assert_eq!(plan.exit_code, 1);
                assert_eq!(value["result"]["error_code"], expected);
                assert!(!String::from_utf8(plan.payload).unwrap().contains("private"));
            }
            assert_eq!(http.requests.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn metadata_http_order_reaches_real_python_query_and_command_streams() {
        let wire = r#"{"future":{"z":null,"a":[1,1.0,-0.0,9007199254740993]},"displayName":"\u96ea","id":"old","type":null,"id":"n","tail":false}"#;
        let root = Element::tenant("T", "t");
        let ws = workspace(&root, "W", "w");
        let item = Element::child(
            "Native",
            Some("n".into()),
            NodeKind::Item(ItemType::parse("Notebook").unwrap()),
            ws.clone(),
        )
        .unwrap();
        let catalog = Catalog::new(vec![ws.clone(), item]);
        for (query, expected) in [
            (
                None,
                json!([
                    "future.z",
                    "future.a[0]",
                    "future.a[1]",
                    "future.a[2]",
                    "future.a[3]",
                    "displayName",
                    "id",
                    "type",
                    "tail"
                ]),
            ),
            (
                Some("keys(@)"),
                json!(["future", "displayName", "id", "type", "tail"]),
            ),
            (
                Some("future.a"),
                json!([1, 1.0, -0.0, 9007199254740993_u64]),
            ),
            (
                Some("."),
                json!([{
                    "future":{"z":null,"a":[1,1.0,-0.0,9007199254740993_u64]},
                    "displayName":"\u{96ea}","id":"n","type":null,"tail":false
                }]),
            ),
        ] {
            let http = http([response(200, wire)]);
            let executor = HttpExecutor::new(
                http.clone(),
                Arc::new(Timer::default()),
                ExecutionOptions::default(),
            )
            .unwrap();
            let credential = Arc::new(Credential::default());
            let client = sdk(credential.clone());
            let host =
                host(&client, &executor, &catalog, ws.clone(), &NoQuery).with_default_projection();
            let mut args = vec!["get", "Native.Notebook", "--output_format", "json"];
            if let Some(query) = query {
                args.extend(["--query", query]);
            }
            let (mut stdout, mut stderr) = (vec![], vec![]);
            let exit = host
                .run(&parse(&args), pending(), &mut stdout, &mut stderr)
                .await
                .unwrap();
            assert_eq!(exit, 0, "{query:?}");
            assert!(stderr.is_empty());
            let value: Value = serde_json::from_slice(&stdout).unwrap();
            assert_eq!(value["result"]["data"], expected, "{query:?}");
            let envelope = json!({
                "timestamp":"2000-01-01T00:00:00Z","status":"Success","command":"get",
                "result":{"data":expected}
            });
            let mut expected_bytes = vec![];
            serde::Serialize::serialize(
                &envelope,
                &mut serde_json::Serializer::with_formatter(
                    &mut expected_bytes,
                    serde_json::ser::PrettyFormatter::with_indent(b"    "),
                ),
            )
            .unwrap();
            expected_bytes = String::from_utf8(expected_bytes)
                .unwrap()
                .replace('\u{96ea}', "\\u96ea")
                .into_bytes();
            expected_bytes.push(b'\n');
            assert_eq!(stdout, expected_bytes, "{query:?}");
            assert_eq!(
                credential.scopes.lock().unwrap().as_slice(),
                [vec!["https://api.fabric.microsoft.com/.default"]]
            );
            let requests = http.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(
                requests[0].url().as_str(),
                "https://api.fabric.microsoft.com/v1/workspaces/w/notebooks/n"
            );
            assert!(Arc::ptr_eq(&host.current, &ws));
        }
    }
}
