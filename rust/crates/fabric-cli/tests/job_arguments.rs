// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-named job-read parser contracts with no host or service execution.

mod parser_support;

use fabric_cli::parser::{Invocation, JobReadCommand, JobReadKind, parse_args};

fn parse(args: &[&str]) -> JobReadCommand {
    let Invocation::JobRead(command) =
        parse_args(std::iter::once("fab-rs").chain(args.iter().copied())).unwrap()
    else {
        panic!("job read required");
    };
    command
}

#[test]
fn only_the_two_read_and_two_execution_leaves_are_registered() {
    let schema = fabric_cli::parser::command();
    let group = schema.find_subcommand("job").unwrap();
    assert_eq!(
        group
            .get_subcommands()
            .map(|leaf| leaf.get_name())
            .collect::<Vec<_>>(),
        ["start", "run", "run-list", "run-status"]
    );
    for leaf in ["run-cancel", "run-sch", "run-rm", "run-update"] {
        assert!(parse_args(["fab-rs", "job", leaf, "item.Notebook"]).is_err());
    }
}

#[test]
fn source_path_id_schedule_and_output_options_retain_values() {
    let command = parse(&[
        "--output_format",
        "text",
        "job",
        "run-status",
        "'Demo.Workspace/Book",
        "One.Notebook'",
        "--id",
        "first",
        "--id=last",
        "--sched",
        "--output_format=json",
    ]);
    assert_eq!(command.kind, JobReadKind::Status);
    assert_eq!(command.path, ["'Demo.Workspace/Book", "One.Notebook'"]);
    assert_eq!(command.id.as_deref(), Some("last"));
    assert!(command.schedule);
    assert_eq!(command.output_format.as_deref(), Some("json"));
    let command = parse(&["job", "run-list", "--schedule", "Book.Notebook"]);
    assert_eq!(command.kind, JobReadKind::List);
    assert_eq!(command.path, ["Book.Notebook"]);
    assert!(command.id.is_none());
    assert_eq!(
        parse(&["--output_format=text", "job", "run-list", "Book.Notebook"])
            .output_format
            .as_deref(),
        Some("text")
    );
}

#[test]
fn empty_values_are_present_and_source_variadic_path_is_not_resumed() {
    let command = parse(&["job", "run-status", "Book.Notebook", "--id=", "--skill="]);
    assert_eq!(command.id.as_deref(), Some(""));
    assert_eq!(command.skill.as_deref(), Some(""));
    for args in [
        vec!["job", "run-list"],
        vec!["job", "run-status", "Book.Notebook"],
        vec!["job", "run-status", "Book.Notebook", "--id"],
        vec!["job", "run-list", "Book.Notebook", "--schedule", "extra"],
        vec!["job", "run-status", "Book.Notebook", "--id=run", "extra"],
        vec!["job", "run-list", "Book.Notebook", "-q", "status"],
        vec!["job", "run-list", "Book.Notebook", "--wait"],
        vec!["job", "run-status", "Book.Notebook", "--id=run", "--force"],
    ] {
        assert!(parse_args(std::iter::once("fab-rs").chain(args)).is_err());
    }
}

#[test]
fn job_help_does_not_select_a_host_or_advertise_mutations() {
    for args in [
        vec!["job", "--help"],
        vec!["job", "run-list", "--help"],
        vec!["job", "run-status", "--help"],
    ] {
        assert!(matches!(
            parse_args(std::iter::once("fab-rs").chain(args)).unwrap(),
            Invocation::Display(_)
        ));
    }
}

#[test]
fn legacy_read_probe_explicitly_rejects_job_invocations() {
    let input = serde_json::json!({
        "id": "job-read-is-not-a-read-probe-command",
        "parse_only": true,
        "command": ["job", "run-list", "Book.Notebook"],
    });
    assert_eq!(
        parser_support::observe(&input),
        Some(serde_json::json!({
            "id": input["id"],
            "parse": {"error": true},
        }))
    );
    let read = parser_support::observe(&serde_json::json!({
        "id": "existing-read",
        "parse_only": true,
        "command": ["ls", "/"],
    }))
    .unwrap();
    assert_eq!(read["parse"]["read"]["kind"], "list");
    assert_eq!(read["parse"]["read"]["path"], serde_json::json!(["/"]));
}
