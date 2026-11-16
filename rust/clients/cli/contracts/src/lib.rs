// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Minimal safe command failures and Python output truthiness.
#![forbid(unsafe_code)]

use fabric_core::{CoreError, ErrorKind};
use serde_json::Value;
use std::fmt;

/// Safe presentation failure. Never construct messages from credential/process/HTTP bodies.
#[derive(Clone, Debug)]
pub struct CommandError {
    /// Python-compatible error code, or a typed engine extension.
    pub code: String,
    /// Safe user-facing explanation.
    pub message: String,
    /// Known, sanitized accepted-operation evidence.
    pub recovery: Option<Value>,
    exit_code: u8,
}

impl CommandError {
    /// Construct a failure without sensitive details.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        let exit_code = if code == "Unauthorized" { 4 } else { 1 };
        Self {
            code,
            message: message.into(),
            recovery: None,
            exit_code,
        }
    }

    /// Unauthorized is exit 4; all other command failures are exit 1.
    pub fn exit_code(&self) -> u8 {
        self.exit_code
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
            recovery: error
                .recovery
                .and_then(|value| serde_json::to_value(value).ok()),
            exit_code,
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
