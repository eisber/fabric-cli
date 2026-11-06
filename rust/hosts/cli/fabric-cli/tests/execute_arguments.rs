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
}
