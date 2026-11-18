// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! JSON envelopes and stdout reporting.

use crate::Host;
use fabric_cli_command_contracts::{AuthOutput, CommandError, CommandOutput, python_truthy};
use fabric_core_json::{Format, Indent};
use serde_json::{Map, Value};
use std::io::Write;

/// Render typed command data using the existing Python-compatible JSON envelope.
pub fn success_typed(timestamp: &str, command: &str, data: CommandOutput) -> String {
    let data = match data {
        CommandOutput::Api(value) => value,
        CommandOutput::Auth(auth) => auth_data(auth),
    };
    success(timestamp, command, data)
}

fn auth_data(auth: AuthOutput) -> Value {
    let (tenant_id, principal_id, login) = match auth {
        AuthOutput::Login {
            tenant_id,
            principal_id,
        } => (tenant_id, principal_id, true),
        AuthOutput::Status {
            tenant_id,
            principal_id,
        } => (tenant_id, principal_id, false),
        AuthOutput::Logout => {
            let mut data = Map::new();
            data.insert("selected".into(), Value::Bool(false));
            return Value::Object(data);
        }
    };
    let mut data = Map::new();
    data.insert("logged_in".into(), Value::Bool(true));
    data.insert(
        "authentication_mode".into(),
        Value::String("Azure CLI".into()),
    );
    data.insert("tenant_id".into(), Value::String(tenant_id));
    data.insert("principal_id".into(), Value::String(principal_id));
    if login {
        data.insert("selected".into(), Value::Bool(true));
        data.insert("mfa_evidence".into(), Value::String("not_checked".into()));
        data.insert("service_access_verified".into(), Value::Bool(false));
    }
    Value::Object(data)
}

/// Ordered success envelope, preserving empty API query objects and strings.
pub(super) fn success(timestamp: &str, command: &str, data: Value) -> String {
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
pub(super) fn failure(timestamp: &str, command: &str, error: &CommandError) -> String {
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
        result.insert("data".into(), Value::Array(vec![*recovery]));
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

pub(super) fn timestamp(host: &dyn Host) -> String {
    let date: chrono::DateTime<chrono::Utc> = host.clock().wall_now().into();
    let precision = if date.timestamp_subsec_micros() == 0 {
        chrono::SecondsFormat::Secs
    } else {
        chrono::SecondsFormat::Micros
    };
    date.to_rfc3339_opts(precision, true)
}

pub(super) fn write_failure(
    host: &dyn Host,
    command: &str,
    error: CommandError,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let code = error.exit_code();
    let output = failure(&timestamp(host), command, &error);
    if stdout.write_all(output.as_bytes()).is_err() {
        let _ = stderr.write_all(b"Cannot write JSON output\n");
        return 1;
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn output_result_lists_stay_flat_and_non_api_falsey_nonlists_omit_data() {
        for (input, expected) in [
            (
                json!([{"id":1},{"id":2}]),
                json!({"data":[{"id":1},{"id":2}]}),
            ),
            (json!([]), json!({"data":[]})),
            (json!({}), json!({})),
            (json!(false), json!({})),
            (json!(0), json!({})),
            (json!(""), json!({})),
        ] {
            let output = success("2000-01-01T00:00:00Z", "auth status", input);
            assert_eq!(
                serde_json::from_str::<Value>(&output).unwrap()["result"],
                expected
            );
            assert!(output.ends_with('\n'));
        }
    }

    #[test]
    fn typed_auth_results_match_legacy_json_including_field_order() {
        let cases = [
            (
                "auth login",
                AuthOutput::Login {
                    tenant_id: "tenant".into(),
                    principal_id: "principal".into(),
                },
                json!({
                    "logged_in": true,
                    "authentication_mode": "Azure CLI",
                    "tenant_id": "tenant",
                    "principal_id": "principal",
                    "selected": true,
                    "mfa_evidence": "not_checked",
                    "service_access_verified": false,
                }),
            ),
            (
                "auth status",
                AuthOutput::Status {
                    tenant_id: "tenant".into(),
                    principal_id: "principal".into(),
                },
                json!({
                    "logged_in": true,
                    "authentication_mode": "Azure CLI",
                    "tenant_id": "tenant",
                    "principal_id": "principal",
                }),
            ),
            (
                "auth logout",
                AuthOutput::Logout,
                json!({"selected": false}),
            ),
        ];
        for (command, auth, legacy) in cases {
            assert_eq!(
                success_typed("timestamp", command, CommandOutput::Auth(auth)),
                success("timestamp", command, legacy),
            );
        }
    }

    #[test]
    fn typed_api_results_preserve_arbitrary_json_and_legacy_envelopes() {
        for value in [
            json!(null),
            json!(false),
            json!(0),
            json!(""),
            json!({}),
            json!([]),
            json!([{"id": 1}, null]),
            json!({"z": 1, "a": {"nested": true}}),
            serde_json::from_str("9007199254740993123456789").unwrap(),
        ] {
            assert_eq!(
                success_typed("timestamp", "api", CommandOutput::Api(value.clone())),
                success("timestamp", "api", value),
            );
        }
    }

    #[test]
    fn recovery_guidance_replaces_public_uncertainty_without_changing_internal_evidence() {
        for uncertain in [true, false] {
            let mut error = CommandError::new("Service", "Original failure");
            let recovery = json!({
                "operation_id": "op-1",
                "status_url": "https://api.fabric.microsoft.com/v1/operations/op-1",
                "outcome_uncertain": uncertain
            });
            error.recovery = Some(Box::new(recovery.clone()));
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
            assert_eq!(error.recovery.as_deref(), Some(&recovery));
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
