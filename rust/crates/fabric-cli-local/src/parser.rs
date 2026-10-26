// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{Arg, ArgMatches, Command};

use crate::{LocalCommand, LocalError, LocalInvocation};

/// Family-owned registrations for the single central Clap front door.
///
/// This first slice registers only implemented leaves, not future placeholders.
pub fn root_commands() -> Vec<Command> {
    vec![
        leaf("version").about("Show the CLI version"),
        Command::new("config").about("Manage CLI configuration")
            .arg_required_else_help(true)
            .subcommand(leaf("set").arg(Arg::new("key").required(true)).arg(Arg::new("value").required(true)))
            .subcommand(leaf("get").arg(Arg::new("key").required(true)))
            .subcommand(leaf("ls").visible_alias("dir"))
            .subcommand(leaf("clear-cache")),
    ]
}

/// Convert family matches without I/O; return `None` for another family's root.
pub fn from_matches(matches: &ArgMatches) -> Result<Option<LocalInvocation>, LocalError> {
    let Some((name, args)) = matches.subcommand() else { return Ok(None); };
    let command = match name {
        "version" => LocalCommand::Version,
        "config" => match args.subcommand() {
            Some(("set", args)) => LocalCommand::ConfigSet { key: value(args, "key")?, value: value(args, "value")? },
            Some(("get", args)) => LocalCommand::ConfigGet { key: value(args, "key")? },
            Some(("ls" | "dir", _)) => LocalCommand::ConfigList,
            Some(("clear-cache", _)) => LocalCommand::ClearCache,
            _ => return Err(LocalError::command("InvalidInput", "A configuration subcommand is required")),
        },
        _ => return Ok(None),
    };
    let leaf_args = if name == "config" {
        args.subcommand().map(|(_, args)| args).ok_or_else(|| LocalError::command("InvalidInput", "A configuration subcommand is required"))?
    } else {
        args
    };
    Ok(Some(LocalInvocation {
        command,
        output_format: leaf_args.get_one::<String>("output_format").cloned(),
        skill: leaf_args.get_one::<String>("skill").cloned(),
    }))
}

fn leaf(name: &'static str) -> Command {
    Command::new(name).args_override_self(true).infer_long_args(true)
        .arg(Arg::new("output_format").long("output_format").value_parser(["json", "text"]))
        .arg(Arg::new("skill").long("skill").hide(true))
}

fn value(args: &ArgMatches, key: &str) -> Result<String, LocalError> {
    args.try_get_one::<String>(key).ok().flatten().cloned()
        .ok_or_else(|| LocalError::command("InvalidInput", "Missing required local command argument"))
}
