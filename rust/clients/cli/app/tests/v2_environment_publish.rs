// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual executable grammar and no-effect preflight checks.
#![cfg(feature = "headless")]
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::process::{Command, Stdio};

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(args)
        .env_remove("FAB_RUST_AUTH_MODE")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn root_and_publish_help_advertise_only_saved_staging_contract() {
    let root = run(&["--help"]);
    assert!(root.status.success());
    assert!(
        String::from_utf8(root.stdout)
            .unwrap()
            .contains("publish <existingEnvironment> [--wait]")
    );
    let help = run(&["publish", "--help"]);
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("--wait"));
    assert!(text.contains("already-saved"));
    assert!(!text.contains("--approve"));
    assert!(!text.contains("--input"));
}

#[test]
fn invalid_paths_and_unselected_flags_fail_before_identity() {
    for args in [
        vec!["publish"],
        vec!["publish", "/W.Workspace/E.Environment/staging"],
        vec!["publish", "/W.Workspace/R.Report/staging"],
        vec![
            "publish",
            "/W.Workspace/E.Environment",
            "--input",
            "source.json",
        ],
        vec!["publish", "/W.Workspace/E.Environment", "--approve"],
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let value: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(value["error"]["code"], "InvalidArguments");
    }
}

#[test]
fn nonterminal_cannot_approve_publication_or_trigger_identity_discovery() {
    let output = run(&["publish", "/W.Workspace/E.Environment", "--wait"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], "ApprovalRequired");
}
