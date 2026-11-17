// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public outcome-to-presentation-to-delivery consumers, with no operation replay.
#![deny(unnameable_test_items)]

use fabric_cli_output::{
    Indent, OutputEnvelope, OutputInput, OutputStream, RenderPlan, Timestamp,
    outcome::{CommandOutcome, EffectKind, EffectState, Recovery, Status},
    presentation::{DeliveryPhase, FlushPolicy, PreparedOutput, PresentationSource, render},
    tabular::{TabularColumn, TabularFormat, TabularTable, TabularType, encode},
    text::TextPlan,
};
use serde_json::{Value, json};
use std::io::{self, Write};

fn accepted() -> CommandOutcome<Value> {
    CommandOutcome::new(Status::Accepted, json!({"operationId":"receipt-1"}))
        .with_effect(EffectKind::Installation, EffectState::Pending)
}

fn json_plan(value: &Value) -> TextPlan {
    render(PresentationSource::Json(value), OutputStream::Stdout, 0).unwrap()
}

#[test]
fn raw_json_remains_raw_including_empty_missing_and_null() {
    for (value, expected) in [
        (json!({}), b"{}\n".as_slice()),
        (json!([]), b"[]\n".as_slice()),
        (Value::Null, b"null\n".as_slice()),
        (json!({"data":null}), b"{\"data\":null}\n".as_slice()),
        (
            json!([{"x":1}, {"unrelated":[true]}]),
            b"[{\"x\":1},{\"unrelated\":[true]}]\n".as_slice(),
        ),
    ] {
        let plan = json_plan(&value);
        assert_eq!(plan.records[0].payload, expected);
        assert_eq!(plan.records[0].stream, OutputStream::Stdout);
    }
    let silent = render(PresentationSource::Silent, OutputStream::Stdout, 0).unwrap();
    assert!(silent.records.is_empty());
    let empty_bytes = render(PresentationSource::Bytes(b""), OutputStream::Stdout, 0).unwrap();
    assert_eq!(empty_bytes.records.len(), 1);
    assert!(empty_bytes.records[0].payload.is_empty());
}

#[test]
fn envelope_and_binary_bytes_are_not_reencoded_or_normalized() {
    let envelope = OutputEnvelope::new(
        Timestamp::parse("2000-01-01T00:00:00Z").unwrap(),
        OutputInput {
            command: Some("get".into()),
            data: Some(json!([])),
            ..OutputInput::default()
        },
    );
    let expected = envelope.render(Indent::Four).unwrap();
    let plan = render(
        PresentationSource::Envelope(&envelope, Indent::Four),
        OutputStream::Stdout,
        0,
    )
    .unwrap();
    assert_eq!(plan.records, [expected]);
    let bytes = b"\0\xff\r\n";
    let plan = render(PresentationSource::Bytes(bytes), OutputStream::Stdout, 2).unwrap();
    assert_eq!(plan.records[0].payload, bytes);
    assert_eq!(plan.exit_code, 2);
}

#[test]
fn explicit_empty_schema_uses_existing_json_csv_and_parquet_codecs() {
    let table = TabularTable::new(
        vec![TabularColumn::new("id", TabularType::String, false)],
        vec![],
    )
    .unwrap();
    for format in [
        TabularFormat::Json,
        TabularFormat::Csv,
        TabularFormat::Parquet,
    ] {
        let plan = render(
            PresentationSource::Tabular(&table, format),
            OutputStream::Stdout,
            0,
        )
        .unwrap();
        assert_eq!(plan.records[0].payload, encode(&table, format).unwrap());
        match format {
            TabularFormat::Json => assert_eq!(plan.records[0].payload, b"[]"),
            TabularFormat::Csv => assert_eq!(plan.records[0].payload, b"id\r\n"),
            TabularFormat::Parquet => {
                assert!(plan.records[0].payload.starts_with(b"PAR1"));
                assert!(plan.records[0].payload.ends_with(b"PAR1"));
            }
        }
    }
}

#[test]
fn a_render_failure_retains_the_accepted_effect_and_recovery_constraint() {
    let table = TabularTable::new(vec![], vec![]).unwrap();
    let rendered = render(
        PresentationSource::Tabular(&table, TabularFormat::Parquet),
        OutputStream::Stdout,
        0,
    );
    let error = PreparedOutput::new(accepted(), rendered, Recovery::DoNotReplay).unwrap_err();
    assert_eq!(error.outcome.status, Status::Accepted);
    assert_eq!(error.outcome.data["operationId"], "receipt-1");
    assert_eq!(error.outcome.effects[0].state, EffectState::Pending);
    assert_eq!(error.recovery, Recovery::DoNotReplay);
}

struct PartialWriter {
    remaining: usize,
    interrupted: bool,
}

impl Write for PartialWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.interrupted {
            self.interrupted = false;
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self.remaining == 0 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let written = bytes.len().min(self.remaining);
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn partial_delivery_retains_typed_receipt_and_counts_each_stream() {
    let plan = TextPlan {
        records: vec![
            RenderPlan {
                stream: OutputStream::Stderr,
                exit_code: 0,
                payload: b"notice\n".to_vec(),
            },
            RenderPlan {
                stream: OutputStream::Stdout,
                exit_code: 0,
                payload: b"receipt\n".to_vec(),
            },
            RenderPlan {
                stream: OutputStream::Stderr,
                exit_code: 0,
                payload: b"not reached".to_vec(),
            },
        ],
        exit_code: 0,
    };
    let rendered = render(PresentationSource::Text(&plan), OutputStream::Stdout, 0);
    let prepared = PreparedOutput::new(accepted(), rendered, Recovery::DoNotReplay).unwrap();
    let mut stderr = Vec::new();
    let failure = prepared
        .deliver(
            &mut PartialWriter {
                remaining: 3,
                interrupted: true,
            },
            &mut stderr,
            FlushPolicy::Stream(OutputStream::Stdout),
        )
        .unwrap_err();
    assert_eq!(stderr, b"notice\n");
    assert_eq!(failure.stdout_written, 3);
    assert_eq!(failure.stderr_written, 7);
    assert_eq!(failure.phase, DeliveryPhase::Write);
    assert_eq!(failure.error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(failure.prepared.outcome.status, Status::Accepted);
    assert_eq!(failure.prepared.outcome.data["operationId"], "receipt-1");
    assert_eq!(failure.prepared.recovery, Recovery::DoNotReplay);
    assert_eq!(failure.prepared.plan, plan);
}

struct FlushFailure(Vec<u8>);

impl Write for FlushFailure {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::Other.into())
    }
}

#[test]
fn flush_failure_is_not_a_failed_or_rolled_back_operation() {
    let outcome = accepted();
    let rendered = Ok::<_, io::Error>(json_plan(&outcome.data));
    let prepared = PreparedOutput::new(outcome, rendered, Recovery::DoNotReplay).unwrap();
    let mut stdout = FlushFailure(Vec::new());
    let failure = prepared
        .deliver(
            &mut stdout,
            &mut Vec::new(),
            FlushPolicy::Stream(OutputStream::Stdout),
        )
        .unwrap_err();
    assert_eq!(failure.phase, DeliveryPhase::Flush);
    assert_eq!(failure.stdout_written, stdout.0.len());
    assert_eq!(failure.stderr_written, 0);
    assert_eq!(failure.prepared.outcome.status, Status::Accepted);
}

#[test]
fn host_selected_stream_exit_and_no_flush_policy_are_preserved() {
    let plan = render(
        PresentationSource::Json(&json!({"error":"invalid"})),
        OutputStream::Stderr,
        2,
    );
    let prepared = PreparedOutput::new((), plan, Recovery::None).unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), FlushFailure(Vec::new()));
    let delivered = prepared
        .deliver(&mut stdout, &mut stderr, FlushPolicy::None)
        .unwrap();
    assert!(stdout.is_empty());
    assert_eq!(stderr.0, b"{\"error\":\"invalid\"}\n");
    assert_eq!(delivered.plan.exit_code, 2);
}
