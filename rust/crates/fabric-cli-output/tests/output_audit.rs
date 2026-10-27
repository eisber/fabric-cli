// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive regressions against independently executed, frozen Python captures.

use fabric_cli_output::{
    CommandError, HiddenItems, Indent, OutputEnvelope, OutputInput, OutputStatus, Timestamp,
    text::{self, OutputFormat, Presentation, TextOptions},
};
use serde_json::{Value, json};
use std::io::{self, Write};

#[test]
fn failed_text_envelopes_match_frozen_python_error_adapter() {
    let captures: Value = serde_json::from_str(include_str!(
        "../../../reviews/20261027/python-output-captures.json"
    ))
    .unwrap();
    for (name, command, code, message) in [
        ("get-text-error", "get", "Unauthorized", "Denied."),
        (
            "ls-text-query-error",
            "ls",
            "InvalidInput",
            "Invalid jmespath query (https://jmespath.org)",
        ),
        (
            "read-text-cancel",
            "get",
            "LongRunningOperationCancelled",
            "Operation cancelled",
        ),
        ("job-text-error", "job run-list", "Unauthorized", "Denied."),
    ] {
        let expected = captures["observations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|capture| capture["name"] == name)
            .unwrap();
        let envelope = CommandError::new(Some(message.into()), Some(code.into())).envelope(
            Timestamp::parse(captures["timestamp"].as_str().unwrap()).unwrap(),
            Some(command.into()),
        );
        let plan = text::render(
            &envelope,
            OutputFormat::Text,
            &TextOptions::default(),
            &text::ascii_width,
            Presentation::default(),
        )
        .unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        plan.write_to(&mut out, &mut err).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            expected["stdout"].as_str().unwrap(),
            "{name}"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            expected["stderr"].as_str().unwrap(),
            "{name}"
        );
        assert_eq!(u64::from(plan.exit_code), expected["exit"], "{name}");
    }
}

#[test]
fn failure_routing_preserves_normalized_fields_and_ignores_success_flags() {
    for (message, code, exit) in [
        ("", "InvalidInput", 1),
        ("Normalized operation message.", "Unauthorized", 4),
        ("OneLake detail.\nRequest Id: selected.", "unauthorized", 1),
    ] {
        let envelope = OutputEnvelope::new(
            Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
            OutputInput {
                command: Some("get".into()),
                status: OutputStatus::Failure,
                message: Some(message.into()),
                error_code: Some(code.into()),
                data: Some(json!([{"invalid-key": ["\u{754c}"]}])),
                hidden_data: Some(HiddenItems::Strings(vec!["hidden".into()])),
            },
        );
        let before = envelope.to_value().unwrap();
        let plan = text::render(
            &envelope,
            OutputFormat::Text,
            &TextOptions {
                subcommand: Some("ls"),
                show_headers: true,
                key_value: true,
                truncate_columns: &["invalid-key".into()],
                ..TextOptions::default()
            },
            &|_| panic!("failure text must not request success table widths"),
            Presentation::default(),
        )
        .unwrap();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        plan.write_to(&mut stdout, &mut stderr).unwrap();
        assert_eq!(stdout, format!("x get: [{code}] {message}\n").as_bytes());
        assert!(stderr.is_empty());
        assert_eq!(plan.exit_code, exit);
        assert_eq!(envelope.to_value().unwrap(), before);
        let json = text::render(
            &envelope,
            OutputFormat::Json,
            &TextOptions::default(),
            &text::ascii_width,
            Presentation::default(),
        )
        .unwrap();
        assert_eq!(json.records, [envelope.render(Indent::Four).unwrap()]);
        assert_eq!(json.exit_code, exit);
    }
}

struct BrokenWriter {
    remaining: usize,
    bytes: Vec<u8>,
}

impl Write for BrokenWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "source error delivery",
            ));
        }
        let count = self.remaining.min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..count]);
        self.remaining -= count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        panic!("caller-owned output must not be flushed")
    }
}

#[test]
fn routed_failure_writer_errors_retain_partial_bytes_without_stderr_fallback() {
    let envelope = CommandError::new(Some("Denied.".into()), Some("Unauthorized".into())).envelope(
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        Some("get".into()),
    );
    let plan = text::render(
        &envelope,
        OutputFormat::Text,
        &TextOptions::default(),
        &text::ascii_width,
        Presentation::default(),
    )
    .unwrap();
    let expected = b"x get: [Unauthorized] Denied\n";
    for remaining in [0, 7] {
        let mut stdout = BrokenWriter {
            remaining,
            bytes: Vec::new(),
        };
        let mut stderr = Vec::new();
        assert_eq!(
            plan.write_to(&mut stdout, &mut stderr).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe,
        );
        assert_eq!(stdout.bytes, expected[..remaining]);
        assert!(stderr.is_empty());
    }
}
