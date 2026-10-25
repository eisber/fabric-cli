// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline JSONL observations through the public wrapper and shared HTTP executor.

#[allow(dead_code, unused_imports)]
mod support {
    include!("../tests/folder_support/mod.rs");
}

use fabric_cli::{
    ItemSort, NoQuery, ReadConfig, ReadHost,
    parser::{Invocation, parse_args},
};
use fabric_cli_context::{Catalog, Element, HomeContext, NodeKind, WorkspaceType};
use fabric_cli_output::Timestamp;
use fabric_sdk::{folders::FolderListLimits, items::ItemListLimits};
use serde_json::{Value, json};
use std::{
    future::poll_fn,
    io::{self, BufRead},
    num::NonZeroUsize,
    sync::Arc,
    task::Poll,
};

fn limit(input: &Value, key: &str, default: NonZeroUsize) -> NonZeroUsize {
    input.get(key).map_or(default, |value| {
        NonZeroUsize::new(usize::try_from(value.as_u64().expect("unsigned bound")).unwrap())
            .expect("nonzero bound")
    })
}

fn caller_arguments(input: &Value) -> Vec<String> {
    let mut tokens = input.as_array().unwrap().iter().peekable();
    let mut arguments = vec!["fab-rs".into()];
    while let Some(token) = tokens.next() {
        let token = token.as_str().unwrap();
        arguments.push(token.into());
        if matches!(token, "--query" | "-q") {
            let mut query = Vec::new();
            while tokens
                .peek()
                .is_some_and(|token| !token.as_str().unwrap().starts_with('-'))
            {
                query.push(tokens.next().unwrap().as_str().unwrap());
            }
            arguments.push(fabric_cli_query::join_query_args(&query));
        }
    }
    arguments
}

fn main() {
    for line in io::stdin().lock().lines() {
        let input: Value = serde_json::from_str(&line.expect("stdin")).expect("case JSON");
        let root = Element::tenant("Offline tenant", "offline-tenant");
        let mut current = root;
        let mut elements = vec![];
        if let Some(row) = input.get("current_workspace") {
            current = Element::child(
                row["displayName"].as_str().unwrap(),
                Some(row["id"].as_str().unwrap().into()),
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
                ),
                current,
            )
            .unwrap();
            elements.push(current.clone());
        }
        if let Some(folders) = input["current_folders"].as_array() {
            for row in folders {
                current = Element::child(
                    row["displayName"].as_str().unwrap(),
                    Some(row["id"].as_str().unwrap().into()),
                    NodeKind::Folder,
                    current,
                )
                .unwrap();
                elements.push(current.clone());
            }
        }
        let catalog = Catalog::new(elements);
        let credential = Arc::new(support::Credential::default());
        let sdk = support::sdk(credential.clone());
        let http = Arc::new(support::Http::new(
            serde_json::from_value::<Vec<support::Reply>>(
                input.get("responses").cloned().unwrap_or_else(|| json!([])),
            )
            .unwrap(),
        ));
        let exec = support::executor(http.clone(), Arc::default());
        let host = ReadHost {
            sdk: &sdk,
            workspaces: &exec,
            items: &exec,
            capacities: &exec,
            lookup: &catalog,
            current: current.clone(),
            home: HomeContext::Unspecified,
            timestamp: Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            config: ReadConfig {
                folder_listing_enabled: input["config"]["folder_listing_enabled"]
                    .as_bool()
                    .unwrap_or(false),
                show_hidden: input["config"]["show_hidden"].as_bool().unwrap_or(false),
                item_sort: if input["config"]["item_sort"] == "bytype" {
                    ItemSort::ByType
                } else {
                    ItemSort::ByName
                },
            },
            projection: &NoQuery,
        };
        let host = if input["config"]["projection"] == "none" {
            host
        } else {
            host.with_default_projection()
        };
        let default_items = ItemListLimits::default();
        let default_folders = FolderListLimits::default();
        let wrapper = host.with_folders(&exec).with_limits(
            ItemListLimits {
                max_pages: limit(&input["item_limits"], "max_pages", default_items.max_pages),
                max_items: limit(&input["item_limits"], "max_items", default_items.max_items),
                max_response_bytes: limit(
                    &input["item_limits"],
                    "max_response_bytes",
                    default_items.max_response_bytes,
                ),
            },
            FolderListLimits {
                max_pages: limit(
                    &input["folder_limits"],
                    "max_pages",
                    default_folders.max_pages,
                ),
                max_items: limit(
                    &input["folder_limits"],
                    "max_items",
                    default_folders.max_items,
                ),
                max_response_bytes: limit(
                    &input["folder_limits"],
                    "max_response_bytes",
                    default_folders.max_response_bytes,
                ),
            },
        );
        let Invocation::Read(command) =
            parse_args(caller_arguments(&input["command"])).expect("read arguments")
        else {
            panic!("read required");
        };
        let (mut stdout, mut stderr) = (vec![], vec![]);
        let cancelled = poll_fn(|_| {
            if input["cancellation"] == "before" {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        let exit =
            support::complete(wrapper.run(&command, cancelled, &mut stdout, &mut stderr)).unwrap();
        assert!(Arc::ptr_eq(&host.current, &current));
        println!(
            "{}",
            json!({
                "id":input["id"],
                "exit_code":exit,
                "stdout":String::from_utf8(stdout).unwrap(),
                "stderr":String::from_utf8(stderr).unwrap(),
                "requests":*http.requests.lock().unwrap(),
                "scopes":*credential.scopes.lock().unwrap()
            })
        );
    }
}
