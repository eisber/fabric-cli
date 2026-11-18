// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Minimal safe command failures and Python output truthiness.
#![forbid(unsafe_code)]

use fabric_core::{CoreError, ErrorKind, JobFailureReason};
use serde_json::Value;
use std::fmt;

/// Safe presentation failure. Never construct messages from credential/process/HTTP bodies.
#[derive(Clone, Debug)]
pub struct CommandError {
    /// Python-compatible error code, or a typed engine extension.
    pub code: String,
    /// Safe user-facing explanation.
    pub message: String,
    /// Known, sanitized accepted-operation evidence, boxed to keep failures small.
    pub recovery: Option<Box<Value>>,
    /// Untrusted service message for final JSON only, never diagnostic output.
    pub failure_reason: Option<JobFailureReason>,
    exit_code: u8,
    kind: ErrorKind,
}

impl CommandError {
    /// Construct a failure without sensitive details.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        let exit_code = if code == "Unauthorized" { 4 } else { 1 };
        let kind = match code.as_str() {
            // Local input/config failures are never service failures.
            "InvalidInput" | "InvalidJson" | "InvalidPath" | "InvalidConfig" => {
                ErrorKind::InvalidInput
            }
            "Unauthorized" => ErrorKind::Unauthorized,
            "Transport" => ErrorKind::Transport,
            "Protocol" => ErrorKind::Protocol,
            "Timeout" => ErrorKind::Timeout,
            "Cancelled" => ErrorKind::Cancelled,
            "Diagnostic" => ErrorKind::Diagnostic,
            _ => ErrorKind::Service,
        };
        Self {
            code,
            message: message.into(),
            recovery: None,
            failure_reason: None,
            exit_code,
            kind,
        }
    }

    /// Unauthorized is exit 4; all other command failures are exit 1.
    pub fn exit_code(&self) -> u8 {
        self.exit_code
    }

    /// Local error category, independent of any service-provided error code.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl From<CoreError> for CommandError {
    fn from(error: CoreError) -> Self {
        let code = match error.kind {
            ErrorKind::InvalidInput => "InvalidInput",
            ErrorKind::Unauthorized => "Unauthorized",
            ErrorKind::Transport => "Transport",
            ErrorKind::Protocol => "Protocol",
            ErrorKind::Timeout => "Timeout",
            ErrorKind::Cancelled => "Cancelled",
            ErrorKind::Service => "Service",
            ErrorKind::Diagnostic => "Diagnostic",
        };
        let exit_code = if error.kind == ErrorKind::Unauthorized {
            4
        } else {
            1
        };
        let code = error.service_code().unwrap_or(code).to_owned();
        Self {
            code,
            message: error.message,
            failure_reason: error.failure_reason,
            recovery: error
                .recovery
                .and_then(|value| serde_json::to_value(value).ok())
                .map(Box::new),
            exit_code,
            kind: error.kind,
        }
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for CommandError {}

/// Python truthiness in the JSON value domain.
pub fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => {
            let text = value.to_string();
            if text.contains(['.', 'e', 'E']) {
                value.as_f64() != Some(0.0)
            } else {
                text != "0" && text != "-0"
            }
        }
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}
