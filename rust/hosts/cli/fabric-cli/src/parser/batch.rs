// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source `main.py` batch ordering, independent of runtime, state and streams.

use super::{Invocation, command, parse_args};
use std::{ffi::OsString, fmt};

pub(super) fn has_root_option(arguments: &[OsString]) -> bool {
    let mut index = 1;
    while let Some(value) = arguments.get(index).and_then(|value| value.to_str()) {
        match value {
            "-c" | "--command" => return true,
            "--output_format" => index += 2,
            "--version" | "-v" | "-V" => index += 1,
            value if value.starts_with("--command=") || value.starts_with("-c") => return true,
            value if value.starts_with("--output_format=") => index += 1,
            _ => return false,
        }
    }
    false
}

/// Source command strings and the outer summary's explicit format override.
#[derive(Clone, Eq, PartialEq)]
pub struct BatchInvocation {
    /// Unmodified strings, including empty entries counted in the source summary.
    pub commands: Vec<String>,
    /// Applies to the summary, never inherited by individual command parsers.
    pub output_format: Option<String>,
}

impl fmt::Debug for BatchInvocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BatchInvocation")
            .field("command_count", &self.commands.len())
            .finish_non_exhaustive()
    }
}

/// Whether execution returned normally or terminated the source command loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOutcome {
    /// A handled command result, including nonzero failures; continue in order.
    Returned(u8),
    /// Help's parser exit, cancellation, or another process-terminating failure.
    Stop(u8),
}

/// Caller-owned dispatcher, mutable session state and ordered output effects.
pub trait BatchHost {
    /// Host execution/output failures propagate without replay or a summary.
    type Error;

    /// Parse and dispatch one whitespace-split command using the existing handler.
    fn execute(&mut self, arguments: &[&str]) -> Result<CommandOutcome, Self::Error>;

    /// Write the grey separator to stderr, after a returned non-final raw entry.
    fn separator(&mut self) -> Result<(), Self::Error>;

    /// Write two stdout newlines and a success summary using the raw list length.
    fn summary(&mut self, invocation: &BatchInvocation) -> Result<(), Self::Error>;
}

/// Run the pinned source loop, preserving empty-entry, error and last-exit quirks.
///
/// This deliberately does not use shell lexing: quotes, backslashes, semicolons,
/// pipes and newlines have exactly Python `str.split()`'s treatment. The host
/// retains state across callbacks and owns cancellation and parser-exit policy.
pub fn run<H: BatchHost>(invocation: &BatchInvocation, host: &mut H) -> Result<u8, H::Error> {
    let mut executed = 0;
    let mut last_exit = 0;
    for (index, command) in invocation.commands.iter().enumerate() {
        let arguments = split(command);
        if arguments.is_empty() {
            continue;
        }
        match host.execute(&arguments)? {
            CommandOutcome::Stop(exit) => return Ok(exit),
            CommandOutcome::Returned(exit) => last_exit = exit,
        }
        executed += 1;
        if index != invocation.commands.len() - 1 {
            host.separator()?;
        }
    }
    if executed > 1 {
        host.summary(invocation)?;
    }
    Ok(last_exit)
}

/// Split on Python's Unicode whitespace set, including U+001C through U+001F.
pub fn split(command: &str) -> Vec<&str> {
    command
        .split(|character: char| {
            character.is_whitespace() || matches!(character, '\u{1c}'..='\u{1f}')
        })
        .filter(|part| !part.is_empty())
        .collect()
}

/// Per-command parsing distinguishes unknown source dictionary keys from misuse.
#[derive(Debug)]
pub enum CommandParseError {
    /// Source raises `KeyError` before invoking a subparser; do not echo the key.
    UnknownCommand,
    /// A registered command rejected its arguments; retain sanitized diagnostics.
    Arguments(clap::Error),
}

/// Parse one batch entry with the existing schema, never accepting nested `-c`.
///
/// The returned boolean identifies an explicit help/version parser exit. Bare
/// source groups instead return normally and count as executed commands.
pub fn parse_command(arguments: &[&str]) -> Result<(Invocation, bool), CommandParseError> {
    let Some(name) = arguments.first() else {
        return Err(CommandParseError::UnknownCommand);
    };
    if command().find_subcommand(name).is_none() {
        return Err(CommandParseError::UnknownCommand);
    }
    let invocation = parse_args(std::iter::once("fab-rs").chain(arguments.iter().copied()))
        .map_err(CommandParseError::Arguments)?;
    let exits = matches!(&invocation, Invocation::Display(_))
        && command()
            .try_get_matches_from(std::iter::once("fab-rs").chain(arguments.iter().copied()))
            .is_err_and(|error| {
                matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                )
            });
    Ok((invocation, exits))
}
