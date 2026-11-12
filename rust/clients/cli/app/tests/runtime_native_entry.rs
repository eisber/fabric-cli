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

#[cfg(feature = "notebook-workload")]
fn selected_binding() -> Value {
    serde_json::json!({
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
fn executable_output_preflight_precedes_even_native_identity_selection() {
    let temp = tempfile::tempdir().unwrap();
    let binding = temp.path().join("binding.json");
    std::fs::write(&binding, serde_json::to_vec(&selected_binding()).unwrap()).unwrap();
    let output = temp.path().join("existing.json");
    std::fs::write(&output, b"preserve").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(["get", CONTENT, "--output"])
        .arg(&output)
        .env("FAB_RUST_NOTEBOOK_BINDING", &binding)
        .env("FAB_RUST_AUTH_MODE", "must-not-be-selected")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    let error: Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["error"]["code"], "OutputUnavailable");
    assert_eq!(std::fs::read(output).unwrap(), b"preserve");
}

#[cfg(feature = "notebook-workload")]
#[test]
fn executable_selected_identity_mismatch_fails_before_credential_construction_or_http() {
    let temp = tempfile::tempdir().unwrap();
    let binding = temp.path().join("binding.json");
    std::fs::write(&binding, serde_json::to_vec(&selected_binding()).unwrap()).unwrap();
    let output = temp.path().join("not-created.json");
    let result = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(["artifact", "notebook", "get", CONTENT, "--output"])
        .arg(&output)
        .env("FAB_RUST_NOTEBOOK_BINDING", &binding)
        .env("FAB_RUST_AUTH_MODE", "client-secret")
        .env("FAB_RUST_TENANT", "11111111-1111-1111-1111-111111111111")
        .env("FAB_RUST_CLIENT_ID", "77777777-7777-7777-7777-777777777777")
        .env("FAB_RUST_CLIENT_SECRET", "synthetic-offline-secret")
        .env_remove("AZURE_TENANT_ID")
        .env_remove("AZURE_CLIENT_ID")
        .env_remove("FAB_RUST_FABRIC_HOST")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    let error: Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["error"]["code"], "NotebookIdentityMismatch");
    assert!(!output.exists());
    assert!(!String::from_utf8_lossy(&result.stderr).contains("synthetic-offline-secret"));
}
