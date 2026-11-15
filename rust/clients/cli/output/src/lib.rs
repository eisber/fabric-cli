// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Python envelopes only. The caller owns timestamps and output streams.
#![forbid(unsafe_code)]

use fabric_cli_command_contracts::{CommandError, python_truthy};
use fabric_core_json::{Format, Indent};
use serde_json::{Map, Value};

/// Ordered success envelope, preserving empty API query objects and strings.
pub fn success(timestamp: &str, command: &str, data: Value) -> String {
    let mut result = Map::new();
    if data.is_array() {
        result.insert("data".into(), data);
    } else if python_truthy(&data)
        || (command == "api" && matches!(&data, Value::Object(_) | Value::String(_)))
    {
        result.insert("data".into(), Value::Array(vec![data]));
    }
    envelope(timestamp, command, "Success", result)
}

/// Ordered Python failure envelope. Recovery evidence is an approved extension.
pub fn failure(timestamp: &str, command: &str, error: &CommandError) -> String {
    let mut result = Map::new();
    if let Some(recovery) = &error.recovery {
        result.insert("data".into(), Value::Array(vec![recovery.clone()]));
    }
    result.insert("error_code".into(), Value::String(error.code.clone()));
    result.insert("message".into(), Value::String(error.message.clone()));
    envelope(timestamp, command, "Failure", result)
}

fn envelope(timestamp: &str, command: &str, status: &str, result: Map<String, Value>) -> String {
    let mut root = Map::new();
    root.insert("timestamp".into(), Value::String(timestamp.into()));
    root.insert("status".into(), Value::String(status.into()));
    if !command.is_empty() {
        root.insert("command".into(), Value::String(command.into()));
    }
    root.insert("result".into(), Value::Object(result));
    let mut text = fabric_core_json::encode(&Value::Object(root), Format::Pretty(Indent::Four));
    text.push('\n');
    text
}
