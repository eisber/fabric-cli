// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public identity and explicit argument contracts across the focused owners.

#![deny(unnameable_test_items)]

use clap::Command;
use fabric_cli_transfer::{TransferCommand, TransferKind};
use serde_json::{Value, json};
use std::any::TypeId;

fn fields(command: &TransferCommand) -> Value {
    json!({
        "kind": command.kind.as_str(), "path": command.path,
        "output": command.output, "input": command.input, "format": command.format,
        "all": command.all, "recursive": command.recursive, "force": command.force,
        "output_format": command.output_format, "config": command.config,
        "target_env": command.target_env, "params": command.params,
        "bulk_publish": command.bulk_publish,
    })
}

#[test]
fn shared_values_and_callbacks_retain_public_type_identity() {
    assert_eq!(
        TypeId::of::<TransferCommand>(),
        TypeId::of::<fabric_cli_import_export_contracts::TransferCommand>()
    );
    assert_eq!(
        TypeId::of::<fabric_cli_transfer::TransferResult>(),
        TypeId::of::<fabric_cli_export::TransferResult>()
    );
    assert_eq!(
        TypeId::of::<fabric_cli_transfer::TransferContext>(),
        TypeId::of::<fabric_cli_import::TransferContext>()
    );
    assert_eq!(
        TypeId::of::<dyn fabric_cli_transfer::TransferFiles>(),
        TypeId::of::<dyn fabric_cli_import_export_contracts::TransferFiles>()
    );
    assert_eq!(
        TypeId::of::<dyn fabric_cli_transfer::TransferUi>(),
        TypeId::of::<dyn fabric_cli_import_export_contracts::TransferUi>()
    );
    assert_eq!(
        TypeId::of::<fabric_cli_transfer::TransferFailure>(),
        TypeId::of::<fabric_cli_import::TransferFailure>()
    );
}

#[test]
fn focused_schemas_preserve_explicit_source_field_contracts() {
    let cases: Vec<(&[&str], Value)> = vec![
        (
            &[
                "export",
                "Demo.Workspace/Pipe.DataPipeline",
                "-o",
                "my",
                "output",
                "--format",
                "ipynb",
                "-a",
                "-f",
                "--output_format",
                "json",
            ],
            json!({
                "kind":"export", "path":["Demo.Workspace/Pipe.DataPipeline"],
                "output":["my","output"], "input":null, "format":"ipynb", "all":true,
                "recursive":false, "force":true, "output_format":"json", "config":null,
                "target_env":"N/A", "params":null, "bulk_publish":false,
            }),
        ),
        (
            &[
                "bulk-export",
                "Demo.Workspace",
                "-o",
                "my output",
                "--recursive",
            ],
            json!({
                "kind":"bulk-export", "path":["Demo.Workspace"],
                "output":["my output"], "input":null, "format":null, "all":false,
                "recursive":true, "force":false, "output_format":null, "config":null,
                "target_env":"N/A", "params":null, "bulk_publish":false,
            }),
        ),
        (
            &[
                "import",
                "Demo.Workspace/Pipe.DataPipeline",
                "-i",
                "'my",
                "input'",
                "--format",
                "",
            ],
            json!({
                "kind":"import", "path":["Demo.Workspace/Pipe.DataPipeline"],
                "output":null, "input":["'my","input'"], "format":"", "all":false,
                "recursive":false, "force":false, "output_format":null, "config":null,
                "target_env":"N/A", "params":null, "bulk_publish":false,
            }),
        ),
    ];
    for (args, expected) in cases {
        let export = args[0] != "import";
        let schemas = if export {
            fabric_cli_export::parser::subcommands()
        } else {
            fabric_cli_import::parser::subcommands()
        };
        let focused_matches = Command::new("test")
            .subcommands(schemas)
            .try_get_matches_from(std::iter::once("test").chain(args.iter().copied()))
            .unwrap();
        let focused = if export {
            fabric_cli_export::parser::command_from_matches(&focused_matches)
        } else {
            fabric_cli_import::parser::command_from_matches(&focused_matches)
        }
        .unwrap();
        let legacy_matches = Command::new("test")
            .subcommands(fabric_cli_transfer::transfer_subcommands())
            .try_get_matches_from(std::iter::once("test").chain(args.iter().copied()))
            .unwrap();
        let legacy = fabric_cli_transfer::command_from_matches(&legacy_matches).unwrap();
        assert_eq!(fields(&focused), expected);
        assert_eq!(fields(&legacy), expected);
        if focused.kind == TransferKind::Import {
            assert_eq!(
                fabric_cli_transfer::process_tokens(focused.input.as_ref().unwrap()),
                "my input"
            );
        }
    }
}

#[test]
fn root_order_and_foreign_schema_rejections_are_preserved() {
    let names = fabric_cli_transfer::transfer_subcommands()
        .into_iter()
        .map(|command| command.get_name().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(names, ["export", "bulk-export", "import", "deploy"]);
    for args in [
        vec!["export", "Demo.Workspace", "-o", "output"],
        vec!["bulk-export", "Demo.Workspace", "-o", "output"],
        vec!["import", "Demo.Workspace/Pipe.DataPipeline", "-i", "input"],
        vec!["deploy", "--config", "config.json"],
    ] {
        let matches = Command::new("test")
            .subcommands(fabric_cli_transfer::transfer_subcommands())
            .try_get_matches_from(std::iter::once("test").chain(args.iter().copied()))
            .unwrap();
        assert_eq!(
            fabric_cli_import::parser::command_from_matches(&matches).is_ok(),
            args[0] == "import"
        );
        assert_eq!(
            fabric_cli_export::parser::command_from_matches(&matches).is_ok(),
            matches!(args[0], "export" | "bulk-export")
        );
    }
    let malformed = Command::new("test")
        .subcommand(Command::new("import"))
        .try_get_matches_from(["test", "import"])
        .unwrap();
    assert!(fabric_cli_import::parser::command_from_matches(&malformed).is_err());
}
