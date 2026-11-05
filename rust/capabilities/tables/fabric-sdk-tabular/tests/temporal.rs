// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Offline temporal value fidelity and Parquet range tests.
#![deny(unnameable_test_items)]

use std::sync::Arc;

use arrow_array::{
    ArrayRef, Date32Array, Date64Array, DurationMicrosecondArray, DurationMillisecondArray,
    DurationNanosecondArray, DurationSecondArray, RecordBatch, Time32MillisecondArray,
    Time32SecondArray, Time64MicrosecondArray, Time64NanosecondArray,
};
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{Field, Schema};
use bytes::Bytes;
use fabric_sdk_tabular::{
    OutputLimits, ResultFormat, TabularError, TabularResult, decode_arrow_stream, encode,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

fn limits() -> OutputLimits {
    OutputLimits::new(100, 1024 * 1024, 10).unwrap()
}

fn result(arrays: Vec<ArrayRef>) -> TabularResult {
    let fields = arrays
        .iter()
        .enumerate()
        .map(|(index, array)| {
            Field::new(format!("column_{index}"), array.data_type().clone(), true)
        })
        .collect::<Vec<_>>();
    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(schema.clone(), arrays).unwrap();
    TabularResult::try_new(schema, vec![batch], limits()).unwrap()
}

#[test]
fn common_dataflow_dates_times_and_durations_roundtrip_all_formats() {
    let result = result(vec![
        Arc::new(Date32Array::from(vec![Some(-1), None])),
        Arc::new(Date64Array::from(vec![Some(-86_400_000), None])),
        Arc::new(Time32MillisecondArray::from(vec![Some(86_399_999), None])),
        Arc::new(Time64MicrosecondArray::from(vec![
            Some(86_399_999_999),
            None,
        ])),
        Arc::new(Time64NanosecondArray::from(vec![
            Some(86_399_999_999_999),
            None,
        ])),
        Arc::new(DurationSecondArray::from(vec![Some(-1), None])),
        Arc::new(DurationMillisecondArray::from(vec![Some(i64::MAX), None])),
        Arc::new(DurationMicrosecondArray::from(vec![Some(i64::MIN), None])),
        Arc::new(DurationNanosecondArray::from(vec![Some(1), None])),
    ]);
    let json: serde_json::Value =
        serde_json::from_slice(&encode(&result, ResultFormat::Json, limits()).unwrap()).unwrap();
    assert_eq!(json["schema"][0]["encoding"], "days-since-epoch-string");
    assert_eq!(json["rows"][0][0], "-1");
    assert_eq!(json["rows"][0][1], "-86400000");
    assert_eq!(json["rows"][0][6], i64::MAX.to_string());
    assert!(
        json["rows"][1]
            .as_array()
            .unwrap()
            .iter()
            .all(|cell| cell.is_null())
    );
    assert!(
        String::from_utf8(encode(&result, ResultFormat::Csv, limits()).unwrap())
            .unwrap()
            .contains("\"-86400000\"")
    );

    let parquet = encode(&result, ResultFormat::Parquet, limits()).unwrap();
    let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(parquet)).unwrap();
    assert_eq!(builder.schema(), result.schema());
    let batches = builder
        .build()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(batches, result.batches());

    let mut ipc = Vec::new();
    let mut writer = StreamWriter::try_new(&mut ipc, result.schema()).unwrap();
    writer.write(&result.batches()[0]).unwrap();
    writer.finish().unwrap();
    drop(writer);
    let decoded = decode_arrow_stream(&ipc, limits()).unwrap();
    assert_eq!(decoded.batches(), result.batches());
}

#[test]
fn second_unit_time_is_exact_text_but_not_silently_coerced_for_parquet() {
    let result = result(vec![Arc::new(Time32SecondArray::from(vec![
        Some(86_399),
        None,
    ]))]);
    let json: serde_json::Value =
        serde_json::from_slice(&encode(&result, ResultFormat::Json, limits()).unwrap()).unwrap();
    assert_eq!(json["rows"][0][0], "86399");
    assert!(matches!(
        encode(&result, ResultFormat::Parquet, limits()),
        Err(TabularError::UnsupportedOutputSchema { .. })
    ));
}

#[test]
fn malformed_date_and_time_values_fail_before_serialization() {
    for array in [
        Arc::new(Date64Array::from(vec![1])) as ArrayRef,
        Arc::new(Time32SecondArray::from(vec![-1])),
        Arc::new(Time32MillisecondArray::from(vec![86_400_000])),
        Arc::new(Time64MicrosecondArray::from(vec![86_400_000_000])),
        Arc::new(Time64NanosecondArray::from(vec![86_400_000_000_000])),
    ] {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "x",
            array.data_type().clone(),
            true,
        )]));
        let batch = RecordBatch::try_new(schema.clone(), vec![array]).unwrap();
        assert!(matches!(
            TabularResult::try_new(schema, vec![batch], limits()),
            Err(TabularError::UnsupportedValue { .. })
        ));
    }

    #[test]
    fn parquet_date_range_does_not_silently_truncate_date64() {
        for days in [i64::from(i32::MAX) + 1, i64::from(i32::MIN) - 1] {
            let result = result(vec![Arc::new(Date64Array::from(vec![days * 86_400_000]))]);
            assert!(encode(&result, ResultFormat::Json, limits()).is_ok());
            assert!(matches!(
                encode(&result, ResultFormat::Parquet, limits()),
                Err(TabularError::UnsupportedValue { .. })
            ));
        }
    }
}
