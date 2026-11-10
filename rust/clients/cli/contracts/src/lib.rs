// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared CLI command contracts, not a capability facade or executable.
//! Callers supply configuration, confirmation, presentation, HTTP and streams.

pub mod parser;
mod rendering;
mod transport;
pub use rendering::{FamilyPresentation, FamilySurface, render_family_error, render_family_output};
pub use transport::ResourceHttpExecutor;

use fabric_cli_output::{Indent, OutputEnvelope, OutputInput, OutputStatus, Timestamp};
use fabric_sdk_api::ApiError;
use serde_json::Value;
use std::{future::Future, io::Write, pin::Pin};
use thiserror::Error;

/// Caller-owned asynchronous confirmation; no library reads a terminal.
pub trait Confirmation {
    /// Resolves a source question. Failure/cancellation must not mean consent.
    fn confirm<'a>(
        &'a mut self,
        question: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, CommandError>> + Send + 'a>>;
}

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
    /// Construct a local failure without diagnostics or mutation side effects.
    pub fn new(code: &str, message: impl Into<String>) -> Self {
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

    /// Retains diagnostics and warnings already emitted by a capability handler.
    pub fn with_context(mut self, value: &CommandValue) -> Self {
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

/// Preserves Python truth-value routing for source JSON command values.
pub fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.0),
    }
}
