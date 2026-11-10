// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Canonical catalog FIND schema and source argument normalization.

use crate::{CommandError, FindOptions};
use clap::{Arg, ArgMatches, Command};
use fabric_cli_command_contracts::parser::{
    ParsedCommand, boolean, flag, invalid, joined, leaf, query, string,
};

/// Source root FIND leaf. `-P` is optional-scalar, unlike raw API parameters.
pub fn find_subcommand() -> Command {
    leaf("find")
        .about("Search the OneLake catalog for items")
        .arg(Arg::new("search_text").required(true))
        .arg(Arg::new("params").long("params").short('P').num_args(0..=1))
        .arg(flag("long", Some('l')))
        .arg(query(true))
}

/// Registers FIND without changing the centrally selected root parser policy.
pub fn register(root: Command) -> Command {
    root.subcommand(find_subcommand())
}

/// Parses only FIND root matches without file, credential or service access.
pub fn from_matches(root: &ArgMatches) -> Result<Option<ParsedCommand<FindOptions>>, CommandError> {
    let Some(("find", group)) = root.subcommand() else {
        return Ok(None);
    };
    let kind = FindOptions {
        search: string(group, "search_text").ok_or_else(invalid)?,
        params: string(group, "params"),
        long: boolean(group, "long"),
        query: joined(group, "query"),
    };
    ParsedCommand::new(kind, group).map(Some)
}
