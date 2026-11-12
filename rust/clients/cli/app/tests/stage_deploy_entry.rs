// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline executable isolation for the opt-in native stage deployment head.

#![deny(unnameable_test_items)]

use std::process::Command;

#[cfg(all(
    feature = "v2-deploy",
    feature = "native-host",
    any(windows, feature = "headless")
))]
#[path = "support/unconfigured_profile.rs"]
mod unconfigured_profile;

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
    assert_eq!(
        output.status.code(),
        Some(
            if cfg!(all(
                feature = "native-host",
                any(windows, feature = "headless")
            )) {
                2
            } else {
                1
            }
        )
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap()["error"]["code"],
        if cfg!(all(
            feature = "native-host",
            any(windows, feature = "headless")
        )) {
            "InputFailure"
        } else {
            "NotConfigured"
        }
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("must-not"));
    let invalid = invoke(&["--v2-deploy", "deploy", "must-not-echo", "--private-option"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains("must-not-echo"));
}

#[cfg(all(
    feature = "v2-deploy",
    feature = "native-host",
    any(windows, feature = "headless")
))]
#[test]
fn native_entry_validates_file_before_identity_and_missing_identity_never_contacts_service() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("native.json");
    let path = "/.deployment-pipelines/11111111-1111-1111-1111-111111111111/stages/33333333-3333-3333-3333-333333333333";
    for wrong_type in [true, false] {
        std::fs::write(&input, format!(
            "{{\"sourceStageId\":\"22222222-2222-2222-2222-222222222222\",\"targetStageId\":\"33333333-3333-3333-3333-333333333333\",\"items\":[{{\"sourceItemId\":\"66666666-6666-6666-6666-666666666666\",\"itemType\":\"{}\"}}]}}",
            if wrong_type { "Lakehouse" } else { "Notebook" }
        )).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
        child.env_clear();
        for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(name) {
                child.env(name, value);
            }
        }
        if wrong_type {
            child.env(
                "FAB_RUST_CONFIG_ROOT",
                directory.path().join("missing-config"),
            );
        }
        let output = child
            .args([
                "--v2-deploy",
                "deploy",
                path,
                "--input",
                input.to_str().unwrap(),
                "--max-input-bytes",
                "4096",
                "--selected-items",
            ])
            .output()
            .unwrap();
        assert!(output.stdout.is_empty());
        assert_eq!(output.status.code(), Some(if wrong_type { 2 } else { 1 }));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap()["error"]["code"],
            if wrong_type {
                "InvalidInput"
            } else {
                unconfigured_profile::error_code()
            }
        );
    }
    assert!(!directory.path().join("missing-config").exists());
}
