// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Python-named arguments for the bounded JSON read preview.

use clap::{Arg, ArgAction, Command, error::ErrorKind};
use std::ffi::OsString;

/// A supported read operation, not a new resource namespace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadKind {
    /// Tenant workspace listing or workspace item listing.
    List,
    /// Generic item metadata retrieval.
    Get,
}

/// Parsed Python read-command arguments; unsupported options are never ignored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadCommand {
    /// Operation selected by the command name.
    pub kind: ReadKind,
    /// Original spelling, including the Python `dir` alias.
    pub name: String,
    /// Path tokens, joined and quote-stripped by the existing context resolver.
    pub path: Vec<String>,
    /// Joined JMESPath expression, when supplied.
    pub query: Option<String>,
    /// Python `ls --long`.
    pub long: bool,
    /// Python `ls --all`.
    pub all: bool,
    /// Python `get --output`, recognized but not implemented.
    pub output: Option<String>,
    /// Python `get --verbose`.
    pub verbose: bool,
    /// Python `get --force`, recognized but not implemented.
    pub force: bool,
    /// Explicit global output override; only JSON execution is implemented.
    pub output_format: Option<String>,
}

/// A parse result with help/version separated from service execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Invocation {
    /// Help/version text, written to stdout without configuring a host.
    Display(String),
    /// A command requiring explicitly supplied dependencies.
    Read(ReadCommand),
}

/// Static diagnostic for invalid invocations; supplied values are never echoed.
pub const INVALID_ARGUMENTS: &str =
    "Only --help, --version, ls, dir, and get are recognized in this preview.";

/// Build the public parser without reading configuration or the environment.
pub fn command() -> Command {
    Command::new("fab-rs")
        .bin_name("fab-rs")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Experimental bounded JSON reads for Microsoft Fabric")
        .after_help("Read execution requires a caller-configured library host. The binary has no production authentication or HTTP transport. No login or deployment commands are available.")
        .arg_required_else_help(true)
        .arg(
            Arg::new("output_format")
                .long("output_format")
                .global(true)
                .value_parser(["json", "text"]),
        )
        .subcommand(
            Command::new("ls")
                .visible_alias("dir")
                .about("List workspaces or workspace items")
                .arg(Arg::new("path").num_args(0..))
                .arg(Arg::new("long").short('l').long("long").action(ArgAction::SetTrue))
                .arg(Arg::new("all").short('a').long("all").action(ArgAction::SetTrue))
                .arg(Arg::new("query").short('q').long("query")),
        )
        .subcommand(
            Command::new("get")
                .about("Get item properties (bounded generic metadata)")
                .arg(Arg::new("path").num_args(1..).required(true))
                .arg(Arg::new("query").short('q').long("query").num_args(1..))
                .arg(Arg::new("output").short('o').long("output").num_args(1..))
                .arg(Arg::new("verbose").short('v').long("verbose").action(ArgAction::SetTrue))
                .arg(Arg::new("force").short('f').long("force").action(ArgAction::SetTrue)),
        )
}

/// Parse arguments including argv[0], without printing or exiting.
///
/// Invalid-input errors are retained for programmatic classification. Executable
/// callers must use the static diagnostic rather than echoing Clap's raw error.
pub fn parse_args<I, T>(args: I) -> Result<Invocation, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let matches = match command().try_get_matches_from(&args) {
        Ok(matches) => matches,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            return Ok(Invocation::Display(error.to_string()));
        }
        Err(error) => return Err(error),
    };
    let Some((name, values)) = matches.subcommand() else {
        return Err(clap::Error::raw(
            ErrorKind::MissingSubcommand,
            INVALID_ARGUMENTS,
        ));
    };
    let list = name == "ls";
    let spelling = args
        .iter()
        .find(|arg| matches!(arg.to_str(), Some("ls" | "dir" | "get")));
    let name = if list && spelling.is_some_and(|arg| arg == "dir") {
        "dir"
    } else {
        name
    };
    let joined = |key: &str| {
        values.get_many::<String>(key).map(|values| {
            values
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
                .trim_matches(['\'', '"'])
                .to_owned()
        })
    };
    Ok(Invocation::Read(ReadCommand {
        kind: if list { ReadKind::List } else { ReadKind::Get },
        name: name.into(),
        path: values
            .get_many::<String>("path")
            .map(|values| values.cloned().collect())
            .unwrap_or_default(),
        query: joined("query"),
        long: list && values.get_flag("long"),
        all: list && values.get_flag("all"),
        output: if list { None } else { joined("output") },
        verbose: !list && values.get_flag("verbose"),
        force: !list && values.get_flag("force"),
        output_format: matches.get_one::<String>("output_format").cloned(),
    }))
}
