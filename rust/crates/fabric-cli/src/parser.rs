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
    /// Scalar LS expression or joined/quote-stripped GET tokens, when supplied.
    /// `Some("")` retains a present empty argument, separately from absence.
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

/// Build the Clap schema/help without reading configuration or the environment.
///
/// Use [`parse_args`] for the source read-command lexical rules as well.
pub fn command() -> Command {
    Command::new("fab-rs")
        .bin_name("fab-rs")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Experimental bounded JSON reads for Microsoft Fabric")
        .after_help("Read execution requires a caller-configured library host. The binary has no production authentication or HTTP transport. No login or deployment commands are available.")
        .arg_required_else_help(true)
        .args_override_self(true)
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
                .args_override_self(true)
                .infer_long_args(true)
                .arg(Arg::new("path").num_args(0..))
                .arg(Arg::new("long").short('l').long("long").action(ArgAction::SetTrue))
                .arg(Arg::new("all").short('a').long("all").action(ArgAction::SetTrue))
                .arg(Arg::new("query").short('q').long("query")),
        )
        .subcommand(
            Command::new("get")
                .about("Get item properties (bounded generic metadata)")
                .args_override_self(true)
                .infer_long_args(true)
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
    let mut parser = command();
    parser.build();
    if args
        .iter()
        .any(|arg| matches!(arg.to_str(), Some("--help" | "-h" | "--version" | "-V")))
        && let Err(error) = parser.clone().try_get_matches_from(&args)
        && matches!(
            error.kind(),
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
        )
    {
        return Ok(Invocation::Display(error.to_string()));
    }
    let source_args = source_read_arguments(&parser, &args)?;
    let matches = match parser.try_get_matches_from(source_args) {
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
            let tokens: Vec<_> = values.map(String::as_str).collect();
            fabric_cli_query::join_query_args(&tokens)
        })
    };
    Ok(Invocation::Read(ReadCommand {
        kind: if list { ReadKind::List } else { ReadKind::Get },
        name: name.into(),
        path: values
            .get_many::<String>("path")
            .map(|values| values.cloned().collect())
            .unwrap_or_default(),
        query: if list {
            values.get_one::<String>("query").cloned()
        } else {
            joined("query")
        },
        long: list && values.get_flag("long"),
        all: list && values.get_flag("all"),
        output: if list { None } else { joined("output") },
        verbose: !list && values.get_flag("verbose"),
        force: !list && values.get_flag("force"),
        output_format: matches.get_one::<String>("output_format").cloned(),
    }))
}

fn invalid_arguments() -> clap::Error {
    clap::Error::raw(ErrorKind::UnknownArgument, INVALID_ARGUMENTS)
}

// Adapt only the bounded read argv lexical rules. Clap still owns the option
// schema, cardinality, validation, defaults, aliases, help and preview errors.
// Attached values consume one token in argparse; separate nargs+ values consume
// through the next option. Its variadic path is consumed once, never resumed.
fn source_read_arguments(
    parser: &Command,
    args: &[OsString],
) -> Result<Vec<OsString>, clap::Error> {
    let Some(start) = args
        .iter()
        .position(|arg| matches!(arg.to_str(), Some("ls" | "dir" | "get")))
    else {
        return Ok(args.to_vec());
    };
    let name = args[start].to_str().expect("matched command");
    let schema = parser
        .find_subcommand(if name == "dir" { "ls" } else { name })
        .expect("bounded read schema");
    let mut normalized = args[..=start].to_vec();
    let mut path = Vec::new();
    let mut path_closed = false;
    let mut index = start + 1;
    while index < args.len() {
        let Some(token) = args[index].to_str() else {
            return Ok(args.to_vec());
        };
        index += 1;
        if token == "--" {
            if path_closed {
                return Err(invalid_arguments());
            }
            path.extend_from_slice(&args[index..]);
            break;
        }
        if !source_option(token) {
            if path_closed {
                return Err(invalid_arguments());
            }
            path.push(args[index - 1].clone());
            continue;
        }
        path_closed |= !path.is_empty();
        let mut options = Vec::new();
        if let Some(long) = token.strip_prefix("--") {
            let (name, attached) = long
                .split_once('=')
                .map_or((long, None), |(name, value)| (name, Some(value)));
            let option = if let Some(exact) = schema
                .get_arguments()
                .find(|arg| arg.get_long() == Some(name))
            {
                exact
            } else {
                let mut candidates = schema
                    .get_arguments()
                    .filter(|arg| arg.get_long().is_some_and(|long| long.starts_with(name)));
                let Some(option) = candidates.next() else {
                    return Ok(args.to_vec());
                };
                if candidates.next().is_some() {
                    return Ok(args.to_vec());
                }
                option
            };
            options.push((option, attached));
        } else {
            for (offset, short) in token[1..].char_indices() {
                let Some(option) = schema
                    .get_arguments()
                    .find(|arg| arg.get_short() == Some(short))
                else {
                    return Ok(args.to_vec());
                };
                let remaining = &token[1 + offset + short.len_utf8()..];
                if option.get_action().takes_values() && !remaining.is_empty() {
                    options.push((
                        option,
                        Some(remaining.strip_prefix('=').unwrap_or(remaining)),
                    ));
                    break;
                }
                options.push((option, None));
            }
        }
        for (option, attached) in options {
            if matches!(option.get_id().as_str(), "help" | "version") {
                return Ok(args.to_vec());
            }
            let key = format!("--{}", option.get_long().expect("bounded option long name"));
            if !option.get_action().takes_values() {
                if attached.is_some() {
                    return Err(invalid_arguments());
                }
                normalized.push(key.into());
                continue;
            }
            let value = if let Some(value) = attached {
                value.to_owned()
            } else {
                let max = option
                    .get_num_args()
                    .expect("built option arity")
                    .max_values();
                let first = index;
                while index < args.len()
                    && index - first < max
                    && args[index]
                        .to_str()
                        .is_some_and(|value| !source_option(value))
                {
                    index += 1;
                }
                if first == index {
                    return Err(invalid_arguments());
                }
                let tokens: Vec<_> = args[first..index]
                    .iter()
                    .map(|value| value.to_str().expect("checked UTF-8"))
                    .collect();
                if max == 1 {
                    tokens[0].to_owned()
                } else {
                    fabric_cli_query::join_query_args(&tokens)
                }
            };
            normalized.push(format!("{key}={value}").into());
        }
    }
    if !path.is_empty() {
        normalized.push("--".into());
        normalized.extend(path);
    }
    Ok(normalized)
}

fn source_option(value: &str) -> bool {
    value.len() > 1 && value.starts_with('-') && !source_negative_number(value)
}

fn source_negative_number(value: &str) -> bool {
    let Some(value) = value.strip_prefix('-') else {
        return false;
    };
    if let Some((integer, fraction)) = value.split_once('.') {
        integer.bytes().all(|byte| byte.is_ascii_digit())
            && !fraction.is_empty()
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
    } else {
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
    }
}
