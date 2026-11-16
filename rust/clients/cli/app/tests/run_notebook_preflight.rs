// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Real-process local validation must precede platform and identity selection.
#![cfg(feature = "v2-run")]
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::{
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const TARGET: &str =
    "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook";
const ROOTS: &[&[&str]] = &[&["run"], &["artifact", "notebook", "run"]];

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
}

fn invoke(root: &[&str], args: &[&str]) -> Output {
    let scratch = tempfile::tempdir().unwrap();
    let config = scratch.path().join("must-not-initialize");
    let destination = scratch.path().join("must-not-publish.json");
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command
        .args(root)
        .args(args)
        .arg("--output")
        .arg(&destination)
        .env_clear()
        .env("FAB_RUST_CONFIG_ROOT", &config)
        .env("FAB_RUST_AUTH_MODE", "invalid")
        .env("FAB_RUST_CONTEXT", "invalid")
        .env("FAB_RUST_TENANT", "invalid")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = Process(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    while process.0.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "Run must not wait on stdin or invoke an identity/backend"
        );
        thread::sleep(Duration::from_millis(10));
    }
    drop(process.0.stdin.take());
    let output = Output {
        status: process.0.wait().unwrap(),
        stdout: std::io::read_to_string(process.0.stdout.take().unwrap())
            .unwrap()
            .into_bytes(),
        stderr: std::io::read_to_string(process.0.stderr.take().unwrap())
            .unwrap()
            .into_bytes(),
    };
    assert!(!config.exists());
    assert!(!destination.exists());
    output
}

#[test]
fn invalid_root_and_facade_fail_before_host_selection_and_input_io() {
    for root in ROOTS {
        for (args, code) in [
            (vec!["arbitrary-executable", "--input", "-"], "InvalidInput"),
            (
                vec![TARGET, "--timeout", "0s", "--input", "-"],
                "InvalidInput",
            ),
            (vec![TARGET, "--input", " "], "invalid_arguments"),
        ] {
            let output = invoke(root, &args);
            assert_eq!(output.status.code(), Some(2), "{root:?} {args:?}");
            assert!(output.stdout.is_empty());
            let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(diagnostic["error"]["code"], code, "{root:?} {args:?}");
            let message = String::from_utf8(output.stderr).unwrap();
            assert!(!message.contains("arbitrary-executable"));
            assert!(!message.contains("must-not-"));
        }
    }
}

#[test]
fn valid_root_and_facade_retain_platform_or_explicit_approval_gate() {
    let expected = if !cfg!(feature = "native-host") {
        "NotConfigured"
    } else if cfg!(any(windows, feature = "headless")) {
        "ApprovalRequired"
    } else {
        "UnsupportedCapability"
    };
    for root in ROOTS {
        let output = invoke(root, &[TARGET]);
        assert_eq!(output.status.code(), Some(1), "{root:?}");
        assert!(output.stdout.is_empty());
        let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(diagnostic["error"]["code"], expected, "{root:?}");
    }
}
