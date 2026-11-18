// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{
    Arg, ArgAction, Command,
    error::{ContextKind, ContextValue, ErrorKind as ParseErrorKind},
};
use fabric_cli_command_contracts::ValidatedInvocation;
use std::ffi::OsString;

pub(crate) fn help_arg() -> Arg {
    Arg::new("help").long("help").action(ArgAction::Help)
}

/// Build the complete small parser, without any host/provider/configuration effects.
pub fn parser() -> Command {
    Command::new(crate::EXECUTABLE_NAME)
        .version(env!("CARGO_PKG_VERSION"))
        .about("Raw API and Azure CLI identity binding. Recognized operations poll by default; --async returns immediately.")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .disable_help_subcommand(true)
        .subcommand_required(true)
        .arg(help_arg())
        .arg(Arg::new("verbose").long("verbose").global(true).action(ArgAction::SetTrue)
            .help("Print debug diagnostics logs to console"))
        .arg(Arg::new("skill").long("skill").global(true).hide(true))
        .arg(Arg::new("version").long("version").action(ArgAction::Version))
        .subcommand(fabric_cli_auth::command())
        .subcommand(fabric_cli_api::command())
        .subcommand(crate::resources::command("workspace"))
        .subcommand(crate::resources::command("item"))
}

pub(crate) fn parse_arguments(args: &[OsString]) -> Result<ValidatedInvocation, clap::Error> {
    let command = command_tokens(args);
    if command.is_some_and(|tokens| tokens.len() == 1 && tokens[0] == "auth") {
        return ValidatedInvocation::parse(
            parser(),
            args.iter().cloned().chain([OsString::from("--help")]),
        );
    }
    ValidatedInvocation::parse(parser(), args.iter().cloned())
}

pub(super) fn command_name(args: &[OsString]) -> &'static str {
    let tokens = command_tokens(args).unwrap_or_default();
    let first = tokens.first().and_then(|arg| arg.to_str());
    let second = tokens.get(1).and_then(|arg| arg.to_str());
    match (first, second) {
        (Some("api"), _) => "api",
        (Some("auth"), Some("login")) => "auth login",
        (Some("auth"), Some("status")) => "auth status",
        (Some("auth"), Some("logout")) => "auth logout",
        (Some("auth"), _) => "auth",
        (Some(kind @ ("workspace" | "item")), action) => resource_name(kind, action),
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
        Some((kind @ ("workspace" | "item"), args)) => resource_name(kind, args.subcommand_name()),
        _ => "",
    }
}

fn resource_name(kind: &str, action: Option<&str>) -> &'static str {
    match (kind, action) {
        ("workspace", Some("list")) => "workspace list",
        ("workspace", Some("get")) => "workspace get",
        ("workspace", Some("create")) => "workspace create",
        ("workspace", Some("update")) => "workspace update",
        ("workspace", Some("delete")) => "workspace delete",
        ("item", Some("list")) => "item list",
        ("item", Some("get")) => "item get",
        ("item", Some("create")) => "item create",
        ("item", Some("update")) => "item update",
        ("item", Some("delete")) => "item delete",
        ("workspace", _) => "workspace",
        ("item", _) => "item",
        _ => "",
    }
}

pub(crate) fn welcome_requested(args: &[OsString]) -> bool {
    command_tokens(args).is_some_and(|tokens| tokens.is_empty())
        && parser()
            .subcommand_required(false)
            .try_get_matches_from(args)
            .is_ok()
}

// Inspect only command/global-option positions, never treating a skill value
// as a command, or an endpoint after "--" as a global flag.
fn command_tokens(args: &[OsString]) -> Option<Vec<&std::ffi::OsStr>> {
    let mut args = args.iter().skip(1).map(OsString::as_os_str);
    let mut tokens = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--verbose" {
            continue;
        }
        if arg == "--skill" {
            let value = args.next()?;
            if value.to_str().is_some_and(|value| value.starts_with('-')) {
                return None;
            }
            continue;
        }
        if arg
            .to_str()
            .is_some_and(|value| value.starts_with("--skill="))
        {
            continue;
        }
        tokens.push(arg);
        if !matches!(arg.to_str(), Some("auth" | "workspace" | "item")) || tokens.len() != 1 {
            tokens.extend(args);
            break;
        }
    }
    Some(tokens)
}

pub(super) fn parse_failure_message(command: &str, error: &clap::Error) -> String {
    if command == "api"
        && let Some(message) = fabric_cli_api::parse_failure_message(error)
    {
        return message.to_owned();
    }
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
        format!("Run {} --help.", crate::EXECUTABLE_NAME)
    } else {
        format!("Run {} {command} --help.", crate::EXECUTABLE_NAME)
    };
    match error.kind() {
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
