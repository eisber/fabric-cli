// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{Arg, ArgAction, Command};
use serde_json::{Value, json};
use std::ffi::OsString;

use super::{
    OwnerKind, ParsedInvocation, ReadError, ReadErrorKind, ReadFacet, ReadOperation, TypedPath,
};

/// One operation descriptor used to generate both canonical and facade schemas.
#[derive(Clone, Copy, Debug)]
pub struct OperationRegistration {
    /// Stable identity.
    pub operation: ReadOperation,
    /// Canonical command spelling.
    pub command: &'static str,
}

/// The single registry. Facades do not add operation IDs or execution handlers.
pub const OPERATIONS: [OperationRegistration; 2] = [
    OperationRegistration {
        operation: ReadOperation::Get,
        command: "get",
    },
    OperationRegistration {
        operation: ReadOperation::List,
        command: "list",
    },
];

impl OperationRegistration {
    /// Registered normalized input schema shared by both spellings.
    ///
    /// Unsupported global/head options are not advertised as capabilities.
    pub fn input_schema(self) -> Value {
        let mut properties = json!({
            "operationId": {"const": self.operation.id()},
            "path": {
                "type": "object", "additionalProperties": false,
                "required": ["owner", "facet"],
                "properties": {
                    "owner": {
                        "type": "object", "additionalProperties": false,
                        "required": ["workspace", "artifact", "kind"],
                        "properties": {
                            "workspace": {"type": "string", "minLength": 1, "maxLength": 1024},
                            "artifact": {"type": "string", "minLength": 1, "maxLength": 1024},
                            "kind": {"enum": ["notebook", "lakehouse", "livy_service"]}
                        }
                    },
                    "facet": {"oneOf": [
                        {"type": "object", "additionalProperties": false,
                            "required": ["kind"], "properties": {"kind": {"const": "metadata"}}},
                        {"type": "object", "additionalProperties": false,
                            "required": ["kind"], "properties": {"kind": {"const": "notebook_content"}}},
                        {"type": "object", "additionalProperties": false,
                            "required": ["kind"], "properties": {"kind": {"const": "sessions"}}},
                        {"type": "object", "additionalProperties": false,
                            "required": ["kind", "id"], "properties": {
                                "kind": {"const": "session"},
                                "id": {"type": "string", "minLength": 1, "maxLength": 1024}
                            }},
                        {"type": "object", "additionalProperties": false,
                            "required": ["kind", "session", "id", "result"], "properties": {
                                "kind": {"const": "statement"},
                                "session": {"type": "string", "minLength": 1, "maxLength": 1024},
                                "id": {"type": "integer", "minimum": 0, "maximum": 9223372036854775807_u64},
                                "result": {"type": "boolean"}
                            }}
                    ]}
                }
            },
            "maxBytes": {"type": "integer", "minimum": 1, "default": 1048576},
            "output": {"type": "string", "minLength": 1, "maxLength": 4096, "default": "-"},
            "limit": {"type": "null"}
        });
        if self.operation == ReadOperation::List {
            properties["limit"] = json!({
                "type": "integer", "minimum": 1, "maximum": 2147483647, "default": 100
            });
        }
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": self.operation.id(),
            "type": "object",
            "additionalProperties": false,
            "required": ["operationId", "path", "maxBytes", "output"],
            "properties": properties
        })
    }

    fn clap_command(self) -> Command {
        let mut output = Arg::new("output").long("output").default_value("-");
        let mut project = Arg::new("project").long("project");
        if self.operation == ReadOperation::Get {
            output = output.short('o');
            project = project.short('q');
        }
        let mut command = Command::new(self.command)
            .arg(Arg::new("path").required(true))
            .arg(Arg::new("format").long("format").default_value("json"))
            .arg(
                Arg::new("max-bytes")
                    .long("max-bytes")
                    .default_value("1048576"),
            )
            .arg(output)
            .arg(Arg::new("profile").long("profile"))
            .arg(project)
            .arg(Arg::new("timeout").long("timeout"));
        if self.operation == ReadOperation::List {
            command = command
                .arg(Arg::new("limit").long("limit").default_value("100"))
                .arg(Arg::new("continuation").long("continuation"));
        } else {
            command = command
                .arg(Arg::new("wait").long("wait").action(ArgAction::SetTrue))
                .arg(
                    Arg::new("recursive")
                        .long("recursive")
                        .action(ArgAction::SetTrue),
                )
                .arg(Arg::new("depth").long("depth"));
        }
        command
    }
}

/// Generate the isolated experimental parser, without changing the existing executable.
///
/// Known-but-unsupported options are recognized solely to return explicit capability
/// errors; their presence does not install projection, identity or a timer.
pub fn command_schema() -> Command {
    let mut root = Command::new("fab-cli-v2").subcommand_required(true);
    let mut notebook = Command::new("notebook").subcommand_required(true);
    for registration in OPERATIONS {
        root = root.subcommand(registration.clap_command());
        notebook = notebook.subcommand(registration.clap_command());
    }
    root.subcommand(
        Command::new("artifact")
            .subcommand_required(true)
            .subcommand(notebook),
    )
}

/// Parse argv including its program name into the shared canonical input.
///
/// # Errors
/// Rejects unsupported options/facets and wrong-owner aliases without I/O.
/// Help is available through [`command_schema`]; parser diagnostics never echo input.
pub fn parse_args<I, T>(args: I) -> Result<ParsedInvocation, ReadError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let invalid = || ReadError::new(ReadErrorKind::InvalidInput);
    let matches = command_schema()
        .try_get_matches_from(args)
        .map_err(|_| invalid())?;
    let (name, mut leaf) = matches.subcommand().ok_or_else(invalid)?;
    let facade = name == "artifact";
    let name = if facade {
        let (_, notebook) = leaf.subcommand().ok_or_else(invalid)?;
        let (name, operation) = notebook.subcommand().ok_or_else(invalid)?;
        leaf = operation;
        name
    } else {
        name
    };
    let registration = OPERATIONS
        .iter()
        .find(|entry| entry.command == name)
        .ok_or_else(invalid)?;
    let unsupported = ["profile", "project", "timeout"]
        .iter()
        .any(|option| leaf.get_one::<String>(option).is_some())
        || if registration.operation == ReadOperation::Get {
            leaf.get_flag("wait")
                || leaf.get_flag("recursive")
                || leaf.get_one::<String>("depth").is_some()
        } else {
            leaf.get_one::<String>("continuation").is_some()
        };
    if unsupported || leaf.get_one::<String>("format").map(String::as_str) != Some("json") {
        return Err(ReadError::new(ReadErrorKind::UnsupportedCapability));
    }
    let path = TypedPath::parse(leaf.get_one::<String>("path").ok_or_else(invalid)?)?;
    if facade && path.owner.kind != OwnerKind::Notebook {
        return Err(ReadError::new(ReadErrorKind::WrongOwner));
    }
    let max_bytes = positive(leaf.get_one::<String>("max-bytes").ok_or_else(invalid)?)?;
    let limit = if registration.operation == ReadOperation::List {
        let value = positive(leaf.get_one::<String>("limit").ok_or_else(invalid)?)?;
        Some(
            u32::try_from(value)
                .ok()
                .filter(|value| *value <= i32::MAX as u32)
                .ok_or_else(invalid)?,
        )
    } else {
        None
    };
    if !matches!(
        (registration.operation, &path.facet),
        (
            ReadOperation::Get,
            ReadFacet::Metadata
                | ReadFacet::NotebookContent
                | ReadFacet::Session { .. }
                | ReadFacet::Statement { .. }
        ) | (ReadOperation::List, ReadFacet::Sessions)
    ) {
        return Err(ReadError::new(ReadErrorKind::UnsupportedCapability));
    }
    let output = leaf
        .get_one::<String>("output")
        .ok_or_else(invalid)?
        .clone();
    if output.is_empty() || output.len() > 4096 || output.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(ParsedInvocation {
        operation: registration.operation,
        path,
        max_bytes,
        limit,
        output,
    })
}

fn positive(value: &str) -> Result<usize, ReadError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ReadError::new(ReadErrorKind::InvalidInput));
    }
    value
        .parse()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| ReadError::new(ReadErrorKind::InvalidInput))
}
