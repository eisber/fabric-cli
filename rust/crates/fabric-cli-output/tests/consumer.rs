// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standalone consumers exercise only public APIs, not test-only library helpers.

mod support;

use fabric_cli_output::{
    Indent, OutputEnvelope, OutputError, OutputInput, OutputStream, Timestamp, finite_number,
    parse_data,
};
use serde_json::{Value, json};
use std::io::{self, Write};

#[test]
fn source_cases_match_semantics_key_order_streams_and_handwritten_bytes() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../tests/compatibility/output/cases.json"
    ))
    .unwrap();
    for case in fixtures["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let plan = support::plan(case, fixtures["timestamp"].as_str().unwrap());
        assert_eq!(plan.stream, OutputStream::Stdout, "{name}");
        assert_eq!(u64::from(plan.exit_code), case["exit"], "{name}");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        plan.write_to(&mut stdout, &mut stderr).unwrap();
        assert!(stderr.is_empty(), "{name}");
        assert_eq!(stdout, plan.payload, "{name}");
        assert!(stdout.ends_with(b"\n") && !stdout.ends_with(b"\n\n"));
        assert!(!stdout.contains(&b'\r'), "{name}: no newline normalization");
        let actual: Value = serde_json::from_slice(&stdout).unwrap();
        let mut expected = json!({
            "timestamp": fixtures["timestamp"],
            "status": if case["exit"] == 0 { "Success" } else { "Failure" },
        });
        if let Some(command) = case.get("command") {
            expected["command"] = command.clone();
        }
        expected["result"] = case["result"].clone();
        assert_eq!(actual, expected, "{name}: complete semantic tree");
        assert_eq!(
            actual.as_object().unwrap().keys().collect::<Vec<_>>(),
            expected.as_object().unwrap().keys().collect::<Vec<_>>(),
            "{name}: envelope key order"
        );
        assert_eq!(
            actual["result"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            case["result"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            "{name}: result key order"
        );
        if let Some(expected_json) = case["json"].as_str() {
            assert_eq!(stdout, format!("{expected_json}\n").as_bytes(), "{name}");
        }
    }
}

#[test]
fn values_keep_nested_object_insertion_order() {
    let value = parse_data(r#"{"z":{"y":2,"b":1},"a":0}"#).unwrap();
    let envelope = OutputEnvelope::new(
        Timestamp::parse("2000-02-29T23:59:59Z").unwrap(),
        OutputInput {
            data: Some(value),
            ..OutputInput::default()
        },
    );
    let value = envelope.to_value().unwrap();
    let nested = &value["result"]["data"][0];
    assert_eq!(
        nested.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["z", "a"]
    );
    assert_eq!(
        nested["z"].as_object().unwrap().keys().collect::<Vec<_>>(),
        ["y", "b"]
    );
    assert_eq!(envelope.result().data().unwrap().len(), 1);
    assert_eq!(envelope.result().hidden_data(), None);
    assert_eq!(envelope.result().message(), None);
}

#[test]
fn timestamp_validation_does_not_read_or_rewrite_a_clock() {
    for value in [
        "0001-01-01T00:00:00Z",
        "2000-02-29T23:59:59.000001Z",
        "9999-12-31T23:59:59.999999Z",
    ] {
        assert_eq!(Timestamp::parse(value).unwrap().as_str(), value);
    }
    for value in [
        "",
        "x",
        "0000-01-01T00:00:00Z",
        "1900-02-29T00:00:00Z",
        "2000-02-30T00:00:00Z",
        "2000-00-01T00:00:00Z",
        "2000-13-01T00:00:00Z",
        "2000-01-00T00:00:00Z",
        "2000-01-01T24:00:00Z",
        "2000-01-01T00:60:00Z",
        "2000-01-01T00:00:60Z",
        "2000-01-01T00:00:00+00:00",
        "2000-01-01T00:00:00.1Z",
        "2000-01-01T00:00:00.0000000Z",
        "2000-01-01T00:00:00.abcdefZ",
        "2000-01-01T00:00:00z",
        "2000-01-01T00:00:00Z\n",
        "\u{1f680}",
    ] {
        assert_eq!(
            Timestamp::parse(value),
            Err(OutputError::InvalidTimestamp),
            "{value:?}"
        );
    }
}

#[test]
fn nonfinite_malformed_and_unsupported_values_fail_explicitly() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(finite_number(value), Err(OutputError::InvalidJson));
    }
    for input in [
        "NaN",
        "Infinity",
        "-Infinity",
        "1e999",
        "{",
        "{1, 2}",
        "b'bytes'",
        "\"\\ud800\"",
        "[null,]",
        "18446744073709551616",
        "[-9223372036854775809]",
        "{\"x\":999999999999999999999999}",
    ] {
        assert_eq!(parse_data(input), Err(OutputError::InvalidJson), "{input}");
    }
    assert_eq!(
        parse_data(r#"["999999999999999999999999", {"x":"1,\"huge\":999999999999999999999999"}]"#)
            .unwrap(),
        json!(["999999999999999999999999", {"x":"1,\"huge\":999999999999999999999999"}])
    );
    assert_eq!(
        finite_number(-0.0).unwrap().as_f64().unwrap().to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(
        OutputError::InvalidJson.to_string(),
        "[InvalidJson] Invalid JSON format"
    );
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
                "test writer failure",
            ));
        }
        let count = self.remaining.min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..count]);
        self.remaining -= count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        panic!("the output library must not flush caller-owned streams");
    }
}

#[test]
fn output_io_failure_is_propagated_without_stderr_fallback() {
    let plan = OutputEnvelope::new(
        Timestamp::parse("2020-01-01T00:00:00Z").unwrap(),
        OutputInput::default(),
    )
    .render(Indent::Four)
    .unwrap();
    for count in [0, 7] {
        let mut stdout = BrokenWriter {
            remaining: count,
            bytes: Vec::new(),
        };
        let mut stderr = Vec::new();
        let error = plan.write_to(&mut stdout, &mut stderr).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(stdout.bytes, plan.payload[..count]);
        assert!(stderr.is_empty());
    }
}

#[test]
fn caller_selected_diagnostic_writer_is_not_replaced_with_stdout() {
    let plan = fabric_cli_output::RenderPlan {
        stream: OutputStream::Stderr,
        exit_code: 1,
        payload: b"diagnostic\n".to_vec(),
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    plan.write_to(&mut stdout, &mut stderr).unwrap();
    assert!(stdout.is_empty());
    assert_eq!(stderr, plan.payload);
    let mut stderr = BrokenWriter {
        remaining: 3,
        bytes: Vec::new(),
    };
    assert_eq!(
        plan.write_to(&mut stdout, &mut stderr).unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert!(stdout.is_empty());
    assert_eq!(stderr.bytes, b"dia");
}
