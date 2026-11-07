// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The real v2 search head validates and describes operations before identity.

#![cfg(feature = "headless")]
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::{
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(args)
        .env_remove("FAB_RUST_AUTH_MODE")
        .env_remove("FAB_RUST_CLIENT_SECRET")
        .env_remove("FAB_RUST_ASSERTION_FILE")
        .env_remove("FAB_RUST_FABRIC_HOST")
        .env_remove("FAB_RUST_RELEVANCE_HOST")
        .env_remove("FAB_RUST_RELEVANCE_AUDIENCE")
        .output()
        .unwrap()
}

#[test]
fn actual_search_help_and_schema_need_no_runtime_or_identity() {
    let help = run(&["search", "--help"]);
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    assert!(String::from_utf8(help.stdout).unwrap().contains("--mode"));
    let schema = run(&["search", "/", "--schema"]);
    assert!(schema.status.success());
    assert!(schema.stderr.is_empty());
    let schema: Value = serde_json::from_slice(&schema.stdout).unwrap();
    assert!(schema["input"].is_object());
    assert!(schema["output"].is_object());
    assert_eq!(
        schema,
        serde_json::to_value(fabric_sdk::search::schemas()).unwrap()
    );
}

#[test]
fn actual_native_search_requires_explicit_identity_and_relevance_binding() {
    for mode in ["catalog", "relevance", "hybrid"] {
        let output = run(&["search", "/", "--mode", mode, "--text", "sales"]);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "NotConfigured");
    }
}

#[test]
fn invalid_search_arguments_are_redacted_before_provider_setup() {
    for args in [
        vec![
            "search",
            "/private.Workspace/secret.Notebook",
            "--text",
            "private",
        ],
        vec!["search", "/", "--mode", "private", "--text", "private"],
        vec!["search", "/", "--text", "private", "--limit", "0"],
        vec![
            "search", "/", "--mode", "hybrid", "--text", "private", "--limit", "51",
        ],
        vec!["search", "/", "--text", "private", "--timeout", "25h"],
        vec!["search", "/", "--text", "private", "--project", "["],
        vec![
            "search",
            "/",
            "--text",
            "private",
            "--continuation-token",
            "secret",
        ],
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("private") && !text.contains("secret"));
        serde_json::from_str::<Value>(&text).unwrap();
    }
    let long_text = "\u{1f600}".repeat(251);
    let output = run(&["search", "/", "--mode", "relevance", "--text", &long_text]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn existing_output_fails_before_identity_or_relevance_configuration() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "fabric-search-binary-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("existing.json");
    std::fs::write(&path, b"preserve").unwrap();
    let output = run(&[
        "search",
        "/",
        "--mode",
        "hybrid",
        "--text",
        "sales",
        "--output",
        path.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "OutputFailed");
    assert_eq!(std::fs::read(&path).unwrap(), b"preserve");
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root).unwrap();
}
