// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual binary preflight: no configured identity, runtime or live service.

#![cfg(feature = "headless")]
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(args)
        .env_remove("FAB_RUST_AUTH_MODE")
        .env_remove("FAB_RUST_FABRIC_HOST")
        .output()
        .unwrap()
}

#[test]
fn actual_exists_help_is_local_and_valid_execution_requires_explicit_identity() {
    let help = run(&["exists", "--help"]);
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("--max-bytes")
    );
    let result = run(&["exists", "/Sales.Workspace/Note.Notebook"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    let error: Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["error"]["code"], "NotConfigured");
}

#[test]
fn actual_invalid_and_physical_targets_fail_before_identity_without_echoing_input() {
    for path in [
        "C:\\private",
        "secret.txt",
        "/private.Workspace/secret.Lakehouse/Tables/dbo/table",
        "/private.Workspace/secret.Notebook/src/definition",
        "/private.Workspace/secret.Notebook/Files/test",
    ] {
        let result = run(&["exists", path]);
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        let text = String::from_utf8(result.stderr).unwrap();
        assert!(!text.contains("private") && !text.contains("secret"));
        let error: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(error["error"]["code"], "InvalidArguments");
    }
}
