// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Credential-free actual binary mount, stream and preflight contracts.

use super::*;
use std::{
    io::Write,
    process::{Child, Command, Output, Stdio},
    thread,
    time::Instant,
};

fn binary(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
    command
        .env_clear()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SYSTEMROOT") {
        command.env("SYSTEMROOT", system_root);
    }
    command
}

fn finish(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(8);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("owned create subprocess exceeded the finite offline fixture deadline");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn actual_binary_root_and_create_help_disclose_the_report_mount_without_credentials() {
    for args in [vec!["--help"], vec!["create", "--help"]] {
        let output = finish(binary(&args).spawn().unwrap());
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("Report"));
        assert!(help.contains("create"));
        assert!(help.contains("approval"));
    }
}

#[test]
fn actual_valid_report_input_cannot_authenticate_or_mutate_without_terminal_approval() {
    let mut child = binary(&["create", PATH, "--input", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&body()).unwrap())
        .unwrap();
    let output = finish(child);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "ApprovalRequired");
}

#[test]
fn actual_wrong_target_invalid_body_policy_and_profile_fail_before_approval_or_identity() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.json");
    std::fs::write(&input, serde_json::to_vec(&body()).unwrap()).unwrap();
    let input = input.to_str().unwrap();
    for args in [
        vec![
            "create",
            "/11111111-1111-1111-1111-111111111111.Workspace/Wrong.Notebook",
            "--input",
            input,
        ],
        vec!["create", PATH, "--input", input, "--force"],
        vec!["create", PATH, "--input", input, "--if-exists", "duplicate"],
        vec!["create", PATH, "--input", input, "--profile", "named"],
    ] {
        let output = finish(binary(&args).spawn().unwrap());
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(matches!(
            error["error"]["code"].as_str(),
            Some("InvalidInput" | "CreatePolicyRejected")
        ));
        assert_eq!(error["evidence"]["mutationMayHaveOccurred"], false);
    }
    std::fs::write(input, br#"{"displayName":"Report","displayName":"Report"}"#).unwrap();
    let output = finish(binary(&["create", PATH, "--input", input]).spawn().unwrap());
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let mut folder_input = body();
    folder_input["folderId"] = json!(OP);
    std::fs::write(input, serde_json::to_vec(&folder_input).unwrap()).unwrap();
    let output = finish(binary(&["create", PATH, "--input", input]).spawn().unwrap());
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "CreatePolicyRejected");
    assert_eq!(error["evidence"]["mutationMayHaveOccurred"], false);
}

#[test]
fn blocked_stdin_exits_under_original_timeout_without_credentials() {
    let mut child = binary(&["create", PATH, "--input", "-", "--timeout", "1ms"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let held_stdin = child.stdin.take().unwrap();
    let output = finish(child);
    drop(held_stdin);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "TimedOut");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no mutation")
    );
}

#[test]
fn blocked_stdout_or_stderr_is_not_success_or_permission_to_dispatch() {
    // Rust stdio ignores EBADF on POSIX; a closed-reader pipe proves a real write failure.
    let (reader, mut writer) = std::io::pipe().unwrap();
    drop(reader);
    assert_eq!(
        writer.write_all(b"probe").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
    let output = finish(
        binary(&["create", "--help"])
            .stdout(writer)
            .spawn()
            .unwrap(),
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "OutputFailed");
    let (reader, mut writer) = std::io::pipe().unwrap();
    drop(reader);
    assert_eq!(
        writer.write_all(b"probe").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
    let output = finish(
        binary(&["create", "--not-a-registered-option"])
            .stderr(writer)
            .spawn()
            .unwrap(),
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}
