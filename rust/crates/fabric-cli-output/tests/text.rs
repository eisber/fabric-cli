// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standalone consumers of the pure shared text/table renderer.

#[path = "support/text.rs"]
mod support;

use fabric_cli_output::{OutputEnvelope, OutputInput, Timestamp, text::*};
use serde_json::{Value, json};
use std::io::{self, Write};

#[test]
fn text_streams_exits_errors_and_handwritten_bytes() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../tests/compatibility/output/text-cases.json"
    ))
    .unwrap();
    for case in cases {
        let actual = support::observe(&case);
        if case.get("stdout").is_some() {
            for key in ["stdout", "stderr", "exit"] {
                assert_eq!(actual[key], case[key], "{} {key}", case["name"]);
            }
        }
        if case.get("error").is_some() {
            for key in ["error", "code"] {
                assert_eq!(actual[key], case[key], "{} {key}", case["name"]);
            }
        } else if case.get("stdout").is_none() {
            assert!(actual.get("error").is_none(), "{}: {actual}", case["name"]);
        }
    }
}

#[test]
fn data_is_not_mutated_by_truncation_or_format_selection() {
    let data = json!({"description":"abcdefghijklmnopqrstuvwxyz012345678901234567890123456789"});
    let output = OutputEnvelope::new(
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        OutputInput {
            data: Some(data.clone()),
            ..OutputInput::default()
        },
    );
    let columns = ["description".to_owned()];
    let options = TextOptions {
        truncate_columns: &columns,
        terminal_width: Some(20),
        ..TextOptions::default()
    };
    let before = output.to_value().unwrap();
    render(
        &output,
        OutputFormat::Text,
        &options,
        &|_| Some(1),
        Presentation::default(),
    )
    .unwrap();
    assert_eq!(output.to_value().unwrap(), before);
    assert_eq!(before["result"]["data"][0], data);
}

#[test]
fn unsupported_unicode_properties_fail_instead_of_guessing() {
    assert_eq!(
        title_key("\u{00e9}"),
        Err(TextError::UnicodeKey("\u{00e9}".to_owned()))
    );
    assert!(matches!(
        render_table(
            &json!({"x":"\u{754c}"}),
            &["x".to_owned()],
            false,
            80,
            &ascii_width,
            Presentation::default()
        ),
        Err(TextError::MissingWidth(0x754c))
    ));
    assert!(matches!(
        python_string(&json!(["\u{200b}"])),
        Err(TextError::UnicodeRepr(_))
    ));
}

struct Broken;
impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "synthetic"))
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("no flush")
    }
}

#[test]
fn text_error_and_notice_writer_failures_are_not_swallowed() {
    let plan = render_notice(Notice::Warning("x"), None, Presentation::default());
    let mut stdout = Vec::new();
    assert_eq!(
        plan.write_to(&mut stdout, &mut Broken).unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert!(stdout.is_empty());
}

#[test]
fn ordered_mixed_streams_stop_at_first_io_error_and_preserve_partial_render_failures() {
    let envelope = OutputEnvelope::new(
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        OutputInput {
            data: Some(json!(["first"])),
            hidden_data: Some(fabric_cli_output::HiddenItems::Strings(vec![
                "hidden".to_owned(),
            ])),
            ..OutputInput::default()
        },
    );
    let plan = render(
        &envelope,
        OutputFormat::Text,
        &TextOptions::default(),
        &ascii_width,
        Presentation::default(),
    )
    .unwrap();
    assert_eq!(
        plan.records
            .iter()
            .map(|record| record.stream)
            .collect::<Vec<_>>(),
        [
            fabric_cli_output::OutputStream::Stdout,
            fabric_cli_output::OutputStream::Stderr,
            fabric_cli_output::OutputStream::Stdout
        ]
    );
    let mut stdout = Vec::new();
    assert_eq!(
        plan.write_to(&mut stdout, &mut Broken).unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(stdout, b"first\n");

    let error = render_key_values(
        &json!({"id":1,"bad-key":2}),
        title_key,
        python_string,
        Presentation::default(),
    )
    .unwrap_err();
    let partial = error.partial_output().unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    partial.write_to(&mut stdout, &mut stderr).unwrap();
    assert_eq!(stdout, b"ID: 1\n");
    assert!(stderr.is_empty());
    assert_eq!(
        error.to_string(),
        "Invalid key format: 'bad-key'. Only underscore-separated words are allowed."
    );
}
