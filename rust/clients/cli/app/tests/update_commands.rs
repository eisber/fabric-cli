// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline outer-command contracts for the optional release updater.

#![cfg(feature = "self-update")]
#![deny(unnameable_test_items)]

use std::process::{Command, Output};

fn invoke(arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command.env_clear();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("CI", "1")
        .env("FAB_RUST_UPDATE_POLICY", "install")
        .env("FAB_RUST_UPDATE_STATE_ROOT", "private-relative-state")
        .args(arguments)
        .output()
        .expect("the update-enabled binary must start without credentials")
}

#[test]
fn updater_help_and_version_never_initialize_update_or_authentication() {
    for arguments in [
        vec!["update", "--help"],
        vec!["--help"],
        vec!["--version"],
        vec!["version"],
    ] {
        let output = invoke(&arguments);
        assert_eq!(output.status.code(), Some(0), "{arguments:?}: {output:?}");
        assert!(output.stderr.is_empty(), "{arguments:?}: {output:?}");
        assert!(!output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("private-relative-state"));
    }
}

#[test]
fn invalid_update_arguments_fail_before_publisher_or_installation_setup() {
    for arguments in [
        vec!["update", "--check", "--apply"],
        vec!["update", "--tag", "https://private-marker.example/release"],
        vec!["update", "--tag", "v1.7.0"],
        vec!["update", "--output_format", "json"],
        vec!["update", "--key", "caller-public-key"],
        vec!["update", "--helper", "caller-program.exe"],
        vec!["update", "--repository", "caller/publisher"],
    ] {
        let output = invoke(&arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}: {output:?}");
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_arguments");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-marker"));
    }
}

#[test]
fn nested_update_stops_the_batch_without_installing_or_running_the_next_command() {
    let output = invoke(&["-c", "update --apply", "-c", "version"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "update_context");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("separate invocation")
    );
}
