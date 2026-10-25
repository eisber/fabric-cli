// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli_output::OutputInput;
use fabric_sdk_operations::OperationError;
use fabric_sdk_workspaces::mutations::ResourceError;
use serde_json::Value;

/// Project a borrowed SDK operation failure through the existing Python CLI policy.
///
/// This pure same-layer integration seam is shared by resource and table/job CLI
/// consumers. It does not log, perform I/O, consume the SDK error, fabricate an
/// HTTP status or discard its operation/uncertainty evidence. Structured service
/// data is intentionally rendered only at this explicit CLI boundary.
///
/// Existing source dictionary/list/scalar/Unicode formatting, period stripping,
/// code selection and unsupported-JSON failure behavior are unchanged.
pub fn operation_error_output(error: &OperationError) -> OutputInput {
    crate::MutationError::from(error.resource_error()).output()
}

pub(crate) fn source_message(error: &ResourceError) -> Result<String, ()> {
    if let Some(status) = error.status
        && !(200..300).contains(&status)
    {
        return Ok(match status {
            401 => "Access is unauthorized".into(),
            403 => "Access is forbidden. You do not have permission to access this resource".into(),
            404 => "The requested resource could not be found".into(),
            _ => error
                .raw_service_error
                .as_deref()
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .filter(|message| !message.is_empty())
                .unwrap_or("An error occurred while processing the operation")
                .to_owned(),
        });
    }
    if let Some(detail) = error.raw_service_error.as_deref() {
        let prefix = match error.code.as_str() {
            "LongRunningOperationFailed" => Some("The operation failed: "),
            "LongRunningOperationCancelled" => Some("The operation was cancelled: "),
            _ => None,
        };
        if let Some(prefix) = prefix {
            let detail = match detail {
                Value::String(value) => value.clone(),
                _ => python_repr(detail)?,
            };
            return Ok(format!("{prefix}{detail}"));
        }
    }
    Ok(error.message.clone())
}

fn python_repr(value: &Value) -> Result<String, ()> {
    Ok(match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(_) => match fabric_cli_query::search(value, "@").map_err(|_| ())? {
            Value::String(value) => value,
            _ => return Err(()),
        },
        Value::String(value) => quoted(value),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_repr)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| { Ok(format!("{}: {}", quoted(key), python_repr(value)?)) })
                .collect::<Result<Vec<_>, ()>>()?
                .join(", ")
        ),
    })
}

fn quoted(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::new();
    result.push(quote);
    for character in value.chars() {
        match character {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            character if character == quote => {
                result.push('\\');
                result.push(character);
            }
            character
                if character.is_control()
                    || character.escape_debug().to_string().starts_with("\\u{") =>
            {
                let code = u32::from(character);
                result.push_str(&if code <= 0xff {
                    format!("\\x{code:02x}")
                } else if code <= 0xffff {
                    format!("\\u{code:04x}")
                } else {
                    format!("\\U{code:08x}")
                });
            }
            character => result.push(character),
        }
    }
    result.push(quote);
    result
}
