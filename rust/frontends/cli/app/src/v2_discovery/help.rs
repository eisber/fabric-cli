// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive local human-help projection; no data-schema provider is consulted.

use super::{DiscoveryFailure, path_argument};
use clap::{Arg, ArgMatches, Command};
use fabric_sdk_discovery::{Operation, TargetSelector, help::TargetHelp};

pub(super) fn command() -> Command {
    Command::new("help")
        .about("Explain exact registered target capabilities locally; no identity or permission probe")
        .after_help("Only registered resource contracts are covered. Unregistered representations never use a parent contract. Docs search is not implemented; use structured schemas for machine discovery.")
        .arg(path_argument())
        .arg(
            Arg::new("verb")
                .long("verb")
                .value_parser(Operation::ALL.map(Operation::as_str))
                .help("Narrow to an exact registered operation without executing it"),
        )
}

pub(super) fn parse(values: &ArgMatches) -> Result<TargetHelp, DiscoveryFailure> {
    let path = values
        .get_one::<String>("path")
        .ok_or(DiscoveryFailure::InvalidArguments)?;
    let target = TargetSelector::parse(path).map_err(DiscoveryFailure::Help)?;
    let verb = values
        .get_one::<String>("verb")
        .map(|value| value.parse::<Operation>())
        .transpose()
        .map_err(DiscoveryFailure::Help)?;
    TargetHelp::new(&target, verb).map_err(DiscoveryFailure::Help)
}
