// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! ACL/label schemas and source-compatible typed argument conversion.

use crate::{AclChange, CommandError, LabelChange};
use clap::{Arg, ArgMatches, Command};
use fabric_cli_command_contracts::parser::{
    ParsedCommand, boolean, flag, invalid, joined, leaf, query, string, tokens, values,
};

/// Focused access operations; catalog and raw API remain separate capabilities.
#[derive(Clone, Debug)]
pub enum AccessKind {
    /// Source ACL listing.
    AclList {
        /// Unresolved source path tokens.
        path: Vec<String>,
        /// Include detailed columns.
        long: bool,
        /// Scalar JMESPath expression.
        query: Option<String>,
    },
    /// Source ACL inspection.
    AclGet {
        /// Unresolved source path tokens.
        path: Vec<String>,
        /// Joined query, preserving absent versus present-empty.
        query: Option<String>,
        /// Requested export path; unsupported hosts fail before service I/O.
        output: Option<String>,
    },
    /// Source ACL grant/revoke.
    AclChange {
        /// Unresolved source path tokens.
        path: Vec<String>,
        /// Identity and role without canonicalizing request case.
        change: AclChange,
        /// Skip confirmation, never validation or operation reconciliation.
        force: bool,
    },
    /// Source sensitivity-label mutation.
    LabelChange {
        /// Unresolved source path tokens.
        path: Vec<String>,
        /// Exact joined label name or removal.
        change: LabelChange,
        /// Skip confirmation, never local definition or support checks.
        force: bool,
    },
    /// Read only caller-configured local definitions.
    LabelListLocal,
}

fn path() -> Arg {
    Arg::new("path").required(true).num_args(1..)
}

/// Source ACL subtree, including v2 guidance and grant/revoke aliases.
pub fn acl_subcommand() -> Command {
    Command::new("acl")
        .about("Manage access controls")
        .arg_required_else_help(true)
        .subcommand(
            leaf("ls")
                .visible_alias("dir")
                .about("List access controls for a workspace or supported item")
                .after_help(ACL_READ_HELP)
                .arg(path())
                .arg(flag("long", Some('l')))
                .arg(query(false)),
        )
        .subcommand(
            leaf("get")
                .about("Inspect access control records for a workspace or supported item")
                .after_help(ACL_GET_HELP)
                .arg(path())
                .arg(query(true))
                .arg(Arg::new("output").long("output").short('o')),
        )
        .subcommand(
            leaf("set")
                .visible_alias("grant")
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
                .visible_aliases(["del", "revoke"])
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

const ACL_READ_HELP: &str = "Targets: <name>.Workspace or <name>.Workspace/<item>.<Kind> (for example, Notebook).\nUse /<name>.Workspace for an absolute path, or <item>.<Kind> in the current workspace.\nSemanticModel, Environment, Dashboard, workfolders and OneLake paths are not supported here.\nDiscover workspace names: fab-rs ls /\nExample: fab-rs acl ls 'Demo.Workspace'\nItem reads require Fabric administrator permission; item ACL mutation is not supported.";

const ACL_GET_HELP: &str = "Targets: <name>.Workspace or <name>.Workspace/<item>.<Kind> (for example, Notebook).\nUse /<name>.Workspace for an absolute path, or <item>.<Kind> in the current workspace.\nSemanticModel, Environment, Dashboard, workfolders and OneLake paths are not supported here.\nDiscover workspace names: fab-rs ls /\nExample: fab-rs acl get 'Demo.Workspace'\nText output shows record values when -q is absent. Use -q '.' for records in JSON, or -q '' for field paths.\nItem reads require Fabric administrator permission; item ACL mutation is not supported.";

/// Source label subtree; bare help performs no file or credential access.
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

/// Mounts access without changing root parser policy.
pub fn register(root: Command) -> Command {
    root.subcommand(acl_subcommand())
        .subcommand(label_subcommand())
}

/// Converts only ACL/label matches; unrelated capabilities remain untouched.
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
