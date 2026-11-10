// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{Arg, ArgMatches, Command};
use fabric_cli_import_export_contracts::{
    TransferCommand, TransferKind,
    parser::{self, base_command, flag, tokens},
};
use fabric_cli_output::CommandError;

/// Preserve original root registration order, including deployment composition.
pub fn transfer_subcommands() -> Vec<Command> {
    let mut commands = fabric_cli_export::parser::subcommands();
    commands.extend(fabric_cli_import::parser::subcommands());
    commands.push(
        base_command(TransferKind::Deploy)
            .arg(Arg::new("config").long("config").required(true))
            .arg(
                Arg::new("target_env")
                    .long("target_env")
                    .default_value("N/A"),
            )
            .arg(tokens("params", 'P'))
            .arg(flag("bulk_publish")),
    );
    commands
}

/// Preserve the original decoder and error precedence for all four leaves.
pub fn command_from_matches(root: &ArgMatches) -> Result<TransferCommand, CommandError> {
    parser::command_from_matches(root)
}
