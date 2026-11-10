// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native root preflight is local even with both headless and broker features.

#![cfg(feature = "headless")]
#![deny(unnameable_test_items)]

use serde_json::{Value, json};
use std::process::{Command, Output};

const TARGET: &str = "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.MirroredDatabase/runtime/mirroring";

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fab-cli-v2"))
        .args(args)
        .env_remove("FAB_RUST_AUTH_MODE")
        .env_remove("FAB_RUST_FABRIC_HOST")
        .output()
        .unwrap()
}

fn error(output: Output, exit: i32, code: &str) {
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], code);
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(!text.contains("private"));
    assert!(!text.contains(TARGET));
}

#[test]
fn root_dispatches_local_stop_help_and_schema() {
    let root = run(&["--help"]);
    assert!(root.status.success());
    assert!(String::from_utf8_lossy(&root.stdout).contains("fab-cli-v2 stop"));
    let help = run(&["stop", "--help"]);
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    let text = String::from_utf8(help.stdout).unwrap();
    for option in ["--yes", "--timeout", "--schema", "--format"] {
        assert!(text.contains(option));
    }
    for option in ["--approve", "--wait", "--force"] {
        assert!(!text.contains(option));
    }
    let schema = run(&["stop", "--schema"]);
    assert!(schema.status.success());
    assert!(schema.stderr.is_empty());
    let value: Value = serde_json::from_slice(&schema.stdout).unwrap();
    assert_eq!(value["operations"][0]["id"], "fabric.stop");
    assert_eq!(value["operations"][0]["verb"], "stop");
    assert_eq!(value["operations"][0]["effects"], json!(["remoteRead"]));
    assert_eq!(
        value["operations"][0]["optionalEffects"],
        json!(["remoteWrite"])
    );
    assert_eq!(value["bindings"].as_array().unwrap().len(), 1);
}

#[test]
fn explicit_timeout_is_required_before_identity_even_without_yes() {
    for args in [
        vec!["stop", TARGET],
        vec!["stop", TARGET, "--yes"],
        vec!["stop", TARGET, "--timeout"],
    ] {
        error(run(&args), 2, "InvalidArguments");
    }
    for timeout in [
        "0",
        "-1",
        "86401",
        "1.5",
        "private-timeout",
        "18446744073709551616",
    ] {
        error(
            run(&["stop", TARGET, "--timeout", timeout]),
            2,
            "InvalidArguments",
        );
    }
    for yes in [false, true] {
        let mut args = vec!["stop", TARGET, "--timeout", "1"];
        if yes {
            args.push("--yes");
        }
        error(run(&args), 1, "NotConfigured");
    }
}

#[test]
fn invalid_targets_formats_and_flags_never_reach_identity() {
    for path in [
        "/".to_owned(),
        TARGET.replace(".MirroredDatabase", ".Notebook"),
        TARGET.replace("22222222-2222-2222-2222-222222222222", "private-item"),
        TARGET.replace("/runtime/mirroring", "/runtime/jobs"),
        format!("{TARGET}/"),
        TARGET.trim_start_matches('/').to_owned(),
    ] {
        error(
            run(&["stop", &path, "--timeout", "1"]),
            2,
            "InvalidArguments",
        );
    }
    for extra in [
        vec!["--approve"],
        vec!["--wait"],
        vec!["--yes", "--yes"],
        vec!["--format", "table"],
        vec!["--time", "1"],
        vec!["--timeout", "2"],
        vec!["--schema"],
    ] {
        let mut args = vec!["stop", TARGET, "--timeout", "1"];
        args.extend(extra);
        error(run(&args), 2, "InvalidArguments");
    }
}
