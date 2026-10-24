// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline test subprocess adapter, never a product mode or runtime bridge.

#[allow(dead_code)]
mod support {
    include!("../tests/support/mod.rs");
}

use fabric_cli::{
    ItemSort, NoQuery, ReadConfig, ReadHost,
    parser::{Invocation, parse_args},
};
use fabric_cli_context::{Catalog, Element, HomeContext, ItemType, NodeKind, WorkspaceType};
use fabric_cli_output::Timestamp;
use serde_json::{Value, json};
use std::{
    future::pending,
    io::{self, BufRead},
    sync::Arc,
};

fn node(row: &Value, parent: &Arc<Element>, item: bool) -> Arc<Element> {
    let kind = if item {
        NodeKind::Item(ItemType::parse(row["type"].as_str().unwrap()).unwrap())
    } else {
        NodeKind::Workspace(
            if row["type"]
                .as_str()
                .unwrap()
                .eq_ignore_ascii_case("personal")
            {
                WorkspaceType::Personal
            } else {
                WorkspaceType::Workspace
            },
        )
    };
    Element::child(
        row["displayName"].as_str().unwrap(),
        Some(row["id"].as_str().unwrap().into()),
        kind,
        parent.clone(),
    )
    .unwrap()
}

fn main() {
    for line in io::stdin().lock().lines() {
        let case: Value = serde_json::from_str(&line.expect("stdin")).expect("case JSON");
        let root = Element::tenant("Offline tenant", "offline-tenant");
        let mut current = root;
        let mut elements = vec![];
        if let Some(row) = case.get("current_workspace") {
            current = node(row, &current, false);
            elements.push(current.clone());
        }
        if let Some(row) = case.get("current_item") {
            current = node(row, &current, true);
            elements.push(current.clone());
        }
        let catalog = Catalog::new(elements);
        let credential = Arc::new(support::Credential::default());
        let sdk = support::sdk(credential.clone());
        let backend = support::Backend::new(
            serde_json::from_value::<Vec<support::Reply>>(
                case.get("responses").cloned().unwrap_or_else(|| json!([])),
            )
            .unwrap(),
        );
        let host = ReadHost {
            sdk: &sdk,
            workspaces: &backend,
            items: &backend,
            capacities: &backend,
            lookup: &catalog,
            current,
            home: HomeContext::Unspecified,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            config: ReadConfig {
                show_hidden: case["config"]["show_hidden"].as_bool().unwrap_or(false),
                item_sort: if case["config"]["item_sort"] == "bytype" {
                    ItemSort::ByType
                } else {
                    ItemSort::ByName
                },
                folder_listing_enabled: case["config"]["folder_listing_enabled"]
                    .as_bool()
                    .unwrap_or(false),
            },
            projection: &NoQuery,
        };
        let args = std::iter::once("fab-rs").chain(
            case["command"]
                .as_array()
                .expect("command")
                .iter()
                .map(|v| v.as_str().unwrap()),
        );
        let Invocation::Read(command) = parse_args(args).expect("read arguments") else {
            panic!("read required");
        };
        let (mut stdout, mut stderr) = (vec![], vec![]);
        let exit =
            support::complete(host.run(&command, pending(), &mut stdout, &mut stderr)).unwrap();
        println!(
            "{}",
            json!({
                "id":case["id"],"exit_code":exit,"stdout":String::from_utf8(stdout).unwrap(),
                "stderr":String::from_utf8(stderr).unwrap(),"requests":*backend.requests.lock().unwrap(),
                "scopes":*credential.scopes.lock().unwrap()
            })
        );
    }
}
