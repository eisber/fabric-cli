// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{CommandError, FamilySurface, FindOptions};
use azure_core::http::Method;
use clap::{Arg, ArgAction, ArgMatches, Command};
use fabric_cli_output::text::OutputFormat;
use fabric_sdk_api::Origin;

/// One family invocation. Values never select a command or discover credentials.
#[derive(Clone, Debug)]
pub struct AccessCommand {
    /// Typed operation and its source arguments.
    pub kind: AccessKind,
    /// Explicit override; absence leaves presentation to the caller.
    pub output_format: Option<OutputFormat>,
    /// Hidden source header option; an unconfigured host rejects it explicitly.
    pub skill: Option<String>,
}

/// The first read-only command batch, independently mountable in the root parser.
#[derive(Clone, Debug)]
pub enum AccessKind {
    /// `acl ls` / `acl dir`; the source query is scalar, not token-joined.
    AclList {
        /// Unresolved source path tokens.
        path: Vec<String>,
        /// Include detailed columns.
        long: bool,
        /// Scalar JMESPath expression.
        query: Option<String>,
    },
    /// `acl get`; query tokens are joined and edge quotes stripped.
    AclGet {
        /// Unresolved source path tokens.
        path: Vec<String>,
        /// Joined query, preserving absent versus present-empty.
        query: Option<String>,
        /// Requested export path; unsupported hosts fail before service I/O.
        output: Option<String>,
    },
    /// `label list-local`; only caller-configured definitions are read.
    LabelListLocal,
    /// `find`; the catalog session owns paging and the source request policy.
    Find(FindOptions),
    /// `api`; explicit source audience, never inferred from an endpoint URL.
    Api(RawApiCommand),
}

/// Raw API arguments before bounded input-file loading.
#[derive(Clone, Debug)]
pub struct RawApiCommand {
    /// Explicit public origin, including the source Fabric default.
    pub origin: Origin,
    /// One of the five source methods.
    pub method: Method,
    /// Scalar endpoint, without a second shell-splitting pass.
    pub endpoint: String,
    /// Token-joined parameter pairs.
    pub params: Option<String>,
    /// Token-joined safe custom header pairs.
    pub headers: Option<String>,
    /// Unopened inline JSON or local file tokens.
    pub input: Vec<String>,
    /// Token-joined projection.
    pub query: Option<String>,
    /// Include response headers.
    pub show_headers: bool,
}

fn leaf(name: &'static str) -> Command {
    Command::new(name)
        .args_override_self(true)
        .infer_long_args(true)
        .arg(
            Arg::new("output_format")
                .long("output_format")
                .value_parser(["json", "text"]),
        )
        .arg(Arg::new("skill").long("skill").hide(true))
}

fn path() -> Arg {
    Arg::new("path").required(true).num_args(1..)
}

fn flag(name: &'static str, short: Option<char>) -> Arg {
    let arg = Arg::new(name).long(name).action(ArgAction::SetTrue);
    if let Some(short) = short {
        arg.short(short)
    } else {
        arg
    }
}

fn query(many: bool) -> Arg {
    let arg = Arg::new("query").long("query").short('q');
    if many { arg.num_args(1..) } else { arg }
}

fn tokens(name: &'static str, short: char) -> Arg {
    Arg::new(name).long(name).short(short).num_args(1..)
}

/// Source ACL subtree. Compose it once in the centrally owned root.
pub fn acl_subcommand() -> Command {
    Command::new("acl")
        .about("Manage access controls")
        .arg_required_else_help(true)
        .subcommand(
            leaf("ls")
                .visible_alias("dir")
                .arg(path())
                .arg(flag("long", Some('l')))
                .arg(query(false)),
        )
        .subcommand(
            leaf("get")
                .arg(path())
                .arg(query(true))
                .arg(Arg::new("output").long("output").short('o')),
        )
}

/// Source label subtree; bare group help performs no file or credential access.
pub fn label_subcommand() -> Command {
    Command::new("label")
        .about("Manage sensitivity labels")
        .arg_required_else_help(true)
        .subcommand(leaf("list-local"))
}

/// Source root FIND leaf. `-P` is optional-scalar, unlike raw API parameters.
pub fn find_subcommand() -> Command {
    leaf("find")
        .about("Search the OneLake catalog for items")
        .arg(Arg::new("search_text").required(true))
        .arg(Arg::new("params").long("params").short('P').num_args(0..=1))
        .arg(flag("long", Some('l')))
        .arg(query(true))
}

/// Source root raw API leaf. No new audience spelling or HTTP method is added.
pub fn api_subcommand() -> Command {
    leaf("api")
        .about("Send an explicitly bound API request")
        .arg(Arg::new("endpoint").required(true))
        .arg(
            Arg::new("method")
                .long("method")
                .short('X')
                .default_value("get")
                .value_parser(["get", "post", "delete", "put", "patch"]),
        )
        .arg(
            Arg::new("audience")
                .long("audience")
                .short('A')
                .default_value("fabric")
                .value_parser(["fabric", "storage", "azure", "powerbi"]),
        )
        .arg(tokens("input", 'i'))
        .arg(tokens("params", 'P'))
        .arg(tokens("headers", 'H'))
        .arg(query(true))
        .arg(flag("show_headers", None))
}

/// Adds the family subtrees/root leaves without changing central parser policy.
pub fn register(root: Command) -> Command {
    root.subcommand(acl_subcommand())
        .subcommand(label_subcommand())
        .subcommand(find_subcommand())
        .subcommand(api_subcommand())
}

fn invalid() -> CommandError {
    CommandError::new("InvalidInput", "Invalid access command arguments")
}

fn string(args: &ArgMatches, name: &str) -> Option<String> {
    args.try_get_one::<String>(name).ok().flatten().cloned()
}

fn values(args: &ArgMatches, name: &str) -> Vec<String> {
    args.try_get_many::<String>(name)
        .ok()
        .flatten()
        .map(|values| values.cloned().collect())
        .unwrap_or_default()
}

fn boolean(args: &ArgMatches, name: &str) -> bool {
    args.try_get_one::<bool>(name)
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}

fn joined(args: &ArgMatches, name: &str) -> Option<String> {
    args.try_get_many::<String>(name)
        .ok()
        .flatten()
        .map(|values| {
            values
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
                .trim_matches(['"', '\''])
                .to_owned()
        })
}

impl AccessCommand {
    /// Converts the centrally parsed root matches. Unrelated families return None.
    /// No filesystem, configuration, credential or service operation occurs.
    pub fn from_matches(root: &ArgMatches) -> Result<Option<Self>, CommandError> {
        let Some((family, group)) = root.subcommand() else {
            return Ok(None);
        };
        let (kind, args) = match family {
            "acl" => {
                let (name, args) = group.subcommand().ok_or_else(invalid)?;
                let path = values(args, "path");
                if path.is_empty() {
                    return Err(invalid());
                }
                let kind = match name {
                    "ls" => AccessKind::AclList {
                        path,
                        long: boolean(args, "long"),
                        query: string(args, "query"),
                    },
                    "get" => AccessKind::AclGet {
                        path,
                        query: joined(args, "query"),
                        output: string(args, "output"),
                    },
                    _ => return Err(invalid()),
                };
                (kind, args)
            }
            "label" => {
                let (name, args) = group.subcommand().ok_or_else(invalid)?;
                if name != "list-local" {
                    return Err(invalid());
                }
                (AccessKind::LabelListLocal, args)
            }
            "find" => (
                AccessKind::Find(FindOptions {
                    search: string(group, "search_text").ok_or_else(invalid)?,
                    params: string(group, "params"),
                    long: boolean(group, "long"),
                    query: joined(group, "query"),
                }),
                group,
            ),
            "api" => (
                AccessKind::Api(RawApiCommand {
                    origin: match string(group, "audience").as_deref() {
                        Some("fabric") => Origin::Fabric,
                        Some("storage") => Origin::OneLake,
                        Some("azure") => Origin::Arm,
                        Some("powerbi") => Origin::PowerBi,
                        _ => return Err(invalid()),
                    },
                    method: match string(group, "method").as_deref() {
                        Some("get") => Method::Get,
                        Some("post") => Method::Post,
                        Some("delete") => Method::Delete,
                        Some("put") => Method::Put,
                        Some("patch") => Method::Patch,
                        _ => return Err(invalid()),
                    },
                    endpoint: string(group, "endpoint").ok_or_else(invalid)?,
                    params: joined(group, "params"),
                    headers: joined(group, "headers"),
                    input: values(group, "input"),
                    query: joined(group, "query"),
                    show_headers: boolean(group, "show_headers"),
                }),
                group,
            ),
            _ => return Ok(None),
        };
        let output_format = match string(args, "output_format").as_deref() {
            None => None,
            Some("json") => Some(OutputFormat::Json),
            Some("text") => Some(OutputFormat::Text),
            _ => return Err(invalid()),
        };
        Ok(Some(Self {
            kind,
            output_format,
            skill: string(args, "skill"),
        }))
    }

    /// Source output command context is the root family, not the alias spelling.
    pub const fn command_name(&self) -> &'static str {
        match self.kind {
            AccessKind::AclList { .. } | AccessKind::AclGet { .. } => "acl",
            AccessKind::LabelListLocal => "label",
            AccessKind::Find(_) => "find",
            AccessKind::Api(_) => "api",
        }
    }

    /// Existing family rendering surface selected from the typed operation.
    pub const fn surface(&self) -> FamilySurface {
        match &self.kind {
            AccessKind::AclList { .. } => FamilySurface::AclList,
            AccessKind::AclGet { .. } => FamilySurface::Get,
            AccessKind::LabelListLocal => FamilySurface::LocalLabels,
            AccessKind::Find(options) => FamilySurface::Find { long: options.long },
            AccessKind::Api(_) => FamilySurface::Api,
        }
    }
}
