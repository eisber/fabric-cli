// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{
    Arg, ArgAction, Command,
    error::{ContextKind, ContextValue, ErrorKind as ParseErrorKind},
};
use fabric_cli_command_contracts::ValidatedInvocation;
use std::ffi::OsString;

fn help_arg() -> Arg {
    Arg::new("help").long("help").action(ArgAction::Help)
}

/// Build the complete small parser, without any host/provider/configuration effects.
pub fn parser() -> Command {
    Command::new("fab-rs")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Raw API and Azure CLI identity binding. Recognized operations poll by default; --async returns immediately.")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .disable_help_subcommand(true)
        .subcommand_required(true)
        .arg(help_arg())
        .arg(Arg::new("verbose").long("verbose").global(true).action(ArgAction::SetTrue)
            .help("Print debug diagnostics logs to console"))
        .arg(Arg::new("version").long("version").action(ArgAction::Version))
        .subcommand(fabric_cli_auth::command())
        .subcommand(fabric_cli_api::command())
}

pub(super) fn parse_arguments(args: &[OsString]) -> Result<ValidatedInvocation, clap::Error> {
    let mut command = args.iter().skip(1).filter(|arg| *arg != "--verbose");
    if command.next().is_some_and(|arg| arg == "auth") && command.next().is_none() {
        return ValidatedInvocation::parse(
            parser(),
            args.iter().cloned().chain([OsString::from("--help")]),
        );
    }
    ValidatedInvocation::parse(parser(), args.iter().cloned())
}

pub(super) fn command_name(args: &[OsString]) -> &'static str {
    let mut command = args.iter().skip(1).skip_while(|arg| *arg == "--verbose");
    let first = command.next().and_then(|arg| arg.to_str());
    let second = command
        .skip_while(|arg| *arg == "--verbose")
        .next()
        .and_then(|arg| arg.to_str());
    match (first, second) {
        (Some("api"), _) => "api",
        (Some("auth"), Some("login")) => "auth login",
        (Some("auth"), Some("status")) => "auth status",
        (Some("auth"), Some("logout")) => "auth logout",
        (Some("auth"), _) => "auth",
        _ => "",
    }
}

pub(super) fn parsed_command_name(matches: &clap::ArgMatches) -> &'static str {
    match matches.subcommand() {
        Some(("api", _)) => "api",
        Some(("auth", auth)) => match auth.subcommand_name() {
            Some("login") => "auth login",
            Some("status") => "auth status",
            Some("logout") => "auth logout",
            _ => "auth",
        },
        _ => "",
    }
}

pub(super) fn welcome_requested(args: &[OsString]) -> bool {
    args.len() == 1 || (args.len() == 2 && args[1] == "--verbose")
}

pub(super) fn parse_failure_message(command: &str, error: &clap::Error) -> String {
    let defined_arg = || match error.get(ContextKind::InvalidArg) {
        Some(ContextValue::String(arg)) => Some(arg.clone()),
        Some(ContextValue::Strings(args)) if !args.is_empty() => Some(args.join(", ")),
        _ => None,
    };
    let valid_values = || match error.get(ContextKind::ValidValue) {
        Some(ContextValue::Strings(values)) if !values.is_empty() => {
            format!(" Supported values: {}.", values.join(", "))
        }
        _ => String::new(),
    };
    let help = if command.is_empty() {
        "Run fab-rs --help.".to_owned()
    } else {
        format!("Run fab-rs {command} --help.")
    };
    match error.kind() {
        ParseErrorKind::InvalidValue
            if command == "api"
                && matches!(
                    error.get(ContextKind::InvalidArg),
                    Some(ContextValue::String(arg)) if arg == "--method <method>"
                ) =>
        {
            "Invalid HTTP method for --method. Supported values are lowercase: get, post, delete, put, patch. For example, use post instead of POST.".to_owned()
        }
        ParseErrorKind::InvalidValue | ParseErrorKind::ValueValidation => match defined_arg() {
            Some(arg) => format!("Invalid value for {arg}.{} {help}", valid_values()),
            None => format!("Invalid argument value. {help}"),
        },
        // The offending token is user input and may carry credentials; do not echo it.
        ParseErrorKind::UnknownArgument => {
            format!("Unrecognized argument or option. {help}")
        }
        ParseErrorKind::InvalidSubcommand => {
            format!("Unrecognized command. {help}")
        }
        ParseErrorKind::MissingRequiredArgument => match defined_arg() {
            Some(args) => format!("Missing required argument: {args}. {help}"),
            None => format!("A required argument is missing. {help}"),
        },
        ParseErrorKind::MissingSubcommand
        | ParseErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            format!("A subcommand is required. {help}")
        }
        ParseErrorKind::ArgumentConflict => match defined_arg() {
            Some(arg) => format!("{arg} cannot be combined with another supplied option. {help}"),
            None => format!("Conflicting options were supplied. {help}"),
        },
        ParseErrorKind::TooManyValues
        | ParseErrorKind::TooFewValues
        | ParseErrorKind::WrongNumberOfValues => match defined_arg() {
            Some(arg) => format!("Wrong number of values for {arg}. {help}"),
            None => format!("Wrong number of argument values. {help}"),
        },
        ParseErrorKind::NoEquals => match defined_arg() {
            Some(arg) => format!("{arg} requires a value attached with '='. {help}"),
            None => format!("An option requires a value attached with '='. {help}"),
        },
        ParseErrorKind::InvalidUtf8 => format!("Arguments must be valid UTF-8. {help}"),
        _ => format!("Invalid command arguments. {help}"),
    }
}
