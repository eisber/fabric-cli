// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual executable preflight without configured identity, authentication or HTTP.

#![cfg(feature = "headless")]
#![deny(unnameable_test_items)]

use serde_json::{Value, json};
use std::process::{Command, Output};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.MirroredDatabase/runtime/mirroring";
const OPERATION_ID: &str = "mirroredDatabase.mirroring.start";

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(args)
        .env_remove("FAB_RUST_AUTH_MODE")
        .env_remove("FAB_RUST_FABRIC_HOST")
        .output()
        .unwrap()
}

fn assert_error(output: &Output, exit: i32, code: &str) -> Value {
    assert_eq!(output.status.code(), Some(exit));
    assert!(
        output.stdout.is_empty(),
        "a refusal must not resemble success"
    );
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], code);
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains(TARGET));
    error
}

#[test]
fn actual_root_and_start_help_advertise_only_the_bounded_local_start_surface() {
    let root = run(&["--help"]);
    assert!(root.status.success());
    assert!(root.stderr.is_empty());
    let root = String::from_utf8(root.stdout).unwrap();
    assert!(root.contains("fab-cli-v2 start"));
    assert!(root.contains("MirroredDatabase"));
    let help = run(&["start", "--help"]);
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    let help = String::from_utf8(help.stdout).unwrap();
    for advertised in [
        "--approve",
        "--timeout",
        "--schema",
        "MirroredDatabase",
        "runtime/mirroring",
    ] {
        assert!(
            help.contains(advertised),
            "missing help contract: {advertised}"
        );
    }
    for unsupported in [
        "--wait",
        "--force",
        "--input",
        "--profile",
        "--output",
        "--query",
        "--preview",
    ] {
        assert!(!help.contains(unsupported), "unbound option: {unsupported}");
    }
}

#[test]
fn actual_schema_is_local_json_and_exposes_one_start_binding_not_a_generic_mutator() {
    let output = run(&["start", "--schema"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(output.stdout.last(), Some(&b'\n'));
    let schema: Value = serde_json::from_slice(&output.stdout).unwrap();
    let operations = schema["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0]["id"], OPERATION_ID);
    assert_eq!(operations[0]["verb"], "start");
    assert_eq!(
        operations[0]["effects"],
        json!(["remoteRead", "remoteWrite"])
    );
    assert_eq!(operations[0]["optionalEffects"], json!([]));
    assert_eq!(operations[0]["inputSchema"]["additionalProperties"], false);
    assert_eq!(operations[0]["inputSchema"]["properties"], json!({}));
    let bindings = schema["bindings"].as_array().unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0]["operationId"], OPERATION_ID);
    assert_eq!(bindings[0]["targetKind"], "item");
    assert_eq!(bindings[0]["ownerType"], "MirroredDatabase");
    assert_eq!(bindings[0]["facet"], "runtime");
    assert_eq!(bindings[0]["representation"], "mirroring");
}

#[test]
fn actual_missing_approval_precedes_identity_and_valid_approval_requires_identity() {
    assert_error(
        &run(&["start", TARGET, "--timeout", "10"]),
        1,
        "ApprovalRequired",
    );
    assert_error(
        &run(&["start", TARGET, "--approve", "--timeout", "10"]),
        1,
        "NotConfigured",
    );
}

#[test]
fn actual_unsupported_targets_fail_before_identity_without_echoing_private_input() {
    for path in [
        "/".to_owned(),
        TARGET.trim_start_matches('/').to_owned(),
        TARGET.replace("11111111-1111-1111-1111-111111111111", "private-workspace"),
        TARGET.replace("22222222-2222-2222-2222-222222222222", "private-item"),
        TARGET.replace(".MirroredDatabase", ".Notebook"),
        TARGET.replace("/runtime/mirroring", ""),
        TARGET.replace("/runtime/mirroring", "/mirroring"),
        TARGET.replace("/runtime/mirroring", "/runtime/jobs"),
        TARGET.replace("/runtime/mirroring", "/runtime/sessions"),
        TARGET.replace("/runtime/mirroring", "/src/definition"),
        TARGET.replace("22222222-2222-2222-2222-222222222222", "*"),
        format!("{TARGET}/private-child"),
        format!("{TARGET}?private-query=value"),
        format!("{TARGET}#private-fragment"),
        "C:\\private-start\\target".to_owned(),
    ] {
        let output = run(&["start", &path, "--timeout", "10"]);
        assert_error(&output, 2, "InvalidArguments");
        let diagnostic = String::from_utf8(output.stderr).unwrap();
        assert!(!diagnostic.contains("private"));
        if path != "/" {
            assert!(!diagnostic.contains(&path), "echoed target: {path:?}");
        }
    }
}

#[test]
fn actual_invalid_deadlines_fail_before_approval_or_identity() {
    for seconds in [
        "0",
        "-1",
        "86401",
        "18446744073709551616",
        "1.5",
        "private-timeout",
    ] {
        let output = run(&["start", TARGET, "--timeout", seconds]);
        assert_error(&output, 2, "InvalidArguments");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-timeout"));
    }
    assert_error(&run(&["start", TARGET]), 2, "InvalidArguments");
    assert_error(&run(&["start", TARGET, "--timeout"]), 2, "InvalidArguments");
}

#[test]
fn actual_schema_conflicts_duplicates_and_partial_flags_are_rejected() {
    for values in [
        vec!["start", "--schema", TARGET],
        vec!["start", "--schema", "--timeout", "10"],
        vec!["start", "--schema", "--approve"],
        vec!["start", "--schema", "--schema"],
        vec!["start", TARGET, TARGET, "--approve", "--timeout", "10"],
        vec!["start", TARGET, "--approve", "--approve", "--timeout", "10"],
        vec!["start", TARGET, "--timeout", "10", "--timeout", "20"],
        vec!["start", TARGET, "--appro", "--timeout", "10"],
        vec!["start", TARGET, "--approve", "--time", "10"],
        vec!["start", "--schem"],
    ] {
        assert_error(&run(&values), 2, "InvalidArguments");
    }
}

#[test]
fn actual_unadvertised_execution_options_do_not_reach_identity_selection() {
    for options in [
        vec!["--wait"],
        vec!["--force"],
        vec!["--input", "private-body.json"],
        vec!["--profile", "private-profile"],
        vec!["--output", "private-output.json"],
        vec!["--output_format", "json"],
        vec!["--query", "private-query"],
        vec!["--preview"],
        vec!["--type", "Full"],
    ] {
        let mut values = vec!["start", TARGET, "--approve", "--timeout", "10"];
        values.extend(options);
        let output = run(&values);
        assert_error(&output, 2, "InvalidArguments");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private"));
    }
}
