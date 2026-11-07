// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public process boundaries remain offline with malformed headless configuration.
#![cfg(feature = "headless")]
#![deny(unnameable_test_items)]

use std::process::{Command, Output};

#[path = "support/unconfigured_profile.rs"]
mod unconfigured_profile;

fn invoke(args: &[&str], environment: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command
        .env_clear()
        .envs(environment.iter().copied())
        .args(args);
    command.output().unwrap()
}

#[test]
fn help_version_and_parser_errors_precede_headless_configuration() {
    for args in [vec!["--help"], vec!["--version"], vec!["ls", "--help"]] {
        let result = invoke(&args, &[("FAB_RUST_AUTH_MODE", "sensitive-invalid-mode")]);
        assert!(result.status.success());
        assert!(result.stderr.is_empty());
        assert!(!String::from_utf8_lossy(&result.stdout).contains("sensitive-invalid-mode"));
    }
    let result = invoke(
        &["ls", "--invalid"],
        &[("FAB_RUST_CLIENT_SECRET", "synthetic-secret")],
    );
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("invalid_arguments"));
    assert!(!String::from_utf8_lossy(&result.stderr).contains("synthetic-secret"));
}

#[test]
fn remote_commands_require_explicit_valid_mode_and_never_echo_values() {
    for inputs in [
        vec![("FAB_RUST_AUTH_MODE", "sensitive-invalid-mode")],
        vec![
            ("FAB_RUST_AUTH_MODE", "client-secret"),
            ("FAB_RUST_TENANT", "sensitive-invalid-tenant"),
        ],
        vec![("FAB_RUST_AUTH_MODE", "managed-identity-vm")],
        vec![
            ("FAB_RUST_AUTH_MODE", "azure-pipelines"),
            ("FAB_RUST_TENANT", "11111111-1111-1111-1111-111111111111"),
            ("FAB_RUST_CLIENT_ID", "22222222-2222-2222-2222-222222222222"),
            ("SYSTEM_ACCESSTOKEN", "sensitive-system-token"),
            (
                "FAB_RUST_PIPELINES_SERVICE_CONNECTION_ID",
                "sensitive-connection",
            ),
        ],
    ] {
        let result = invoke(&["ls", "/"], &inputs);
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        let text = String::from_utf8_lossy(&result.stderr);
        assert!(text.contains("invalid_configuration"));
        assert!(!text.contains("sensitive-"));
    }
    let result = invoke(
        &["ls", "/"],
        &[("FAB_RUST_CLIENT_SECRET", "synthetic-secret")],
    );
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result.stderr).unwrap()["error"]["code"],
        unconfigured_profile::error_code()
    );
}

#[test]
fn pipeline_system_token_alone_never_selects_authentication() {
    let result = invoke(
        &["ls", "/"],
        &[("SYSTEM_ACCESSTOKEN", "sensitive-system-token")],
    );
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    let error = String::from_utf8_lossy(&result.stderr);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&error).unwrap()["error"]["code"],
        unconfigured_profile::error_code()
    );
    assert!(!error.contains("sensitive-system-token"));
}

#[test]
fn certificate_presence_alone_never_selects_authentication() {
    let result = invoke(
        &["ls", "/"],
        &[
            ("FAB_RUST_CERTIFICATE_STORE", "sensitive-store"),
            ("FAB_RUST_CERTIFICATE_SHA256", "sensitive-fingerprint"),
        ],
    );
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    let error = String::from_utf8_lossy(&result.stderr);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&error).unwrap()["error"]["code"],
        unconfigured_profile::error_code()
    );
    assert!(!error.contains("sensitive-"));
}

#[cfg(all(windows, feature = "certificate-store-windows"))]
#[test]
fn malformed_certificate_configuration_and_help_never_open_a_store() {
    let inputs = [
        ("FAB_RUST_AUTH_MODE", "certificate-store-windows"),
        ("FAB_RUST_TENANT", "11111111-1111-1111-1111-111111111111"),
        ("FAB_RUST_CLIENT_ID", "22222222-2222-2222-2222-222222222222"),
        ("FAB_RUST_CERTIFICATE_STORE", "current-user-my"),
        ("FAB_RUST_CERTIFICATE_SHA256", "sensitive-fingerprint"),
        ("FAB_RUST_CERTIFICATE_PROVIDER", "microsoft-software"),
    ];
    let result = invoke(&["ls", "/"], &inputs);
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(error.contains("invalid_configuration"));
    assert!(!error.contains("sensitive-"));
    for args in [vec!["--help"], vec!["--version"], vec!["ls", "--help"]] {
        let result = invoke(&args, &inputs);
        assert!(result.status.success());
        assert!(result.stderr.is_empty());
        assert!(!String::from_utf8_lossy(&result.stdout).contains("sensitive-"));
    }
}
