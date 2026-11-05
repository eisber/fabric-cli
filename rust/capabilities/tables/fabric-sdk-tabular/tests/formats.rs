// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use std::collections::HashMap;
use std::io::{self, Cursor, Write};
use std::sync::Arc;

use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Decimal128Array, Float32Array, Float64Array, Int8Array,
    Int16Array, Int32Array, Int64Array, LargeBinaryArray, LargeStringArray, RecordBatch,
    StringArray, TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
    TimestampSecondArray, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use bytes::Bytes;
use fabric_sdk_tabular::{
    LimitKind, MetadataClassification, MetadataLocation, OutputLimits, ResultFormat, TabularError,
    TabularResult, decode_arrow_stream, encode, encode_to_writer,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::{Value, json};

fn limits() -> OutputLimits {
    OutputLimits::new(10_000, 4 * 1024 * 1024, 100).unwrap()
}

fn dataset(columns: Vec<(&str, ArrayRef)>) -> TabularResult {
    let fields = columns
        .iter()
        .map(|(name, array)| Field::new(*name, array.data_type().clone(), true))
        .collect::<Vec<_>>();
    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(
        schema.clone(),
        columns.into_iter().map(|(_, array)| array).collect(),
    )
    .unwrap();
    TabularResult::try_new(schema, vec![batch], limits()).unwrap()
}

fn fixture() -> TabularResult {
    dataset(vec![
        (
            "signed",
            Arc::new(Int64Array::from(vec![Some(i64::MIN), Some(i64::MAX), None])),
        ),
        (
            "unsigned",
            Arc::new(UInt64Array::from(vec![Some(u64::MAX), Some(0), None])),
        ),
        (
            "text,\"\n",
            Arc::new(StringArray::from(vec![
                Some("hé,\"llo\n世界"),
                Some(""),
                None,
            ])),
        ),
        (
            "flag",
            Arc::new(BooleanArray::from(vec![Some(true), Some(false), None])),
        ),
        (
            "float",
            Arc::new(Float64Array::from(vec![Some(-0.0), Some(1.25), None])),
        ),
        (
            "decimal",
            Arc::new(
                Decimal128Array::from(vec![
                    Some(99_999_999_999_999_999_999_999_999_999_999_999_999),
                    Some(-1),
                    None,
                ])
                .with_precision_and_scale(38, 2)
                .unwrap(),
            ),
        ),
        (
            "when",
            Arc::new(
                TimestampNanosecondArray::from(vec![
                    Some(1_234_567_890_123_456_789),
                    Some(-1),
                    None,
                ])
                .with_timezone("UTC"),
            ),
        ),
        (
            "bytes",
            Arc::new(BinaryArray::from(vec![
                Some(&b"\0\xff\""[..]),
                Some(&b""[..]),
                None,
            ])),
        ),
    ])
}

fn stream(result: &TabularResult) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer = StreamWriter::try_new(&mut bytes, result.schema()).unwrap();
    for batch in result.batches() {
        writer.write(batch).unwrap();
    }
    writer.finish().unwrap();
    drop(writer);
    bytes
}

fn parsed_json(result: &TabularResult) -> Value {
    serde_json::from_slice(&encode(result, ResultFormat::Json, limits()).unwrap()).unwrap()
}

fn read_parquet(bytes: Vec<u8>) -> (SchemaRef, Vec<RecordBatch>) {
    let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes)).unwrap();
    let schema = builder.schema().clone();
    let batches = builder
        .with_batch_size(1024)
        .build()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (schema, batches)
}

#[test]
fn json_preserves_exact_values_schema_nulls_and_order() {
    let result = fixture();
    let json = parsed_json(&result);
    assert_eq!(json["schema"].as_array().unwrap().len(), 8);
    assert_eq!(json["schema"][2]["name"], "text,\"\n");
    assert_eq!(json["schema"][5]["encoding"], "decimal-string");
    assert_eq!(json["schema"][6]["encoding"], "signed-ticks-string");
    assert_eq!(json["schema"][7]["encoding"], "base64");
    assert_eq!(
        json["rows"][0],
        json!([
            i64::MIN.to_string(),
            u64::MAX.to_string(),
            "hé,\"llo\n世界",
            true,
            -0.0,
            "999999999999999999999999999999999999.99",
            "1234567890123456789",
            "AP8i"
        ])
    );
    assert_eq!(
        json["rows"][1],
        json!([
            i64::MAX.to_string(),
            "0",
            "",
            false,
            1.25,
            "-0.01",
            "-1",
            ""
        ])
    );
    assert_eq!(
        json["rows"][2],
        json!([null, null, null, null, null, null, null, null])
    );
    assert!(json["rows"][0][4].as_f64().unwrap().is_sign_negative());
}

#[test]
fn csv_is_independently_readable_and_quotes_unicode_quotes_and_newlines() {
    let result = fixture();
    let csv = encode(&result, ResultFormat::Csv, limits()).unwrap();
    let text = std::str::from_utf8(&csv).unwrap();
    assert!(text.contains("\"text,\"\"\n\""));
    assert!(text.contains("\"hé,\"\"llo\n世界\""));
    assert!(text.contains("\"\","));
    assert!(text.ends_with(",,,,,,,\r\n"));
    let string_schema = Arc::new(Schema::new(
        result
            .schema()
            .fields()
            .iter()
            .map(|field| Field::new(field.name(), DataType::Utf8, true))
            .collect::<Vec<_>>(),
    ));
    let read = arrow_csv::ReaderBuilder::new(string_schema)
        .with_header(true)
        .build(Cursor::new(csv))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(read[0].num_rows(), 3);
    let strings = read[0]
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(strings.value(0), "hé,\"llo\n世界");
    assert_eq!(
        read[0]
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0),
        i64::MIN.to_string()
    );
}

#[test]
fn csv_null_is_bare_but_empty_text_and_binary_are_quoted() {
    let result = dataset(vec![
        (
            "s",
            Arc::new(StringArray::from(vec![None, Some(""), Some("null")])),
        ),
        (
            "b",
            Arc::new(BinaryArray::from(vec![
                None,
                Some(&b""[..]),
                Some(&b"x"[..]),
            ])),
        ),
    ]);
    assert_eq!(
        encode(&result, ResultFormat::Csv, limits()).unwrap(),
        b"\"s\",\"b\"\r\n,\r\n\"\",\"\"\r\n\"null\",\"eA==\"\r\n"
    );
}

#[test]
fn real_parquet_readback_preserves_supported_types_nulls_and_order() {
    let result = fixture();
    let bytes = encode(&result, ResultFormat::Parquet, limits()).unwrap();
    let (schema, batches) = read_parquet(bytes);
    assert_eq!(schema, *result.schema());
    assert_eq!(batches, result.batches());
}

#[test]
fn all_supported_primitive_widths_and_large_arrays_roundtrip() {
    let result = dataset(vec![
        ("i8", Arc::new(Int8Array::from(vec![Some(i8::MIN), None]))),
        (
            "i16",
            Arc::new(Int16Array::from(vec![Some(i16::MIN), None])),
        ),
        (
            "i32",
            Arc::new(Int32Array::from(vec![Some(i32::MIN), None])),
        ),
        ("u8", Arc::new(UInt8Array::from(vec![Some(u8::MAX), None]))),
        (
            "u16",
            Arc::new(UInt16Array::from(vec![Some(u16::MAX), None])),
        ),
        (
            "u32",
            Arc::new(UInt32Array::from(vec![Some(u32::MAX), None])),
        ),
        ("f32", Arc::new(Float32Array::from(vec![Some(0.1), None]))),
        (
            "large-text",
            Arc::new(LargeStringArray::from(vec![Some("é"), None])),
        ),
        (
            "large-bytes",
            Arc::new(LargeBinaryArray::from(vec![Some(&b"\xff"[..]), None])),
        ),
        (
            "millis",
            Arc::new(TimestampMillisecondArray::from(vec![Some(-1), None])),
        ),
        (
            "micros",
            Arc::new(TimestampMicrosecondArray::from(vec![Some(i64::MAX), None])),
        ),
    ]);
    let json = parsed_json(&result);
    assert_eq!(json["rows"][0][6], json!(0.1));
    let (schema, batches) = read_parquet(encode(&result, ResultFormat::Parquet, limits()).unwrap());
    assert_eq!(schema, *result.schema());
    assert_eq!(batches, result.batches());
    assert!(
        !encode(&result, ResultFormat::Csv, limits())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn empty_results_keep_schema_in_json_csv_and_parquet() {
    let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, true)]));
    let result = TabularResult::try_new(schema.clone(), vec![], limits()).unwrap();
    assert_eq!(parsed_json(&result)["rows"], json!([]));
    assert_eq!(parsed_json(&result)["schema"][0]["name"], "x");
    assert_eq!(
        encode(&result, ResultFormat::Csv, limits()).unwrap(),
        b"\"x\"\r\n"
    );
    let (read_schema, batches) =
        read_parquet(encode(&result, ResultFormat::Parquet, limits()).unwrap());
    assert_eq!(read_schema, schema);
    assert!(batches.is_empty());
    assert_eq!(
        decode_arrow_stream(&stream(&result), limits())
            .unwrap()
            .schema(),
        &schema
    );
}

#[test]
fn zero_column_schema_is_json_only() {
    let result = TabularResult::try_new(Arc::new(Schema::empty()), vec![], limits()).unwrap();
    assert_eq!(parsed_json(&result), json!({"schema":[],"rows":[]}));
    for format in [ResultFormat::Csv, ResultFormat::Parquet] {
        assert!(matches!(
            encode(&result, format, limits()),
            Err(TabularError::UnsupportedOutputSchema { .. })
        ));
    }
}

#[test]
fn duplicate_names_remain_positional_and_parquet_rejects_them() {
    let result = dataset(vec![
        ("same", Arc::new(Int64Array::from(vec![1]))),
        ("same", Arc::new(Int64Array::from(vec![2]))),
    ]);
    let json = parsed_json(&result);
    assert_eq!(json["schema"][0]["name"], json["schema"][1]["name"]);
    assert_eq!(json["rows"], json!([["1", "2"]]));
    assert_eq!(
        encode(&result, ResultFormat::Csv, limits()).unwrap(),
        b"\"same\",\"same\"\r\n\"1\",\"2\"\r\n"
    );
    assert!(matches!(
        encode(&result, ResultFormat::Parquet, limits()),
        Err(TabularError::UnsupportedOutputSchema { .. })
    ));
}

#[test]
fn seconds_timestamps_and_negative_decimal_scale_are_exact_or_rejected() {
    let result = dataset(vec![
        (
            "seconds",
            Arc::new(TimestampSecondArray::from(vec![i64::MAX])),
        ),
        (
            "decimal",
            Arc::new(
                Decimal128Array::from(vec![123])
                    .with_precision_and_scale(5, -2)
                    .unwrap(),
            ),
        ),
    ]);
    assert_eq!(
        parsed_json(&result)["rows"],
        json!([[i64::MAX.to_string(), "12300"]])
    );
    assert!(matches!(
        encode(&result, ResultFormat::Parquet, limits()),
        Err(TabularError::UnsupportedOutputSchema { .. })
    ));
    let decimal_only = dataset(vec![(
        "d",
        Arc::new(
            Decimal128Array::from(vec![123])
                .with_precision_and_scale(5, -2)
                .unwrap(),
        ),
    )]);
    assert!(matches!(
        encode(&decimal_only, ResultFormat::Parquet, limits()),
        Err(TabularError::UnsupportedOutputSchema { .. })
    ));
}

#[test]
fn ipc_roundtrip_keeps_all_batches_and_row_order() {
    let one = fixture();
    let result = TabularResult::try_new(
        one.schema().clone(),
        vec![one.batches()[0].clone(), one.batches()[0].clone()],
        limits(),
    )
    .unwrap();
    let decoded = decode_arrow_stream(&stream(&result), limits()).unwrap();
    assert_eq!(decoded.num_rows(), 6);
    assert_eq!(decoded.batches(), result.batches());
    assert_eq!(parsed_json(&decoded)["rows"].as_array().unwrap().len(), 6);
    let (_, read) = read_parquet(encode(&result, ResultFormat::Parquet, limits()).unwrap());
    assert_eq!(read, result.batches());
}

#[test]
fn malformed_truncated_and_unterminated_ipc_never_succeed() {
    let bytes = stream(&fixture());
    for cut in [
        0,
        1,
        4,
        7,
        bytes.len() / 2,
        bytes.len() - 8,
        bytes.len() - 1,
    ] {
        assert!(
            matches!(
                decode_arrow_stream(&bytes[..cut], limits()),
                Err(TabularError::InvalidIpc { .. })
            ),
            "cut={cut}"
        );
    }
    for bad in [
        &b"not arrow"[..],
        &b"\xff\xff\xff\xff\xff\xff\xff\x7f"[..],
        &b"\x04\0\0\0\0\0\0\0"[..],
    ] {
        assert!(decode_arrow_stream(bad, limits()).is_err());
    }
}

#[test]
fn concatenated_datasets_and_trailing_garbage_are_not_silently_dropped() {
    let first = stream(&fixture());
    let mut combined = first.clone();
    combined.extend_from_slice(&first);
    assert!(matches!(
        decode_arrow_stream(&combined, limits()),
        Err(TabularError::MultipleDatasets)
    ));
    let mut changed_schema = first[..first.len() - 8].to_vec();
    changed_schema.extend_from_slice(&first);
    assert!(matches!(
        decode_arrow_stream(&changed_schema, limits()),
        Err(TabularError::MultipleDatasets)
    ));
    let mut garbage = first;
    garbage.push(42);
    assert!(matches!(
        decode_arrow_stream(&garbage, limits()),
        Err(TabularError::MultipleDatasets)
    ));
}

#[test]
fn schema_mismatch_and_unsupported_types_are_explicit() {
    let result = fixture();
    let different = Arc::new(Schema::new(vec![Field::new(
        "different",
        DataType::Int64,
        true,
    )]));
    assert!(matches!(
        TabularResult::try_new(different, result.batches().to_vec(), limits()),
        Err(TabularError::SchemaMismatch { .. })
    ));
    for data_type in [
        DataType::Null,
        DataType::Decimal256(76, 2),
        DataType::List(Arc::new(Field::new("item", DataType::Int64, true))),
        DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
    ] {
        let schema = Arc::new(Schema::new(vec![Field::new("x", data_type, true)]));
        assert!(matches!(
            TabularResult::try_new(schema.clone(), vec![], limits()),
            Err(TabularError::UnsupportedType { .. })
        ));
        let mut bytes = Vec::new();
        let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
        writer.finish().unwrap();
        drop(writer);
        assert!(matches!(
            decode_arrow_stream(&bytes, limits()),
            Err(TabularError::UnsupportedType { .. })
        ));
    }
}

#[test]
fn nonfinite_float_and_out_of_precision_decimal_are_rejected() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let array = Arc::new(Float64Array::from(vec![value]));
        let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Float64, true)]));
        let batch = RecordBatch::try_new(schema.clone(), vec![array]).unwrap();
        assert!(matches!(
            TabularResult::try_new(schema, vec![batch], limits()),
            Err(TabularError::UnsupportedValue { .. })
        ));
    }
    let schema = Arc::new(Schema::new(vec![Field::new(
        "d",
        DataType::Decimal128(2, 0),
        true,
    )]));
    let array = Decimal128Array::from(vec![123])
        .with_precision_and_scale(2, 0)
        .unwrap();
    let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(array)]).unwrap();
    assert!(matches!(
        TabularResult::try_new(schema, vec![batch], limits()),
        Err(TabularError::UnsupportedValue { .. })
    ));
}

#[test]
fn schema_and_field_metadata_fail_with_unknown_typed_evidence() {
    for field_level in [false, true] {
        let metadata = HashMap::from([(
            "Metadata".to_owned(),
            "{\"error\":\"synthetic\"}".to_owned(),
        )]);
        let field = Field::new("x", DataType::Int64, true);
        let schema = if field_level {
            Schema::new(vec![field.with_metadata(metadata)])
        } else {
            Schema::new_with_metadata(vec![field], metadata)
        };
        let schema = Arc::new(schema);
        let error = TabularResult::try_new(schema.clone(), vec![], limits()).unwrap_err();
        let TabularError::Metadata { evidence } = error else {
            panic!("expected metadata evidence")
        };
        assert_eq!(evidence[0].key, "Metadata");
        assert_eq!(evidence[0].classification, MetadataClassification::Unknown);
        assert_eq!(
            evidence[0].location,
            if field_level {
                MetadataLocation::Field(0)
            } else {
                MetadataLocation::Schema
            }
        );
        let mut bytes = Vec::new();
        let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
        writer.finish().unwrap();
        drop(writer);
        assert!(matches!(
            decode_arrow_stream(&bytes, limits()),
            Err(TabularError::Metadata { .. })
        ));
    }
}

#[test]
fn row_byte_and_batch_limits_are_never_clipped() {
    assert!(OutputLimits::new(1, 0, 1).is_err());
    assert!(OutputLimits::new(1, 1, 0).is_err());
    assert!(OutputLimits::new(0, 1, 1).is_ok());
    let result = fixture();
    let bytes = stream(&result);
    for (limits, kind) in [
        (
            OutputLimits::new(2, 4 * 1024 * 1024, 100).unwrap(),
            LimitKind::Rows,
        ),
        (
            OutputLimits::new(100, bytes.len() - 1, 100).unwrap(),
            LimitKind::Bytes,
        ),
    ] {
        assert!(
            matches!(decode_arrow_stream(&bytes, limits), Err(TabularError::LimitExceeded { kind: actual, .. }) if actual == kind)
        );
    }
    let two = TabularResult::try_new(
        result.schema().clone(),
        vec![result.batches()[0].clone(); 2],
        limits(),
    )
    .unwrap();
    let one_batch = OutputLimits::new(100, 4 * 1024 * 1024, 1).unwrap();
    assert!(matches!(
        decode_arrow_stream(&stream(&two), one_batch),
        Err(TabularError::LimitExceeded {
            kind: LimitKind::Batches,
            ..
        })
    ));
    assert!(matches!(
        encode(&two, ResultFormat::Json, one_batch),
        Err(TabularError::LimitExceeded {
            kind: LimitKind::Batches,
            ..
        })
    ));
    assert!(matches!(
        TabularResult::try_new(
            result.schema().clone(),
            result.batches().to_vec(),
            OutputLimits::new(100, 256, 100).unwrap()
        ),
        Err(TabularError::LimitExceeded {
            kind: LimitKind::Bytes,
            ..
        })
    ));
    let empty_batch = RecordBatch::new_empty(result.schema().clone());
    let empty_batches = TabularResult::try_new(
        result.schema().clone(),
        vec![empty_batch.clone(), empty_batch],
        limits(),
    )
    .unwrap();
    assert!(matches!(
        decode_arrow_stream(&stream(&empty_batches), one_batch),
        Err(TabularError::LimitExceeded {
            kind: LimitKind::Batches,
            ..
        })
    ));
}

#[test]
fn encoded_byte_limit_counts_escaping_and_does_not_touch_the_sink() {
    let result = dataset(vec![(
        "s",
        Arc::new(StringArray::from(vec!["\"".repeat(1024)])),
    )]);
    let limits = OutputLimits::new(1, 1500, 1).unwrap();
    for format in [ResultFormat::Json, ResultFormat::Csv, ResultFormat::Parquet] {
        let mut sink = Vec::new();
        assert!(matches!(
            encode_to_writer(&result, format, limits, &mut sink),
            Err(TabularError::LimitExceeded {
                kind: LimitKind::Bytes,
                ..
            })
        ));
        assert!(sink.is_empty());
    }
}

struct FailingWriter {
    remaining: usize,
    fail_flush: bool,
}

impl Write for FailingWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "synthetic output failure",
            ));
        }
        let written = data.len().min(self.remaining);
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            return Err(io::Error::other("synthetic flush failure"));
        }
        Ok(())
    }
}

#[test]
fn write_and_flush_failures_are_reported_for_every_format() {
    let result = fixture();
    for format in [ResultFormat::Csv, ResultFormat::Json, ResultFormat::Parquet] {
        for (remaining, fail_flush) in [(0, false), (3, false), (usize::MAX, true)] {
            let mut writer = FailingWriter {
                remaining,
                fail_flush,
            };
            assert!(matches!(
                encode_to_writer(&result, format, limits(), &mut writer),
                Err(TabularError::Output(_))
            ));
        }
    }
}
