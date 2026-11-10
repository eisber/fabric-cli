// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-compatible schemas owned by this command capability.

use clap::{Arg, ArgMatches, Command};
use fabric_cli_import_export_contracts::{
    TransferCommand, TransferKind,
    parser::{self, base_command, flag, invalid, tokens},
};
use fabric_cli_output::CommandError;

/// Register this capability's existing root leaves without adding a namespace.
pub fn subcommands() -> Vec<Command> {
    [TransferKind::Export, TransferKind::BulkExport]
        .into_iter()
        .map(|kind| match kind {
            TransferKind::Export => base_command(kind)
                .arg(tokens("output", 'o').required(true))
                .arg(flag("all").short('a'))
                .arg(Arg::new("format").long("format")),
            TransferKind::BulkExport => base_command(kind)
                .arg(Arg::new("output").long("output").short('o').required(true))
                .arg(flag("recursive")),
            _ => unreachable!(),
        })
        .collect()
}

/// Decode this capability's registered schema without reparsing tokens.
pub fn command_from_matches(root: &ArgMatches) -> Result<TransferCommand, CommandError> {
    let command = parser::command_from_matches(root)?;
    if ![TransferKind::Export, TransferKind::BulkExport].contains(&command.kind) {
        return Err(invalid("Unexpected command for this capability"));
    }
    Ok(command)
}
