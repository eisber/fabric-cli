// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Deployment selects Python before native configuration, credentials or effects.

#![deny(unnameable_test_items)]

use std::process::{Command, Output};

fn invoke(args: &[&str], interpreter: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command.env_clear();
    for name in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("FAB_RUST_DEPLOY_PYTHON", interpreter)
        .env("FAB_RUST_CONFIG_ROOT", "invalid-native-config-secret")
        .env("FAB_RUST_TENANT", "invalid-native-tenant-secret")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn deployment_start_failure_precedes_native_setup_without_echoing_arguments() {
    let output = invoke(
        &[
            "deploy",
            "--config",
            "config secret with spaces.yml",
            "--params",
            r#"[{"secret":"not echoed"}]"#,
            "--force",
            "--bulk_publish",
        ],
        "nonexistent secret python executable",
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "NotConfigured");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("FAB_RUST_DEPLOY_PYTHON"));
    assert!(!message.contains("secret"));
    assert!(!message.contains("credential"));
}

#[test]
fn interpreter_selector_is_not_a_command_shell_and_is_deployment_only() {
    let output = invoke(&["deploy", "--config", "config.yml", "-f"], "");
    assert_eq!(output.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "invalid_configuration");
    for arguments in [vec!["--version"], vec!["deploy", "--help"], vec!["--help"]] {
        let output = invoke(&arguments, "not-an-executable ; echo secret");
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
    }
    let output = invoke(
        &["deploy", "--config", "config.yml", "-f"],
        "not-an-executable ; echo secret",
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret"));
}

#[test]
fn root_help_declares_the_deliberate_deployment_only_python_contract() {
    let output = invoke(&["--help"], "not-an-executable");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    for declaration in [
        "ms-fabric-cli",
        "FAB_RUST_DEPLOY_PYTHON",
        "deployment-only",
        "file-command",
        "fallbacks",
    ] {
        assert!(help.contains(declaration));
    }
    assert!(!help.contains("bounded native Notebook host"));
    assert!(!help.contains("explicit unpublish.skip"));
}
