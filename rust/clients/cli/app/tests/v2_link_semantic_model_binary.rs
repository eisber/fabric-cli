// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline executable grammar and preflight; no credentials or provider writes.

#![cfg(feature = "headless")]
#![deny(unnameable_test_items)]

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(args)
        .env_remove("FAB_RUST_AUTH_MODE")
        .env_remove("FAB_RUST_FABRIC_HOST")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn actual_semantic_model_leaf_and_root_help_are_mounted() {
    for args in [
        vec!["artifact", "semantic-model", "link", "--help"],
        vec!["link", "--help"],
    ] {
        let output = run(&args);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("--input"));
        assert!(help.contains("--wait"));
        assert!(!help.contains("--force"));
    }
}

#[test]
fn actual_leaf_rejects_wrong_owner_scope_and_invented_flags_before_input_or_auth() {
    for args in [
        vec![
            "link",
            "/private.Workspace/secret.Lakehouse/connections",
            "--input",
            "missing-private.json",
        ],
        vec![
            "artifact",
            "semantic-model",
            "link",
            "/private.Workspace/secret.SemanticModel",
            "--input",
            "missing-private.json",
        ],
        vec![
            "artifact",
            "semantic-model",
            "link",
            "/private.Workspace/secret.SemanticModel/connections",
            "--input",
            "-",
            "--force",
        ],
        vec![
            "artifact",
            "semantic-model",
            "link",
            "/private.Workspace/secret.SemanticModel/connections",
        ],
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let diagnostic = String::from_utf8(output.stderr).unwrap();
        assert!(!diagnostic.contains("private") && !diagnostic.contains("secret"));
        serde_json::from_str::<serde_json::Value>(&diagnostic).unwrap();
    }
}

#[test]
fn legacy_ln_is_not_remounted_in_v2() {
    let output = run(&["ln", "--help"]);
    assert!(!output.status.success());
}

#[test]
fn actual_native_leaf_captures_file_input_before_requiring_explicit_identity() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("binding.json");
    std::fs::write(
        &input,
        r#"{"connectionBinding":{
        "id":"33333333-3333-4333-8333-333333333333",
        "connectivityType":"ShareableCloud",
        "connectionDetails":{"type":"SQL","path":"server.example.invalid;database"}
    }}"#,
    )
    .unwrap();
    let output = run(&[
        "artifact",
        "semantic-model",
        "link",
        "/11111111-1111-4111-8111-111111111111.Workspace/22222222-2222-4222-8222-222222222222.SemanticModel/connections",
        "--input",
        input.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "NotConfigured");
}
