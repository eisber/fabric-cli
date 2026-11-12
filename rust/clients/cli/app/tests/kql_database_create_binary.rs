// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual optional native head, without Fabric, identity or provisioning access.
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::{
    io::{self, Write},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Database_1.KQLDatabase";
const INPUT: &[u8] = br#"{"displayName":"Database_1","creationPayload":{"databaseType":"ReadWrite","parentEventhouseItemId":"22222222-2222-2222-2222-222222222222"}}"#;

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
    command
        .env_remove("FAB_RUST_AUTH_MODE")
        .env_remove("FAB_RUST_CLIENT_SECRET")
        .env_remove("FAB_RUST_ASSERTION_FILE")
        .env_remove("FAB_RUST_FABRIC_HOST");
    command
}

fn run(args: &[&str]) -> Output {
    command().args(args).output().unwrap()
}

fn code(output: &Output) -> String {
    assert!(output.stdout.is_empty());
    serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"]["code"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn executable_help_is_local_and_names_only_the_selected_native_owner() {
    for args in [vec!["--help"], vec!["create", "--help"]] {
        let output = run(&args);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("KQLDatabase"));
        assert!(!help.contains("Selected native owner: Notebook"));
        if args[0] == "create" {
            assert!(help.contains("Selected native owner: KQLDatabase"));
            assert!(help.contains("Enabled policies: error and skip."));
        }
    }
}

#[test]
fn executable_policy_and_wrong_owner_reject_without_consuming_blocked_stdin() {
    for args in [
        vec!["create", TARGET, "--input", "-", "--force"],
        vec!["create", TARGET, "--input", "-", "--if-exists", "overwrite"],
        vec!["create", TARGET, "--input", "-", "--if-exists", "duplicate"],
        vec![
            "create",
            "/11111111-1111-1111-1111-111111111111.Workspace/Database_1.Eventhouse",
            "--input",
            "-",
        ],
    ] {
        let mut child = command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let blocked_stdin = child.stdin.take().unwrap();
        let started = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if started.elapsed() > Duration::from_secs(5) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("policy/owner admission blocked on unread stdin");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(blocked_stdin);
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(matches!(
            code(&output).as_str(),
            "CreatePolicyRejected" | "InvalidInput"
        ));
    }
}

#[test]
fn executable_file_and_stdin_require_terminal_approval_before_identity() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.json");
    std::fs::write(&input, INPUT).unwrap();
    let output = run(&["create", TARGET, "--input", input.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(code(&output), "ApprovalRequired");
    let mut child = command()
        .args(["create", TARGET, "--input", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(INPUT).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(code(&output), "ApprovalRequired");
}

#[test]
fn executable_rejects_missing_parent_and_preserves_existing_output_locally() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.json");
    std::fs::write(
        &input,
        br#"{"displayName":"Database_1","creationPayload":{"databaseType":"ReadWrite"}}"#,
    )
    .unwrap();
    let output = run(&[
        "create",
        TARGET,
        "--input",
        input.to_str().unwrap(),
        "--if-exists",
        "skip",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(code(&output), "CreatePolicyRejected");
    std::fs::write(&input, INPUT).unwrap();
    let receipt = temp.path().join("receipt.json");
    std::fs::write(&receipt, b"preserve").unwrap();
    let output = run(&[
        "create",
        TARGET,
        "--input",
        input.to_str().unwrap(),
        "--output",
        receipt.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert_ne!(code(&output), "NotConfigured");
    assert_eq!(std::fs::read(&receipt).unwrap(), b"preserve");
}

#[test]
fn executable_original_deadline_bounds_blocked_input_and_approval_is_not_bypassed() {
    let mut child = command()
        .args(["create", TARGET, "--input", "-", "--timeout", "50ms"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let blocked_stdin = child.stdin.take().unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(5) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("original deadline did not bound blocked input");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(blocked_stdin);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(code(&output), "TimedOut");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn executable_help_closed_reader_pipe_is_established_before_spawn() {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let output = command()
        .args(["create", "--help"])
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "OutputFailed");
}
