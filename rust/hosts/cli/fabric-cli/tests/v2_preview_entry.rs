// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Opt-in executable boundary; no inherited credential or configuration access.
#![deny(unnameable_test_items)]

use std::process::{Command, Output};

fn invoke(args: &[&str]) -> Output {
    let mut process = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    process.env_clear();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            process.env(name, value);
        }
    }
    process
        .args(args)
        .env("NO_COLOR", "1")
        .env("FAB_RUST_CONFIG_ROOT", "private-invalid-relative")
        .env("FAB_RUST_AUTH_MODE", "private-invalid-mode")
        .output()
        .unwrap()
}

#[test]
fn v1_help_version_and_grammar_do_not_select_the_preview() {
    let help = invoke(&["--help"]);
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(!text.contains("--v2-preview"));
    assert!(!text.contains("artifact ontology"));
    assert_eq!(
        invoke(&["--version"]).stdout,
        format!("fab-rs {}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    );
    let rejected = invoke(&["artifact", "ontology", "get", "/Demo.Workspace/O.Ontology"]);
    assert_eq!(rejected.status.code(), Some(2));
    assert!(rejected.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&rejected.stderr).unwrap()["error"]["code"],
        "invalid_arguments"
    );
}

#[cfg(not(feature = "v2-preview"))]
#[test]
fn default_binary_keeps_preview_marker_outside_its_grammar() {
    let output = invoke(&["--v2-preview", "get", "/Demo.Workspace/O.Ontology"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap()["error"]["code"],
        "invalid_arguments"
    );
}

#[cfg(feature = "v2-preview")]
#[test]
fn preview_help_bypasses_native_configuration_and_is_schema_backed() {
    for args in [
        vec!["--v2-preview", "--help"],
        vec!["--v2-preview", "artifact", "ontology", "--help"],
        vec!["--v2-preview", "artifact", "ontology", "update", "--help"],
        vec!["--v2-preview", "update", "--help"],
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        assert!(String::from_utf8(output.stdout).unwrap().contains("Usage:"));
    }
}

#[cfg(feature = "v2-preview")]
#[test]
fn native_features_are_not_ontology_binding_or_blanket_approval() {
    for args in [
        vec!["--v2-preview", "get", "/Demo.Workspace/O.Ontology"],
        vec![
            "--v2-preview",
            "artifact",
            "ontology",
            "get",
            "/Demo.Workspace/O.Ontology/src/definition",
        ],
        vec![
            "--v2-preview",
            "update",
            "/Demo.Workspace/O.Ontology/src/model/minimized",
            "--input",
            "-",
            "--mode",
            "replace",
        ],
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "NotConfigured");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-invalid"));
    }
}

#[cfg(feature = "v2-preview")]
#[test]
fn preview_invalid_arguments_are_sanitized_and_nonzero() {
    let output = invoke(&[
        "--v2-preview",
        "get",
        "must-not-echo",
        "--private-invalid-option",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("must-not-echo"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-invalid"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap()["error"]["code"],
        "invalid_arguments"
    );
}
