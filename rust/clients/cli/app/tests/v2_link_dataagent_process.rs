// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Real executable preflight without ambient identity or live service access.
#![cfg(feature = "headless")]
#![deny(unnameable_test_items)]

use std::{
    io::Write,
    process::{Command, Stdio},
};

const TARGET: &str = "/11111111-1111-4111-8111-111111111111.Workspace/22222222-2222-4222-8222-222222222222.DataAgent/settings/staging/datasources";

fn run(args: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .env_clear()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn leaf_help_and_malformed_input_work_without_identity_or_config() {
    for prefix in [vec!["link"], vec!["artifact", "data-agent", "link"]] {
        let mut args = prefix.clone();
        args.push("--help");
        let help = run(&args, b"");
        assert!(help.status.success());
        assert!(String::from_utf8(help.stdout).unwrap().contains("--input"));
        assert!(help.stderr.is_empty());
        let mut args = prefix;
        args.extend([TARGET, "--input", "-"]);
        let invalid = run(&args, br#"{"type":"FabricItem","type":"FabricItem"}"#);
        assert_eq!(invalid.status.code(), Some(2));
        let error: serde_json::Value = serde_json::from_slice(&invalid.stderr).unwrap();
        assert_eq!(error["error"]["code"], "LinkPreflightFailed");
        assert_eq!(error["evidence"]["mutationMayHaveOccurred"], false);
        assert!(invalid.stdout.is_empty());
    }
}

#[test]
fn root_rejects_foreign_owner_before_opening_input_and_has_no_unattended_bypass() {
    let target = TARGET.replace("DataAgent", "Lakehouse");
    let invalid = run(&["link", &target, "--input", "missing-input-file"], b"");
    let error: serde_json::Value = serde_json::from_slice(&invalid.stderr).unwrap();
    assert_eq!(error["error"]["code"], "InvalidArguments");
    let invalid = run(
        &[
            "artifact",
            "data-agent",
            "link",
            &target,
            "--input",
            "missing-input-file",
        ],
        b"",
    );
    let error: serde_json::Value = serde_json::from_slice(&invalid.stderr).unwrap();
    assert_eq!(error["evidence"]["code"], "InvalidTarget");
    for option in ["--force", "--yes", "--publish"] {
        let output = run(
            &[
                "artifact",
                "data-agent",
                "link",
                TARGET,
                "--input",
                "-",
                option,
            ],
            b"",
        );
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap()["error"]["code"],
            "InvalidArguments"
        );
    }
    let valid = r#"{"type":"LakehouseTables","lakehouseReference":{"referenceType":"ById","workspaceId":"11111111-1111-4111-8111-111111111111","itemId":"33333333-3333-4333-8333-333333333333"}}"#;
    let output = run(&["link", TARGET, "--input", "-"], valid.as_bytes());
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap()["error"]["code"],
        "ApprovalRequired"
    );
}
