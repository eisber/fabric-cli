// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual opt-in binary boundaries; no live identity, endpoint or mutation.
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::{
    io::{self, Read, Write},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Eventhouse";

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
    command
        .env("FAB_RUST_AUTH_MODE", "forbidden-no-identity")
        .env("FAB_RUST_FABRIC_HOST", "forbidden.invalid")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn finish(mut child: Child) -> Output {
    let capture = |stream: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(stream) = stream {
                stream.take(128 * 1024).read_to_end(&mut bytes).unwrap();
            }
            bytes
        })
    };
    let stdout = capture(child.stdout.take().map(|stream| Box::new(stream) as _));
    let stderr = capture(child.stderr.take().map(|stream| Box::new(stream) as _));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait().unwrap();
        }
        thread::sleep(Duration::from_millis(10));
    };
    let result = Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    };
    assert!(!timed_out, "local Eventhouse child exceeded its deadline");
    result
}

fn run(args: &[&str], input: Option<&[u8]>) -> Output {
    let mut command = command();
    command.args(args);
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().unwrap();
    drop(command);
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    finish(child)
}

fn error(output: &Output, exit: i32, code: &str) {
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], code, "{error}");
}

#[test]
fn actual_help_selects_eventhouse_and_default_v1_is_unchanged() {
    let result = run(&["create", "--help"], None);
    assert!(result.status.success());
    assert!(result.stderr.is_empty());
    let help = String::from_utf8(result.stdout).unwrap();
    assert!(help.contains("Selected native owner: Eventhouse."));
    assert!(help.contains("/<workspaceGUID>.Workspace/<literal-name>.Eventhouse"));
    assert!(help.contains("Enabled policies: error and skip."));
    assert!(help.contains("--force never bypasses it."));
    let root = run(&["--help"], None);
    assert!(root.status.success());
    assert!(
        String::from_utf8(root.stdout)
            .unwrap()
            .contains("Metadata-only Eventhouse")
    );
    let mut legacy = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    legacy
        .args(["create", "--help"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let result = finish(legacy.spawn().unwrap());
    assert!(result.status.success());
    assert!(
        !String::from_utf8(result.stdout)
            .unwrap()
            .contains("Selected native owner: Eventhouse.")
    );
}

#[test]
fn actual_preflight_validates_input_even_for_skip_and_never_uses_poisoned_identity() {
    for (input, code) in [
        (b"".as_slice(), "InvalidInput"),
        (b"{", "InvalidInput"),
        (
            br#"{"displayName":"Exact","displayName":"Exact"}"#,
            "InvalidInput",
        ),
        (
            br#"{"displayName":"Exact","definition":{}}"#,
            "CreatePolicyRejected",
        ),
        (br#"{"displayName":"Other"}"#, "CreatePolicyRejected"),
        (
            br#"{"displayName":"Exact","creationPayload":{}}"#,
            "CreatePolicyRejected",
        ),
    ] {
        error(
            &run(
                &["create", TARGET, "--input", "-", "--if-exists", "skip"],
                Some(input),
            ),
            2,
            code,
        );
    }
    error(&run(&["create", TARGET], None), 1, "ApprovalRequired");
    error(
        &run(
            &["create", TARGET, "--input", "-"],
            Some(br#"{"displayName":"Exact","description":"supported"}"#),
        ),
        1,
        "ApprovalRequired",
    );
}

#[test]
fn actual_policy_and_target_rejection_precede_input_io_and_authentication() {
    for options in [
        vec!["--force"],
        vec!["--if-exists", "overwrite"],
        vec!["--if-exists", "duplicate"],
    ] {
        let args: Vec<_> = ["create", TARGET, "--input", "nonexistent-eventhouse.json"]
            .into_iter()
            .chain(options)
            .collect();
        error(&run(&args, None), 2, "CreatePolicyRejected");
    }
    for path in [
        "/ambient.Workspace/Exact.Eventhouse",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.KQLDatabase",
        "/11111111-1111-1111-1111-111111111111.Workspace/Exact.Eventhouse/Files/input",
    ] {
        error(
            &run(
                &["create", path, "--input", "nonexistent-eventhouse.json"],
                None,
            ),
            2,
            "InvalidInput",
        );
    }
    for args in [
        vec!["create", TARGET, "--force", "--if-exists", "skip"],
        vec!["create", TARGET, "--timeout", "0s"],
        vec!["create", TARGET, "--timeout", "25h"],
        vec!["create", TARGET, "--timeout", "1.5s"],
    ] {
        error(&run(&args, None), 2, "InvalidArguments");
    }
}

#[test]
fn actual_local_file_and_no_clobber_preflight_need_no_identity() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.json");
    let output = dir.path().join("receipt.json");
    std::fs::write(&input, br#"{"displayName":"Exact"}"#).unwrap();
    error(
        &run(
            &[
                "create",
                TARGET,
                "--input",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ],
            None,
        ),
        1,
        "ApprovalRequired",
    );
    assert!(!output.exists());
    std::fs::write(&output, b"keep").unwrap();
    error(
        &run(
            &[
                "create",
                TARGET,
                "--input",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ],
            None,
        ),
        1,
        "CreateFailed",
    );
    assert_eq!(std::fs::read(output).unwrap(), b"keep");
}

#[test]
fn actual_closed_stdout_and_stderr_pipes_fail_without_hanging_or_identity() {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let mut help = command();
    help.args(["create", "--help"]).stdout(writer);
    let child = help.spawn().unwrap();
    drop(help);
    let result = finish(child);
    error(&result, 1, "OutputFailed");
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let mut invalid = command();
    invalid.args(["create", TARGET, "--force"]).stderr(writer);
    let child = invalid.spawn().unwrap();
    drop(invalid);
    let result = finish(child);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty() && result.stderr.is_empty());
}

#[test]
fn actual_whole_command_deadline_bounds_unfinished_stdin_preflight() {
    let (reader, writer) = io::pipe().unwrap();
    let mut command = command();
    command
        .args(["create", TARGET, "--input", "-", "--timeout", "50ms"])
        .stdin(reader);
    let child = command.spawn().unwrap();
    drop(command);
    let result = finish(child);
    drop(writer);
    error(&result, 1, "TimedOut");
}
