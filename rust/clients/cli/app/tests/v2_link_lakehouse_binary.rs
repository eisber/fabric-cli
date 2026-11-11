// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual optional-head isolation; no live identity, endpoint or resource use.
#![cfg(feature = "headless")]
#![deny(unnameable_test_items)]

#[cfg(feature = "v2-link-lakehouse")]
use serde_json::{Value, json};
use std::process::{Command, Output, Stdio};

#[cfg(feature = "v2-link-lakehouse")]
const PATH: &str = "/11111111-1111-4111-8111-111111111111.Workspace/22222222-2222-4222-8222-222222222222.Lakehouse/Files/sales.Shortcut";

fn run(args: &[&str], input: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(args)
        .env_remove("FAB_RUST_AUTH_MODE")
        .env_remove("FAB_RUST_CLIENT_SECRET")
        .env_remove("FAB_RUST_ASSERTION_FILE")
        .env_remove("FAB_RUST_FABRIC_HOST")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(input).unwrap();
    } else {
        drop(child.stdin.take());
    }
    child.wait_with_output().unwrap()
}

#[test]
fn real_head_link_help_and_feature_isolation() {
    for args in [
        vec!["link", "--help"],
        vec!["artifact", "lakehouse", "link", "--help"],
    ] {
        let output = run(&args, None);
        assert_eq!(output.status.success(), cfg!(feature = "v2-link-lakehouse"));
        if cfg!(feature = "v2-link-lakehouse") {
            assert!(output.stderr.is_empty());
            assert!(String::from_utf8_lossy(&output.stdout).contains("--input"));
        } else {
            assert!(output.stdout.is_empty());
        }
    }
    let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .args(["link", "--help"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "v1 must not mount native v2 link");
}

#[test]
#[cfg(feature = "v2-link-lakehouse")]
fn real_head_strict_preflight_precedes_identity_and_valid_input_requires_explicit_host() {
    for prefix in [vec!["link"], vec!["artifact", "lakehouse", "link"]] {
        let mut args = prefix;
        args.extend([PATH, "--input", "-"]);
        let malformed = run(&args, Some(br#"{"path":"Files","path":"Files"}"#));
        assert_eq!(malformed.status.code(), Some(2));
        assert!(malformed.stdout.is_empty());
        let error: Value = serde_json::from_slice(&malformed.stderr).unwrap();
        assert_eq!(error["error"]["code"], "InvalidInput");
        let valid = serde_json::to_vec(&json!({"path":"Files","name":"sales","target":{"oneLake":{
            "workspaceId":"11111111-1111-4111-8111-111111111111",
            "itemId":"33333333-3333-4333-8333-333333333333","path":"Files/source"
        }}}))
        .unwrap();
        let output = run(&args, Some(&valid));
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "NotConfigured");
    }
    for args in [
        vec![
            "link",
            PATH,
            "--input",
            "missing-private-input.json",
            "--force",
        ],
        vec![
            "link",
            "private-invalid-target",
            "--input",
            "missing-private-input.json",
        ],
    ] {
        let output = run(&args, None);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private"));
    }
}
