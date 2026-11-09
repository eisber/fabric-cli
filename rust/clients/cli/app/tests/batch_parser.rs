// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source batch registration without altering per-command parsing or diagnostics.
#![deny(unnameable_test_items)]

use fabric_cli::parser::{
    Invocation,
    batch::{self, CommandParseError},
    invalid_arguments_message, parse_args,
};

#[test]
fn repeats_preserve_raw_order_empty_entries_and_outer_format() {
    let Invocation::Batch(batch) = parse_args([
        "fab-rs",
        "--output_format",
        "json",
        "-c",
        "version",
        "--command=",
        "-cconfig get output_format",
        "-c",
        "  ",
    ])
    .unwrap() else {
        panic!("batch required")
    };
    assert_eq!(
        batch.commands,
        ["version", "", "config get output_format", "  "]
    );
    assert_eq!(batch.output_format.as_deref(), Some("json"));
    let debug = format!("{batch:?}");
    assert!(!debug.contains("config get"));
    assert!(!debug.contains("version"));
}

#[test]
fn subcommand_overrides_preceding_batch_as_in_the_source_shared_destination() {
    assert!(matches!(
        parse_args(["fab-rs", "-c", "unknown", "version"]).unwrap(),
        Invocation::Local(_)
    ));
    assert!(parse_args(["fab-rs", "version", "-c", "version"]).is_err());
}

#[test]
fn overriding_subcommands_keep_source_lexical_normalization_and_actual_alias() {
    for ignored in ["get", "dir", "deploy", "ls"] {
        let Invocation::Read(read) =
            parse_args(["fab-rs", "-c", ignored, "get", ".", "-q", "name", "-1"]).unwrap()
        else {
            panic!("expected overriding GET");
        };
        assert_eq!(read.query.as_deref(), Some("name -1"));
        assert_eq!(read.name, "get");
        let Invocation::Read(read) = parse_args(["fab-rs", "-c", ignored, "ls", "/"]).unwrap()
        else {
            panic!("expected overriding LS");
        };
        assert_eq!(read.name, "ls");
        let Invocation::Deploy(deploy) = parse_args([
            "fab-rs",
            "-c",
            ignored,
            "deploy",
            "--config",
            "config.yml",
            "-tenv",
            "Test",
        ])
        .unwrap() else {
            panic!("expected overriding deployment");
        };
        assert_eq!(deploy.target_env, "Test");
    }
}

#[test]
fn missing_batch_value_is_sanitized_and_legacy_help_is_unchanged() {
    let message = invalid_arguments_message(&parse_args(["fab-rs", "-c"]).unwrap_err());
    assert!(message.contains("--command"), "{message}");
    assert!(matches!(
        parse_args(["fab-rs", "-help"]).unwrap(),
        Invocation::Display(_)
    ));
    assert!(matches!(
        parse_args(["fab-rs", "-c", "version", "--help"]).unwrap(),
        Invocation::Display(_)
    ));
}

#[test]
fn source_lexing_keeps_quotes_and_operators_and_uses_python_whitespace() {
    assert_eq!(
        batch::split(" get \"Demo Workspace\";x | y\r\nz\u{1c}q\u{85}r "),
        ["get", "\"Demo", "Workspace\";x", "|", "y", "z", "q", "r"]
    );
}

#[test]
fn child_parsing_distinguishes_group_help_parser_exit_and_unknown_keys() {
    assert!(matches!(
        batch::parse_command(&["config"]).unwrap(),
        (Invocation::Display(_), false)
    ));
    assert!(matches!(
        batch::parse_command(&["config", "-help"]).unwrap(),
        (Invocation::Display(_), true)
    ));
    for first in ["-c", "--help", "unknown-private-value"] {
        assert!(matches!(
            batch::parse_command(&[first]),
            Err(CommandParseError::UnknownCommand)
        ));
    }
}

#[test]
fn abbreviated_and_clustered_help_keep_parser_exit_classification() {
    for spelling in ["--he", "--hel", "-h", "-lh", "-lah", "-help"] {
        assert!(
            matches!(
                batch::parse_command(&["ls", spelling]).unwrap(),
                (Invocation::Display(_), true)
            ),
            "{spelling}"
        );
    }
}

#[test]
fn child_values_do_not_acquire_outer_options_or_echo_rejected_secrets() {
    let (Invocation::Read(read), exits) = batch::parse_command(&["dir", "/", "-q-help"]).unwrap()
    else {
        panic!("read required")
    };
    assert!(!exits);
    assert_eq!(read.query.as_deref(), Some("-help"));
    assert_eq!(read.output_format, None);
    let Err(CommandParseError::Arguments(error)) =
        batch::parse_command(&["ls", "--token=private-batch-marker"])
    else {
        panic!("rejected secret required")
    };
    assert!(!invalid_arguments_message(&error).contains("private-batch-marker"));
}

#[test]
fn outer_version_does_not_replace_a_batch_but_help_still_exits() {
    for flag in ["--version", "-v", "-V"] {
        assert!(matches!(
            parse_args(["fab-rs", flag, "-c", "version"]).unwrap(),
            Invocation::Batch(_)
        ));
        assert!(matches!(
            parse_args(["fab-rs", "-c", "version", flag]).unwrap(),
            Invocation::Batch(_)
        ));
    }
    assert!(matches!(
        parse_args(["fab-rs", "--version"]).unwrap(),
        Invocation::Display(_)
    ));
    assert!(matches!(
        parse_args(["fab-rs", "-c", "version", "-help"]).unwrap(),
        Invocation::Display(_)
    ));
}
