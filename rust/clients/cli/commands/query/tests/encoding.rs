// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Typed result encoding contracts for the query command capability.

#![deny(unnameable_test_items)]

use arrow_array::{
    Array, ArrayRef, BinaryArray, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use bytes::Bytes;
use fabric_cli_query_commands::{QueryEncodingError, encode_presentation, encode_single};
use fabric_sdk_tabular::{OutputLimits, ResultFormat, TabularResult};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::{Value, json};
use std::sync::Arc;

fn limits() -> OutputLimits {
    OutputLimits::new(100, 1024 * 1024, 8).unwrap()
}

fn table(duplicate_names: bool, empty: bool) -> TabularResult {
    let schema = Arc::new(Schema::new(vec![
        Field::new("integer", DataType::Int64, true),
        Field::new(
            if duplicate_names { "integer" } else { "text" },
            DataType::Utf8,
            true,
        ),
        Field::new("binary", DataType::Binary, true),
        Field::new(
            "instant",
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            true,
        ),
    ]));
    let arrays: Vec<ArrayRef> = if empty {
        vec![
            Arc::new(Int64Array::from(Vec::<Option<i64>>::new())),
            Arc::new(StringArray::from(Vec::<Option<&str>>::new())),
            Arc::new(BinaryArray::from(Vec::<Option<&[u8]>>::new())),
            Arc::new(
                TimestampNanosecondArray::from(Vec::<Option<i64>>::new()).with_timezone("UTC"),
            ),
        ]
    } else {
        vec![
            Arc::new(Int64Array::from(vec![Some(9_007_199_254_740_993), None])),
            Arc::new(StringArray::from(vec![Some(""), None])),
            Arc::new(BinaryArray::from(vec![Some(&b"\0\xff"[..]), None])),
            Arc::new(
                TimestampNanosecondArray::from(vec![Some(1_234_567_890), None])
                    .with_timezone("UTC"),
            ),
        ]
    };
    let batch = RecordBatch::try_new(Arc::clone(&schema), arrays).unwrap();
    TabularResult::try_new(schema, vec![batch], limits()).unwrap()
}

#[test]
fn zero_or_multiple_sets_never_select_first_for_any_single_file_format() {
    let table = table(false, false);
    for format in [ResultFormat::Json, ResultFormat::Csv, ResultFormat::Parquet] {
        for tables in [vec![], vec![&table, &table]] {
            let error = encode_single(&tables, format, limits()).unwrap_err();
            assert!(matches!(
                error,
                QueryEncodingError::ResultSetCount(count) if count == tables.len()
            ));
        }
    }
}

#[test]
fn json_retains_schema_duplicates_wide_integer_binary_temporal_and_null() {
    let table = table(true, false);
    let bytes = encode_single(&[&table], ResultFormat::Json, limits()).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["schema"][0]["name"], "integer");
    assert_eq!(value["schema"][1]["name"], "integer");
    assert_eq!(
        value["rows"][0],
        json!(["9007199254740993", "", "AP8=", "1234567890"])
    );
    assert_eq!(value["rows"][1], json!([null, null, null, null]));
    assert_eq!(
        value["schema"][3]["type"],
        DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())).to_string()
    );
}

#[test]
fn csv_retains_positional_duplicates_exact_text_and_null_empty_distinction() {
    let table = table(true, false);
    let bytes = encode_single(&[&table], ResultFormat::Csv, limits()).unwrap();
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        "\"integer\",\"integer\",\"binary\",\"instant\"\r\n\
         \"9007199254740993\",\"\",\"AP8=\",\"1234567890\"\r\n\
         ,,,\r\n"
    );
}

#[test]
fn real_parquet_readback_retains_arrow_schema_and_values() {
    let table = table(false, false);
    let bytes = encode_single(&[&table], ResultFormat::Parquet, limits()).unwrap();
    assert_eq!(&bytes[..4], b"PAR1");
    assert_eq!(&bytes[bytes.len() - 4..], b"PAR1");
    let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes)).unwrap();
    assert_eq!(builder.schema().as_ref(), table.schema().as_ref());
    let batches = builder
        .build()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0], table.batches()[0]);
    let integers = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap();
    assert_eq!(integers.value(0), 9_007_199_254_740_993);
    assert!(integers.is_null(1));
}

#[test]
fn declared_empty_schema_is_not_inferred_from_rows() {
    let table = table(false, true);
    let json = encode_single(&[&table], ResultFormat::Json, limits()).unwrap();
    let value: Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(value["schema"].as_array().unwrap().len(), 4);
    assert_eq!(value["rows"], json!([]));
    let parquet = encode_single(&[&table], ResultFormat::Parquet, limits()).unwrap();
    let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(parquet)).unwrap();
    assert_eq!(builder.schema().as_ref(), table.schema().as_ref());
    assert_eq!(builder.metadata().file_metadata().num_rows(), 0);
}

#[test]
fn unsupported_parquet_duplicate_names_remain_explicit_codec_failure() {
    let table = table(true, false);
    assert!(matches!(
        encode_single(&[&table], ResultFormat::Parquet, limits()),
        Err(QueryEncodingError::Codec(_))
    ));
}

#[test]
fn normal_presentation_retains_every_rich_dataset_without_reinference() {
    let first = table(true, false);
    let second = table(false, true);
    let bytes = encode_presentation(&[&first, &second], limits()).unwrap();
    assert_eq!(bytes.last(), Some(&b'\n'));
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["resultsets"].as_array().unwrap().len(), 2);
    assert_eq!(
        value["resultsets"][0]["rows"][0],
        json!(["9007199254740993", "", "AP8=", "1234567890"])
    );
    assert_eq!(value["resultsets"][1]["rows"], json!([]));
    assert_eq!(
        value["resultsets"][1]["schema"].as_array().unwrap().len(),
        4
    );
    assert_eq!(
        value["resultsets"][0]["schema"][0]["name"],
        value["resultsets"][0]["schema"][1]["name"]
    );
}

#[test]
fn normal_presentation_checks_exact_envelope_inclusive_byte_threshold() {
    let value = "\"".repeat(4096);
    let schema = Arc::new(Schema::new(vec![Field::new("text", DataType::Utf8, true)]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(StringArray::from(vec![value.as_str()]))],
    )
    .unwrap();
    let table = TabularResult::try_new(schema, vec![batch], limits()).unwrap();
    let expected = encode_presentation(&[&table], limits()).unwrap();
    let exact = OutputLimits::new(100, expected.len(), 8).unwrap();
    assert_eq!(encode_presentation(&[&table], exact).unwrap(), expected);
    let short = OutputLimits::new(100, expected.len() - 1, 8).unwrap();
    assert!(matches!(
        encode_presentation(&[&table], short),
        Err(QueryEncodingError::OutputLimit(max)) if max == expected.len() - 1
    ));
}

#[test]
fn normal_presentation_applies_aggregate_rows_and_batches_across_sets() {
    let table = table(false, false);
    assert!(matches!(
        encode_presentation(
            &[&table, &table],
            OutputLimits::new(3, 1024 * 1024, 8).unwrap()
        ),
        Err(QueryEncodingError::AggregateRows(3))
    ));
    assert!(matches!(
        encode_presentation(
            &[&table, &table],
            OutputLimits::new(100, 1024 * 1024, 1).unwrap()
        ),
        Err(QueryEncodingError::AggregateBatches(1))
    ));
}
