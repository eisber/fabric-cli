// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Source-compatible aggregate types; schemas and conversion belong to capabilities.

use crate::{AclChange, CommandError, FamilySurface, FindOptions, LabelChange};
use clap::{ArgMatches, Command};
use fabric_cli_access::parser::AccessKind as FocusedAccessKind;
use fabric_cli_command_contracts::parser::ParsedCommand;
use fabric_cli_output::text::OutputFormat;

pub use fabric_cli_access::parser::{acl_subcommand, label_subcommand};
pub use fabric_cli_api::parser::{RawApiCommand, api_subcommand};
pub use fabric_cli_catalog::parser::find_subcommand;

/// Existing aggregate invocation, retained for public parser and host consumers.
#[derive(Clone, Debug)]
pub struct AccessCommand {
    /// Capability-owned operation and source arguments.
    pub kind: AccessKind,
    /// Explicit override; absence leaves presentation to the caller.
    pub output_format: Option<OutputFormat>,
    /// Hidden source header option; an unconfigured host rejects it explicitly.
    pub skill: Option<String>,
}

/// Compatibility union; no grammar, preparation or execution is owned here.
#[derive(Clone, Debug)]
pub enum AccessKind {
    /// Source ACL listing.
    AclList {
        /// Unresolved path tokens.
        path: Vec<String>,
        /// Include detailed columns.
        long: bool,
        /// Scalar query.
        query: Option<String>,
    },
    /// Source ACL inspection.
    AclGet {
        /// Unresolved path tokens.
        path: Vec<String>,
        /// Joined query, preserving present-empty.
        query: Option<String>,
        /// Requested export path.
        output: Option<String>,
    },
    /// ACL grant/revoke.
    AclChange {
        /// Unresolved path tokens.
        path: Vec<String>,
        /// Original identity and role.
        change: AclChange,
        /// Bypass confirmation only.
        force: bool,
    },
    /// Sensitivity-label mutation.
    LabelChange {
        /// Unresolved path tokens.
        path: Vec<String>,
        /// Joined label name or removal.
        change: LabelChange,
        /// Bypass confirmation only.
        force: bool,
    },
    /// Caller-selected local definitions.
    LabelListLocal,
    /// Catalog-owned FIND.
    Find(FindOptions),
    /// Raw API-owned arguments.
    Api(RawApiCommand),
}

impl From<FocusedAccessKind> for AccessKind {
    fn from(kind: FocusedAccessKind) -> Self {
        match kind {
            FocusedAccessKind::AclList { path, long, query } => Self::AclList { path, long, query },
            FocusedAccessKind::AclGet {
                path,
                query,
                output,
            } => Self::AclGet {
                path,
                query,
                output,
            },
            FocusedAccessKind::AclChange {
                path,
                change,
                force,
            } => Self::AclChange {
                path,
                change,
                force,
            },
            FocusedAccessKind::LabelChange {
                path,
                change,
                force,
            } => Self::LabelChange {
                path,
                change,
                force,
            },
            FocusedAccessKind::LabelListLocal => Self::LabelListLocal,
        }
    }
}

impl From<ParsedCommand<FocusedAccessKind>> for AccessCommand {
    fn from(command: ParsedCommand<FocusedAccessKind>) -> Self {
        Self {
            kind: command.kind.into(),
            output_format: command.output_format,
            skill: command.skill,
        }
    }
}

impl From<ParsedCommand<RawApiCommand>> for AccessCommand {
    fn from(command: ParsedCommand<RawApiCommand>) -> Self {
        Self {
            kind: AccessKind::Api(command.kind),
            output_format: command.output_format,
            skill: command.skill,
        }
    }
}

impl From<ParsedCommand<FindOptions>> for AccessCommand {
    fn from(command: ParsedCommand<FindOptions>) -> Self {
        Self {
            kind: AccessKind::Find(command.kind),
            output_format: command.output_format,
            skill: command.skill,
        }
    }
}

/// Compatibility registration, preserving the original subtree order.
pub fn register(root: Command) -> Command {
    fabric_cli_api::parser::register(fabric_cli_catalog::parser::register(
        fabric_cli_access::parser::register(root),
    ))
}

impl AccessCommand {
    /// Delegates conversion to the capability that owns the matching grammar.
    pub fn from_matches(root: &ArgMatches) -> Result<Option<Self>, CommandError> {
        if let Some(command) = fabric_cli_access::parser::from_matches(root)? {
            return Ok(Some(command.into()));
        }
        if let Some(command) = fabric_cli_catalog::parser::from_matches(root)? {
            return Ok(Some(command.into()));
        }
        Ok(fabric_cli_api::parser::from_matches(root)?.map(Into::into))
    }

    /// Adapts the old union to focused access without reinterpreting arguments.
    pub fn access(&self) -> Option<ParsedCommand<FocusedAccessKind>> {
        let kind = match self.kind.clone() {
            AccessKind::AclList { path, long, query } => {
                FocusedAccessKind::AclList { path, long, query }
            }
            AccessKind::AclGet {
                path,
                query,
                output,
            } => FocusedAccessKind::AclGet {
                path,
                query,
                output,
            },
            AccessKind::AclChange {
                path,
                change,
                force,
            } => FocusedAccessKind::AclChange {
                path,
                change,
                force,
            },
            AccessKind::LabelChange {
                path,
                change,
                force,
            } => FocusedAccessKind::LabelChange {
                path,
                change,
                force,
            },
            AccessKind::LabelListLocal => FocusedAccessKind::LabelListLocal,
            AccessKind::Find(_) | AccessKind::Api(_) => return None,
        };
        Some(ParsedCommand {
            kind,
            output_format: self.output_format,
            skill: self.skill.clone(),
        })
    }

    /// Existing root command context, not alias spelling.
    pub const fn command_name(&self) -> &'static str {
        match self.kind {
            AccessKind::AclList { .. }
            | AccessKind::AclGet { .. }
            | AccessKind::AclChange { .. } => "acl",
            AccessKind::LabelListLocal | AccessKind::LabelChange { .. } => "label",
            AccessKind::Find(_) => "find",
            AccessKind::Api(_) => "api",
        }
    }

    /// Existing rendering surface for compatibility consumers.
    pub const fn surface(&self) -> FamilySurface {
        match &self.kind {
            AccessKind::AclList { .. } => FamilySurface::AclList,
            AccessKind::AclGet { .. } => FamilySurface::Get,
            AccessKind::AclChange { .. } | AccessKind::LabelChange { .. } => {
                FamilySurface::Mutation
            }
            AccessKind::LabelListLocal => FamilySurface::LocalLabels,
            AccessKind::Find(options) => FamilySurface::Find { long: options.long },
            AccessKind::Api(_) => FamilySurface::Api,
        }
    }
}
