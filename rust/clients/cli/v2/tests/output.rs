// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Output failures preserve known effects and never own execution callbacks.
#![deny(unnameable_test_items)]

use std::io::{self, Write};

use fabric_cli_v2::{
    CliError, EffectKind, EffectState, OperationResult, Status, output, parser::Format,
};
use serde_json::json;

#[test]
fn public_local_types_are_identity_preserving_shared_contract_reexports() {
    let result: fabric_cli_output::outcome::OperationResult =
        OperationResult::succeeded(json!({"value":null}));
    let _: OperationResult = result;
    let error: fabric_cli_output::outcome::CliError = CliError::new("InvalidInput", "Invalid.");
    let _: CliError = error;
}

#[test]
fn pure_json_plans_preserve_the_existing_envelope_stream_and_exit_matrix() {
    for (status, exit) in [
        (Status::Succeeded, 0),
        (Status::Prepared, 0),
        (Status::Handoff, 0),
        (Status::Accepted, 0),
        (Status::Partial, 1),
        (Status::Unknown, 1),
    ] {
        let result = OperationResult::new(status, json!({"missingIsAbsent":null,"empty":[]}));
        let expected = json!({
            "schemaVersion":"0.1",
            "command":"cli show",
            "status":status,
            "data":{"missingIsAbsent":null,"empty":[]},
            "effects":[],
            "error":null,
        });
        let prepared = output::render_outcome("cli show", Ok(result), Format::Json).unwrap();
        let mut bytes = serde_json::to_vec(&expected).unwrap();
        bytes.push(b'\n');
        assert_eq!(prepared.plan.exit_code, exit);
        assert_eq!(prepared.plan.records.len(), 1);
        assert_eq!(
            prepared.plan.records[0].stream,
            fabric_cli_output::OutputStream::Stdout
        );
        assert_eq!(prepared.plan.records[0].payload, bytes);
        assert_eq!(prepared.outcome.unwrap().status, status);
    }
    for (code, exit) in [("InvalidInput", 2), ("Unauthorized", 1), ("Cancelled", 1)] {
        for format in [Format::Json, Format::Text] {
            let prepared = output::render_outcome(
                "cli show",
                Err(CliError::new(code, "Original diagnostic.")),
                format,
            )
            .unwrap();
            assert_eq!(prepared.plan.exit_code, exit);
            assert!(
                prepared
                    .plan
                    .records
                    .iter()
                    .all(|record| { record.stream == fabric_cli_output::OutputStream::Stderr })
            );
            assert_eq!(
                prepared.outcome.unwrap_err().message,
                "Original diagnostic."
            );
        }
    }
}

#[test]
fn pure_text_plan_is_the_existing_key_value_presentation_not_tabular_inference() {
    use fabric_cli_output::{
        production_unicode::{key_formatter, value_formatter},
        text::{Presentation, render_key_values},
    };
    let data = json!([{"x":1},{"unrelated":null}]);
    let expected = render_key_values(
        &json!({
            "schema_version":"0.1",
            "command":"cli show",
            "status":"succeeded",
            "data":data,
            "effects":[],
            "error":null,
        }),
        key_formatter,
        value_formatter,
        Presentation::default(),
    )
    .unwrap();
    let prepared = output::render_outcome(
        "cli show",
        Ok(OperationResult::succeeded(data)),
        Format::Text,
    )
    .unwrap();
    assert_eq!(prepared.plan, expected);
}

#[test]
fn text_render_failure_retains_the_original_accepted_local_effect() {
    use fabric_cli_output::{outcome::Recovery, presentation::PresentationError};

    let result = OperationResult::new(
        Status::Accepted,
        json!({"preview":"x".repeat(4 * 1024 * 1024 + 1)}),
    )
    .with_effect(EffectKind::Installation, EffectState::Pending);
    let failure = output::render_outcome("cli upgrade", Ok(result), Format::Text).unwrap_err();
    assert!(matches!(failure.error, PresentationError::Text(_)));
    let original = failure.outcome.unwrap();
    assert_eq!(original.status, Status::Accepted);
    assert_eq!(original.effects[0].state, EffectState::Pending);
    assert_eq!(failure.recovery, Recovery::Reconcile);
}

struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("synthetic writer failure"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct FlushFailure;

impl Write for FlushFailure {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("synthetic flush failure"))
    }
}

#[test]
fn a_buffer_flush_failure_retains_the_observed_effect() {
    let mut stderr = Vec::new();
    assert!(
        output::write_outcome(
            "cli show",
            Ok(
                OperationResult::succeeded(json!({"display_verified": false}))
                    .with_effect(EffectKind::ViewerLaunch, EffectState::Acknowledged)
            ),
            Format::Json,
            FlushFailure,
            &mut stderr,
        )
        .is_err()
    );
    let retained: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(
        retained["observedOutcome"]["effects"][0]["state"],
        "acknowledged"
    );
}

#[test]
fn observed_effects_and_receipt_survive_stdout_failure() {
    let receipt = OperationResult::new(
        Status::Accepted,
        json!({"installed": false, "transaction": "synthetic-transaction"}),
    )
    .with_effect(EffectKind::Installation, EffectState::Pending);
    let mut stderr = Vec::new();
    assert!(
        output::write_outcome(
            "cli upgrade",
            Ok(receipt),
            Format::Json,
            BrokenWriter,
            &mut stderr,
        )
        .is_err()
    );
    let retained: serde_json::Value = serde_json::from_slice(&stderr).unwrap();
    assert_eq!(retained["error"]["code"], "OutputFailure");
    assert_eq!(retained["observedOutcome"]["status"], "accepted");
    assert_eq!(
        retained["observedOutcome"]["data"]["transaction"],
        "synthetic-transaction"
    );
    assert_eq!(
        retained["observedOutcome"]["effects"][0]["state"],
        "pending"
    );
}

#[test]
fn draft_output_failure_does_not_duplicate_private_payload_in_diagnostic() {
    let mut stderr = Vec::new();
    assert!(
        output::write_outcome(
            "cli report",
            Ok(OperationResult::new(
                Status::Prepared,
                json!({"message": "caller-only-private-preview"}),
            )),
            Format::Json,
            BrokenWriter,
            &mut stderr,
        )
        .is_err()
    );
    assert!(
        !String::from_utf8(stderr)
            .unwrap()
            .contains("caller-only-private-preview")
    );
}

#[test]
fn failure_streams_and_effects_are_preserved() {
    for format in [Format::Json, Format::Text] {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = output::write_outcome(
            "cli show",
            Err(CliError::new("Timeout", "Launcher outcome is unknown.")
                .with_effect(EffectKind::ViewerLaunch, EffectState::Unknown)),
            format,
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(exit, 1);
        assert!(stdout.is_empty());
        assert!(String::from_utf8(stderr).unwrap().contains("unknown"));
    }
}
