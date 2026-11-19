// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Root execute parsing and no-host validation contracts.
#![deny(unnameable_test_items)]

use fabric_cli::{
    execute::{QueryTarget, ResultFormat},
    parser::{Invocation, parse_args},
};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Dataflow/queries/Orders";

#[test]
fn root_execute_selects_dataflow_and_common_formats() {
    for (name, expected) in [
        ("csv", ResultFormat::Csv),
        ("json", ResultFormat::Json),
        ("parquet", ResultFormat::Parquet),
    ] {
        let Invocation::Execute(command) = parse_args([
            "fab-rs",
            "execute",
            TARGET,
            "--wait",
            "--format",
            name,
            "--output",
            "result.data",
            "--timeout",
            "90s",
        ])
        .unwrap() else {
            panic!("expected native execution");
        };
        assert_eq!(command.target.query, "Orders");
        assert_eq!(command.format, expected);
        assert_eq!(command.timeout.as_secs(), 90);
    }
}

#[test]
fn unsupported_target_and_submission_combinations_fail_before_host_setup() {
    for arguments in [
        vec!["fab-rs", "execute", TARGET],
        vec!["fab-rs", "execute", TARGET, "--wait", "--format", "excel"],
        vec!["fab-rs", "execute", TARGET, "--wait", "--format", "parquet"],
        vec!["fab-rs", "execute", TARGET, "--wait", "--text", "SELECT 1"],
        vec!["fab-rs", "execute", TARGET, "--wait", "--timeout", "0s"],
        vec!["fab-rs", "execute", TARGET, "--wait", "--timeout", "25h"],
        vec!["fab-rs", "execute", TARGET, "--wait", "--limit", "0"],
        vec!["fab-rs", "execute", TARGET, "--wait", "--max-bytes", "0"],
        vec![
            "fab-rs", "execute", TARGET, "--wait", "--input", "-", "--input", "-",
        ],
        vec![
            "fab-rs", "execute", TARGET, "--wait", "--format", "csv", "--format", "json",
        ],
        vec![
            "fab-rs", "execute", TARGET, "--wait", "--output", "one", "--output", "two",
        ],
        vec![
            "fab-rs",
            "execute",
            TARGET,
            "--wait",
            "--output_format",
            "text",
        ],
    ] {
        assert!(parse_args(arguments).is_err());
    }
    for path in [
        "Orders",
        "/Name.Workspace/Name.Dataflow/queries/Orders",
        "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook/queries/Orders",
        "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Dataflow/queries/../Orders",
    ] {
        assert!(QueryTarget::parse(path).is_err());
    }
}

#[test]
fn help_is_available_without_credentials_or_files() {
    let Invocation::Display(help) = parse_args(["fab-rs", "execute", "--help"]).unwrap() else {
        panic!("expected help");
    };
    assert!(help.contains("parquet"));
    assert!(help.contains("Dataflow"));
    assert!(help.contains("--wait"));
    assert!(
        !help
            .lines()
            .any(|line| line.trim_start().starts_with("--list-queries"))
    );
    let Invocation::Display(get_help) = parse_args(["fab-rs", "get", "--help"]).unwrap() else {
        panic!("expected GET help");
    };
    assert!(get_help.contains("--list-queries"));
    assert!(help.contains("mExpression"));
}

#[test]
fn metadata_discovery_is_explicit_and_never_ignores_execution_options() {
    let item = TARGET.strip_suffix("/queries/Orders").unwrap();
    let Invocation::DataflowQueries(command) =
        parse_args(["fab-rs", "get", item, "--list-queries", "--timeout", "30s"]).unwrap()
    else {
        panic!("expected metadata discovery")
    };
    assert_eq!(
        command.target.workspace_id(),
        "11111111-1111-1111-1111-111111111111"
    );
    assert_eq!(command.timeout.as_secs(), 30);
    for extra in [
        vec!["--wait"],
        vec!["--input", "-"],
        vec!["--format", "json"],
        vec!["--output", "-"],
        vec!["--limit", "10"],
        vec!["--max-bytes", "16777217"],
        vec!["--timeout", "0s"],
        vec!["--time", "30s"],
        vec!["--time=30s"],
        vec!["--max-b", "1024"],
        vec!["--que", "queries[].queryName"],
        vec!["--query", "["],
        vec!["--verbose"],
        vec!["--force"],
        vec!["--output_format", "json"],
        vec!["--list-queries"],
    ] {
        let mut args = vec!["fab-rs", "get", item, "--list-queries"];
        args.extend(extra);
        assert!(parse_args(args).is_err());
    }
    assert!(parse_args(["fab-rs", "get", TARGET, "--list-queries"]).is_err());
    assert!(parse_args(["fab-rs", "execute", item, "--list-queries"]).is_err());
    assert!(parse_args(["fab-rs", "execute", TARGET, "--wait", "--list-queries"]).is_err());
    assert!(parse_args(["fab-rs", "get", item, "--list-q"]).is_err());
    assert!(parse_args(["fab-rs", "get", item, "--timeout", "30s"]).is_err());
    assert!(parse_args(["fab-rs", "get", item, "--max-bytes", "1024"]).is_err());
    assert!(
        parse_args([
            "fab-rs",
            "get",
            item,
            "--list-queries",
            "--timeout",
            "30s",
            "--timeout",
            "60s"
        ])
        .is_err()
    );
    assert!(parse_args(["fab-rs", "execute", item]).is_err());
}

#[test]
fn get_discovery_preserves_metadata_and_local_projection_without_execution() {
    let item = TARGET.strip_suffix("/queries/Orders").unwrap();
    let Invocation::Read(metadata) = parse_args(["fab-rs", "get", item, "-q", "id"]).unwrap()
    else {
        panic!("ordinary metadata GET");
    };
    assert_eq!(metadata.query.as_deref(), Some("id"));
    let Invocation::Read(abbreviated) = parse_args(["fab-rs", "get", item, "--que", "id"]).unwrap()
    else {
        panic!("ordinary metadata GET retains its existing abbreviations");
    };
    assert_eq!(abbreviated.query.as_deref(), Some("id"));
    let Invocation::DataflowQueries(discovery) = parse_args([
        "fab-rs",
        "get",
        item,
        "--list-queries",
        "-q",
        "queries[].queryName",
    ])
    .unwrap() else {
        panic!("query metadata discovery")
    };
    assert_eq!(discovery.query.as_deref(), Some("queries[].queryName"));
    assert_eq!(discovery.timeout.as_secs(), 300);
    assert_eq!(discovery.max_bytes, 16 * 1024 * 1024);
    for path in [
        "Name.Workspace/Item.Dataflow",
        "/Name.Workspace/Item.Dataflow",
        "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook",
    ] {
        assert!(parse_args(["fab-rs", "get", path, "--list-queries"]).is_err());
    }
}
