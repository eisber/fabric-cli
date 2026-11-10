// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Focused schemas and parsed operations compose through the old nominal API.

#![deny(unnameable_test_items)]

use clap::Command;
use fabric_cli_resources::{
    CommandError,
    frontdoor::{AccessCommand, AccessKind, RawApiCommand, register},
};
use std::any::TypeId;

fn matches(args: &[&str]) -> clap::ArgMatches {
    register(Command::new("fab-rs"))
        .try_get_matches_from(std::iter::once("fab-rs").chain(args.iter().copied()))
        .unwrap()
}

fn assert_common<T>(
    focused: &fabric_cli_command_contracts::parser::ParsedCommand<T>,
    legacy: &AccessCommand,
) {
    assert_eq!(
        format!("{:?}", focused.output_format),
        format!("{:?}", legacy.output_format)
    );
    assert_eq!(focused.skill, legacy.skill);
}

#[test]
fn focused_registration_retains_order_aliases_and_the_old_schema_entry_points() {
    let mut focused = fabric_cli_api::parser::register(fabric_cli_catalog::parser::register(
        fabric_cli_access::parser::register(Command::new("fab-rs")),
    ));
    assert_eq!(
        focused
            .get_subcommands()
            .map(Command::get_name)
            .collect::<Vec<_>>(),
        ["acl", "label", "find", "api"]
    );
    assert_eq!(
        focused.render_long_help().to_string(),
        register(Command::new("fab-rs"))
            .render_long_help()
            .to_string()
    );
    let parsed = matches(&["acl", "dir", "workspace"]);
    assert!(matches!(
        fabric_cli_access::parser::from_matches(&parsed)
            .unwrap()
            .unwrap()
            .kind,
        fabric_cli_access::parser::AccessKind::AclList { .. }
    ));
    let parsed = matches(&["label", "del", "item", "--force"]);
    assert!(matches!(
        fabric_cli_access::parser::from_matches(&parsed)
            .unwrap()
            .unwrap()
            .kind,
        fabric_cli_access::parser::AccessKind::LabelChange {
            change: fabric_cli_access::LabelChange::Remove,
            force: true,
            ..
        }
    ));
}

#[test]
fn all_access_leaves_map_to_unchanged_legacy_variants_and_common_arguments() {
    let cases: &[&[&str]] = &[
        &["acl", "ls", "workspace", "--query", "'scalar'"],
        &[
            "acl",
            "get",
            "workspace",
            "-q",
            "'first",
            "second'",
            "-o",
            "file",
        ],
        &[
            "acl",
            "set",
            "workspace",
            "-I",
            "identity",
            "-R",
            "rOlE",
            "-f",
        ],
        &["acl", "del", "workspace", "-I", "identity"],
        &[
            "label",
            "list-local",
            "--output_format",
            "json",
            "--skill",
            "value",
        ],
        &["label", "set", "item", "-n", "'First", "Second'", "-f"],
        &["label", "rm", "item"],
    ];
    for args in cases {
        let parsed = matches(args);
        let focused = fabric_cli_access::parser::from_matches(&parsed)
            .unwrap()
            .unwrap();
        let legacy = AccessCommand::from_matches(&parsed).unwrap().unwrap();
        assert_common(&focused, &legacy);
        assert_eq!(format!("{:?}", focused.kind), format!("{:?}", legacy.kind));
    }
    let parsed = matches(&["acl", "get", "workspace", "-q", "''"]);
    let focused = fabric_cli_access::parser::from_matches(&parsed)
        .unwrap()
        .unwrap();
    assert!(matches!(
        focused.kind,
        fabric_cli_access::parser::AccessKind::AclGet { query: Some(query), .. } if query.is_empty()
    ));
}

#[test]
fn focused_catalog_preserves_optional_scalar_and_joined_query_distinction() {
    let cases: &[&[&str]] = &[
        &["find", "search"],
        &["find", "search", "-P"],
        &[
            "find",
            "search",
            "-P",
            "type=Notebook",
            "-l",
            "-q",
            "'first",
            "second'",
        ],
        &[
            "find",
            "search",
            "--output_format",
            "text",
            "--skill",
            "value",
        ],
    ];
    for args in cases {
        let parsed = matches(args);
        let focused = fabric_cli_catalog::parser::from_matches(&parsed)
            .unwrap()
            .unwrap();
        let legacy = AccessCommand::from_matches(&parsed).unwrap().unwrap();
        assert_common(&focused, &legacy);
        let AccessKind::Find(options) = legacy.kind else {
            panic!("wrong legacy variant")
        };
        assert_eq!(format!("{:?}", focused.kind), format!("{options:?}"));
    }
}

#[test]
fn focused_api_maps_every_field_into_the_original_nominal_raw_type() {
    let cases: &[&[&str]] = &[
        &["api", "workspaces"],
        &[
            "api",
            "workspaces",
            "-A",
            "storage",
            "-X",
            "post",
            "-i",
            "'first",
            "second'",
            "-P",
            "'a=1,",
            "b=2'",
            "-H",
            "'x-one=1,",
            "x-two=2'",
            "-q",
            "'text",
            ".value'",
            "--show_headers",
            "--output_format",
            "json",
            "--skill",
            "value",
        ],
        &["api", "workspaces", "-A", "azure", "-X", "delete"],
        &["api", "workspaces", "-A", "powerbi", "-X", "put"],
        &["api", "workspaces", "-X", "patch"],
    ];
    for args in cases {
        let parsed = matches(args);
        let focused = fabric_cli_api::parser::from_matches(&parsed)
            .unwrap()
            .unwrap();
        let legacy = AccessCommand::from_matches(&parsed).unwrap().unwrap();
        assert_common(&focused, &legacy);
        let AccessKind::Api(options) = legacy.kind else {
            panic!("wrong legacy variant")
        };
        assert_eq!(format!("{:?}", focused.kind), format!("{options:?}"));
    }
    assert_ne!(
        TypeId::of::<RawApiCommand>(),
        TypeId::of::<fabric_cli_api::parser::RawApiCommand>()
    );
    assert_ne!(
        TypeId::of::<AccessKind>(),
        TypeId::of::<fabric_cli_access::parser::AccessKind>()
    );
}

#[test]
fn focused_parsers_reject_only_their_own_malformed_commands_and_ignore_other_families() {
    let root = Command::new("fab-rs")
        .subcommand(Command::new("acl").subcommand(Command::new("get")))
        .subcommand(Command::new("find"))
        .subcommand(Command::new("api"))
        .subcommand(Command::new("other"));
    let expected = CommandError::new("InvalidInput", "Invalid access command arguments");
    for family in ["acl", "find", "api"] {
        let args = if family == "acl" {
            vec!["fab-rs", family, "get"]
        } else {
            vec!["fab-rs", family]
        };
        let parsed = root.clone().try_get_matches_from(args).unwrap();
        assert_eq!(AccessCommand::from_matches(&parsed).unwrap_err(), expected);
        let access = fabric_cli_access::parser::from_matches(&parsed);
        let catalog = fabric_cli_catalog::parser::from_matches(&parsed);
        let api = fabric_cli_api::parser::from_matches(&parsed);
        if family == "acl" {
            assert_eq!(access.unwrap_err(), expected);
            assert!(catalog.unwrap().is_none());
            assert!(api.unwrap().is_none());
        } else if family == "find" {
            assert!(access.unwrap().is_none());
            assert_eq!(catalog.unwrap_err(), expected);
            assert!(api.unwrap().is_none());
        } else {
            assert!(access.unwrap().is_none());
            assert!(catalog.unwrap().is_none());
            assert_eq!(api.unwrap_err(), expected);
        }
    }
    let parsed = root.try_get_matches_from(["fab-rs", "other"]).unwrap();
    assert!(AccessCommand::from_matches(&parsed).unwrap().is_none());
    assert!(
        fabric_cli_access::parser::from_matches(&parsed)
            .unwrap()
            .is_none()
    );
    assert!(
        fabric_cli_catalog::parser::from_matches(&parsed)
            .unwrap()
            .is_none()
    );
    assert!(
        fabric_cli_api::parser::from_matches(&parsed)
            .unwrap()
            .is_none()
    );
}
