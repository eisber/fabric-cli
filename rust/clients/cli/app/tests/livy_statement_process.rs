// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual opt-in executable mount; these processes have no usable credentials.

#![cfg(all(feature = "runtime-v2", feature = "headless"))]
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::process::{Command, Output};

const PATH: &str = concat!(
    "/11111111-1111-1111-1111-111111111111.Workspace/",
    "22222222-2222-2222-2222-222222222222.Lakehouse/sessions/session-7/statements/2"
);

fn process(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
    command
        .env_clear()
        .env("FAB_RUST_FABRIC_HOST", "msitapi.fabric.microsoft.com")
        .args(args);
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    command
}

fn failure(output: Output, code: &str) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.len() < 1024);
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], code);
    assert_eq!(value["remoteCancelled"], false);
    assert_eq!(value["remoteReplayed"], false);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("11111111"));
}

#[test]
fn selected_statement_help_is_offline_and_describes_full_json_not_dataframes() {
    for path in [PATH.to_owned(), format!("{PATH}/result")] {
        let output = process(&["get", &path, "--help"])
            .env("FAB_RUST_AUTH_MODE", "invalid-mode-must-not-be-inspected")
            .env("FAB_RUST_FABRIC_HOST", "invalid-host-must-not-be-inspected")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("same full JSON envelope"));
        assert!(text.contains("2023-12-01"));
        assert!(text.contains("no submit"));
    }
}

#[test]
fn missing_explicit_identity_and_native_service_are_sanitized_failures_not_fallbacks() {
    failure(
        process(&["get", PATH]).output().unwrap(),
        "unsupported_capability",
    );
    failure(
        process(&[
            "get",
            "/W.Workspace/Native.LivyService/sessions/0/statements/2",
        ])
        .output()
        .unwrap(),
        "unsupported_capability",
    );
}

#[test]
fn negative_and_overflow_statement_ids_never_reach_identity_or_generic_get() {
    for id in ["-1", "9223372036854775808", "not-a-statement-id"] {
        failure(
            process(&[
                "get",
                &PATH.replace("statements/2", &format!("statements/{id}")),
            ])
            .env("FAB_RUST_AUTH_MODE", "invalid-mode")
            .output()
            .unwrap(),
            "invalid_input",
        );
    }
}

#[test]
fn existing_output_is_rejected_before_selected_identity_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let output_path = temp.path().join("statement.json");
    std::fs::write(&output_path, b"existing owner").unwrap();
    failure(
        process(&["get", PATH, "-o", "statement.json"])
            .current_dir(temp.path())
            .env("FAB_RUST_AUTH_MODE", "invalid-mode")
            .output()
            .unwrap(),
        "output_unavailable",
    );
    assert_eq!(std::fs::read(output_path).unwrap(), b"existing owner");
}
