// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual executable entrypoints without credentials, network or saved settings.

#![deny(unnameable_test_items)]

use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output},
};

const TARGET: &str = "/11111111-1111-4111-8111-111111111111.Workspace/22222222-2222-4222-8222-222222222222.KQLDatabase";

fn binaries() -> [&'static str; 2] {
    [
        env!("CARGO_BIN_EXE_fab-rs"),
        env!("CARGO_BIN_EXE_fab-cli-v2"),
    ]
}

fn run(binary: &str, root: &Path, args: &[&str]) -> Output {
    Command::new(binary)
        .env_clear()
        .env("FAB_RUST_CONFIG_ROOT", root)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn both_real_query_entrypoints_render_shared_help_without_identity_or_config() {
    let directory = tempfile::tempdir().unwrap();
    for binary in binaries() {
        let output = run(binary, directory.path(), &["query", "--help"]);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let help = String::from_utf8(output.stdout).unwrap();
        for flag in [
            "--text",
            "--file",
            "--parameters",
            "--engine",
            "--format",
            "--output",
        ] {
            assert!(help.contains(flag));
        }
        assert!(!help.contains("--provider"));
        assert!(!help.contains("--resultset"));
        assert!(!help.contains("--language"));
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn both_real_roots_reject_unsupported_parameters_before_file_or_identity_access() {
    let directory = tempfile::tempdir().unwrap();
    for binary in binaries() {
        let output = run(
            binary,
            directory.path(),
            &[
                "query",
                TARGET,
                "--file",
                "private-statement-sentinel",
                "--parameters",
                "private-parameters-sentinel",
            ],
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(matches!(
            error["error"]["code"].as_str(),
            Some("invalid_arguments" | "UnsupportedParameters")
        ));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!stderr.contains("private-statement-sentinel"));
        assert!(!stderr.contains("private-parameters-sentinel"));
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn missing_explicit_native_context_fails_before_reading_query_stdin() {
    let directory = tempfile::tempdir().unwrap();
    for binary in binaries() {
        let output = run(binary, directory.path(), &["query", TARGET, "--file", "-"]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(!matches!(
            error["error"]["code"].as_str(),
            Some("invalid_arguments" | "InvalidInput" | "input_failed" | "InputFailed")
        ));
    }
}

#[test]
fn no_unapproved_query_provider_or_resultset_selectors_are_added_to_either_binary() {
    let directory = tempfile::tempdir().unwrap();
    for binary in binaries() {
        for flag in ["--provider", "--resultset", "--language"] {
            let output = run(
                binary,
                directory.path(),
                &[
                    "query",
                    TARGET,
                    "--text",
                    "private-query-sentinel",
                    flag,
                    "private-value-sentinel",
                ],
            );
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(!stderr.contains("private-query-sentinel"));
            assert!(!stderr.contains("private-value-sentinel"));
        }
    }
}

#[test]
fn v2_root_help_advertises_actual_opted_in_typed_query_entrypoint() {
    let directory = tempfile::tempdir().unwrap();
    let output = run(binaries()[1], directory.path(), &["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("fab-cli-v2 query"));
    assert!(help.contains("query_connection"));
    assert!(help.contains("native KQL"));
}
