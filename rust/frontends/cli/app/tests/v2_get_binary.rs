// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline subprocess contracts for the actual optional v2 execution head.

#![cfg(feature = "headless")]
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::{
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const WS: &str = "11111111-1111-1111-1111-111111111111";

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(args)
        .env_remove("FAB_RUST_AUTH_MODE")
        .env_remove("FAB_RUST_CLIENT_SECRET")
        .env_remove("FAB_RUST_ASSERTION_FILE")
        .env_remove("FAB_RUST_FABRIC_HOST")
        .output()
        .unwrap()
}

#[test]
fn actual_head_help_and_version_never_need_identity() {
    for args in [vec!["--help"], vec!["get", "--help"], vec!["--version"]] {
        let output = run(&args);
        assert!(output.status.success());
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn actual_head_valid_metadata_requires_an_explicit_provider() {
    let path = format!("/workspaces/{WS}");
    let output = run(&["get", &path]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "NotConfigured");
}

#[test]
fn actual_head_rejects_bad_options_and_paths_without_echoing_them() {
    for args in [
        vec!["get", "private-customer-input"],
        vec![
            "get",
            "/private.Workspace/secret.Notebook",
            "--format",
            "yaml",
        ],
        vec!["get", "/private.Workspace/secret.Notebook", "--depth", "-1"],
        vec![
            "get",
            "/private.Workspace/secret.Notebook",
            "--timeout",
            "0s",
        ],
        vec![
            "get",
            "/private.Workspace/secret.Notebook",
            "--format",
            "raw",
            "--project",
            "private",
        ],
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let diagnostic = String::from_utf8(output.stderr).unwrap();
        assert!(!diagnostic.contains("private") && !diagnostic.contains("secret"));
        serde_json::from_str::<Value>(&diagnostic).unwrap();
    }
}

#[test]
fn actual_head_existing_output_precedes_identity_configuration() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "fabric-v2-binary-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("existing.json");
    std::fs::write(&path, b"preserve").unwrap();
    let output = run(&[
        "get",
        &format!("/workspaces/{WS}"),
        "--output",
        path.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_ne!(error["error"]["code"], "NotConfigured");
    assert_eq!(std::fs::read(&path).unwrap(), b"preserve");
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn actual_head_dataagent_stage_and_facade_contract_precedes_identity() {
    let agent = format!("/{WS}.Workspace/{WS}.DataAgent");
    for args in [
        vec!["get", "--help"],
        vec!["artifact", "data-agent", "get", "--help"],
    ] {
        let output = run(&args);
        assert!(output.status.success());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("--stage")
        );
    }

    #[test]
    fn actual_head_explicit_broker_mode_never_uses_headless_or_account_fallback() {
        let root = tempfile::tempdir().unwrap();
        let invoke = |args: &[&str]| {
            Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
                .args(args)
                .env("FAB_RUST_AUTH_MODE", "broker")
                .env("FAB_RUST_CONFIG_ROOT", root.path())
                .env_remove("FAB_RUST_TENANT")
                .env_remove("FAB_RUST_ACCOUNT")
                .env_remove("FAB_RUST_CLIENT_ID")
                .output()
                .unwrap()
        };
        let output = invoke(&["get", &format!("/workspaces/{WS}")]);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(
            error["error"]["code"],
            if cfg!(all(windows, feature = "windows-broker")) {
                "NotConfigured"
            } else {
                "UnsupportedCredential"
            }
        );
        assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
        for args in [vec!["get", "--help"], vec!["get", "invalid-private-path"]] {
            let output = invoke(&args);
            assert_eq!(
                output.status.code(),
                Some(if args[1] == "--help" { 0 } else { 2 })
            );
        }
        #[cfg(all(windows, feature = "windows-broker"))]
        {
            let binding = root.path().join("broker-session.json");
            std::fs::write(&binding, br#"{"schema":999}"#).unwrap();
            let output = invoke(&["get", &format!("/workspaces/{WS}")]);
            assert_eq!(output.status.code(), Some(2));
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["error"]["code"], "invalid_configuration");
            assert_eq!(std::fs::read(binding).unwrap(), br#"{"schema":999}"#);
        }
    }
    for args in [
        vec!["get".into(), format!("{agent}/settings")],
        vec![
            "get".into(),
            agent.clone(),
            "--stage".into(),
            "draft".into(),
        ],
        vec![
            "get".into(),
            format!("{agent}/settings"),
            "--stage".into(),
            "staging".into(),
        ],
        vec![
            "get".into(),
            format!("{agent}/src/draft"),
            "--stage".into(),
            "published".into(),
        ],
        vec![
            "get".into(),
            format!("/workspaces/{WS}/items/{WS}/src/draft"),
        ],
        vec!["get".into(), format!("{agent}/src/definition/formats/TMDL")],
        vec![
            "artifact".into(),
            "data-agent".into(),
            "get".into(),
            format!("/{WS}.Workspace/{WS}.Notebook"),
        ],
    ] {
        let refs: Vec<_> = args.iter().map(String::as_str).collect();
        let output = run(&refs);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
    }
    for args in [
        vec![
            "get".into(),
            format!("{agent}/settings"),
            "--stage".into(),
            "draft".into(),
        ],
        vec![
            "artifact".into(),
            "data-agent".into(),
            "get".into(),
            format!("{agent}/src/published"),
        ],
    ] {
        let refs: Vec<_> = args.iter().map(String::as_str).collect();
        let output = run(&refs);
        assert_eq!(output.status.code(), Some(1));
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "NotConfigured");
    }
}
