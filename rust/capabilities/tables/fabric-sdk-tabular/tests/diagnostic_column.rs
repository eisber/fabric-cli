// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline native diagnostic column and metadata evidence regressions.
#![deny(unnameable_test_items)]

use std::sync::Arc;

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, Field, Metadata, Schema};
use fabric_sdk_tabular::{OutputLimits, TabularError, TabularResult, decode_arrow_stream};

fn stream(diagnostics: Vec<Option<&str>>) -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("value", DataType::Int64, false),
        Field::new("PQ Arrow Metadata", DataType::Utf8, true),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![1; diagnostics.len()])),
            Arc::new(StringArray::from(diagnostics)),
        ],
    )
    .unwrap();
    let mut bytes = Vec::new();
    let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
    writer.write(&batch).unwrap();
    writer.finish().unwrap();
    drop(writer);
    bytes
}

#[test]
fn valid_arrow_can_still_contain_a_dataflow_application_error() {
    let limits = OutputLimits::new(10, 1024 * 1024, 10).unwrap();
    let bytes = stream(vec![None, Some(r#"{"error":"source failure"}"#)]);
    let result = decode_arrow_stream(&bytes, limits).unwrap();
    let TabularError::Metadata { evidence } = result
        .check_diagnostic_column("PQ Arrow Metadata")
        .unwrap_err()
    else {
        panic!("expected application diagnostic evidence");
    };
    assert_eq!(evidence[0].key, "PQ Arrow Metadata");
    assert!(evidence[0].value.contains("source failure"));
}

#[test]
fn null_diagnostics_are_not_mistaken_for_failed_execution() {
    let limits = OutputLimits::new(10, 1024 * 1024, 10).unwrap();
    let result = decode_arrow_stream(&stream(vec![None, None]), limits).unwrap();
    result.check_diagnostic_column("PQ Arrow Metadata").unwrap();
    result
        .check_diagnostic_column("another engine diagnostic")
        .unwrap();
}

#[test]
fn unsupported_types_do_not_discard_diagnostic_metadata() {
    let limits = OutputLimits::new(10, 1024 * 1024, 10).unwrap();
    let schema = Arc::new(Schema::new_with_metadata(
        vec![
            Field::new(
                "unsupported",
                DataType::List(Arc::new(Field::new("item", DataType::Int64, true))),
                true,
            ),
            Field::new("later", DataType::Utf8, true)
                .with_metadata(Metadata::new().with("field-diagnostic", "later error")),
        ],
        Metadata::new().with("schema-diagnostic", "source failure"),
    ));
    let mut bytes = Vec::new();
    let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
    writer.finish().unwrap();
    drop(writer);
    for outcome in [
        TabularResult::try_new(schema, vec![], limits),
        decode_arrow_stream(&bytes, limits),
    ] {
        let TabularError::Metadata { evidence } = outcome.unwrap_err() else {
            panic!("diagnostic evidence must precede unsupported-type classification");
        };
        assert_eq!(evidence.len(), 2);
        assert!(evidence.iter().any(|entry| entry.value == "source failure"));
        assert!(evidence.iter().any(|entry| entry.value == "later error"));
    }
}
