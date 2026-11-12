// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual native process tests, without a live credential or Fabric operation.

#![deny(unnameable_test_items)]

use serde_json::Value;
use std::process::Command;

const CONTENT: &str = "/Team.Workspace/Analysis.Notebook/src/content";

fn process(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(args)
        .env("FAB_RUST_AUTH_MODE", "must-not-be-selected")
        .env("FAB_RUST_FABRIC_HOST", "private.invalid.endpoint")
        .env_remove("FAB_RUST_NOTEBOOK_BINDING")
        .output()
        .unwrap()
}

#[test]
fn native_content_and_facade_fail_honestly_before_identity_or_http() {
    for args in [
        vec!["get", CONTENT],
        vec!["artifact", "notebook", "get", CONTENT],
        vec!["get", "--format", "json", CONTENT],
        vec!["artifact", "notebook", "get", "--format", "json", CONTENT],
        vec!["get", "--", CONTENT],
        vec!["artifact", "notebook", "get", "--", CONTENT],
    ] {
        let result = process(&args);
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        let error: Value = serde_json::from_slice(&result.stderr).unwrap();
        assert_eq!(
            error["error"]["code"],
            if cfg!(feature = "notebook-workload") {
                "NotebookBindingRequired"
            } else {
                "NotebookWorkloadBindingRequired"
            }
        );
        assert_eq!(error["remoteReplayed"], false);
        assert!(!String::from_utf8_lossy(&result.stderr).contains("private.invalid.endpoint"));
    }
}

#[test]
fn runtime_help_is_offline_and_names_the_actual_production_prerequisite() {
    for args in [
        vec!["get", CONTENT, "--help"],
        vec!["artifact", "notebook", "get", "--help"],
    ] {
        let result = process(&args);
        assert!(result.status.success());
        assert!(result.stderr.is_empty());
        let help = String::from_utf8(result.stdout).unwrap();
        assert!(help.contains("NotebookWorkloadBindingRequired"));
        assert!(help.contains("saved outputs"));
    }
}

#[test]
fn update_facade_help_and_default_help_are_not_swallowed_by_runtime_dispatch() {
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["artifact", "notebook", "update", "--help"],
    ] {
        let result = process(&args);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stderr.is_empty());
    }
}

#[test]
fn unsupported_options_and_excessive_budget_never_look_like_missing_authentication() {
    for args in [
        vec!["get", CONTENT, "--recursive"],
        vec!["get", CONTENT, "--max-bytes", "1048577"],
    ] {
        let result = process(&args);
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        let error: Value = serde_json::from_slice(&result.stderr).unwrap();
        assert_ne!(error["error"]["code"], "NotebookWorkloadBindingRequired");
    }
}
