// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Host selector ordering without credentials, network or shared environment writes.

use std::process::{Command, Output};

fn invoke(host: Option<&str>, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command.env_clear();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    if let Some(host) = host {
        command.env("FAB_API_ENDPOINT_FABRIC", host);
    }
    command.args(args).output().unwrap()
}

#[test]
fn endpoint_selector_never_changes_help_or_argument_validation() {
    for args in [vec!["--help"], vec!["--version"], vec!["ls", "--help"]] {
        let output = invoke(Some("unknown.example.test"), &args);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
    }
    let output = invoke(Some("unknown.example.test"), &["ls", "--invalid"]);
    assert_eq!(output.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "invalid_arguments");
}

#[cfg(feature = "native-read")]
#[test]
fn invalid_host_fails_before_missing_identity_selectors_on_every_platform() {
    for host in [
        "",
        "unknown.example.test",
        "https://msitapi.fabric.microsoft.com",
        "msitapi.fabric.microsoft.com:444",
        "msitapi.fabric.microsoft.com/path",
        "user@msitapi.fabric.microsoft.com",
    ] {
        let output = invoke(Some(host), &["ls", "/"]);
        assert_eq!(output.status.code(), Some(2), "{host:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap(),
            serde_json::json!({"error":{
                "code":"invalid_configuration",
                "message":"Invalid Fabric endpoint hostname."
            }})
        );
    }
}

#[test]
fn public_daily_and_msit_do_not_discover_an_identity() {
    for host in [
        None,
        Some("api.fabric.microsoft.com"),
        Some("dailyapi.fabric.microsoft.com"),
        Some("msitapi.fabric.microsoft.com"),
    ] {
        let output = invoke(host, &["ls", "/"]);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "NotConfigured");
    }
}

#[cfg(feature = "native-read")]
#[test]
fn non_public_unmigrated_family_fails_before_identity_or_network() {
    let output = invoke(
        Some("msitapi.fabric.microsoft.com"),
        &["exists", "Book.Notebook"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "NotSupported");
}

#[cfg(feature = "native-read")]
#[test]
fn migrated_job_leaves_require_explicit_identity_without_discovering_one() {
    let invocations: &[&[&str]] = &[
        &["job", "start", "Book.Notebook"],
        &["job", "run", "Book.Notebook"],
        &["job", "run-status", "Book.Notebook", "--id", "run"],
        &["job", "run-list", "Book.Notebook"],
        &["job", "run-cancel", "Book.Notebook", "--id", "run"],
        &["job", "run-sch", "Book.Notebook", "--input", "{}"],
        &[
            "job",
            "run-update",
            "Book.Notebook",
            "--id",
            "schedule",
            "--enable",
        ],
        &["job", "run-rm", "Book.Notebook", "--id", "schedule"],
    ];
    for host in [
        "dailyapi.fabric.microsoft.com",
        "msitapi.fabric.microsoft.com",
    ] {
        for args in invocations {
            let output = invoke(Some(host), args);
            assert_eq!(output.status.code(), Some(1), "{host}: {args:?}");
            assert!(output.stdout.is_empty(), "{host}: {args:?}");
            let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["error"]["code"], "NotConfigured", "{host}: {args:?}");
        }
    }
}
