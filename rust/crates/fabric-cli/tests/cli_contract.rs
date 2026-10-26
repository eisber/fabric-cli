// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Subprocess contracts without inherited credentials or developer tools.

use std::process::{Command, Output};

fn invoke(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command.env_clear();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .expect("the scaffold binary must start without external tools")
}

#[test]
fn help_succeeds_on_stdout_only() {
    for flag in ["--help", "-h"] {
        let output = invoke(&[flag]);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("Usage: fab-rs"));
        assert!(stdout.contains("--help"));
        assert!(stdout.contains("--version"));
        if cfg!(feature = "windows-broker") {
            assert!(stdout.contains("Windows broker authentication is opt-in"));
            assert!(stdout.contains("auth"));
        } else {
            assert!(stdout.contains("no production authentication or HTTP transport"));
        }
        assert!(stdout.contains("Commands:"));
        assert!(stdout.contains("ls"));
        assert!(stdout.contains("get"));
    }
}

#[test]
fn version_has_an_exact_stdout_contract() {
    for flag in ["--version", "-V"] {
        let output = invoke(&[flag]);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("fab-rs {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn invalid_commands_fail_with_sanitized_json_on_stderr() {
    for args in [
        vec![],
        vec!["unknown-command"],
        vec!["auth", "login"],
        vec!["deploy"],
        vec!["--not-a-flag"],
        vec!["--client-secret", "must-not-be-echoed"],
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!stderr.contains("must-not-be-echoed"));
        assert!(stderr.ends_with('\n'));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stderr).unwrap(),
            serde_json::json!({
                "error": {
                    "code": "invalid_arguments",
                    "message": fabric_cli::parser::INVALID_ARGUMENTS
                }
            })
        );
    }
}

#[cfg(feature = "windows-broker")]
#[test]
fn auth_help_and_invalid_selectors_never_start_a_login() {
    for action in ["login", "status", "logout"] {
        let help = invoke(&["auth", action, "--help"]);
        assert!(help.status.success());
        let stdout = String::from_utf8(help.stdout).unwrap();
        assert!(stdout.contains("--tenant"));
        assert!(stdout.contains("--account"));
        assert!(stdout.contains("--client-id"));
        let output = invoke(&[
            "auth",
            action,
            "--tenant",
            "not-a-guid",
            "--account",
            "must-not-be-echoed",
        ]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!stderr.contains("must-not-be-echoed"));
        let data: serde_json::Value = serde_json::from_str(&stderr).unwrap();
        assert_eq!(
            data["error"]["code"],
            if cfg!(windows) {
                "invalid_configuration"
            } else {
                "unsupported"
            }
        );
    }
}

#[cfg(feature = "windows-broker")]
#[test]
fn authentication_rejects_raw_tokens_and_invalid_timeouts_before_native_calls() {
    for extra in [
        vec!["--token", "must-not-be-echoed"],
        vec!["--password", "must-not-be-echoed"],
        vec!["--timeout-seconds", "0"],
        vec!["--timeout-seconds", "601"],
    ] {
        let mut args = vec![
            "auth",
            "login",
            "--tenant",
            "11111111-2222-3333-4444-555555555555",
            "--account",
            "user@example.test",
        ];
        args.extend(extra);
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(
            !String::from_utf8(output.stderr)
                .unwrap()
                .contains("must-not-be-echoed")
        );
    }
}

#[test]
fn reads_are_recognized_but_never_discover_credentials_or_fake_success() {
    for args in [
        vec!["ls"],
        vec!["dir", "/", "-la"],
        vec!["ls", "Secret.Workspace", "--output_format", "json"],
        vec!["get", "Secret.Workspace/Secret.Notebook", "-q", ".", "-f"],
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap(),
            serde_json::json!({"error":{
                "code":"NotConfigured",
                "message": fabric_cli::HostError::NotConfigured.to_string(),
            }})
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Secret"));
    }
}

#[test]
fn read_help_and_invalid_read_options_remain_distinct() {
    for command in ["ls", "dir", "get"] {
        let output = invoke(&[command, "--help"]);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(String::from_utf8_lossy(&output.stdout).contains("--query"));
    }

    for args in [
        vec!["get"],
        vec!["ls", "--query"],
        vec!["get", ".", "--all"],
        vec!["ls", "--output_format", "xml"],
        vec!["get", ".", "--client-secret", "must-not-be-echoed"],
        vec!["ls", "must-not-be-echoed", "-q", "name", "extra"],
        vec!["get", ".", "-q", "-must-not-be-echoed"],
        vec!["get", ".", "--query=must-not-be-echoed", "extra"],
        vec!["ls", "--query", "--must-not-be-echoed"],
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("must-not-be-echoed"));
    }
}

#[cfg(feature = "native-read")]
#[test]
fn native_reads_have_a_real_host_but_invalid_selection_never_accesses_credentials() {
    let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .env_clear()
        .env("FAB_RUST_TENANT", "invalid-tenant")
        .env("FAB_RUST_ACCOUNT", "must-not-be-echoed")
        .env("FAB_RUST_CLIENT_ID", "invalid-client")
        .args(["ls", "/"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(!text.contains("must-not-be-echoed"));
    let error: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        error["error"]["code"],
        if cfg!(windows) {
            "invalid_configuration"
        } else {
            "unsupported"
        }
    );
    assert_ne!(error["error"]["code"], "NotConfigured");
}

#[cfg(all(feature = "native-read", windows))]
#[test]
fn invalid_context_configuration_fails_before_native_access() {
    for (name, value) in [
        ("FAB_RUST_CONTEXT", "relative-path"),
        ("FAB_RUST_HOME", ""),
        ("FAB_RUST_HOME", "not-a-workspace"),
        ("FAB_RUST_CLIENT_ID", ""),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
            .env_clear()
            .env("FAB_RUST_TENANT", "11111111-2222-3333-4444-555555555555")
            .env("FAB_RUST_ACCOUNT", "synthetic@example.test")
            .env("FAB_RUST_CLIENT_ID", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
            .env(name, value)
            .args(["ls", "/"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid_configuration");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic"));
    }
}
