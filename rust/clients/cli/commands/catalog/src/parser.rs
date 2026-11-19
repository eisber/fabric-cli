// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Catalog FIND grammar and source-compatible typed argument conversion.

use crate::{CommandError, FindOptions};
use clap::{Arg, ArgMatches, Command};
use fabric_cli_command_contracts::parser::{
    ParsedCommand, boolean, flag, invalid, joined, leaf, query, string,
};

/// Source FIND leaf; `-P` remains optional-scalar rather than API token pairs.
pub fn find_subcommand() -> Command {
    leaf("find")
        .about("Search the OneLake catalog for items")
        .arg(Arg::new("search_text").required(true))
        .arg(Arg::new("params").long("params").short('P').num_args(0..=1))
        .arg(flag("long", Some('l')))
        .arg(query(true))
}

/// Mounts FIND without changing root parser policy.
pub fn register(root: Command) -> Command {
    root.subcommand(find_subcommand())
}

/// Converts FIND matches without configuration, credentials or service access.
pub fn from_matches(root: &ArgMatches) -> Result<Option<ParsedCommand<FindOptions>>, CommandError> {
    let Some(("find", group)) = root.subcommand() else {
        return Ok(None);
    };
    ParsedCommand::new(
        FindOptions {
            search: string(group, "search_text").ok_or_else(invalid)?,
            params: string(group, "params"),
            long: boolean(group, "long"),
            query: joined(group, "query"),
        },
        group,
    )
    .map(Some)
}
