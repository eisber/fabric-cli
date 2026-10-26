// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Typed command consumers, not a second parser, executable or runtime.
//! The front door supplies resolved targets, configuration, prompts and streams.

mod api;
mod export;
mod find;
mod hidden;
mod interactive_find;
mod mutations;
mod projection;
mod read_targets;
mod rendering;
mod transport;
pub use api::{ApiInput, ApiOptions, api, build_api_request, read_api_input};
pub use export::{AclExportSink, export_acls};
pub use find::{FindOptions, build_search_options, find, project_search};
pub use hidden::{
    HiddenNameKind, HiddenRead, domain_name_by_id, find_hidden_name, read_hidden_metadata,
    resolve_hidden_name,
};
pub use interactive_find::{FindOutput, find_interactive};
pub use mutations::{
    AclChange, Confirmation, LabelChange, MutationOptions, NamedResource, mutate_acl, mutate_label,
};
pub use projection::project_acls;
pub use read_targets::{acl_resource, read_named_acls};
pub use rendering::{FamilyPresentation, FamilySurface, render_family_error, render_family_output};
pub use transport::ResourceHttpExecutor;

use fabric_cli_output::{Indent, OutputEnvelope, OutputInput, OutputStatus, Timestamp};
use fabric_sdk_access::{AccessClient, Resource};
use fabric_sdk_api::{ApiError, Limits};
use serde_json::Value;
use std::{
    future::Future,
    io::{Read, Write},
};
use thiserror::Error;

/// Source registration supplied to the separately owned CLI front door.
#[derive(Clone, Copy, Debug)]
pub struct Registration {
    /// Canonical Python command path.
    pub path: &'static str,
    /// Full alias paths.
    pub aliases: &'static [&'static str],
}

/// Implemented library consumers; not claims of binary registration/live parity.
pub const REGISTRATIONS: &[Registration] = &[
    Registration {
        path: "acl ls",
        aliases: &["acl dir"],
    },
    Registration {
        path: "acl get",
        aliases: &[],
    },
    Registration {
        path: "label list-local",
        aliases: &[],
    },
    Registration {
        path: "acl set",
        aliases: &[],
    },
    Registration {
        path: "acl rm",
        aliases: &["acl del"],
    },
    Registration {
        path: "label set",
        aliases: &[],
    },
    Registration {
        path: "label rm",
        aliases: &["label del"],
    },
    Registration {
        path: "find",
        aliases: &[],
    },
    Registration {
        path: "api",
        aliases: &[],
    },
];

/// Error code/message pair consumed by the existing output envelope.
#[derive(Clone, Debug, Error, PartialEq)]
#[error("{code}: {message}")]
pub struct CommandError {
    /// Python-compatible code or explicitly documented safety failure.
    pub code: String,
    /// Diagnostic, never provider/token text.
    pub message: String,
    /// Source diagnostics emitted before failure, written to stderr.
    pub diagnostics: Vec<String>,
    /// Source warnings emitted before failure, for the caller's logger.
    pub warnings: Vec<String>,
    /// Exact shared operation classification, ID and service error data.
    pub operation: Option<Box<fabric_sdk_api::OperationError>>,
    /// Cancellation or a lost response is not evidence of mutation rollback.
    pub mutation_may_have_occurred: bool,
    /// The source handler printed an error internally without raising to its
    /// outer decorator. JSON remains Failure, but the command exit is zero.
    pub handled: bool,
}

impl CommandError {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into().trim_end_matches('.').to_owned(),
            diagnostics: Vec::new(),
            warnings: Vec::new(),
            operation: None,
            mutation_may_have_occurred: false,
            handled: false,
        }
    }

    pub(crate) fn with_context(mut self, value: &CommandValue) -> Self {
        self.diagnostics = value.diagnostics.clone();
        self.warnings = value.warnings.clone();
        self
    }
}

impl From<ApiError> for CommandError {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::MutationUncertain(cause) => {
                let mut error = Self::from(*cause);
                error.mutation_may_have_occurred = true;
                error
            }
            ApiError::Operation(operation) => {
                use fabric_sdk_api::OperationErrorKind;
                let mut error = match &operation.kind {
                    OperationErrorKind::Cancelled => Self::new("Cancelled", "Operation cancelled"),
                    OperationErrorKind::Credential => {
                        Self::new("Unauthorized", "Access is unauthorized")
                    }
                    OperationErrorKind::Http { status, code, .. } => {
                        let message = operation
                            .raw_service_error
                            .as_ref()
                            .and_then(|v| v.get("message"))
                            .and_then(Value::as_str);
                        Self::new(code.as_deref().unwrap_or("UnknownError"),message.unwrap_or(match status {
                            401=>"Access is unauthorized",403=>"Access is forbidden. You do not have permission to access this resource",
                            404=>"The requested resource could not be found",_=>"Fabric request failed",
                        }))
                    }
                    OperationErrorKind::OperationFailed { status, .. } => {
                        let cancelled = status == "Cancelled";
                        let code = if cancelled {
                            "LongRunningOperationCancelled"
                        } else {
                            "LongRunningOperationFailed"
                        };
                        let raw = operation
                            .raw_service_error
                            .as_deref()
                            .unwrap_or(&Value::Null);
                        let detail = if let Some(text) = raw.as_str() {
                            text.to_owned()
                        } else if raw.is_null() {
                            "None".into()
                        } else {
                            raw.to_string()
                        };
                        Self::new(
                            code,
                            format!(
                                "The operation {}: {detail}",
                                if cancelled { "was cancelled" } else { "failed" }
                            ),
                        )
                    }
                    _ => Self::new("UnexpectedError", operation.to_string()),
                };
                error.mutation_may_have_occurred = operation.mutation_may_have_occurred;
                error.operation = Some(operation);
                error
            }
            ApiError::UnsupportedRequest => Self::new("NotSupported", error.to_string()),
            ApiError::InvalidAudience | ApiError::InvalidInput => {
                Self::new("InvalidInput", error.to_string())
            }
            ApiError::Credential => Self::new("Unauthorized", "Access is unauthorized"),
            ApiError::InvalidJson => Self::new("InvalidJson", "Invalid JSON format"),
            ApiError::Service { code, message, .. } => {
                Self::new(&code, message.trim_end_matches('.'))
            }
            ApiError::Http { status: 401, code } => Self::new(&code, "Access is unauthorized"),
            ApiError::Http { status: 403, code } => Self::new(
                &code,
                "Access is forbidden. You do not have permission to access this resource",
            ),
            ApiError::Http { status: 404, .. } => {
                Self::new("NotFound", "The requested resource could not be found")
            }
            ApiError::Http { code, .. } => Self::new(&code, "Fabric request failed"),
            ApiError::Cancelled => Self::new("Cancelled", error.to_string()),
            _ => Self::new("UnexpectedError", error.to_string()),
        }
    }
}

/// Successful command value, including source-silent empty collections.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommandValue {
    /// Absent suppresses the envelope entirely.
    pub data: Option<Value>,
    /// Optional success message, even when no data is emitted.
    pub message: Option<String>,
    /// Source progress/help text for stderr, not mixed into JSON stdout.
    pub diagnostics: Vec<String>,
    /// Warnings belong to the host's logger, not JSON output.
    pub warnings: Vec<String>,
}

impl CommandValue {
    /// Writes source JSON output to caller-owned streams and returns its exit code.
    /// I/O/serialization errors remain explicit; no process exit occurs here.
    pub fn write_json(
        &self,
        command: &str,
        timestamp: Timestamp,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> std::io::Result<u8> {
        for diagnostic in &self.diagnostics {
            writeln!(stderr, "{diagnostic}")?;
        }
        if self.data.is_none() && self.message.is_none() {
            return Ok(0);
        }
        let plan = OutputEnvelope::new(
            timestamp,
            OutputInput {
                command: Some(command.into()),
                data: self.data.clone(),
                message: self.message.clone(),
                ..OutputInput::default()
            },
        )
        .render(Indent::Four)
        .map_err(std::io::Error::other)?;
        plan.write_to(stdout, stderr)?;
        Ok(plan.exit_code)
    }
}

/// Writes a typed failure with existing stream separation and Unauthorized exit 4.
pub fn write_error(
    error: &CommandError,
    command: &str,
    timestamp: Timestamp,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> std::io::Result<u8> {
    for diagnostic in &error.diagnostics {
        writeln!(stderr, "{diagnostic}")?;
    }
    let plan = OutputEnvelope::new(
        timestamp,
        OutputInput {
            command: Some(command.into()),
            status: OutputStatus::Failure,
            error_code: Some(error.code.clone()),
            message: Some(error.message.clone()),
            ..OutputInput::default()
        },
    )
    .render(Indent::Four)
    .map_err(std::io::Error::other)?;
    plan.write_to(stdout, stderr)?;
    Ok(if error.handled { 0 } else { plan.exit_code })
}

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

pub(crate) fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.0),
    }
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
