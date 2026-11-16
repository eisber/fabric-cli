// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual executable help and fail-closed configuration, without live identity or HTTP.

#![deny(unnameable_test_items)]

use serde_json::Value;
use std::process::{Command, Output};

const LAKEHOUSE: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Lakehouse/sessions";
const NATIVE: &str = "/Team.Workspace/Native.LivyService/sessions";

fn invoke(args: &[&str], environment: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .env_clear()
        .envs(environment.iter().copied())
        .args(args)
        .output()
        .unwrap()
}

fn error(output: &Output, exit: i32) -> Value {
    assert_eq!(output.status.code(), Some(exit));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.len() <= 16 * 1024);
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(!text.contains("private-"));
    serde_json::from_slice(&output.stderr).unwrap()
}

#[test]
fn executable_local_help_lists_only_this_runtime_leaf_without_configuration_or_identity() {
    for args in [
        vec!["--help"],
        vec!["list", "--help"],
        vec!["list", LAKEHOUSE, "-h"],
        vec!["list", NATIVE, "--help"],
    ] {
        let output = invoke(
            &args,
            &[
                ("FAB_RUST_AUTH_MODE", "private-invalid"),
                ("FAB_RUST_CLIENT_SECRET", "private-secret"),
                ("FAB_RUST_FABRIC_HOST", "private-invalid"),
            ],
        );
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(!output.stdout.is_empty() && output.stdout.len() <= 16 * 1024);
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("Livy"));
        assert!(!text.contains("private-"));
    }
}

#[test]
fn executable_requires_explicit_fabric_binding_and_does_not_discover_names_or_authentication() {
    let output = invoke(&["list", LAKEHOUSE], &[]);
    assert_eq!(error(&output, 1)["error"]["code"], "NotConfigured");
    let output = invoke(
        &["list", LAKEHOUSE],
        &[("FAB_RUST_FABRIC_HOST", "msitapi.fabric.microsoft.com")],
    );
    assert_eq!(error(&output, 1)["error"]["code"], "NotConfigured");
    let output = invoke(
        &["list", "/Team.Workspace/Named.Lakehouse/sessions"],
        &[
            ("FAB_RUST_FABRIC_HOST", "msitapi.fabric.microsoft.com"),
            ("FAB_RUST_AUTH_MODE", "private-invalid"),
        ],
    );
    assert_eq!(error(&output, 2)["error"]["code"], "invalid_input");
}

#[test]
fn executable_native_service_is_explicitly_unsupported_and_never_uses_fabric_fallback() {
    for environment in [
        vec![],
        vec![
            ("FAB_RUST_FABRIC_HOST", "msitapi.fabric.microsoft.com"),
            ("FAB_RUST_AUTH_MODE", "private-invalid"),
        ],
    ] {
        let output = invoke(&["list", NATIVE], &environment);
        assert_eq!(error(&output, 2)["error"]["code"], "unsupported_capability");
    }
}

#[test]
fn executable_rejects_unsafe_origin_and_unsupported_options_before_identity() {
    for host in [
        "https://msitapi.fabric.microsoft.com/v1",
        "selected.pbidedicated.windows.net",
        "private-attacker.invalid",
        "msitapi.fabric.microsoft.com:443",
    ] {
        let output = invoke(
            &["list", LAKEHOUSE],
            &[
                ("FAB_RUST_FABRIC_HOST", host),
                ("FAB_RUST_AUTH_MODE", "private-invalid"),
            ],
        );
        assert_eq!(error(&output, 2)["error"]["code"], "InvalidConfiguration");
    }
    for option in ["--continuation", "--profile", "--timeout"] {
        let output = invoke(
            &["list", LAKEHOUSE, option, "private-value"],
            &[("FAB_RUST_AUTH_MODE", "private-invalid")],
        );
        assert_eq!(error(&output, 2)["error"]["code"], "unsupported_capability");
    }
    let output = invoke(&["list", LAKEHOUSE, "--limit", "1001"], &[]);
    assert_eq!(error(&output, 1)["error"]["code"], "budget_exceeded");
}

#[test]
fn executable_no_clobber_preflight_precedes_identity_and_preserves_existing_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("existing.json");
    std::fs::write(&path, "original owner").unwrap();
    let output = invoke(
        &["list", LAKEHOUSE, "--output", path.to_str().unwrap()],
        &[
            ("FAB_RUST_FABRIC_HOST", "msitapi.fabric.microsoft.com"),
            ("FAB_RUST_AUTH_MODE", "private-invalid"),
        ],
    );
    assert_eq!(error(&output, 1)["error"]["code"], "output_unavailable");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "original owner");
}

#[test]
fn executable_does_not_swallow_notebook_or_individual_session_routes() {
    for args in [
        vec!["list", "/Team.Workspace/N.Notebook/sessions"],
        vec!["get", NATIVE],
        vec!["list", "/Team.Workspace/Native.LivyService/sessions/0"],
        vec!["artifact", "notebook", "list", NATIVE],
    ] {
        let output = invoke(&args, &[("FAB_RUST_AUTH_MODE", "private-invalid")]);
        let value = error(&output, 2);
        assert_ne!(value["error"]["code"], "NotConfigured");
        assert_ne!(value["error"]["code"], "invalid_configuration");
    }
}
