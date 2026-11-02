// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual public parser entry, without caller-side query token joining.

use fabric_cli::parser::{Invocation, ReadCommand, parse_args};

fn read(args: &[&str]) -> ReadCommand {
    let Invocation::Read(command) =
        parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
    else {
        panic!("expected read arguments");
    };
    command
}

#[test]
fn source_ls_scalar_is_not_get_nargs_and_preserves_quotes() {
    for expression in ["", "'name'", "\"name\"", "[?name != 'skip'] | [].name"] {
        assert_eq!(
            read(&["ls", "-q", expression]).query.as_deref(),
            Some(expression)
        );
    }
    let command = read(&["dir", "-q", "name", "|", "keys(@)"]);
    assert_eq!(command.name, "dir");
    assert_eq!(command.query.as_deref(), Some("name"));
    assert_eq!(command.path, ["|", "keys(@)"]);
}

#[test]
fn source_get_joins_real_tokens_and_preserves_empty_presence() {
    for args in [
        vec!["get", ".", "-q", "properties", "|", "keys(@)"],
        vec!["get", ".", "-q", "properties | keys(@)"],
        vec!["get", ".", "-q", "\"'properties", "|", "keys(@)'\""],
    ] {
        assert_eq!(read(&args).query.as_deref(), Some("properties | keys(@)"));
    }
    assert_eq!(read(&["get", "."]).query, None);
    assert_eq!(read(&["get", ".", "-q", ""]).query.as_deref(), Some(""));
    assert_eq!(read(&["ls"]).query, None);
    assert_eq!(read(&["ls", "-q", ""]).query.as_deref(), Some(""));
}

#[test]
fn source_last_occurrence_wins_without_accumulating_query_or_output() {
    let command = read(&[
        "get", ".", "-q", "old", "value", "-o", "old", "file", "--query", "", "--output", "new",
        "file", "-vf", "-v",
    ]);
    assert_eq!(command.query.as_deref(), Some(""));
    assert_eq!(command.output.as_deref(), Some("new file"));
    assert!(command.verbose && command.force);
    let command = read(&["ls", "-la", "--long", "-q", "old", "--query", "new"]);
    assert!(command.long && command.all);
    assert_eq!(command.query.as_deref(), Some("new"));
}

#[test]
fn source_negative_numbers_are_values_but_exponents_and_options_are_not() {
    let command = read(&[
        "get", ".", "-q", "name", "-1", "-.5", "-0.25", "--output", "file",
    ]);
    assert_eq!(command.query.as_deref(), Some("name -1 -.5 -0.25"));
    for value in ["-1e3", "-1.", "--all", "--secret-value"] {
        assert!(parse_args(["fab-rs", "get", ".", "-q", value]).is_err());
    }
    assert_eq!(read(&["ls", "--query=-1e3"]).query.as_deref(), Some("-1e3"));
}

#[test]
fn source_attached_query_stops_and_single_path_cannot_resume() {
    for args in [
        vec!["ls", ".", "-q", "name", "tail"],
        vec!["ls", ".", "-l", "tail"],
        vec!["get", ".", "-qname", "tail"],
        vec!["get", ".", "--query=name", "tail"],
        vec!["get", ".", "-q", "name", "--", "tail"],
    ] {
        assert!(parse_args(std::iter::once("fab-rs").chain(args)).is_err());
    }
    let command = read(&["get", "-qname", "."]);
    assert_eq!(command.path, ["."]);
    assert_eq!(command.query.as_deref(), Some("name"));
    assert_eq!(read(&["ls", ".", "--", "-q"]).path, [".", "-q"]);
    let command = read(&["get", "-q", "name", "--", "."]);
    assert_eq!(command.path, ["."]);
    assert_eq!(command.query.as_deref(), Some("name"));
}

#[test]
fn source_query_abbreviations_and_option_stops_keep_flag_meaning() {
    let command = read(&[
        "get",
        "My",
        "Workspace.Workspace/Item.Notebook",
        "--que",
        "properties",
        "|",
        "keys(@)",
        "--output_format",
        "json",
    ]);
    assert_eq!(command.path, ["My", "Workspace.Workspace/Item.Notebook"]);
    assert_eq!(command.query.as_deref(), Some("properties | keys(@)"));
    assert_eq!(command.output_format.as_deref(), Some("json"));
}

#[test]
fn lexical_adapter_keeps_existing_preview_help_and_version_precedence() {
    for args in [
        vec!["fab-rs", "--help", "ls", "--query"],
        vec!["fab-rs", "--version", "get", "--query"],
        vec!["fab-rs", "ls", "-q", "--help"],
        vec!["fab-rs", "get", ".", "-q", "-h"],
    ] {
        let raw = fabric_cli::parser::command().try_get_matches_from(&args);
        let raw_display = raw.as_ref().is_err_and(|error| {
            matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            )
        });
        assert_eq!(
            matches!(parse_args(args), Ok(Invocation::Display(_))),
            raw_display
        );
    }
    let command = read(&["ls", "--", "--help"]);
    assert_eq!(command.path, ["--help"]);
}
