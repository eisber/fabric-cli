// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Local documentation namespace and item-area routing preserve service GET boundaries.
#![cfg(feature = "docs-search")]
#![deny(unnameable_test_items)]

use fabric_cli::parser::{Invocation, parse_args};
use std::process::Command;

#[test]
fn local_docs_addresses_and_item_modes_parse_without_changing_remote_get() {
    for args in [
        vec!["fab-rs", "get", "docs/"],
        vec!["fab-rs", "--output_format", "json", "get", "--", "docs/"],
        vec![
            "fab-rs",
            "get",
            "--output_format",
            "json",
            "docs/data-engineering/lakehouse-overview.md",
        ],
        vec![
            "fab-rs",
            "get",
            "--",
            "docs/data-engineering/lakehouse-overview.md",
        ],
        vec![
            "fab-rs",
            "get",
            "docs/data-engineering/lakehouse-overview.md",
        ],
        vec![
            "fab-rs",
            "get",
            "docs",
            "data-engineering/lakehouse-overview.md",
        ],
        vec![
            "fab-rs",
            "get",
            "docs/data-engineering/lakehouse-overview.md#section-1",
        ],
        vec!["fab-rs", "item", "lakehouse", "docs"],
        vec!["fab-rs", "item", "lakehouse", "docs", "--text", "tables"],
        vec!["fab-rs", "item", "lakehouse", "docs", "--suggest", "lakeh"],
    ] {
        assert!(matches!(
            parse_args(args).unwrap(),
            Invocation::DocsBrowse(_)
        ));
    }
    assert!(matches!(
        parse_args(["fab-rs", "get", "Demo.Workspace"]).unwrap(),
        Invocation::Read(_)
    ));
    assert!(matches!(
        parse_args(["fab-rs", "--version", "get", "docs/"]).unwrap(),
        Invocation::Display(_)
    ));
}

#[test]
fn unsafe_or_unsupported_local_criteria_fail_before_any_host_setup() {
    for args in [
        vec!["fab-rs", "get", "docs/../secret.md"],
        vec!["fab-rs", "get", "docs/topic.md", "--max-bytes", "0"],
        vec!["fab-rs", "get", "docs/topic.md", "--query", "private"],
        vec!["fab-rs", "get", "docs/topic.md", "extra.md"],
        vec![
            "fab-rs",
            "get",
            "docs/topic.md",
            "--format",
            "text",
            "--output_format",
            "json",
        ],
        vec!["fab-rs", "item", "unknown", "docs"],
        vec!["fab-rs", "item", "lakehouse", "docs", "--limit", "3"],
    ] {
        assert!(parse_args(args).is_err());
    }
}

#[test]
fn namespace_selection_never_treats_an_option_value_as_the_target() {
    use std::ffi::OsString;
    let args = [
        "get",
        "--query",
        "docs/data-engineering/lakehouse-overview.md",
        "Demo.Workspace",
    ]
    .map(OsString::from);
    assert!(!fabric_cli::docs_browse::selects_docs_get(&args));
}

#[test]
fn area_listing_honors_the_explicit_global_output_format() {
    let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args(["--output_format", "json", "item", "lakehouse", "docs"])
        .env("FAB_RUST_AUTH_MODE", "invalid-provider")
        .output()
        .unwrap();
    if fabric_docs_search::DocsSearch::embedded().is_ok() {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["source"], "embeddedFabricDocs");
        assert_eq!(value["data"]["articles"].as_array().unwrap().len(), 13);
    } else {
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn docs_get_help_and_engine_only_failures_do_not_read_ambient_configuration() {
    let root = tempfile::tempdir().unwrap();
    let invalid = root.path().join("not-a-directory");
    std::fs::write(&invalid, b"preserve").unwrap();
    for args in [
        vec!["get", "docs/", "--help"],
        vec!["item", "lakehouse", "docs", "--help"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
            .args(args)
            .env("FAB_RUST_CONFIG_ROOT", &invalid)
            .env("FAB_RUST_AUTH_MODE", "invalid-provider")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stderr.is_empty());
    }
    let result = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args([
            "get",
            "docs/data-engineering/lakehouse-overview.md",
            "--format",
            "json",
        ])
        .env("FAB_RUST_CONFIG_ROOT", &invalid)
        .env("FAB_RUST_AUTH_MODE", "invalid-provider")
        .output()
        .unwrap();
    match fabric_docs_search::DocsSearch::embedded() {
        Ok(_) => {
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(value["source"], "embeddedFabricDocs");
            assert!(!value["data"]["sections"].as_array().unwrap().is_empty());
            assert!(
                value["data"]["reference"]
                    .as_str()
                    .unwrap()
                    .starts_with("fab-rs get ")
            );
        }
        Err(fabric_docs_search::SearchError::NotEmbedded) => {
            assert_eq!(result.status.code(), Some(1));
            assert!(result.stdout.is_empty());
        }
        Err(error) => panic!("Broken test corpus: {error}"),
    }
    assert_eq!(std::fs::read(invalid).unwrap(), b"preserve");
}

#[test]
fn local_docs_only_batches_preserve_configuration_independence() {
    let root = tempfile::tempdir().unwrap();
    let invalid = root.path().join("invalid");
    std::fs::write(&invalid, b"preserve").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args(["-c", "get docs/", "-c", "item lakehouse docs"])
        .env("FAB_RUST_CONFIG_ROOT", &invalid)
        .output()
        .unwrap();
    let expected = if fabric_docs_search::DocsSearch::embedded().is_ok() {
        0
    } else {
        1
    };
    assert_eq!(result.status.code(), Some(expected));
    assert!(String::from_utf8_lossy(&result.stdout).contains("2 commands executed."));
    assert_eq!(std::fs::read(invalid).unwrap(), b"preserve");
}
