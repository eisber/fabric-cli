// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Credential-free executable admission and portable closed-pipe contracts.
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::{
    io::{self, Write},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact.SemanticModel";

fn process(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
    command.env_clear();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("FAB_RUST_AUTH_MODE", "private-invalid-mode")
        .env("FAB_RUST_CONFIG_ROOT", "private-invalid-root")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn finish(mut child: Child) -> Output {
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= end {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("owned create child exceeded its offline deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn invoke(args: &[&str], input: Option<&[u8]>) -> Output {
    let mut child = process(args).spawn().unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    } else {
        drop(child.stdin.take());
    }
    finish(child)
}

fn error(output: &Output, code: &str) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], code);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-invalid"));
}

#[test]
fn selected_help_and_v1_behavior_are_local_and_truthful() {
    let help = invoke(&["create", "--help"], None);
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("Selected native owner: SemanticModel."));
    assert!(help.contains("/<workspaceGUID>.Workspace/<literal-name>.SemanticModel"));
    assert!(help.contains("Enabled policies: error and skip."));
    assert!(!help.contains("Selected native owner: Notebook"));
    let v1 = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(v1.status.success());
    assert!(
        !String::from_utf8(v1.stdout)
            .unwrap()
            .contains("blank SemanticModel")
    );
}

#[test]
fn malformed_or_unrelated_input_fails_without_ambient_identity_or_fallback() {
    error(
        &invoke(
            &["create", TARGET, "--input", "-"],
            Some(br#"{"displayName":"Exact","definition":{}}"#),
        ),
        "CreatePolicyRejected",
    );
    error(
        &invoke(
            &["create", TARGET, "--input", "-"],
            Some(br#"{"displayName":"Exact","connectionId":"private-invalid"}"#),
        ),
        "CreatePolicyRejected",
    );
    error(
        &invoke(&["create", "/Name.Workspace/Exact.SemanticModel"], None),
        "InvalidInput",
    );
    error(
        &invoke(
            &[
                "create",
                "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Report",
            ],
            None,
        ),
        "InvalidInput",
    );
    error(
        &invoke(
            &[
                "create",
                TARGET,
                "--force",
                "--input",
                "private-invalid-missing",
            ],
            None,
        ),
        "CreatePolicyRejected",
    );
    error(
        &invoke(
            &[
                "create",
                TARGET,
                "--if-exists",
                "duplicate",
                "--input",
                "private-invalid-missing",
            ],
            None,
        ),
        "CreatePolicyRejected",
    );
    error(
        &invoke(&["create", TARGET, "--connection", "private-invalid"], None),
        "InvalidArguments",
    );
    error(&invoke(&["create", TARGET], None), "ApprovalRequired");
}

#[test]
fn blocked_stdin_uses_the_original_deadline_without_provider_access() {
    let mut child = process(&["create", TARGET, "--input", "-", "--timeout", "25ms"])
        .spawn()
        .unwrap();
    let writer = child.stdin.take().unwrap();
    let started = Instant::now();
    let output = finish(child);
    drop(writer);
    error(&output, "TimedOut");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn closed_reader_is_a_real_portable_pipe_not_a_read_only_descriptor() {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let mut command = process(&["create", "--help"]);
    command.stdout(Stdio::from(writer)).stdin(Stdio::null());
    let child = command.spawn().unwrap();
    // Drop Command's retained writer before waiting, on Windows as well as Unix.
    drop(command);
    let output = finish(child);
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], "OutputFailed");
}
