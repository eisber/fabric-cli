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

/// Ordered Python failure envelope. Recovery and job failure details are approved extensions.
pub fn failure(timestamp: &str, command: &str, error: &CommandError) -> String {
    let mut result = Map::new();
    let mut message = error.message.clone();
    if let Some(recovery) = &error.recovery {
        let mut recovery = recovery.clone();
        if let Some(fields) = recovery.as_object_mut()
            && fields.remove("outcome_uncertain") == Some(Value::Bool(true))
        {
            message.push_str(
                "\nThe operation's final status is unknown; it may still be running. Check status_url before retrying.",
            );
        }
        result.insert("data".into(), Value::Array(vec![recovery]));
    }
    result.insert("error_code".into(), Value::String(error.code.clone()));
    result.insert("message".into(), Value::String(message));
    if let Some(reason) = &error.failure_reason {
        let mut details = Map::new();
        details.insert("message".into(), Value::String(reason.message.clone()));
        result.insert("failure_reason".into(), Value::Object(details));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recovery_guidance_replaces_public_uncertainty_without_changing_internal_evidence() {
        for uncertain in [true, false] {
            let mut error = CommandError::new("Service", "Original failure");
            let recovery = json!({
                "operation_id": "op-1",
                "status_url": "https://api.fabric.microsoft.com/v1/operations/op-1",
                "outcome_uncertain": uncertain
            });
            error.recovery = Some(recovery.clone());
            let output: Value = serde_json::from_str(&failure("", "api", &error)).unwrap();
            assert_eq!(
                output["result"]["data"],
                json!([{
                    "operation_id": "op-1",
                    "status_url": "https://api.fabric.microsoft.com/v1/operations/op-1"
                }])
            );
            assert_eq!(
                output["result"]["message"],
                if uncertain {
                    "Original failure\nThe operation's final status is unknown; it may still be running. Check status_url before retrying."
                } else {
                    "Original failure"
                }
            );
            assert_eq!(error.recovery, Some(recovery));
            assert_eq!(output["result"]["error_code"], "Service");
        }
    }

    #[test]
    fn failures_without_an_accepted_operation_have_no_recovery_guidance() {
        let error = CommandError::new("InvalidInput", "Invalid arguments");
        let output: Value = serde_json::from_str(&failure("", "api", &error)).unwrap();
        assert_eq!(
            output["result"],
            json!({"error_code": "InvalidInput", "message": "Invalid arguments"})
        );
    }
}
