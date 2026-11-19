// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline executable contracts for Dataflow targets of the root query command.
#![deny(unnameable_test_items)]

use std::process::{Command, Output};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Dataflow/queries/Orders";

fn invoke(args: &[&str]) -> Output {
    let mut process = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    process.env_clear().args(args);
    for name in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(name) {
            process.env(name, value);
        }
    }
    // An invalid provider selector is intentional: validation and approval must
    // happen first, without discovering identity or starting remote work.
    process.env("FAB_RUST_AUTH_MODE", "must-not-be-selected");
    process.output().unwrap()
}

#[test]
fn malformed_dataflow_query_has_no_data_or_identity_diagnostic() {
    for args in [
        vec!["query", "/sensitive-target.Dataflow"],
        vec![
            "query",
            TARGET,
            "--text",
            "sensitive-query",
            "--file",
            "m.pq",
        ],
        vec!["query", TARGET, "--force"],
        vec!["query", TARGET, "--wait"],
        vec!["query", TARGET, "--parameters", "-", "--parameters", "-"],
        vec!["execute", TARGET, "--wait"],
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_arguments");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("sensitive-"));
    }
}

#[test]
fn valid_dataflow_query_needs_a_host_or_explicit_interactive_approval() {
    let output = invoke(&["query", TARGET]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    #[cfg(not(feature = "native-host"))]
    assert_eq!(error["error"]["code"], "NotConfigured");
    #[cfg(all(feature = "native-host", any(windows, feature = "headless")))]
    assert_eq!(error["error"]["code"], "approval_required");
    #[cfg(all(feature = "native-host", not(any(windows, feature = "headless"))))]
    assert_eq!(error["error"]["code"], "unsupported");
}

#[test]
fn explicit_discovery_needs_a_host_but_not_execution_approval_or_stdin() {
    let item = TARGET.strip_suffix("/queries/Orders").unwrap();
    let output = invoke(&["get", item, "--list-queries"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    #[cfg(not(feature = "native-host"))]
    assert_eq!(error["error"]["code"], "NotConfigured");
    #[cfg(all(feature = "native-host", any(windows, feature = "headless")))]
    assert_eq!(error["error"]["code"], "invalid_configuration");
    #[cfg(all(feature = "native-host", not(any(windows, feature = "headless"))))]
    assert_eq!(error["error"]["code"], "unsupported");
}

#[test]
fn removed_execute_discovery_and_invalid_get_projection_fail_before_host_setup() {
    let item = TARGET.strip_suffix("/queries/Orders").unwrap();
    for args in [
        vec!["execute", item, "--list-queries"],
        vec!["query", TARGET, "--list-queries"],
        vec!["get", item, "--list-queries", "-q", "["],
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_arguments");
    }
}
#[cfg(all(feature = "native-host", any(windows, feature = "headless")))]
#[test]
fn invalid_input_file_is_rejected_before_provider_selection() {
    for source in ["--parameters", "--file"] {
        let output = invoke(&["query", TARGET, source, "nonexistent-dataflow-input"]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_arguments");
    }
}

#[cfg(all(feature = "native-host", any(windows, feature = "headless")))]
#[test]
fn malformed_input_and_existing_output_fail_before_approval_or_identity() {
    let root = tempfile::Builder::new()
        .prefix("execute-cli-contract-")
        .tempdir()
        .unwrap();
    let input = root.path().join("invalid-input.json");
    let output_path = root.path().join("existing-output.json");
    for value in [
        "",
        "{\"arrowVersion\":3}",
        "{\"unknown\":true}",
        "{\"customMashupDocument\":null}",
    ] {
        std::fs::write(&input, value).unwrap();
        let output = invoke(&["query", TARGET, "--parameters", input.to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_arguments");
    }
    for (parameters, expression) in [
        ("{\"mExpression\":\"1\"}", "2"),
        ("{\"customMashupDocument\":\"section S;\"}", "2"),
        ("{\"arrowVersion\":1}", "1;\nshared Other = 2"),
    ] {
        std::fs::write(&input, parameters).unwrap();
        let output = invoke(&[
            "query",
            TARGET,
            "--parameters",
            input.to_str().unwrap(),
            "--text",
            expression,
        ]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_arguments");
    }
    std::fs::remove_file(input).unwrap();
    std::fs::write(&output_path, b"existing owner").unwrap();
    let output = invoke(&["query", TARGET, "--output", output_path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "output_failed");
    assert_eq!(std::fs::read(&output_path).unwrap(), b"existing owner");
    std::fs::remove_file(output_path).unwrap();
}
