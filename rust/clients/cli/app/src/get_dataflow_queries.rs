// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit metadata-only Dataflow query discovery owned by the GET surface.

use clap::{Arg, ArgAction, ArgMatches, Command, builder::ArgPredicate, error::ErrorKind};
use std::time::Duration;

/// One bounded definition metadata read, never a query evaluation.
#[derive(Clone, Debug)]
pub struct ListQueriesCommand {
    /// Validated explicit workspace and Dataflow identifiers.
    pub target: fabric_sdk_dataflow::ExecuteQueryTarget,
    /// Total credential, definition observation and output deadline.
    pub timeout: Duration,
    /// Complete definition response and JSON output ceiling.
    pub max_bytes: usize,
    /// Optional local JMESPath projection over the query inventory.
    pub query: Option<String>,
}

pub(crate) fn register(command: Command) -> Command {
    command
        .arg(
            Arg::new("list_queries")
                .long("list-queries")
                .action(ArgAction::SetTrue)
                .help("Read a Dataflow's named-query metadata without evaluating M (JSON stdout)"),
        )
        .arg(
            Arg::new("query_timeout")
                .long("timeout")
                .requires("list_queries")
                .default_value_if("list_queries", ArgPredicate::IsPresent, Some("5m"))
                .help("Total query-discovery deadline; requires --list-queries"),
        )
        .arg(
            Arg::new("query_max_bytes")
                .long("max-bytes")
                .requires("list_queries")
                .value_parser(clap::value_parser!(usize))
                .default_value_if("list_queries", ArgPredicate::IsPresent, Some("16777216"))
                .help("Complete discovery response/output bound, at most 16 MiB"),
        )
}

pub(crate) fn from_matches(values: &ArgMatches) -> Result<ListQueriesCommand, clap::Error> {
    let invalid = || {
        clap::Error::raw(
            ErrorKind::InvalidValue,
            "Invalid Dataflow GET discovery arguments.",
        )
    };
    for field in ["output", "verbose", "force"] {
        if values.value_source(field) == Some(clap::parser::ValueSource::CommandLine) {
            return Err(invalid());
        }
    }
    let paths = values
        .get_many::<String>("path")
        .ok_or_else(invalid)?
        .collect::<Vec<_>>();
    let [path] = paths.as_slice() else {
        return Err(invalid());
    };
    let crate::get_target::GetTarget::Item {
        workspace,
        item,
        item_type: Some(kind),
        representation: crate::get_target::GetRepresentation::Metadata,
    } = crate::get_target::GetTarget::parse(path).map_err(|_| invalid())?
    else {
        return Err(invalid());
    };
    if kind != "Dataflow" {
        return Err(invalid());
    }
    let target =
        fabric_sdk_dataflow::ExecuteQueryTarget::new(&workspace, &item).map_err(|_| invalid())?;
    let max_bytes = *values
        .get_one::<usize>("query_max_bytes")
        .ok_or_else(invalid)?;
    if !(1..=16 * 1024 * 1024).contains(&max_bytes) {
        return Err(invalid());
    }
    let query = values.get_many::<String>("query").map(|values| {
        fabric_cli_query::join_query_args(&values.map(String::as_str).collect::<Vec<_>>())
    });
    if let Some(query) = &query {
        fabric_cli_query::validate_projection(query).map_err(|_| invalid())?;
    }
    Ok(ListQueriesCommand {
        target,
        timeout: crate::execute::duration(
            values
                .get_one::<String>("query_timeout")
                .ok_or_else(invalid)?,
        )?,
        max_bytes,
        query,
    })
}
