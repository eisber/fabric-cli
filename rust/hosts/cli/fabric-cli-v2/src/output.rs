// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Separate operation evidence from rendering; output failure never replays work.

use std::io::{self, Write};

use fabric_cli_output::{
    production_unicode::{key_formatter, value_formatter},
    text::{Presentation, render_key_values},
};
use serde_json::{Value, json};

use crate::{CliError, OperationResult, Status, parser::Format};

fn envelope(command: &str, outcome: &Result<OperationResult, CliError>) -> Value {
    match outcome {
        Ok(result) => json!({
            "schemaVersion": "0.1",
            "command": command,
            "status": result.status,
            "data": result.data,
            "effects": result.effects,
            "error": null,
        }),
        Err(error) => json!({
            "schemaVersion": "0.1",
            "command": command,
            "status": "failed",
            "data": null,
            "effects": error.effects,
            "error": {"code": error.code, "message": error.message},
        }),
    }
}

fn write_json(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, value).map_err(io::Error::other)?;
    writer.write_all(b"\n")
}

/// Render one outcome once; retain observed effects if writing stdout fails.
///
/// Success/draft/handoff/accepted receipts use stdout, failures use stderr.
/// A pending accepted update is not an installed receipt. Partial/unknown
/// outcomes and errors are nonzero. No operation callback is accepted here.
///
/// # Errors
///
/// Returns output failures after a best-effort structured diagnostic. The
/// diagnostic retains any operation result with observed effects so a known
/// receipt is not silently discarded or retried.
pub fn write_outcome(
    command: &str,
    outcome: Result<OperationResult, CliError>,
    format: Format,
    mut stdout: impl Write,
    mut stderr: impl Write,
) -> io::Result<u8> {
    let value = envelope(command, &outcome);
    let exit = match &outcome {
        Ok(result) if matches!(result.status, Status::Partial | Status::Unknown) => 1,
        Ok(_) => 0,
        Err(error) if error.code == "InvalidInput" => 2,
        Err(_) => 1,
    };
    let failed = outcome.is_err();
    let written = if format == Format::Json {
        if failed {
            write_json(&mut stderr, &value)
        } else {
            write_json(&mut stdout, &value)
        }
    } else {
        let text_value = json!({
            "schema_version": value["schemaVersion"],
            "command": value["command"],
            "status": value["status"],
            "data": value["data"],
            "effects": value["effects"],
            "error": value["error"],
        });
        let plan = render_key_values(
            &text_value,
            key_formatter,
            value_formatter,
            Presentation::default(),
        )
        .map_err(io::Error::other);
        match plan {
            Ok(plan) if failed => {
                let mut errors = Vec::new();
                plan.write_to(&mut errors, &mut Vec::new())
                    .and_then(|()| stderr.write_all(&errors))
            }
            Ok(plan) => plan.write_to(&mut stdout, &mut stderr),
            Err(error) => Err(error),
        }
    }
    .and_then(|()| {
        if failed {
            stderr.flush()
        } else {
            stdout.flush()
        }
    });
    if let Err(error) = written {
        let retained = match &outcome {
            Ok(result) if !result.effects.is_empty() => value,
            Err(failure) if !failure.effects.is_empty() => value,
            _ => Value::Null,
        };
        let diagnostic = json!({
            "schemaVersion": "0.1",
            "command": command,
            "status": "failed",
            "error": {
                "code": "OutputFailure",
                "message": "Writing the result failed. Effects were not repeated."
            },
            "observedOutcome": retained,
        });
        let _diagnostic_result = write_json(&mut stderr, &diagnostic);
        return Err(error);
    }
    Ok(exit)
}
