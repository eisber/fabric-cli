// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual opt-in executable boundaries without credentials or Fabric access.
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::{
    io::Write,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Example.Notebook";

#[test]
fn opting_into_v2_does_not_change_the_frozen_v1_create_alias() {
    let create = fabric_cli::parser::parse_args(["fab-rs", "create", TARGET]).unwrap();
    let mkdir = fabric_cli::parser::parse_args(["fab-rs", "mkdir", TARGET]).unwrap();
    assert_eq!(format!("{create:?}"), format!("{mkdir:?}"));
    for verb in ["create", "mkdir"] {
        let output = Command::new(env!("CARGO_BIN_EXE_fab-rs"))
            .args([verb, "--help"])
            .env("FAB_RUST_AUTH_MODE", "not-an-auth-mode")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(!help.contains("if-exists"));
        assert!(!help.contains("Selected native owner"));
    }
}

fn process(args: &[&str], input: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(args)
        .env("FAB_RUST_AUTH_MODE", "not-an-auth-mode")
        .env("FAB_RUST_FABRIC_HOST", "not-a-service-host")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    } else {
        drop(child.stdin.take());
    }
    child.wait_with_output().unwrap()
}

fn failure(output: &Output, code: &str, exit: i32) {
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], code);
}

#[test]
fn root_and_create_help_describe_the_actual_bounded_mount() {
    for args in [vec!["--help"], vec!["create", "--help"]] {
        let output = process(&args, None);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("Notebook"));
        assert!(help.contains("error and skip"));
        assert!(help.contains("approval"));
    }
}

#[test]
fn actual_native_process_cannot_use_force_or_nonterminal_input_as_consent() {
    let output = process(&["create", TARGET], None);
    failure(&output, "ApprovalRequired", 1);
    for policy in ["overwrite", "duplicate"] {
        let output = process(&["create", TARGET, "--if-exists", policy], None);
        failure(&output, "CreatePolicyRejected", 2);
    }
    let output = process(&["create", TARGET, "--force"], None);
    failure(&output, "CreatePolicyRejected", 2);
}

#[test]
fn actual_stdin_json_is_validated_before_approval_or_identity_selection() {
    for input in [
        b"".as_slice(),
        b"{",
        b"null",
        br#"{"displayName":"Example","unknown":true}"#,
        br#"{"displayName":"Example","displayName":"Example"}"#,
        br#"{"displayName":"Different"}"#,
        br#"{"displayName":"Example","definition":null}"#,
    ] {
        let output = process(&["create", TARGET, "--input", "-"], Some(input));
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(matches!(
            error["error"]["code"].as_str(),
            Some("InvalidInput" | "CreatePolicyRejected")
        ));
        assert_eq!(error["evidence"]["mutationMayHaveOccurred"], false);
    }
    let output = process(
        &["create", TARGET, "--input", "-"],
        Some(br#"{"displayName":"Example","description":"kept"}"#),
    );
    failure(&output, "ApprovalRequired", 1);
}

#[test]
fn parser_rejects_invented_flags_facades_and_other_owner_types() {
    for args in [
        vec!["create", TARGET, "--name", "different"],
        vec!["create", TARGET, "--force", "--if-exists", "overwrite"],
        vec!["artifact", "Notebook", "create", TARGET],
    ] {
        let output = process(&args, None);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    for path in [
        "/11111111-1111-1111-1111-111111111111.Workspace/Example.Report",
        "/11111111-1111-1111-1111-111111111111.Workspace/Example.Lakehouse",
        "/Name.Workspace/Example.Notebook",
        "/11111111-1111-1111-1111-111111111111.Workspace",
        "/11111111-1111-1111-1111-111111111111.Workspace/Example.Notebook/src",
    ] {
        failure(&process(&["create", path], None), "InvalidInput", 2);
    }
}

#[test]
fn native_output_preflight_is_no_clobber_before_identity_or_approval() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("receipt.json");
    std::fs::write(&path, b"existing").unwrap();
    let output = process(
        &["create", TARGET, "--output", path.to_str().unwrap()],
        None,
    );
    failure(&output, "CreateFailed", 1);
    assert_eq!(std::fs::read(path).unwrap(), b"existing");
}

#[test]
fn original_native_deadline_also_bounds_stalled_stdin_and_runtime_shutdown() {
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(["create", TARGET, "--input", "-", "--timeout", "100ms"])
        .env("FAB_RUST_AUTH_MODE", "not-an-auth-mode")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"{").unwrap();
    let watchdog = started + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= watchdog {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("owned create process exceeded its stdin/deadline watchdog");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    drop(input);
    failure(&output, "TimedOut", 1);
    assert!(started.elapsed() < Duration::from_secs(5));
}
