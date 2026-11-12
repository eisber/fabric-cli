// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual optional executable dispatch, without provider access.

#![cfg(feature = "headless")]
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::process::{Command, Output};

const COLLECTION: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.DataAgent/settings/staging/datasources";

fn run(facade: bool, tail: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
    command.args(if facade {
        vec!["artifact", "data-agent", "list"]
    } else {
        vec!["list"]
    });
    command
        .args(tail)
        .env_remove("FAB_RUST_AUTH_MODE")
        .env_remove("FAB_RUST_CLIENT_SECRET")
        .env_remove("FAB_RUST_ASSERTION_FILE")
        .env_remove("FAB_RUST_FABRIC_HOST")
        .output()
        .unwrap()
}

#[test]
fn actual_root_and_facade_help_are_effect_free_and_identical() {
    let root = run(false, &["--help"]);
    let facade = run(true, &["--help"]);
    for output in [&root, &facade] {
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(String::from_utf8_lossy(&output.stdout).contains("--stage"));
    }
    assert_eq!(root.stdout, facade.stdout);
}

#[test]
fn actual_root_and_facade_validate_before_identity_and_require_explicit_provider() {
    for facade in [false, true] {
        for tail in [
            vec![COLLECTION],
            vec![COLLECTION, "--stage", "published"],
            vec!["/private.Workspace/secret.DataAgent", "--stage", "draft"],
            vec![
                COLLECTION,
                "--stage",
                "draft",
                "--continuation",
                "private\nsecret",
            ],
            vec![COLLECTION, "--stage", "draft", "--all"],
            vec![COLLECTION, "--stage", "draft", "--limit", "0"],
        ] {
            let output = run(facade, &tail);
            assert_eq!(output.status.code(), Some(2));
            assert!(output.stdout.is_empty());
            let text = String::from_utf8(output.stderr).unwrap();
            assert!(!text.contains("private") && !text.contains("secret"));
            let error: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(error["error"]["code"], "InvalidArguments");
        }
        let output = run(facade, &[COLLECTION, "--stage", "draft"]);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "NotConfigured");
    }
}

#[test]
fn actual_existing_output_fails_before_identity_and_remains_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("existing.json");
    std::fs::write(&path, b"preserve").unwrap();
    for facade in [false, true] {
        let output = run(
            facade,
            &[
                COLLECTION,
                "--stage",
                "draft",
                "--output",
                path.to_str().unwrap(),
            ],
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "OutputFailed");
        assert_eq!(std::fs::read(&path).unwrap(), b"preserve");
    }
}
