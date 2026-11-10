// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared source argument conventions, without capability schemas or dispatch.

use crate::CommandError;
use clap::{Arg, ArgAction, ArgMatches, Command};
use fabric_cli_output::text::OutputFormat;

/// A focused operation and its common source presentation/header arguments.
#[derive(Clone, Debug)]
pub struct ParsedCommand<T> {
    /// Capability-owned operation after argument normalization.
    pub kind: T,
    /// Explicit override; absence leaves presentation to the caller.
    pub output_format: Option<OutputFormat>,
    /// Hidden source header option; an unconfigured host rejects it explicitly.
    pub skill: Option<String>,
}

impl<T> ParsedCommand<T> {
    /// Parses shared options after the capability has validated its operation.
    pub fn new(kind: T, args: &ArgMatches) -> Result<Self, CommandError> {
        let output_format = match string(args, "output_format").as_deref() {
            None => None,
            Some("json") => Some(OutputFormat::Json),
            Some("text") => Some(OutputFormat::Text),
            _ => return Err(invalid()),
        };
        Ok(Self {
            kind,
            output_format,
            skill: string(args, "skill"),
        })
    }

    /// Maps only the operation, preserving already-parsed shared arguments.
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> ParsedCommand<U> {
        ParsedCommand {
            kind: map(self.kind),
            output_format: self.output_format,
            skill: self.skill,
        }
    }
}

/// Builds the common source leaf options without choosing a capability.
pub fn leaf(name: &'static str) -> Command {
    Command::new(name)
        .args_override_self(true)
        .infer_long_args(true)
        .arg(
            Arg::new("output_format")
                .long("output_format")
                .value_parser(["json", "text"]),
        )
        .arg(Arg::new("skill").long("skill").hide(true))
}

/// Builds a source boolean flag with an optional short spelling.
pub fn flag(name: &'static str, short: Option<char>) -> Arg {
    let arg = Arg::new(name).long(name).action(ArgAction::SetTrue);
    if let Some(short) = short {
        arg.short(short)
    } else {
        arg
    }
}

/// Builds the source scalar or token-joined query argument.
pub fn query(many: bool) -> Arg {
    let arg = Arg::new("query").long("query").short('q');
    if many { arg.num_args(1..) } else { arg }
}

/// Builds an optional source token-list option.
pub fn tokens(name: &'static str, short: char) -> Arg {
    Arg::new(name).long(name).short(short).num_args(1..)
}

/// Retains the original aggregate's invalid-argument error for every capability.
pub fn invalid() -> CommandError {
    CommandError::new("InvalidInput", "Invalid access command arguments")
}

/// Reads an optional scalar without panicking on a foreign argument schema.
pub fn string(args: &ArgMatches, name: &str) -> Option<String> {
    args.try_get_one::<String>(name).ok().flatten().cloned()
}

/// Reads source tokens, preserving order and treating absent values as empty.
pub fn values(args: &ArgMatches, name: &str) -> Vec<String> {
    args.try_get_many::<String>(name)
        .ok()
        .flatten()
        .map(|values| values.cloned().collect())
        .unwrap_or_default()
}

/// Reads a source flag, treating absent or foreign-schema flags as false.
pub fn boolean(args: &ArgMatches, name: &str) -> bool {
    args.try_get_one::<bool>(name)
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}

/// Joins source tokens and strips edge quotes, preserving present-empty values.
pub fn joined(args: &ArgMatches, name: &str) -> Option<String> {
    args.try_get_many::<String>(name)
        .ok()
        .flatten()
        .map(|values| {
            values
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
                .trim_matches(['"', '\''])
                .to_owned()
        })
}
