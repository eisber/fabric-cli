// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Combined native binding selection, without identity or service operations.
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::process::{Command, Output, Stdio};

const ROOT: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact";
const EVENTHOUSE: &[u8] = br#"{"displayName":"Exact","description":"metadata only"}"#;
const KQL: &[u8] = br#"{"displayName":"Exact","creationPayload":{"databaseType":"ReadWrite","parentEventhouseItemId":"22222222-2222-2222-2222-222222222222"}}"#;

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .env("FAB_RUST_AUTH_MODE", "forbidden-no-identity")
        .env("FAB_RUST_FABRIC_HOST", "forbidden.invalid")
        .stdin(Stdio::null())
        .args(args)
        .output()
        .unwrap()
}

fn error(output: &Output, exit: i32, code: &str) {
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], code, "{value}");
    if code == "ApprovalRequired" {
        assert!(value["evidence"].is_null());
    } else {
        assert_eq!(value["evidence"]["mutationMayHaveOccurred"], false);
    }
}

#[test]
fn combined_help_retains_each_owner_and_eventhouse_effect_disclosures() {
    for args in [vec!["--help"], vec!["create", "--help"]] {
        let output = run(&args);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        for owner in ["Report", "KQLDatabase", "Eventhouse"] {
            assert!(text.contains(owner), "{text}");
            if args[0] == "create" {
                assert!(text.contains(&format!("Selected native owner: {owner}.")));
            }
        }
        if args[0] == "--help" {
            assert!(text.contains("Metadata-only Eventhouse"));
            assert!(text.contains("same-name default KQL database"));
            assert!(text.contains("capacity or storage consumption"));
            #[cfg(feature = "runtime-v2")]
            assert!(text.contains("Livy"));
        }
        assert!(!text.contains("Selected native owner: Notebook"));
    }
}

#[test]
fn combined_dispatch_validates_each_schema_before_approval_or_identity() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.json");
    for (owner, bytes, exit, code) in [
        ("Eventhouse", EVENTHOUSE, 1, "ApprovalRequired"),
        ("KQLDatabase", KQL, 1, "ApprovalRequired"),
        ("Eventhouse", KQL, 2, "CreatePolicyRejected"),
        ("KQLDatabase", EVENTHOUSE, 2, "CreatePolicyRejected"),
        ("Report", EVENTHOUSE, 2, "CreatePolicyRejected"),
        ("Report", KQL, 2, "CreatePolicyRejected"),
    ] {
        std::fs::write(&input, bytes).unwrap();
        error(
            &run(&[
                "create",
                &format!("{ROOT}.{owner}"),
                "--input",
                input.to_str().unwrap(),
            ]),
            exit,
            code,
        );
    }
}

#[test]
fn combined_dispatch_rejects_nonmetadata_or_unknown_targets_before_input_io() {
    for destination in [
        format!("{ROOT}.eventhouse"),
        format!("{ROOT}.Notebook"),
        format!("{ROOT}.Eventhouse/src/definition"),
        format!("{ROOT}.KQLDatabase/src/definition"),
    ] {
        error(
            &run(&["create", &destination, "--input", "nonexistent.json"]),
            2,
            "InvalidInput",
        );
    }
    for owner in ["Eventhouse", "KQLDatabase", "Report"] {
        error(
            &run(&[
                "create",
                &format!("{ROOT}.{owner}"),
                "--input",
                "nonexistent.json",
                "--force",
            ]),
            2,
            "CreatePolicyRejected",
        );
    }
}
