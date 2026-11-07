// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{Arg, ArgAction, ArgMatches, Command};

/// One explicit ID-only target. Call `fabric_cli::cancel::preflight` before I/O.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancelCommand {
    /// Absolute typed Notebook run path, not resolved against current context.
    pub path: String,
    /// Optional v2 format; only JSON is supported.
    pub format: Option<String>,
    /// An inherited v1 override is explicitly rejected, including JSON.
    pub output_format: Option<String>,
}

pub(super) fn command() -> Command {
    Command::new("cancel")
        .args_override_self(false)
        .about("Request cancellation of one Notebook run (accepted is not cancelled)")
        .arg(Arg::new("path").required(true).num_args(1))
        .arg(Arg::new("format").long("format").num_args(1).action(ArgAction::Append))
        .after_help("Usage: fab-rs cancel /<workspace-guid>.Workspace/<notebook-guid>.Notebook/runs/<run-guid> [--format json]\nJSON is the default and only format. Legacy --output_format is not supported. No waiting, name resolution, bulk operations or automatic replay is performed.")
}

pub(super) fn parse(root: &ArgMatches, values: &ArgMatches) -> Result<CancelCommand, clap::Error> {
    let mut formats = values.get_many::<String>("format").into_iter().flatten();
    let format = formats.next().cloned();
    if formats.next().is_some() {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ArgumentConflict,
            super::INVALID_ARGUMENTS,
        ));
    }
    Ok(CancelCommand {
        path: values
            .get_one::<String>("path")
            .expect("required path")
            .clone(),
        format,
        output_format: root
            .get_one::<String>("output_format")
            .cloned()
            .or_else(|| values.get_one::<String>("output_format").cloned()),
    })
}
