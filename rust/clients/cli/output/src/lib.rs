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
    envelope(timestamp, command, "Success", result, false)
}

/// Resource commands expose native data in a non-indented UTF-8 envelope.
pub fn resource_success(timestamp: &str, command: &str, data: Value) -> String {
    let mut result = Map::new();
    result.insert("data".into(), data);
    envelope(timestamp, command, "Success", result, true)
}

/// Ordered Python failure envelope. Recovery evidence is an approved extension.
pub fn failure(timestamp: &str, command: &str, error: &CommandError) -> String {
    let mut result = Map::new();
    if let Some(recovery) = &error.recovery {
        result.insert("data".into(), Value::Array(vec![recovery.clone()]));
    }
    result.insert("error_code".into(), Value::String(error.code.clone()));
    result.insert("message".into(), Value::String(error.message.clone()));
    envelope(timestamp, command, "Failure", result, false)
}

fn envelope(
    timestamp: &str,
    command: &str,
    status: &str,
    result: Map<String, Value>,
    resource: bool,
) -> String {
    let mut root = Map::new();
    root.insert("timestamp".into(), Value::String(timestamp.into()));
    root.insert("status".into(), Value::String(status.into()));
    if !command.is_empty() {
        root.insert("command".into(), Value::String(command.into()));
    }
    root.insert("result".into(), Value::Object(result));
    let value = Value::Object(root);
    let mut text = if resource {
        fabric_core_json::encode_unicode(&value, Format::Default)
    } else {
        fabric_core_json::encode(&value, Format::Pretty(Indent::Four))
    };
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::resource_success;
    use serde_json::{Value, json};

    #[test]
    fn resource_envelope_avoids_pretty_print_amplification() {
        let mut nested = json!("é");
        for _ in 0..80 {
            nested = json!({"x": nested});
        }
        let text = resource_success("time", "item list", nested.clone());
        assert!(text.len() < 1024);
        assert!(text.contains('é'));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["result"]["data"], nested);
    }
}
