// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit v2 Dataflow execution arguments, independent of credentials and I/O.

use clap::{Arg, ArgAction, ArgMatches, Command, error::ErrorKind};
use std::time::Duration;

#[path = "execute_host.rs"]
pub mod host;

#[path = "execute_m.rs"]
mod m;

/// Maximum structured request input, independent of the result budget.
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
/// Hard allocation ceiling for a single native result or encoded output.
pub const MAX_RESULT_BYTES: usize = 64 * 1024 * 1024;
/// Hard row ceiling; exceeding any budget fails without truncation.
pub const MAX_RESULT_ROWS: usize = 1_000_000;

/// Supported result formats, not the native service transport encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultFormat {
    /// Schema-bearing JSON.
    Json,
    /// Delimited UTF-8 data with the shared CSV null convention.
    Csv,
    /// Typed Apache Parquet data.
    Parquet,
}

/// One explicit Dataflow named-query target.
#[derive(Clone, Eq, PartialEq)]
pub struct QueryTarget {
    /// Owning workspace UUID.
    pub workspace: String,
    /// Dataflow item UUID.
    pub dataflow: String,
    /// Exact Power Query name, not SQL or an item job type.
    pub query: String,
}

impl std::fmt::Debug for QueryTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QueryTarget")
            .finish_non_exhaustive()
    }
}

impl QueryTarget {
    /// Parse an absolute, ID-qualified Dataflow query path without discovery.
    ///
    /// # Errors
    /// Rejects names in place of UUIDs, ambiguous paths and unsupported targets.
    pub fn parse(path: &str) -> Result<Self, clap::Error> {
        let parts: Vec<_> = path.split('/').collect();
        let ["", workspace, item, "queries", query] = parts.as_slice() else {
            return Err(invalid());
        };
        let workspace = workspace.strip_suffix(".Workspace").ok_or_else(invalid)?;
        let dataflow = item.strip_suffix(".Dataflow").ok_or_else(invalid)?;
        if !uuid(workspace)
            || !uuid(dataflow)
            || query.trim().is_empty()
            || matches!(*query, "." | "..")
            || query
                .chars()
                .any(|c| c.is_control() || matches!(c, '\\' | '%'))
        {
            return Err(invalid());
        }
        Ok(Self {
            workspace: workspace.into(),
            dataflow: dataflow.into(),
            query: (*query).into(),
        })
    }
}

/// Parsed root execute command. Construction never accesses ambient state.
#[derive(Clone, Debug)]
pub struct ExecuteCommand {
    /// Explicit native execution target.
    pub target: QueryTarget,
    /// Optional JSON input file or a single stdin reader.
    pub input: Option<String>,
    /// Result serialization.
    pub format: ResultFormat,
    /// File destination, or `None` for stdout.
    pub output: Option<String>,
    /// One total submission/observation budget.
    pub timeout: Duration,
    /// Maximum complete returned rows; exceeding this fails, never clips.
    pub max_rows: usize,
    /// Maximum native response and serialized output bytes.
    pub max_bytes: usize,
}

/// An explicit metadata-only query listing; never an evaluation request.
#[derive(Clone, Debug)]
pub struct ListQueriesCommand {
    /// Validated workspace/Dataflow identifiers.
    pub target: fabric_sdk_dataflow::ExecuteQueryTarget,
    /// One total definition retrieval and output budget.
    pub timeout: Duration,
    /// Complete definition-response and JSON output ceiling.
    pub max_bytes: usize,
}

/// Parse metadata-only discovery without accepting ignored execution options.
///
/// # Errors
/// Rejects named-query paths, execution flags and invalid finite budgets.
pub fn list_queries_from_matches(values: &ArgMatches) -> Result<ListQueriesCommand, clap::Error> {
    use clap::parser::ValueSource;
    for field in ["input", "format", "output", "wait", "max_rows"] {
        if values.value_source(field) == Some(ValueSource::CommandLine) {
            return Err(invalid());
        }
    }
    let path = values.get_one::<String>("path").ok_or_else(invalid)?;
    let target = QueryTarget::parse(&format!("{path}/queries/_discovery"))?;
    let max_bytes = *values.get_one::<usize>("max_bytes").ok_or_else(invalid)?;
    if !(1..=16 * 1024 * 1024).contains(&max_bytes) {
        return Err(invalid());
    }
    Ok(ListQueriesCommand {
        target: fabric_sdk_dataflow::ExecuteQueryTarget::new(&target.workspace, &target.dataflow)
            .map_err(|_| invalid())?,
        timeout: duration(values.get_one::<String>("timeout").ok_or_else(invalid)?)?,
        max_bytes,
    })
}

/// Additive root command. Existing v1 command schemas remain unchanged.
pub fn command() -> Command {
    Command::new("execute")
        .args_override_self(false)
        .about("Execute an explicitly addressed Dataflow Power Query (v2 preview)")
        .after_help("Evaluation target: /<workspace-id>.Workspace/<dataflow-id>.Dataflow/queries/<query-name>. Requires explicit approval and --wait. Input JSON: optional customMashupDocument OR mExpression, plus arrowVersion (1 or 2). Discovery: execute /<workspace-id>.Workspace/<dataflow-id>.Dataflow --list-queries; returns query metadata as JSON without evaluating M. No automatic connection changes or query replay. Detailed refresh-log download remains an API gap.")
        .arg(Arg::new("path").required(true))
        .arg(Arg::new("input").long("input"))
        .arg(Arg::new("format").long("format").value_parser(["json", "csv", "parquet"]).default_value("json"))
        .arg(Arg::new("output").long("output").short('o'))
        .arg(Arg::new("wait").long("wait").action(ArgAction::SetTrue).required_unless_present("list_queries"))
        .arg(Arg::new("list_queries").long("list-queries").action(ArgAction::SetTrue).conflicts_with("wait"))
        .arg(Arg::new("timeout").long("timeout").default_value("5m"))
        .arg(Arg::new("max_rows").long("limit").value_parser(clap::value_parser!(usize)).default_value("100000"))
        .arg(Arg::new("max_bytes").long("max-bytes").value_parser(clap::value_parser!(usize)).default_value("16777216"))
}

/// Convert schema-checked values into an execution contract.
///
/// # Errors
/// Rejects unsupported output/target combinations and invalid finite budgets.
pub fn from_matches(values: &ArgMatches) -> Result<ExecuteCommand, clap::Error> {
    let target = QueryTarget::parse(values.get_one::<String>("path").ok_or_else(invalid)?)?;
    let format = match values.get_one::<String>("format").map(String::as_str) {
        Some("json") => ResultFormat::Json,
        Some("csv") => ResultFormat::Csv,
        Some("parquet") => ResultFormat::Parquet,
        _ => return Err(invalid()),
    };
    let output = values
        .get_one::<String>("output")
        .filter(|value| value.as_str() != "-")
        .cloned();
    if output.as_ref().is_some_and(|path| path.is_empty())
        || (format == ResultFormat::Parquet && output.is_none())
    {
        return Err(invalid());
    }
    let timeout = duration(values.get_one::<String>("timeout").ok_or_else(invalid)?)?;
    let max_rows = *values.get_one::<usize>("max_rows").ok_or_else(invalid)?;
    let max_bytes = *values.get_one::<usize>("max_bytes").ok_or_else(invalid)?;
    if max_rows == 0
        || max_rows > MAX_RESULT_ROWS
        || max_bytes == 0
        || max_bytes > MAX_RESULT_BYTES
        || values
            .get_one::<String>("input")
            .is_some_and(String::is_empty)
        || values.contains_id("output_format")
    {
        return Err(invalid());
    }
    Ok(ExecuteCommand {
        target,
        input: values.get_one::<String>("input").cloned(),
        format,
        output,
        timeout,
        max_rows,
        max_bytes,
    })
}

fn duration(value: &str) -> Result<Duration, clap::Error> {
    let (digits, factor) = if let Some(value) = value.strip_suffix("ms") {
        (value, 1)
    } else if let Some(value) = value.strip_suffix('s') {
        (value, 1_000)
    } else if let Some(value) = value.strip_suffix('m') {
        (value, 60_000)
    } else if let Some(value) = value.strip_suffix('h') {
        (value, 3_600_000)
    } else {
        return Err(invalid());
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let millis = digits
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(factor))
        .filter(|value| *value > 0 && *value <= 86_400_000)
        .ok_or_else(invalid)?;
    Ok(Duration::from_millis(millis))
}

fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn invalid() -> clap::Error {
    clap::Error::raw(
        ErrorKind::InvalidValue,
        "Invalid execution arguments. Use an ID-qualified Dataflow query path, finite budgets, and an advertised result format.",
    )
}
