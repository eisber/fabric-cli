// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Canonical raw API schema and source argument normalization.

use crate::CommandError;
use azure_core::http::Method;
use clap::{Arg, ArgMatches, Command};
use fabric_cli_command_contracts::parser::{
    ParsedCommand, boolean, flag, invalid, joined, leaf, query, string, tokens, values,
};
use fabric_sdk_api::Origin;

/// Focused raw API arguments before bounded input-file loading.
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

/// Registers raw API without changing the centrally selected root parser policy.
pub fn register(root: Command) -> Command {
    root.subcommand(api_subcommand())
}

/// Parses only raw API root matches, without opening files or selecting clients.
/// Audience, method and endpoint validation retain their original precedence.
pub fn from_matches(
    root: &ArgMatches,
) -> Result<Option<ParsedCommand<RawApiCommand>>, CommandError> {
    let Some(("api", group)) = root.subcommand() else {
        return Ok(None);
    };
    let kind = RawApiCommand {
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
    };
    ParsedCommand::new(kind, group).map(Some)
}
