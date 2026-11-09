// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline executable isolation for the opt-in native stage deployment head.

#![deny(unnameable_test_items)]

use std::process::Command;

fn invoke(args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command.env_clear();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .args(args)
        .env("FAB_RUST_AUTH_MODE", "must-not-echo")
        .env("FAB_RUST_CONFIG_ROOT", "must-not-access")
        .output()
        .unwrap()
}

#[test]
fn v1_help_does_not_advertise_or_select_native_stage_deploy() {
    let output = invoke(&["--help"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("--v2-deploy")
    );
    assert!(invoke(&["--version"]).status.success());
}

#[cfg(not(feature = "v2-deploy"))]
#[test]
fn marker_is_rejected_without_feature() {
    let output = invoke(&["--v2-deploy", "--help"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[cfg(feature = "v2-deploy")]
#[test]
fn feature_help_is_local_and_parsed_deploy_has_no_ambient_host() {
    assert!(
        invoke(&["--v2-deploy", "deploy", "--help"])
            .status
            .success()
    );
    let path = "/.deployment-pipelines/11111111-1111-1111-1111-111111111111/stages/33333333-3333-3333-3333-333333333333";
    let output = invoke(&[
        "--v2-deploy",
        "deploy",
        path,
        "--input",
        "missing.json",
        "--max-input-bytes",
        "4096",
        "--all-items",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap()["error"]["code"],
        "NotConfigured"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("must-not"));
    let invalid = invoke(&["--v2-deploy", "deploy", "must-not-echo", "--private-option"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains("must-not-echo"));
}
