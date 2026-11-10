// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-compatible schemas owned by this command capability.

use clap::{Arg, ArgMatches, Command};
use fabric_cli_import_export_contracts::{
    TransferCommand, TransferKind,
    parser::{self, base_command, invalid, tokens},
};
use fabric_cli_output::CommandError;

/// Register this capability's existing root leaves without adding a namespace.
pub fn subcommands() -> Vec<Command> {
    [TransferKind::Import]
        .into_iter()
        .map(|kind| match kind {
            TransferKind::Import => base_command(kind)
                .arg(tokens("input", 'i').required(true))
                .arg(Arg::new("format").long("format")),
            _ => unreachable!(),
        })
        .collect()
}

/// Decode this capability's registered schema without reparsing tokens.
pub fn command_from_matches(root: &ArgMatches) -> Result<TransferCommand, CommandError> {
    let command = parser::command_from_matches(root)?;
    if ![TransferKind::Import].contains(&command.kind) {
        return Err(invalid("Unexpected command for this capability"));
    }
    Ok(command)
}
