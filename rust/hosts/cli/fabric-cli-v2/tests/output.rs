// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Output failures preserve known effects and never own execution callbacks.
#![deny(unnameable_test_items)]

use std::io::{self, Write};

use fabric_cli_v2::{
    CliError, EffectKind, EffectState, OperationResult, Status, output, parser::Format,
};
use serde_json::json;

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
