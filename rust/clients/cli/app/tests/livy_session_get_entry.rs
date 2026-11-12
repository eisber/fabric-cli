// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Real process help and preflight evidence; no live identity or HTTP.

#![deny(unnameable_test_items)]

use serde_json::Value;
use std::{
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const PATH: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Lakehouse/sessions/opaque_~.7";

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"));
    command
        .env_clear()
        .env("FAB_RUST_AUTH_MODE", "must-not-be-selected")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    command
}

fn run(args: &[&str]) -> Output {
    let mut child = command().args(args).spawn().unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= end {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("local help/preflight exceeded its five-second test bound");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn exact_path_help_precedes_identity_and_describes_admitted_fabric_binding() {
    let output = run(&["get", PATH, "--help"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("Exact classic Livy session GET"));
    assert!(help.contains("2023-12-01"));
    assert!(help.contains("session IDs are opaque"));
    assert!(help.contains("30-second deadline"));
    assert!(!help.contains("must-not-be-selected"));
}

#[test]
fn sdk_id_and_explicit_owner_validation_precede_identity() {
    for (path, code) in [
        (PATH.replace("opaque_~.7", "bad id"), "InvalidArguments"),
        (
            "/Team.Workspace/Data.Lakehouse/sessions/opaque".into(),
            "ExplicitBindingRequired",
        ),
        (
            "/Team.Workspace/Native.LivyService/sessions/opaque".into(),
            "UnsupportedCapability",
        ),
    ] {
        let output = run(&["get", &path]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], code);
        assert!(
            !String::from_utf8(output.stderr)
                .unwrap()
                .contains("must-not-be-selected")
        );
    }
}

#[test]
fn existing_output_is_not_clobbered_or_followed_by_credential_selection() {
    let temp = tempfile::tempdir().unwrap();
    let output_path = temp.path().join("existing.json");
    std::fs::write(&output_path, "original").unwrap();
    let output = run(&["get", PATH, "-o", output_path.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "RuntimeReadFailed");
    assert_eq!(error["evidence"]["kind"], "output_unavailable");
    assert_eq!(std::fs::read_to_string(output_path).unwrap(), "original");
}

#[test]
fn broken_help_stdout_fails_locally_without_identity_or_synchronous_fallback() {
    let mut child = command().args(["get", PATH, "--help"]).spawn().unwrap();
    drop(child.stdout.take());
    let end = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= end {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("broken owned stdout did not finish within the test bound");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
    assert!(child.wait_with_output().unwrap().stderr.is_empty());
}

#[test]
fn root_help_and_version_remain_local_with_poisoned_identity_configuration() {
    for arg in ["--help", "--version"] {
        let output = run(&[arg]);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(!output.stdout.is_empty());
    }
}
