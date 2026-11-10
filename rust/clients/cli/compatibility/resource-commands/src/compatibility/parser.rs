// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{AclChange, CommandError, FamilySurface, FindOptions, LabelChange};
use azure_core::http::Method;
use clap::{ArgMatches, Command};
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

/// The nine source leaves, independently mountable in the root parser.
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

pub use fabric_cli_access::parser::{acl_subcommand, label_subcommand};
pub use fabric_cli_api::parser::api_subcommand;
pub use fabric_cli_catalog::parser::find_subcommand;

/// Adds the family subtrees/root leaves without changing central parser policy.
pub fn register(root: Command) -> Command {
    fabric_cli_api::parser::register(fabric_cli_catalog::parser::register(
        fabric_cli_access::parser::register(root),
    ))
}

impl From<fabric_cli_access::parser::AccessKind> for AccessKind {
    fn from(kind: fabric_cli_access::parser::AccessKind) -> Self {
        use fabric_cli_access::parser::AccessKind as Focused;
        match kind {
            Focused::AclList { path, long, query } => Self::AclList { path, long, query },
            Focused::AclGet {
                path,
                query,
                output,
            } => Self::AclGet {
                path,
                query,
                output,
            },
            Focused::AclChange {
                path,
                change,
                force,
            } => Self::AclChange {
                path,
                change,
                force,
            },
            Focused::LabelChange {
                path,
                change,
                force,
            } => Self::LabelChange {
                path,
                change,
                force,
            },
            Focused::LabelListLocal => Self::LabelListLocal,
        }
    }
}

impl From<fabric_cli_api::parser::RawApiCommand> for RawApiCommand {
    fn from(command: fabric_cli_api::parser::RawApiCommand) -> Self {
        Self {
            origin: command.origin,
            method: command.method,
            endpoint: command.endpoint,
            params: command.params,
            headers: command.headers,
            input: command.input,
            query: command.query,
            show_headers: command.show_headers,
        }
    }
}

impl AccessCommand {
    /// Converts the centrally parsed root matches. Unrelated families return None.
    /// No filesystem, configuration, credential or service operation occurs.
    pub fn from_matches(root: &ArgMatches) -> Result<Option<Self>, CommandError> {
        let parsed = match root.subcommand_name() {
            Some("acl" | "label") => fabric_cli_access::parser::from_matches(root)?
                .map(|command| command.map(AccessKind::from)),
            Some("find") => fabric_cli_catalog::parser::from_matches(root)?
                .map(|command| command.map(AccessKind::Find)),
            Some("api") => fabric_cli_api::parser::from_matches(root)?
                .map(|command| command.map(|kind| AccessKind::Api(kind.into()))),
            _ => return Ok(None),
        };
        Ok(parsed.map(|command| Self {
            kind: command.kind,
            output_format: command.output_format,
            skill: command.skill,
        }))
    }

    /// Source output command context is the root family, not the alias spelling.
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

    /// Existing family rendering surface selected from the typed operation.
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
