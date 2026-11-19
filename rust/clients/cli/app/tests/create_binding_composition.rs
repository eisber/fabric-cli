// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Combined native binding selection, without identity or service operations.
#![deny(unnameable_test_items)]

use serde_json::Value;
use std::process::{Command, Output, Stdio};

const ROOT: &str = "/11111111-1111-1111-1111-111111111111.Workspace/Exact";
const EVENTHOUSE: &[u8] = br#"{"displayName":"Exact","description":"metadata only"}"#;
const KQL: &[u8] = br#"{"displayName":"Exact","creationPayload":{"databaseType":"ReadWrite","parentEventhouseItemId":"22222222-2222-2222-2222-222222222222"}}"#;
const WAREHOUSE: &[u8] = br#"{"displayName":"Exact","creationPayload":{"collationType":"Latin1_General_100_CI_AS_KS_WS_SC_UTF8"}}"#;

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
        for owner in [
            "Report",
            "Notebook",
            "KQLDatabase",
            "Eventhouse",
            "Warehouse",
            #[cfg(feature = "v2-copyjob-create")]
            "CopyJob",
            #[cfg(feature = "v2-create-semantic-model")]
            "SemanticModel",
        ] {
            assert!(text.contains(owner), "{text}");
            if args[0] == "create" {
                assert!(text.contains(&format!("Selected native owner: {owner}.")));
            }
        }
        if args[0] == "--help" {
            assert!(text.contains("Metadata-only Eventhouse"));
            assert!(text.contains("same-name default KQL database"));
            assert!(text.contains("capacity or storage consumption"));
            #[cfg(feature = "v2-copyjob-create")]
            {
                assert!(text.contains("empty CopyJob metadata only"));
                assert!(text.contains("not a configured runnable job"));
            }
            #[cfg(feature = "runtime-v2")]
            assert!(text.contains("Livy"));
            #[cfg(feature = "v2-create-semantic-model")]
            assert!(text.contains("pinned five-part blank template"));
        }
    }
}

#[test]
fn combined_dispatch_validates_each_schema_before_approval_or_identity() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.json");
    for (owner, bytes, exit, code) in [
        #[cfg(feature = "v2-copyjob-create")]
        ("CopyJob", EVENTHOUSE, 1, "ApprovalRequired"),
        #[cfg(feature = "v2-copyjob-create")]
        ("CopyJob", KQL, 2, "CreatePolicyRejected"),
        #[cfg(feature = "v2-copyjob-create")]
        ("CopyJob", WAREHOUSE, 2, "CreatePolicyRejected"),
        #[cfg(feature = "v2-create-semantic-model")]
        ("SemanticModel", EVENTHOUSE, 1, "ApprovalRequired"),
        #[cfg(feature = "v2-create-semantic-model")]
        ("SemanticModel", KQL, 2, "CreatePolicyRejected"),
        #[cfg(feature = "v2-create-semantic-model")]
        ("SemanticModel", WAREHOUSE, 2, "CreatePolicyRejected"),
        ("Eventhouse", EVENTHOUSE, 1, "ApprovalRequired"),
        ("KQLDatabase", KQL, 1, "ApprovalRequired"),
        ("Notebook", EVENTHOUSE, 1, "ApprovalRequired"),
        ("Warehouse", EVENTHOUSE, 1, "ApprovalRequired"),
        ("Warehouse", WAREHOUSE, 1, "ApprovalRequired"),
        ("Eventhouse", KQL, 2, "CreatePolicyRejected"),
        ("KQLDatabase", EVENTHOUSE, 2, "CreatePolicyRejected"),
        ("Report", EVENTHOUSE, 2, "CreatePolicyRejected"),
        ("Report", KQL, 2, "CreatePolicyRejected"),
        ("Notebook", KQL, 2, "CreatePolicyRejected"),
        ("Warehouse", KQL, 2, "CreatePolicyRejected"),
        ("Eventhouse", WAREHOUSE, 2, "CreatePolicyRejected"),
        ("KQLDatabase", WAREHOUSE, 2, "CreatePolicyRejected"),
        ("Report", WAREHOUSE, 2, "CreatePolicyRejected"),
        ("Notebook", WAREHOUSE, 2, "CreatePolicyRejected"),
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
        format!("{ROOT}.Lakehouse"),
        format!("{ROOT}.Notebook/src/definition"),
        format!("{ROOT}.Eventhouse/src/definition"),
        format!("{ROOT}.KQLDatabase/src/definition"),
        format!("{ROOT}.Warehouse/src/definition"),
        format!("{ROOT}.warehouse"),
        #[cfg(feature = "v2-copyjob-create")]
        format!("{ROOT}.CopyJob/src/definition"),
        #[cfg(feature = "v2-copyjob-create")]
        format!("{ROOT}.copyjob"),
        #[cfg(feature = "v2-create-semantic-model")]
        format!("{ROOT}.SemanticModel/src/definition"),
        #[cfg(feature = "v2-create-semantic-model")]
        format!("{ROOT}.semanticmodel"),
    ] {
        error(
            &run(&["create", &destination, "--input", "nonexistent.json"]),
            2,
            "InvalidInput",
        );
    }
    for owner in [
        "Eventhouse",
        "KQLDatabase",
        "Report",
        "Notebook",
        "Warehouse",
        #[cfg(feature = "v2-copyjob-create")]
        "CopyJob",
        #[cfg(feature = "v2-create-semantic-model")]
        "SemanticModel",
    ] {
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

#[cfg(feature = "v2-copyjob-create")]
#[test]
fn copyjob_metadata_never_selects_another_owner_or_authorizes_unsupported_policy() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.json");
    for (owner, bytes, code) in [
        (
            "CopyJob",
            br#"{"displayName":"Exact","definition":{}}"#.as_slice(),
            "CreatePolicyRejected",
        ),
        (
            "CopyJob",
            br#"{"displayName":"Exact","displayName":"Exact"}"#,
            "InvalidInput",
        ),
        (
            "CopyJob",
            br#"{"displayName":"Other"}"#,
            "CreatePolicyRejected",
        ),
        ("Report", EVENTHOUSE, "CreatePolicyRejected"),
        ("KQLDatabase", EVENTHOUSE, "CreatePolicyRejected"),
    ] {
        std::fs::write(&input, bytes).unwrap();
        error(
            &run(&[
                "create",
                &format!("{ROOT}.{owner}"),
                "--input",
                input.to_str().unwrap(),
            ]),
            2,
            code,
        );
    }
    for policy in ["overwrite", "duplicate"] {
        error(
            &run(&[
                "create",
                &format!("{ROOT}.CopyJob"),
                "--input",
                "nonexistent.json",
                "--if-exists",
                policy,
            ]),
            2,
            "CreatePolicyRejected",
        );
    }
    for policy in ["error", "skip"] {
        error(
            &run(&["create", &format!("{ROOT}.CopyJob"), "--if-exists", policy]),
            1,
            "ApprovalRequired",
        );
    }
}
