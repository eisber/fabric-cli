// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pinned Python argv contrasts; no ambient identity, configuration or network.

use fabric_cli::parser::{Invocation, parse_args};
use std::process::{Command, Output};

fn invoke(args: &[&str], invalid_selectors: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fab-rs"));
    command.env_clear();
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    if invalid_selectors {
        command
            .env("FAB_RUST_TENANT", "invalid-tenant")
            .env("FAB_RUST_ACCOUNT", "argv-private-marker")
            .env("FAB_RUST_CLIENT_ID", "invalid-client");
    }
    command.args(args).output().unwrap()
}

fn diagnostic(args: &[&str]) -> String {
    let output = invoke(args, true);
    assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
    assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
    assert!(output.stderr.ends_with(b"\n"));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error.as_object().unwrap().len(), 1);
    assert_eq!(error["error"].as_object().unwrap().len(), 2);
    assert_eq!(error["error"]["code"], "invalid_arguments");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(!message.contains("argv-private-marker"));
    assert!(!message.contains("Only --help, --version, ls, dir, and get"));
    assert!(message.contains("--help"), "{args:?}: {message}");
    message.to_owned()
}

#[test]
fn missing_job_path_names_the_schema_argument() {
    let message = diagnostic(&["job", "run-list"]);
    assert!(message.contains("path"), "{message}");
}

#[test]
fn missing_job_id_names_the_schema_option_without_echoing_path() {
    let message = diagnostic(&["job", "run-status", "argv-private-marker.Notebook"]);
    assert!(message.contains("--id"), "{message}");
}

#[test]
fn missing_local_and_configuration_values_name_schema_arguments() {
    for (args, required) in [
        (vec!["cd"], "path"),
        (vec!["config", "set", "argv-private-marker"], "value"),
        (vec!["config", "get"], "key"),
        (vec!["api"], "endpoint"),
        (vec!["export", "argv-private-marker.Notebook"], "--output"),
    ] {
        let message = diagnostic(&args);
        assert!(message.contains(required), "{args:?}: {message}");
    }
}

#[test]
fn missing_option_values_name_the_schema_option() {
    for args in [
        vec!["ls", "--query"],
        vec!["get", "argv-private-marker.Notebook", "--output"],
        vec!["job", "run-status", "argv-private-marker.Notebook", "--id"],
    ] {
        let option = *args.last().unwrap();
        let message = diagnostic(&args);
        assert!(message.contains(option), "{args:?}: {message}");
    }
}

#[test]
fn generic_errors_remain_truthful_sanitized_and_precede_any_host() {
    for args in [
        vec!["argv-private-marker"],
        vec!["--argv-private-marker=argv-private-marker"],
        vec![
            "job",
            "run-list",
            "argv-private-marker",
            "--bad=argv-private-marker",
        ],
        vec!["ls", "--output_format=argv-private-marker"],
    ] {
        diagnostic(&args);
    }
}

#[test]
fn bare_source_groups_display_registered_help_without_configuration() {
    for (group, leaf) in [
        ("job", "run-list"),
        ("acl", "ls"),
        ("label", "list-local"),
        ("config", "clear-cache"),
    ] {
        for invalid_selectors in [false, true] {
            let output = invoke(&[group], invalid_selectors);
            assert_eq!(output.status.code(), Some(0), "{group}: {output:?}");
            assert!(output.stderr.is_empty(), "{group}: {output:?}");
            let help = String::from_utf8(output.stdout).unwrap();
            assert!(help.contains(leaf), "{group}: {help}");
            assert!(help.contains(&format!("Usage: fab-rs {group}")), "{help}");
            assert!(!help.contains("argv-private-marker"));
        }
    }
}

#[test]
fn legacy_help_matches_long_help_for_registered_commands_and_aliases() {
    for args in [
        vec![],
        vec!["job"],
        vec!["job", "run-list"],
        vec!["config"],
        vec!["config", "dir"],
        vec!["cls"],
        vec!["get"],
        vec!["dir"],
        vec!["acl"],
        vec!["acl", "dir"],
        vec!["label"],
        vec!["create"],
        vec!["del"],
        vec!["export"],
    ] {
        let mut long = args.clone();
        long.push("--help");
        let mut legacy = args;
        legacy.push("-help");
        let expected = invoke(&long, true);
        let actual = invoke(&legacy, true);
        assert_eq!(actual.status.code(), Some(0), "{legacy:?}: {actual:?}");
        assert!(actual.stderr.is_empty(), "{legacy:?}: {actual:?}");
        assert_eq!(actual.stdout, expected.stdout, "{legacy:?}");
    }
}

#[test]
fn help_tokens_in_values_and_after_delimiter_are_not_rewritten() {
    for args in [
        vec!["ls", "--query=-help"],
        vec!["ls", "-q-help"],
        vec!["get", ".", "--query=-help"],
        vec!["get", ".", "-q", "'-help'"],
    ] {
        let Invocation::Read(read) =
            parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
        else {
            panic!("a query value must not select help: {args:?}");
        };
        assert_eq!(read.query.as_deref(), Some("-help"));
    }
    let Invocation::Read(read) = parse_args(["fab-rs", "ls", "--", "-help"]).unwrap()
    else {
        panic!("a positional value must not select help");
    };
    assert_eq!(read.path, ["-help"]);
    let Invocation::Read(read) =
        parse_args(["fab-rs", "get", ".", "--output=-help"]).unwrap()
    else {
        panic!("an output value must not select help");
    };
    assert_eq!(read.output.as_deref(), Some("-help"));
    assert!(matches!(
        parse_args(["fab-rs", "config", "set", "--", "key", "-help"]).unwrap(),
        Invocation::Local(_)
    ));
}

#[test]
fn malformed_arguments_before_help_or_version_still_fail_without_echo() {
    for flag in ["--help", "-help", "--version"] {
        for mut args in [
            vec!["--argv-private-marker=argv-private-marker"],
            vec!["ls", "--output_format=argv-private-marker"],
            vec!["job", "run-list", "--bad=argv-private-marker"],
        ] {
            args.push(flag);
            diagnostic(&args);
        }
    }
}

#[test]
fn root_help_lists_only_registered_feature_gated_commands() {
    let output = invoke(&["--help"], true);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    let schema = fabric_cli::parser::command();
    for command in schema
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
    {
        assert!(help.contains(command.get_name()), "{help}");
    }
    assert_eq!(
        schema.find_subcommand("auth").is_some(),
        cfg!(feature = "windows-broker")
    );
    assert_eq!(
        help.lines()
            .any(|line| line.trim_start().starts_with("auth ")),
        cfg!(feature = "windows-broker")
    );
}
