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

#[test]
fn opaque_unbound_paths_keep_the_existing_receipt_before_config_identity_or_stdin() {
    let directory = tempfile::tempdir().unwrap();
    for binary in binaries() {
        let output = Command::new(binary)
            .env_clear()
            .env(
                "FAB_RUST_CONFIG_ROOT",
                directory.path().join("must-not-initialize"),
            )
            .env("FAB_RUST_AUTH_MODE", "invalid")
            .args(["query", "arbitrary-table", "--file", "-"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stderr.is_empty());
        let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(receipt["delivery"]["error"]["code"], "NotConfigured");
        assert_eq!(receipt["execution"]["status"], "not_started");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("arbitrary-table"));
    }

    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn both_real_roots_require_explicit_xlsx_output_and_reject_binary_stdout() {
    let directory = tempfile::tempdir().unwrap();
    for binary in binaries() {
        for suffix in [vec![], vec!["--output", "-"]] {
            let mut args = vec![
                "query",
                TARGET,
                "--text",
                "private-query",
                "--format",
                "xlsx",
            ];
            args.extend(suffix);
            let output = run(binary, directory.path(), &args);
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert!(matches!(
                error["error"]["code"].as_str(),
                Some("invalid_arguments" | "InvalidArguments")
            ));
            assert!(!String::from_utf8_lossy(&output.stderr).contains("private-query"));
        }
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn both_real_roots_reject_native_kql_and_id_only_xlsx_before_identity_or_input() {
    let directory = tempfile::tempdir().unwrap();
    let id_only = "/workspaces/11111111-1111-4111-8111-111111111111/items/22222222-2222-4222-8222-222222222222";
    for binary in binaries() {
        for target in [TARGET, id_only] {
            let output = Command::new(binary)
                .env_clear()
                .env(
                    "FAB_RUST_CONFIG_ROOT",
                    directory.path().join("must-not-initialize"),
                )
                .env("FAB_RUST_AUTH_MODE", "invalid")
                .args([
                    "query",
                    target,
                    "--file",
                    "-",
                    "--format",
                    "xlsx",
                    "--output",
                    "native.xlsx",
                ])
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert!(matches!(
                error["error"]["code"].as_str(),
                Some("unsupported_format" | "UnsupportedFormat")
            ));
        }
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}
