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
        assert!(stdout.contains("No authentication, service, or deployment commands"));
        assert!(!stdout.contains("Commands:"));
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
                    "message": "Only --help and --version are available in this scaffold."
                }
            })
        );
    }
}
