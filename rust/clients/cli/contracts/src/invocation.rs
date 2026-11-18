// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::CommandError;
use clap::{ArgMatches, Command};
use std::ffi::OsString;

/// Executable input accepted by the root CLI grammar, including global options.
/// Help, version, missing commands, and invalid syntax never produce this type.
///
/// Raw matches cannot be passed directly to a command runner:
///
/// ```compile_fail
/// use clap::ArgMatches;
/// use fabric_cli_command_contracts::ValidatedInvocation;
///
/// let _ = ValidatedInvocation(ArgMatches::default());
/// ```
pub struct ValidatedInvocation(ArgMatches);

impl ValidatedInvocation {
    /// Parse the registered CLI grammar and require an executable subcommand.
    pub fn parse(
        parser: Command,
        args: impl IntoIterator<Item = OsString>,
    ) -> Result<Self, clap::Error> {
        parser
            .subcommand_required(true)
            .try_get_matches_from(args)
            .map(Self)
    }

    /// Return the root parser's validated matches for naming and dispatch.
    pub fn matches(&self) -> &ArgMatches {
        &self.0
    }

    /// Obtain the argument matches only for the requested command family.
    pub fn command(&self, family: &str) -> Result<&ArgMatches, CommandError> {
        match self.0.subcommand() {
            Some((name, matches)) if name == family => Ok(matches),
            _ => Err(CommandError::new(
                "InvalidInput",
                "Invocation does not match the command",
            )),
        }
    }
}
