// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native job-read subprocess contracts without inherited credentials or live I/O.

use std::process::{Command, Output};

fn invoke(args: &[&str], selectors: bool) -> Output {
    let mut binary = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    binary.env_clear();
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            binary.env(key, value);
        }
    }
    if selectors {
        binary
            .env("FAB_RUST_TENANT", "invalid-tenant")
            .env("FAB_RUST_ACCOUNT", "must-not-be-echoed")
            .env("FAB_RUST_CLIENT_ID", "invalid-client");
    }
    binary.args(args).output().unwrap()
}

#[test]
fn job_reads_never_discover_credentials_or_fabricate_success() {
    for args in [
        vec!["job", "run-list", "Secret.Workspace/Book.Notebook"],
        vec![
            "job",
            "run-status",
            "Secret.Workspace/Book.Notebook",
            "--id=run",
        ],
    ] {
        let output = invoke(&args, false);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "NotConfigured");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Secret"));
    }
}

#[test]
fn malformed_job_arguments_precede_host_selection() {
    for args in [
        vec!["job", "run-status", "Book.Notebook"],
        vec![
            "job",
            "run-list",
            "Book.Notebook",
            "--token=must-not-be-echoed",
        ],
        vec!["job", "run-list", "Book.Notebook", "--output_format=xml"],
        vec![
            "job",
            "run-cancel",
            "Book.Notebook",
            "--id=must-not-be-echoed",
        ],
    ] {
        let output = invoke(&args, true);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_arguments");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("must-not-be-echoed"));
    }
}

#[cfg(feature = "native-read")]
#[test]
fn native_job_selection_preserves_read_failure_order_without_credentials() {
    let output = invoke(&["job", "run-list", "Book.Notebook"], true);
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(
        error["error"]["code"],
        if cfg!(windows) {
            "invalid_configuration"
        } else {
            "unsupported"
        }
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("must-not-be-echoed"));
}

#[cfg(all(feature = "native-read", windows))]
#[test]
fn job_context_and_invalid_skill_fail_before_native_access() {
    for (context, home, skill, code) in [
        ("relative", "Demo.Workspace", false, "invalid_configuration"),
        ("/", "not-a-workspace", false, "invalid_configuration"),
        ("/", "Demo.Workspace", true, "invalid_arguments"),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
        command
            .env_clear()
            .env("FAB_RUST_TENANT", "11111111-2222-3333-4444-555555555555")
            .env("FAB_RUST_ACCOUNT", "synthetic@example.test")
            .env("FAB_RUST_CLIENT_ID", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
            .env("FAB_RUST_CONTEXT", context)
            .env("FAB_RUST_HOME", home)
            .args(["job", "run-list", "Book.Notebook"]);
        if skill {
            command.arg("--skill=bad\r\nheader");
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], code);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic"));
    }
}
