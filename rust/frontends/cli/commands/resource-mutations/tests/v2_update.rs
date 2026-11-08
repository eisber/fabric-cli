// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use fabric_cli_mutations::v2_update::{
    self, UpdateInput,
    update::{UpdateMode, contract::Precondition},
};
use serde_json::json;
use std::path::Path;

const TARGET: &str =
    "/11111111-2222-3333-4444-555555555555.Workspace/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.Notebook";

#[test]
fn root_and_facade_normalize_to_the_same_typed_request() {
    let root = v2_update::parse([
        "fab-cli-v2",
        "update",
        TARGET,
        "--input",
        "-",
        "--mode",
        "patch",
    ])
    .unwrap();
    let facade = v2_update::parse([
        "fab-cli-v2",
        "artifact",
        "notebook",
        "update",
        TARGET,
        "--input",
        "-",
        "--mode",
        "patch",
    ])
    .unwrap();
    assert_eq!(root, facade);
    assert_eq!(root.input, UpdateInput::Stdin);
    assert_eq!(root.options.mode, UpdateMode::Patch);
    assert_eq!(root.options.timeout_millis, 30_000);
    let cwd = if cfg!(windows) {
        Path::new("C:/")
    } else {
        Path::new("/")
    };
    let plan = root.prepare(br#"{"description":""}"#, cwd).unwrap();
    assert_eq!(plan.prepared().input()["body"], json!({"description":""}));
    assert_eq!(
        plan.prepared().preconditions(),
        [
            Precondition::ExistingOnly,
            Precondition::UnguardedConcurrency
        ]
    );
}

#[test]
fn exact_machine_path_keeps_owner_identity_and_original_path() {
    let path = "/workspaces/11111111-2222-3333-4444-555555555555/lakehouses/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let target = v2_update::parse_target(path).unwrap();
    assert_eq!(target.selection().owner_type, "Lakehouse");
    assert_eq!(target.canonical_path(), path);
    assert_eq!(
        target.identity().item_id(),
        Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
    );
}

#[test]
fn invalid_scope_or_fallback_never_becomes_an_item_update() {
    for path in [
        "relative.Notebook",
        "/name.Workspace/name.Notebook",
        "/w.Workspace/*",
        "/workspaces/11111111-2222-3333-4444-555555555555/items/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        "/11111111-2222-3333-4444-555555555555.Workspace/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.Notebook/src/definition",
        "/11111111-2222-3333-4444-555555555555.Workspace/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.Notebook/docs/schema",
        "/11111111-2222-3333-4444-555555555555.Workspace/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.SQLDatabase",
    ] {
        assert!(v2_update::parse_target(path).is_err(), "{path}");
    }
    assert!(
        v2_update::parse([
            "fab-cli-v2",
            "artifact",
            "lakehouse",
            "update",
            TARGET,
            "--input",
            "-",
            "--mode",
            "patch"
        ])
        .is_err()
    );
}

#[test]
fn required_input_mode_and_unsupported_controls_are_not_silent_defaults() {
    for args in [
        vec!["fab-cli-v2", "update", TARGET],
        vec!["fab-cli-v2", "update", TARGET, "--input", "-"],
        vec![
            "fab-cli-v2",
            "update",
            TARGET,
            "--input",
            "-",
            "--mode",
            "replace",
        ],
        vec![
            "fab-cli-v2",
            "update",
            TARGET,
            "--input",
            "-",
            "--mode",
            "patch",
            "--if-match",
            "opaque",
        ],
        vec![
            "fab-cli-v2",
            "update",
            TARGET,
            "--input",
            "-",
            "--mode",
            "patch",
            "--wait",
        ],
        vec![
            "fab-cli-v2",
            "update",
            TARGET,
            "--input",
            "-",
            "--mode",
            "patch",
            "--force",
        ],
        vec![
            "fab-cli-v2",
            "update",
            TARGET,
            "--input",
            "-",
            "--mode",
            "patch",
            "--format",
            "text",
        ],
        vec![
            "fab-cli-v2",
            "update",
            TARGET,
            "--input",
            "-",
            "--mode",
            "patch",
            "--input",
            "file",
        ],
        vec![
            "fab-cli-v2",
            "update",
            TARGET,
            "--input",
            "-",
            "--mode",
            "patch",
            "--project",
            "[",
        ],
    ] {
        assert!(v2_update::parse(args).is_err());
    }
}

#[test]
fn positive_duration_units_maximum_and_projection_are_exact() {
    for (value, millis) in [
        ("1ms", 1),
        ("30s", 30_000),
        ("5m", 300_000),
        ("24h", 86_400_000),
    ] {
        let command = v2_update::parse([
            "fab-cli-v2",
            "update",
            TARGET,
            "--input",
            "-",
            "--mode",
            "patch",
            "--timeout",
            value,
            "--project",
            "displayName",
        ])
        .unwrap();
        assert_eq!(command.options.timeout_millis, millis);
    }
    for value in [
        "0s",
        "1.5s",
        "25h",
        "86400001ms",
        "-1s",
        "100",
        "18446744073709551615h",
    ] {
        assert!(
            v2_update::parse([
                "fab-cli-v2",
                "update",
                TARGET,
                "--input",
                "-",
                "--mode",
                "patch",
                "--timeout",
                value
            ])
            .is_err()
        );
    }
}

#[test]
fn parsed_and_manually_constructed_unsupported_projection_fails_preparation() {
    assert!(
        v2_update::parse([
            "fab-cli-v2",
            "update",
            TARGET,
            "--input",
            "-",
            "--mode",
            "patch",
            "--project",
            "&displayName"
        ])
        .is_err()
    );
    let mut command = v2_update::parse([
        "fab-cli-v2",
        "update",
        TARGET,
        "--input",
        "-",
        "--mode",
        "patch",
    ])
    .unwrap();
    command.options.project = Some("&displayName".into());
    let cwd = if cfg!(windows) {
        Path::new("C:/")
    } else {
        Path::new("/")
    };
    assert!(command.prepare(br#"{"displayName":"new"}"#, cwd).is_err());
    command.options.project = Some(".".into());
    assert!(command.prepare(br#"{"displayName":"new"}"#, cwd).is_ok());
}
