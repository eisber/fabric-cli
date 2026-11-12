// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native binary admission and process I/O boundaries; no credentials or service.
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::{
    io::{self, Write},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact.DataPipeline";

fn binary(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
    command
        .args(args)
        .env_remove("FAB_RUST_AUTH_MODE")
        .env("FAB_RUST_FABRIC_HOST", "invalid/no-service")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn finish(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "native process exceeded offline test deadline: {:?}",
                output.status
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn error(output: &Output, code: &str, exit: i32) {
    assert_eq!(output.status.code(), Some(exit));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], code, "{value}");
}

#[test]
fn actual_native_root_and_selected_help_advertise_only_metadata_creation() {
    let output = finish(binary(&["--help"]).spawn().unwrap());
    assert!(output.status.success());
    let root = String::from_utf8(output.stdout).unwrap();
    assert!(root.contains("empty item metadata only"));
    assert!(root.contains("not a configured runnable pipeline"));
    let output = finish(binary(&["create", "--help"]).spawn().unwrap());
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("Selected native owner: DataPipeline."));
    assert!(help.contains("/<workspaceGUID>.Workspace/<literal-name>.DataPipeline"));
    assert!(help.contains("Enabled policies: error and skip."));
    assert!(help.contains("--force never bypasses it"));
    assert!(!help.contains("Notebook"));
    assert!(!help.contains("Lakehouse"));
}

#[test]
fn native_empty_item_stops_at_terminal_approval_before_any_identity_setup() {
    let output = finish(binary(&["create", TARGET]).spawn().unwrap());
    error(&output, "ApprovalRequired", 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("no credential or provider access"));
}

#[test]
fn native_unsupported_policy_and_wrong_target_never_read_blocked_stdin() {
    for args in [
        vec!["create", TARGET, "--input", "-", "--force"],
        vec!["create", TARGET, "--input", "-", "--if-exists", "duplicate"],
        vec![
            "create",
            "/Name.Workspace/Exact.DataPipeline",
            "--input",
            "-",
        ],
        vec![
            "create",
            "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Notebook",
            "--input",
            "-",
        ],
    ] {
        let mut command = binary(&args);
        command.stdin(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let _keep_stdin_open = child.stdin.take().unwrap();
        let output = finish(child);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let value: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(value["evidence"]["mutationMayHaveOccurred"], false);
    }
}

#[test]
fn native_input_schema_rejects_definition_run_duplicate_keys_and_invalid_utf8() {
    for input in [
        br#"{"displayName":"Exact","definition":{}}"#.as_slice(),
        br#"{"displayName":"Exact","run":true}"#,
        br#"{"displayName":"Exact","jobType":"Pipeline"}"#,
        br#"{"displayName":"Other"}"#,
        br#"{"displayName":"Exact","displayName":"Exact"}"#,
        &[0xff],
        b"",
    ] {
        let mut command = binary(&["create", TARGET, "--input", "-"]);
        command.stdin(Stdio::piped());
        let mut child = command.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = finish(child);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let value: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(value["evidence"]["mutationMayHaveOccurred"], false);
    }
}

#[test]
fn native_default_file_and_stdin_inputs_have_distinct_truthful_admission() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("input.json");
    std::fs::write(
        &path,
        br#"{"displayName":"Exact","description":"empty metadata"}"#,
    )
    .unwrap();
    error(
        &finish(
            binary(&["create", TARGET, "--input", path.to_str().unwrap()])
                .spawn()
                .unwrap(),
        ),
        "ApprovalRequired",
        1,
    );
    let mut command = binary(&["create", TARGET, "--input", "-"]);
    command.stdin(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"displayName":"Exact"}"#)
        .unwrap();
    error(&finish(child), "ApprovalRequired", 1);
    let output = finish(
        binary(&["create", TARGET, "--input", "does-not-exist.json"])
            .spawn()
            .unwrap(),
    );
    error(&output, "CreateFailed", 1);
}

#[test]
fn native_total_deadline_bounds_blocked_stdin_and_does_not_hang_on_shutdown() {
    let mut command = binary(&["create", TARGET, "--input", "-", "--timeout", "20ms"]);
    command.stdin(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let _keep_stdin_open = child.stdin.take().unwrap();
    error(&finish(child), "TimedOut", 1);
}

#[test]
fn closed_reader_before_spawn_makes_help_delivery_fail_portably() {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let mut command = binary(&["create", "--help"]);
    command.stdout(Stdio::from(writer));
    let output = finish(command.spawn().unwrap());
    error(&output, "OutputFailed", 1);
}

#[test]
fn closed_error_reader_before_spawn_is_a_nonzero_exit_not_silent_success() {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let mut command = binary(&["create", TARGET]);
    command.stderr(Stdio::from(writer));
    let output = finish(command.spawn().unwrap());
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[test]
fn native_no_clobber_output_rejects_locally_without_creating_or_modifying_files() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("receipt.json");
    let args = ["create", TARGET, "--output", path.to_str().unwrap()];
    error(
        &finish(binary(&args).spawn().unwrap()),
        "ApprovalRequired",
        1,
    );
    assert!(!path.exists());
    std::fs::write(&path, b"existing").unwrap();
    error(&finish(binary(&args).spawn().unwrap()), "CreateFailed", 1);
    assert_eq!(std::fs::read(path).unwrap(), b"existing");
}

#[test]
fn v1_create_and_mkdir_aliases_remain_on_the_original_handler() {
    for leaf in ["create", "mkdir"] {
        let output = finish(
            Command::new(env!("CARGO_BIN_EXE_fab-rs"))
                .args([leaf, "--help"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(!help.contains("Selected native owner: DataPipeline."));
        assert!(!help.contains("empty item metadata only"));
        assert!(output.stderr.is_empty());
    }
}
