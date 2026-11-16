// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::error::ErrorKind;
use fabric_cli::parser::{command, invalid_arguments_message};
use fabric_cli_output::text::OutputFormat;
use std::{ffi::OsString, io, io::Write, process::ExitCode};

pub(crate) fn missing_target(
    arguments: &[OsString],
    error: &clap::Error,
) -> Option<io::Result<ExitCode>> {
    if error.kind() != ErrorKind::MissingRequiredArgument {
        return None;
    }
    // Reparse with only these path requirements relaxed. Unknown options, bad
    // values and other missing arguments must retain the ordinary parser error.
    let matches = command()
        .mut_subcommand("acl", |acl| {
            acl.mut_subcommand("ls", |leaf| {
                leaf.mut_arg("path", |path| path.required(false))
            })
            .mut_subcommand("get", |leaf| {
                leaf.mut_arg("path", |path| path.required(false))
            })
        })
        .try_get_matches_from(arguments)
        .ok()?;
    let (leaf, args) = matches.subcommand_matches("acl")?.subcommand()?;
    if !matches!(leaf, "ls" | "get") || args.contains_id("path") {
        return None;
    }
    let explicit = args.get_one::<String>("output_format").map(String::as_str);
    Some(write(leaf, explicit, error))
}

fn format(explicit: Option<&str>) -> Result<OutputFormat, ()> {
    if let Some(explicit) = explicit {
        return OutputFormat::resolve(Some(explicit), None).map_err(|_| ());
    }
    let settings = crate::local_host::available_store()
        .map_err(|_| ())?
        .map(|store| store.read())
        .transpose()
        .map_err(|_| ())?;
    let saved = settings.as_ref().and_then(|settings| {
        settings
            .get("output_format")
            .or_else(|| settings.get("fab_output_format"))
    });
    match saved {
        None => Ok(OutputFormat::Text),
        Some(value) => OutputFormat::resolve(None, value.as_str()).map_err(|_| ()),
    }
}

fn write(leaf: &str, explicit: Option<&str>, error: &clap::Error) -> io::Result<ExitCode> {
    match format(explicit) {
        Ok(OutputFormat::Json) => super::write_error(
            super::CommandError {
                code: "invalid_arguments",
                message: invalid_arguments_message(error),
            },
            2,
        ),
        Ok(OutputFormat::Text) => {
            let mut stderr = io::stderr().lock();
            writeln!(
                stderr,
                "Error: missing ACL target.\nUsage: fab-rs acl {leaf} <path> [OPTIONS]\nExample: fab-rs acl {leaf} 'Demo.Workspace'\nDiscover workspaces: fab-rs ls /\nRun fab-rs acl {leaf} --help for supported targets."
            )?;
            Ok(ExitCode::from(2))
        }
        Err(()) => super::write_error(
            super::CommandError {
                code: "InvalidInput",
                message: "Cannot read ACL diagnostic output settings; use --output_format text or json explicitly".into(),
            },
            2,
        ),
    }
}
