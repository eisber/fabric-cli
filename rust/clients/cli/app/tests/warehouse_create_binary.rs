// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual optional executable boundaries, without ambient credentials or network.
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::{
    io::{self, Read, Write},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Warehouse";

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
    for key in [
        "FAB_RUST_AUTH_MODE",
        "FAB_RUST_CLIENT_SECRET",
        "FAB_RUST_ASSERTION_FILE",
        "FAB_RUST_FABRIC_HOST",
        "FAB_RUST_ACCOUNT",
        "FAB_RUST_TENANT",
    ] {
        command.env_remove(key);
    }
    command.stdin(Stdio::null());
    command
}

fn run(args: &[&str]) -> Output {
    command().args(args).output().unwrap()
}

fn assert_error(output: Output, exit: i32, code: &str) {
    assert_eq!(output.status.code(), Some(exit));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], code);
}

#[test]
fn selected_help_and_root_discovery_name_only_the_warehouse_create_scope() {
    for args in [vec!["--help"], vec!["create", "--help"]] {
        let output = run(&args);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("Warehouse"));
        assert!(!help.contains("Selected native owner: Notebook"));
    }
    let output = run(&["create", "--help"]);
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("Enabled policies: error and skip."));
    assert!(help.contains("--force never bypasses it."));
}

#[test]
fn unsupported_policies_fail_before_missing_file_or_unclosed_stdin_and_credentials() {
    for options in [
        vec!["--force"],
        vec!["--if-exists", "overwrite"],
        vec!["--if-exists", "duplicate"],
    ] {
        for input in ["-", "definitely-missing-warehouse-input.json"] {
            let mut child = command()
                .args(["create", TARGET, "--input", input])
                .args(&options)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let started = Instant::now();
            while child.try_wait().unwrap().is_none() {
                if started.elapsed() > Duration::from_secs(5) {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("unsupported policy blocked on input");
                }
                thread::sleep(Duration::from_millis(10));
            }
            assert_error(child.wait_with_output().unwrap(), 2, "CreatePolicyRejected");
        }
    }
}

#[test]
fn invalid_target_and_schema_reject_before_approval_or_authentication() {
    assert_error(
        run(&["create", "/not-guid.Workspace/Exact.Warehouse"]),
        2,
        "InvalidInput",
    );
    assert_error(
        run(&["create", TARGET, "--collation", "anything"]),
        2,
        "InvalidArguments",
    );
    let mut child = command()
        .args(["create", TARGET, "--input", "-", "--if-exists", "skip"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"creationPayload":{"collationType":"arbitrary"}}"#)
        .unwrap();
    assert_error(child.wait_with_output().unwrap(), 2, "CreatePolicyRejected");
}

#[test]
fn nonterminal_approval_is_not_a_force_or_ambient_identity_fallback() {
    assert_error(run(&["create", TARGET]), 1, "ApprovalRequired");
}

#[test]
fn original_timeout_includes_blocked_local_stdin() {
    let mut child = command()
        .args(["create", TARGET, "--input", "-", "--timeout", "10ms"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let held = child.stdin.take().unwrap();
    let started = Instant::now();
    let output = child.wait_with_output().unwrap();
    drop(held);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_error(output, 1, "TimedOut");
}

#[test]
fn closed_stdout_and_stderr_are_real_portable_closed_pipes() {
    for stderr in [false, true] {
        let (reader, writer) = io::pipe().unwrap();
        drop(reader);
        let mut command = command();
        let output = if stderr {
            command
                .args(["create", TARGET])
                .stderr(writer)
                .output()
                .unwrap()
        } else {
            command
                .args(["create", "--help"])
                .stdout(writer)
                .output()
                .unwrap()
        };
        assert_eq!(output.status.code(), Some(1));
        if !stderr {
            let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(diagnostic["error"]["code"], "OutputFailed");
        }
    }
}

#[test]
fn existing_output_is_unchanged_before_identity_or_provider_setup() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("existing.json");
    std::fs::write(&path, b"preserve").unwrap();
    let output = run(&["create", TARGET, "--output", path.to_str().unwrap()]);
    assert_error(output, 1, "CreateFailed");
    let mut contents = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .read_to_end(&mut contents)
        .unwrap();
    assert_eq!(contents, b"preserve");
}
