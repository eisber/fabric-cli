// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Canonical ACL/label schemas and source argument normalization.

use crate::{AclChange, CommandError, LabelChange};
use clap::{Arg, ArgMatches, Command};
use fabric_cli_command_contracts::parser::{
    ParsedCommand, boolean, flag, invalid, joined, leaf, query, string, tokens, values,
};

/// A focused access operation; mixed catalog/API variants remain facade-owned.
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
    /// `acl set` / `acl rm` / `acl del`, with existing source mutation semantics.
    AclChange {
        /// Unresolved source path tokens.
        path: Vec<String>,
        /// Supplied identity and role, without canonicalizing request case.
        change: AclChange,
        /// Skip confirmation, never validation or operation reconciliation.
        force: bool,
    },
    /// `label set` / `label rm` / `label del` for existing supported items.
    LabelChange {
        /// Unresolved source path tokens.
        path: Vec<String>,
        /// Exact joined label name or removal.
        change: LabelChange,
        /// Skip confirmation, never local definition or support checks.
        force: bool,
    },
    /// `label list-local`; only caller-configured definitions are read.
    LabelListLocal,
}

fn path() -> Arg {
    Arg::new("path").required(true).num_args(1..)
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
        .subcommand(
            leaf("set")
                .arg(path())
                .arg(
                    Arg::new("identity")
                        .long("identity")
                        .short('I')
                        .required(true),
                )
                .arg(Arg::new("role").long("role").short('R').required(true))
                .arg(flag("force", Some('f'))),
        )
        .subcommand(
            leaf("rm")
                .visible_alias("del")
                .arg(path())
                .arg(
                    Arg::new("identity")
                        .long("identity")
                        .short('I')
                        .required(true),
                )
                .arg(flag("force", Some('f'))),
        )
}

/// Source label subtree; bare group help performs no file or credential access.
pub fn label_subcommand() -> Command {
    Command::new("label")
        .about("Manage sensitivity labels")
        .arg_required_else_help(true)
        .subcommand(leaf("list-local"))
        .subcommand(
            leaf("set")
                .arg(path())
                .arg(tokens("name", 'n').required(true))
                .arg(flag("force", Some('f'))),
        )
        .subcommand(
            leaf("rm")
                .visible_alias("del")
                .arg(path())
                .arg(flag("force", Some('f'))),
        )
}

/// Registers ACL then label subtrees without changing the root parser policy.
pub fn register(root: Command) -> Command {
    root.subcommand(acl_subcommand())
        .subcommand(label_subcommand())
}

/// Parses only ACL/label root matches; unrelated capabilities return None.
/// Domain validation precedes common output/header normalization as in the source.
pub fn from_matches(root: &ArgMatches) -> Result<Option<ParsedCommand<AccessKind>>, CommandError> {
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
                "set" => AccessKind::AclChange {
                    path,
                    change: AclChange::Set {
                        identity: string(args, "identity").ok_or_else(invalid)?,
                        role: string(args, "role").ok_or_else(invalid)?,
                    },
                    force: boolean(args, "force"),
                },
                "rm" => AccessKind::AclChange {
                    path,
                    change: AclChange::Remove {
                        identity: string(args, "identity").ok_or_else(invalid)?,
                    },
                    force: boolean(args, "force"),
                },
                _ => return Err(invalid()),
            };
            (kind, args)
        }
        "label" => {
            let (name, args) = group.subcommand().ok_or_else(invalid)?;
            let kind = match name {
                "list-local" => AccessKind::LabelListLocal,
                "set" | "rm" => {
                    let path = values(args, "path");
                    if path.is_empty() {
                        return Err(invalid());
                    }
                    AccessKind::LabelChange {
                        path,
                        change: if name == "set" {
                            LabelChange::Set(joined(args, "name").ok_or_else(invalid)?)
                        } else {
                            LabelChange::Remove
                        },
                        force: boolean(args, "force"),
                    }
                }
                _ => return Err(invalid()),
            };
            (kind, args)
        }
        _ => return Ok(None),
    };
    ParsedCommand::new(kind, args).map(Some)
}
