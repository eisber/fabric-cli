// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Consume independently hand-authored Python expectations without regenerating them.

use fabric_cli_context::{
    Catalog, ContextError, Element, ErrorKind, HomeContext, ItemType, Lookup, LookupKind,
    LookupRequest, NodeKind, PathInput, ResolveOptions, Resolver, UnsupportedBranch, WorkspaceType,
};
use serde_json::{Value, json};
use std::{cell::RefCell, collections::BTreeMap, sync::Arc};

const FIXTURES: &str = include_str!("../../../tests/compatibility/context/cases.json");

fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("missing string field {field}: {value}"))
}

fn graph(fixture: &Value) -> (BTreeMap<String, Arc<Element>>, Catalog) {
    let mut nodes = BTreeMap::<String, Arc<Element>>::new();
    let mut ordered = Vec::new();
    for record in fixture["catalog"].as_array().unwrap() {
        let kind = match text(record, "kind") {
            "Tenant" => NodeKind::Tenant,
            "Workspace" => NodeKind::Workspace(match text(record, "type") {
                "Workspace" => WorkspaceType::Workspace,
                "Personal" => WorkspaceType::Personal,
                value => panic!("unknown workspace fixture type: {value}"),
            }),
            "Folder" => NodeKind::Folder,
            "Item" => NodeKind::Item(ItemType::parse(text(record, "type")).unwrap()),
            value => panic!("unknown fixture node kind: {value}"),
        };
        let node = if kind == NodeKind::Tenant {
            Element::tenant(text(record, "name"), text(record, "id"))
        } else {
            Element::child(
                text(record, "name"),
                record["id"].as_str().map(str::to_owned),
                kind,
                Arc::clone(&nodes[text(record, "parent")]),
            )
            .unwrap()
        };
        assert!(
            nodes
                .insert(text(record, "key").to_owned(), Arc::clone(&node))
                .is_none()
        );
        ordered.push(node);
    }
    (nodes, Catalog::new(ordered))
}

fn snapshot(element: &Element) -> Value {
    json!({
        "path": element.path(),
        "path_id": element.path_id(),
        "kind": element.kind().as_str(),
        "name": element.name(),
        "id": element.id(),
    })
}

fn lookup_trace(request: LookupRequest<'_>) -> Value {
    let op = match request.kind {
        LookupKind::Workspace => "workspace",
        LookupKind::Folder => "folder",
        LookupKind::NestedFolder => "nested_folder",
        LookupKind::Item => "item",
    };
    json!({ "op": op, "parent": request.parent.path(), "name": request.name })
}

fn current_key<'a>(nodes: &'a BTreeMap<String, Arc<Element>>, current: &Arc<Element>) -> &'a str {
    nodes
        .iter()
        .find(|(_, node)| Arc::ptr_eq(node, current))
        .unwrap()
        .0
}

fn unsupported_category(branch: UnsupportedBranch) -> &'static str {
    match branch {
        UnsupportedBranch::VirtualWorkspace => "virtual_context",
        UnsupportedBranch::VirtualItemContainer => "external_sharing",
        UnsupportedBranch::OneLake => "onelake_traversal",
        UnsupportedBranch::LocalFilesystem => "local_path_fallback",
        UnsupportedBranch::HomeDiscovery => "ambient_home_discovery",
    }
}

#[test]
fn independent_python_cases_compare_exact_paths_errors_lookups_and_context() {
    let fixture: Value = serde_json::from_str(FIXTURES).unwrap();
    assert_eq!(fixture["schema_version"], 1);
    assert_eq!(
        fixture["source_commit"],
        "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    );
    let (nodes, catalog) = graph(&fixture);
    let original_snapshots: Vec<_> = catalog
        .elements()
        .iter()
        .map(|node| snapshot(node))
        .collect();
    let parents: Vec<_> = catalog
        .elements()
        .iter()
        .map(|node| node.parent().cloned())
        .collect();
    let mut exact = 0;
    let mut unsupported = 0;
    for case in fixture["cases"].as_array().unwrap() {
        let name = text(case, "name");
        let trace = RefCell::new(Vec::<Value>::new());
        let lookup = |request: LookupRequest<'_>| {
            let call = lookup_trace(request);
            trace.borrow_mut().push(call.clone());
            if let Some(failures) = case["failures"].as_array() {
                for failure in failures {
                    if ["op", "parent", "name"]
                        .iter()
                        .all(|key| failure[key] == call[key])
                    {
                        // Fixture inputs are raw FabricCLIError constructor arguments.
                        let code = failure["error"]["code"]
                            .as_str()
                            .filter(|value| !value.is_empty())
                            .unwrap_or("UnknownError");
                        let message = failure["error"]["message"]
                            .as_str()
                            .filter(|value| !value.is_empty())
                            .unwrap_or("An error occurred while processing the operation")
                            .trim_end_matches('.');
                        return Err(ContextError::lookup(code, message));
                    }
                }
            }
            catalog.lookup(request)
        };
        let original = &nodes[text(case, "current")];
        let home = case["home"]
            .as_str()
            .map_or(HomeContext::Unspecified, |name| {
                HomeContext::Workspace(name.to_owned())
            });
        let resolver = Resolver::new(&lookup, Arc::clone(original), home);
        let before = current_key(&nodes, resolver.current()).to_owned();
        let tokens: Vec<&str> = case["input"]
            .as_array()
            .map(|tokens| tokens.iter().map(|value| value.as_str().unwrap()).collect())
            .unwrap_or_default();
        let input = case["input"]
            .as_str()
            .map_or(PathInput::Tokens(&tokens), PathInput::Scalar);
        let outcome = resolver.resolve(
            input,
            ResolveOptions {
                raise_error: case["raise_error"].as_bool().unwrap(),
                supports_local_path: case["supports_local_path"].as_bool().unwrap(),
            },
        );
        let expected = &case["expected"];
        if let Some(category) = expected["unsupported"].as_str() {
            let error = outcome.unwrap_err();
            let ErrorKind::Unsupported(branch) = error.kind else {
                panic!("{name}: expected explicit unsupported, got {error:?}");
            };
            assert_eq!(unsupported_category(branch), category, "{name}");
            unsupported += 1;
        } else {
            let mut observed = json!({
                "lookups": *trace.borrow(),
                "current_before": before,
                "current_after": current_key(&nodes, resolver.current()),
            });
            match outcome {
                Ok(node) => observed["result"] = snapshot(&node),
                Err(error) => {
                    observed["error"] = json!({ "code": error.code, "message": error.message })
                }
            }
            assert_eq!(&observed, expected, "{name}");
            exact += 1;
        }
        assert!(
            Arc::ptr_eq(original, resolver.current()),
            "{name}: current identity"
        );
        for ((node, parent), before) in catalog
            .elements()
            .iter()
            .zip(&parents)
            .zip(&original_snapshots)
        {
            assert_eq!(&snapshot(node), before, "{name}: catalog snapshot");
            match (node.parent(), parent) {
                (Some(left), Some(right)) => {
                    assert!(Arc::ptr_eq(left, right), "{name}: parent identity")
                }
                (None, None) => {}
                _ => panic!("{name}: parent link changed"),
            }
        }
    }
    assert_eq!(
        (exact, unsupported),
        (81, 6),
        "Keep exact and unported denominators separate"
    );
}
