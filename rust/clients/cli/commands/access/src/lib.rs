// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! ACL and label command consumers over caller-injected SDK clients and hosts.
//! No parser, runtime, credential discovery or terminal is installed here.

mod export;
mod mutations;
pub mod parser;
mod projection;
mod read_targets;

pub use export::{AclExportSink, export_acls};
use fabric_cli_command_contracts::python_truthy;
pub use fabric_cli_command_contracts::{CommandError, CommandValue, Confirmation};
pub use mutations::{
    AclChange, LabelChange, MutationOptions, NamedResource, mutate_acl, mutate_label,
};
pub use projection::project_acls;
pub use read_targets::{acl_resource, read_named_acls};

use fabric_sdk_access::{AccessClient, Resource};
use fabric_sdk_api::Limits;
use serde_json::Value;
use std::{future::Future, io::Read};

/// `acl ls` and `acl get` options after the source token-joining boundary.
#[derive(Clone, Debug)]
pub enum AclRead {
    /// Sorted source columns, or a query over full transformed rows.
    List {
        /// Include source long columns.
        long: bool,
        /// Already-joined JMESPath expression.
        query: Option<String>,
    },
    /// Raw collection query; no query means source path discovery, not raw JSON.
    Get {
        /// `.` requests the unmodified collection.
        query: Option<String>,
    },
}

/// Composes SDK reads, source projections and command output on the caller runtime.
pub async fn read_acls(
    client: &AccessClient,
    resource: &Resource,
    options: AclRead,
    limits: Limits,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<CommandValue, CommandError> {
    let context = read_targets::acl_context(resource);
    let rows = client
        .list(resource, limits, cancelled)
        .await
        .map_err(|e| CommandError::from(e.error).with_context(&context))?
        .rows;
    let mut result = match options {
        AclRead::List { long, query } => {
            if rows.is_empty() {
                CommandValue::default()
            } else {
                CommandValue {
                    data: Some(
                        project_acls(resource, rows, long, query.as_deref())
                            .map_err(|e| e.with_context(&context))?,
                    ),
                    ..CommandValue::default()
                }
            }
        }
        AclRead::Get { query } => {
            let value = read_targets::project_get(rows, query.as_deref())
                .map_err(|e| e.with_context(&context))?;
            CommandValue {
                data: python_truthy(&value).then_some(value),
                ..CommandValue::default()
            }
        }
    };
    result.warnings = context.warnings;
    Ok(result)
}

/// Reads only caller-selected local label definitions. Never discovers config.
/// Missing configuration preserves the source warning/help rather than reading home.
pub fn list_local_labels(path: Option<&std::path::Path>) -> Result<CommandValue, CommandError> {
    let Some(path) = path.filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(CommandValue {
            warnings: vec!["Label definitions for CLI not set".into()],
            diagnostics: vec![
                "\u{2192} Run 'config set local_definition_labels <json_path>'".into(),
            ],
            ..CommandValue::default()
        });
    };
    labels_from_value(read_labels_value(path)?)
}

pub(crate) fn read_labels_value(path: &std::path::Path) -> Result<Value, CommandError> {
    let file = std::fs::File::open(path).map_err(|_| {
        CommandError::new(
            "InvalidPath",
            "The specified file or directory does not exist",
        )
    })?;
    let limit = fabric_sdk_api::RequestBounds::default()
        .max_body_bytes
        .get();
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            CommandError::new(
                "InvalidPath",
                "The specified file or directory does not exist",
            )
        })?;
    parse_labels_value(&bytes)
}

/// Parses a local definition buffer using the source `labels` output contract.
pub fn labels_from_bytes(bytes: &[u8]) -> Result<CommandValue, CommandError> {
    labels_from_value(parse_labels_value(bytes)?)
}

fn parse_labels_value(bytes: &[u8]) -> Result<Value, CommandError> {
    if bytes.len()
        > fabric_sdk_api::RequestBounds::default()
            .max_body_bytes
            .get()
    {
        return Err(CommandError::new(
            "InvalidInput",
            "Label definitions exceed their byte bound",
        ));
    }
    serde_json::from_slice(bytes)
        .map_err(|_| CommandError::new("InvalidJson", "Invalid JSON format"))
}

fn labels_from_value(value: Value) -> Result<CommandValue, CommandError> {
    let labels = value.get("labels").ok_or_else(|| CommandError::new("InvalidEntriesFormat",
        "The entries format in the labels definition file is invalid. Please check the file structure and ensure it follows the expected format"))?;
    Ok(CommandValue {
        data: Some(labels.clone()),
        ..CommandValue::default()
    })
}
