// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline argument and capability boundaries for v2 Notebook deletion.

#![deny(unnameable_test_items)]

use fabric_cli::{
    delete::preflight,
    parser::{DeleteCommand, Invocation, delete::parse_timeout, parse_args},
};
use std::time::Duration;

const TARGET: &str =
    "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook";

fn command(options: &[&str]) -> DeleteCommand {
    let args = ["fab-rs", "delete", TARGET]
        .into_iter()
        .chain(options.iter().copied());
    let Invocation::Delete(command) = parse_args(args).unwrap() else {
        panic!("expected v2 delete")
    };
    command
}

#[test]
fn defaults_are_explicit_and_never_approve_permanent_deletion() {
    let command = command(&[]);
    assert!(!command.permanent);
    assert!(!command.recursive);
    assert!(!command.wait);
    assert_eq!(command.timeout, Duration::from_secs(30));
    assert_eq!(command.format, "json");
    assert_eq!(command.output, "-");
    assert!(preflight(&command, None).is_err());
}

#[test]
fn explicit_boolean_forms_and_wait_budget_are_retained() {
    for options in [
        vec!["--permanent", "true", "--wait", "true"],
        vec!["--permanent=true", "--wait=true"],
        vec!["--permanent", "--wait"],
    ] {
        let command = command(&options);
        assert!(command.permanent && command.wait);
        assert_eq!(command.timeout, Duration::from_secs(300));
    }
    let command = command(&["--permanent=false", "--recursive=false", "--wait=false"]);
    assert!(!command.permanent && !command.recursive && !command.wait);
    let Invocation::Delete(command) =
        parse_args(["fab-rs", "delete", "--permanent", "--wait", TARGET]).unwrap()
    else {
        panic!("expected delete")
    };
    assert!(command.permanent && command.wait);
}

#[test]
fn timeout_accepts_only_positive_bounded_integer_units() {
    for (value, milliseconds) in [
        ("1ms", 1),
        ("30s", 30_000),
        ("5m", 300_000),
        ("24h", 86_400_000),
    ] {
        assert_eq!(
            parse_timeout(value).unwrap(),
            Duration::from_millis(milliseconds)
        );
        assert_eq!(
            command(&["--timeout", value]).timeout.as_millis(),
            u128::from(milliseconds)
        );
    }
    for value in [
        "0ms",
        "0s",
        "-1s",
        "1.5s",
        "25h",
        "86400001ms",
        "1",
        "1S",
        "",
        " 1s",
        "18446744073709551615h",
    ] {
        assert!(parse_args(["fab-rs", "delete", TARGET, "--timeout", value]).is_err());
    }
}

#[test]
fn removed_aliases_and_multiple_targets_are_not_registered() {
    for option in ["--force", "--approved", "--input", "--perm", "--query"] {
        assert!(
            parse_args(["fab-rs", "delete", TARGET, option]).is_err(),
            "{option}"
        );
    }
    assert!(parse_args(["fab-rs", "delete", TARGET, TARGET]).is_err());
    assert!(parse_args(["fab-rs", "delete"]).is_err());
    assert!(parse_args(["fab-rs", "delete", TARGET, "--format", "text"]).is_err());
}

#[test]
fn global_text_never_overrides_json_silently() {
    for args in [
        vec![
            "fab-rs",
            "--output_format",
            "text",
            "delete",
            TARGET,
            "--permanent",
        ],
        vec![
            "fab-rs",
            "delete",
            TARGET,
            "--permanent",
            "--output_format",
            "text",
        ],
    ] {
        let Invocation::Delete(command) = parse_args(args).unwrap() else {
            panic!("expected delete")
        };
        assert_eq!(command.output_format.as_deref(), Some("text"));
        assert_eq!(
            preflight(&command, None).unwrap_err().code(),
            "UnsupportedCapability"
        );
    }
    assert!(preflight(&command(&["--permanent", "--output_format", "json"]), None).is_ok());
}

#[test]
fn recursive_and_if_match_fail_before_the_operation_host() {
    for options in [
        vec!["--permanent", "--recursive"],
        vec!["--permanent", "--if-match", "\"etag\""],
        vec!["--permanent", "--if-match="],
    ] {
        assert_eq!(
            preflight(&command(&options), None).unwrap_err().code(),
            "UnsupportedCapability"
        );
    }
}

#[test]
fn profile_is_an_assertion_about_caller_selection_not_a_loader() {
    let command = command(&["--permanent", "--profile", "chosen"]);
    assert!(preflight(&command, Some("chosen")).is_ok());
    for selected in [None, Some("other")] {
        assert_eq!(
            preflight(&command, selected).unwrap_err().code(),
            "UnsupportedCapability"
        );
    }
}

#[test]
fn only_one_exact_absolute_typed_guid_notebook_is_admitted() {
    let valid = command(&["--permanent"]);
    for path in [
        "/",
        "*",
        "Book.Notebook",
        "/Team.Workspace/Book.Notebook",
        "/11111111-1111-1111-1111-111111111111.Workspace",
        "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Lakehouse",
        "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook/src",
        "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook/docs",
        "/11111111-1111-1111-1111-111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook/cells/0",
        "/11111111-1111-1111-1111-111111111111.Workspace/*.Notebook",
        "/11111111111111111111111111111111.Workspace/22222222-2222-2222-2222-222222222222.Notebook",
    ] {
        let mut command = valid.clone();
        command.path = path.into();
        assert_eq!(
            preflight(&command, None).unwrap_err().code(),
            "InvalidInput",
            "{path}"
        );
    }
}

#[test]
fn query_and_output_short_options_remain_data_only() {
    let command = command(&["--permanent", "-q", "resource.id", "-o", "receipt.json"]);
    assert_eq!(command.project.as_deref(), Some("resource.id"));
    assert_eq!(command.output, "receipt.json");
    assert!(preflight(&command, None).is_ok());
    for query in ["[?", "unknown_function(@)", "", " "] {
        let mut invalid = command.clone();
        invalid.project = Some(query.into());
        assert!(preflight(&invalid, None).is_err(), "{query}");
    }
}

#[test]
fn v1_remove_schema_and_v2_help_remain_separate() {
    assert!(matches!(
        parse_args(["fab-rs", "rm", "Book.Notebook", "--force"]).unwrap(),
        Invocation::Resource(_)
    ));
    let Invocation::Display(help) = parse_args(["fab-rs", "delete", "--help"]).unwrap() else {
        panic!("expected help")
    };
    assert!(help.contains(".Workspace/"));
    assert!(help.contains("Workspace Admin"));
    assert!(help.contains("Default recoverable deletion is unsupported"));
    assert!(help.contains("/notebooks/{id} endpoint"));
    assert!(!help.contains("--approved"));
    assert!(!help.contains("--force"));
}
