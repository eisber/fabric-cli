// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline device-code argument selection and provider-routing contracts.

#![cfg(feature = "interactive")]
#![deny(unnameable_test_items)]

use fabric_cli::parser::{AuthAction, AuthMethod, Invocation, parse_args};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn isolated_root() -> PathBuf {
    let parent = std::env::var_os("FAB_IDENTITY_TEST_ROOT")
        .or_else(|| std::env::var_os("IMPACT_TEST_ROOT"))
        .map(PathBuf::from)
        .expect("Set FAB_IDENTITY_TEST_ROOT or IMPACT_TEST_ROOT to an isolated absolute test directory.");
    assert!(parent.is_absolute());
    let root = parent.join(format!(
        "interactive-arguments-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    assert!(!root.exists());
    root
}

fn process(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command.env_clear().env("FAB_RUST_CONFIG_ROOT", root);
    if let Some(value) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", value);
    }
    command
}

#[test]
fn device_code_method_is_explicit_for_every_auth_action() {
    for (name, action) in [
        ("login", AuthAction::Login),
        ("status", AuthAction::Status),
        ("logout", AuthAction::Logout),
    ] {
        let Invocation::Auth(command) =
            parse_args(["fab-rs", "auth", name, "--method", "device-code"]).unwrap()
        else {
            panic!("auth was not registered");
        };
        assert_eq!(command.method, Some(AuthMethod::DeviceCode));
        assert_eq!(command.action, action);
        assert_eq!(command.allow_interaction, name == "login");
    }
}

#[test]
fn silent_login_does_not_authorize_device_interaction_and_defaults_do_not_select_a_method() {
    let Invocation::Auth(command) =
        parse_args(["fab-rs", "auth", "login", "--no-interactive"]).unwrap()
    else {
        panic!("auth was not registered");
    };
    assert!(!command.allow_interaction);
    assert_eq!(command.method, None);
    assert!(!command.client_id_explicit);
    assert!(parse_args(["fab-rs", "auth", "login", "--method", "automatic"]).is_err());
}

#[test]
fn missing_explicit_device_selectors_and_headless_modes_fail_before_network_or_store_creation() {
    let root = isolated_root();
    for mode in ["device-code", "client-secret"] {
        let output = process(&root)
            .args(["--output_format", "json", "auth", "login"])
            .env("FAB_RUST_AUTH_MODE", mode)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(value["error"]["code"], "invalid_configuration");
        assert!(!root.exists());
    }
}

#[test]
fn explicit_broker_mode_never_uses_device_code_for_status() {
    let root = isolated_root();
    let output = process(&root)
        .args(["--output_format", "json", "auth", "status"])
        .env("FAB_RUST_AUTH_MODE", "broker")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    let expected = if cfg!(all(windows, feature = "windows-broker")) {
        "NotConfigured"
    } else {
        "unsupported"
    };
    assert_eq!(value["error"]["code"], expected);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("user_code"));
    assert!(!root.exists());
}

#[test]
fn argv_device_code_overrides_broker_environment_without_accessing_a_profile() {
    let root = isolated_root();
    let output = process(&root)
        .args([
            "--output_format",
            "json",
            "auth",
            "status",
            "--method",
            "device-code",
        ])
        .env("FAB_RUST_AUTH_MODE", "broker")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], "invalid_configuration");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Device-code")
    );
    assert!(!root.exists());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_device_code_rejects_missing_or_invalid_helper_trust_before_store_or_network() {
    let root = isolated_root();
    for hash in [None, Some("untrusted-secret-canary")] {
        let mut command = process(&root);
        command.args([
            "--output_format",
            "json",
            "auth",
            "login",
            "--method",
            "device-code",
            "--tenant",
            "11111111-1111-1111-1111-111111111111",
            "--client-id",
            "22222222-2222-2222-2222-222222222222",
        ]);
        if let Some(hash) = hash {
            command
                .env("FAB_RUST_PROTECTED_STORE_HELPER", "/not-executed-helper")
                .env("FAB_RUST_PROTECTED_STORE_HELPER_SHA256", hash);
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let value: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(value["error"]["code"], "invalid_configuration");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("canary"));
        assert!(!root.exists());
    }
}
