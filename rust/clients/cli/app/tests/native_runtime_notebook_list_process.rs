// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual executable offline registration, local preflight and zero-identity failures.

#![deny(unnameable_test_items)]

use serde_json::Value;
#[cfg(feature = "notebook-workload")]
use serde_json::json;
use std::{
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

const PATH: &str = "/Team.Workspace/Analysis.Notebook/sessions";

fn command(args: &[&str], binding: Option<&Path>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    if let Some(binding) = binding {
        command.env("FAB_RUST_NOTEBOOK_BINDING", binding);
    }
    command
}

fn run(mut command: Command) -> Output {
    let mut child = command.spawn().unwrap();
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= end {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("local-only executable exceeded bounded test watchdog");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[cfg(feature = "notebook-workload")]
fn binding() -> Value {
    json!({
        "schemaVersion":1,
        "owner":{"workspace":"Team.Workspace","artifact":"Analysis.Notebook","kind":"notebook"},
        "environment":"production","catalogOrigin":"https://api.fabric.microsoft.com",
        "clusterOrigin":"https://selected-cluster.analysis.windows.net",
        "workloadOrigin":"https://selected-notebook.fabric.microsoft.com",
        "capacityId":"44444444-4444-4444-4444-444444444444",
        "workspaceId":"55555555-5555-5555-5555-555555555555",
        "notebookId":"66666666-6666-6666-6666-666666666666",
        "account":"22222222-2222-2222-2222-222222222222",
        "tenant":"11111111-1111-1111-1111-111111111111"
    })
}

#[cfg(feature = "notebook-workload")]
#[test]
fn list_help_is_offline_without_binding_identity_or_secret_material() {
    for args in [
        vec!["list", "--help"],
        vec!["artifact", "notebook", "list", "--help"],
    ] {
        let output = run(command(&args, None));
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(
            help.contains("--limit") && help.contains("--max-bytes") && help.contains("--output")
        );
    }
    let output = run(command(&["--help"], None));
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("typedNotebook")
    );
}

#[cfg(feature = "notebook-workload")]
#[test]
fn collection_and_facade_require_explicit_binding_not_empty_success_or_identity_fallback() {
    for args in [
        vec!["list", PATH],
        vec!["artifact", "notebook", "list", PATH],
    ] {
        let output = run(command(&args, None));
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "NotebookBindingRequired");
        assert!(!String::from_utf8(output.stderr).unwrap().contains(PATH));
    }
}

#[cfg(feature = "notebook-workload")]
#[test]
fn unsupported_options_and_host_ceilings_fail_before_binding_or_identity() {
    for options in [
        vec!["--limit", "0"],
        vec!["--limit", "1001"],
        vec!["--max-bytes", "1048577"],
        vec!["--continuation", "private-token"],
        vec!["--timeout", "1"],
        vec!["--project", "private-query"],
        vec!["--profile", "private-account"],
        vec!["--format", "text"],
        vec!["--all"],
    ] {
        let mut args = vec!["list", PATH];
        args.extend(options);
        let output = run(command(&args, None));
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let text = String::from_utf8(output.stderr).unwrap();
        assert!(!text.contains("private-") && !text.contains(PATH));
        let error: Value = serde_json::from_str(&text).unwrap();
        assert_ne!(error["error"]["code"], "NotebookBindingRequired");
        assert_ne!(error["error"]["code"], "NotConfigured");
    }
}

#[cfg(feature = "notebook-workload")]
#[test]
fn invalid_binding_and_existing_output_fail_before_identity_and_never_clobber() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("binding.json");
    std::fs::write(&config, b"{").unwrap();
    let output = run(command(&["list", PATH], Some(&config)));
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "InvalidNotebookBinding");
    std::fs::write(&config, serde_json::to_vec(&binding()).unwrap()).unwrap();
    let destination = root.path().join("existing.json");
    std::fs::write(&destination, b"original").unwrap();
    let output = run(command(
        &["list", PATH, "--output", destination.to_str().unwrap()],
        Some(&config),
    ));
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "OutputUnavailable");
    assert_eq!(std::fs::read(destination).unwrap(), b"original");
    let destination = root.path().join("new.json");
    let output = run(command(
        &["list", PATH, "--output", destination.to_str().unwrap()],
        Some(&config),
    ));
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "NotConfigured");
    assert!(!destination.exists());
}

#[test]
fn ordinary_get_route_and_version_are_not_claimed_by_collection_mount() {
    let output = run(command(
        &["get", "/Team.Workspace/Analysis.Notebook/src/content"],
        None,
    ));
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "NotConfigured");
    let output = run(command(&["--version"], None));
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}

#[cfg(not(feature = "notebook-workload"))]
#[test]
fn default_headless_build_does_not_mount_notebook_runtime_or_access_identity() {
    for args in [
        vec!["list", PATH],
        vec!["artifact", "notebook", "list", PATH],
    ] {
        let output = run(command(&args, None));
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_ne!(error["error"]["code"], "NotebookBindingRequired");
        assert_ne!(error["error"]["code"], "NotConfigured");
    }
}
