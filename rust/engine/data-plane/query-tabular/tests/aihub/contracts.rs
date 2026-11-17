// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Actual public Q1 native model through the conversion and shared codec boundary.
#![deny(unnameable_test_items)]

mod aliases;

use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use arrow_array::{
    Array, BinaryArray, Decimal128Array, Float64Array, RecordBatch, TimestampNanosecondArray,
    UInt64Array,
};
use arrow_schema::{DataType, SchemaRef, TimeUnit};
use bytes::Bytes;
use fabric_sdk_aihub_query::{
    Column, Completeness, EmbeddedResource, NativeValue, QueryOutcome, QueryResource,
    TypedQueryResult,
};
use fabric_sdk_query_tabular::{
    AihubTabularResult, ConversionControl, ConversionError, ConversionLimit, ConversionLimits,
    convert_aihub,
};
use fabric_sdk_tabular::{OutputLimits, ResultFormat, TabularError, TabularResult, encode};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::{Value, json};

fn limits() -> ConversionLimits {
    ConversionLimits::new(8, 100, 32, 1024 * 1024, 8, 16).unwrap()
}

fn output_limits() -> OutputLimits {
    OutputLimits::new(100, 1024 * 1024, 8).unwrap()
}

fn outcome(columns: &[(&str, &str, &str, &str)], rows: Vec<Vec<NativeValue>>) -> QueryOutcome {
    QueryOutcome::Complete(TypedQueryResult {
        resultsets: vec![QueryResource {
            uri: "opaque:typed-resource".into(),
            schema_version: 1,
            result_id: "11111111-2222-4333-8444-555555555555".into(),
            columns: columns
                .iter()
                .map(|(name, engine, logical, encoding)| Column {
                    name: (*name).into(),
                    engine_type: (*engine).into(),
                    logical_type: (*logical).into(),
                    encoding: (*encoding).into(),
                })
                .collect(),
            row_count: rows.len(),
            rows,
            truncated: false,
            max_rows: Some(100),
            total_row_count: None,
        }],
        resources: vec![EmbeddedResource {
            uri: "https://never-fetch.example.invalid/template".into(),
            mime_type: "text/plain".into(),
            text: Some("synthetic template".into()),
            blob: None,
            metadata: Some("{\"synthetic\":true}".into()),
        }],
        messages: vec!["synthetic service message, not table data".into()],
        completeness: Completeness::Complete,
        protocol_version: "2025-06-18".into(),
    })
}

fn native(outcome: QueryOutcome) -> TypedQueryResult {
    match outcome {
        QueryOutcome::Complete(result) | QueryOutcome::Incomplete(result) => result,
    }
}

fn convert(source: QueryOutcome) -> Result<AihubTabularResult, ConversionError> {
    let cancelled = AtomicBool::new(false);
    convert_aihub(
        source,
        limits(),
        &ConversionControl::new(Instant::now() + Duration::from_secs(30), &cancelled),
    )
}

fn json_table(table: &TabularResult) -> Value {
    serde_json::from_slice(&encode(table, ResultFormat::Json, output_limits()).unwrap()).unwrap()
}

fn parquet_table(table: &TabularResult) -> (SchemaRef, Vec<RecordBatch>) {
    let bytes = encode(table, ResultFormat::Parquet, output_limits()).unwrap();
    assert_eq!(&bytes[..4], b"PAR1");
    let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes)).unwrap();
    let schema = builder.schema().clone();
    let batches = builder
        .build()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (schema, batches)
}

#[test]
fn all_declared_integer_widths_and_signs_roundtrip_without_json_floats() {
    let columns = [
        ("i8", "SByte", "integer", "json-number"),
        ("u8", "Byte", "integer", "json-number"),
        ("i16", "Int16", "integer", "json-number"),
        ("u16", "UInt16", "integer", "json-number"),
        ("i32", "Int32", "integer", "json-number"),
        ("u32", "UInt32", "integer", "json-number"),
        ("i64", "Int64", "integer", "json-number"),
        ("u64", "UInt64", "integer", "json-number"),
    ];
    let values = [
        i8::MIN.to_string(),
        u8::MAX.to_string(),
        i16::MIN.to_string(),
        u16::MAX.to_string(),
        i32::MIN.to_string(),
        u32::MAX.to_string(),
        i64::MIN.to_string(),
        u64::MAX.to_string(),
    ];
    let high = [
        i8::MAX.to_string(),
        "0".into(),
        i16::MAX.to_string(),
        "0".into(),
        i32::MAX.to_string(),
        "0".into(),
        i64::MAX.to_string(),
        "0".into(),
    ];
    let result = convert(outcome(
        &columns,
        vec![
            values.iter().cloned().map(NativeValue::Integer).collect(),
            high.iter().cloned().map(NativeValue::Integer).collect(),
            vec![NativeValue::Null; 8],
        ],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    let types = [
        DataType::Int8,
        DataType::UInt8,
        DataType::Int16,
        DataType::UInt16,
        DataType::Int32,
        DataType::UInt32,
        DataType::Int64,
        DataType::UInt64,
    ];
    for (field, expected) in table.schema().fields().iter().zip(types) {
        assert_eq!(field.data_type(), &expected);
    }
    let json = json_table(table);
    assert_eq!(json["rows"][0], json!(values));
    assert_eq!(json["rows"][1], json!(high));
    assert_eq!(
        json["rows"][2],
        json!([null, null, null, null, null, null, null, null])
    );
    let csv =
        String::from_utf8(encode(table, ResultFormat::Csv, output_limits()).unwrap()).unwrap();
    assert!(csv.contains("\"18446744073709551615\""));
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    assert_eq!(
        batches[0]
            .column(7)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap()
            .value(0),
        u64::MAX
    );
}

#[test]
fn explicitly_declared_decimal_precision_scale_and_exponents_are_exact() {
    let rows = vec![
        vec![
            NativeValue::Decimal("999999999999999999999999999999999999.99".into()),
            NativeValue::Decimal("0.1000000000000000000000000001".into()),
        ],
        vec![
            NativeValue::Decimal("-1e-2".into()),
            NativeValue::Decimal("1e-28".into()),
        ],
        vec![
            NativeValue::Decimal("1.230000000000000000000000000000000000000".into()),
            NativeValue::Null,
        ],
    ];
    let result = convert(outcome(
        &[
            ("amount", "decimal(38,2)", "decimal", "json-number"),
            ("exact", "numeric(38,28)", "decimal", "json-number"),
        ],
        rows,
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    assert_eq!(
        table.schema().field(0).data_type(),
        &DataType::Decimal128(38, 2)
    );
    let json = json_table(table);
    assert_eq!(
        json["rows"][0],
        json!([
            "999999999999999999999999999999999999.99",
            "0.1000000000000000000000000001"
        ])
    );
    assert_eq!(
        json["rows"][1],
        json!(["-0.01", "0.0000000000000000000000000001"])
    );
    assert_eq!(json["rows"][2], json!(["1.23", null]));
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    assert_eq!(
        batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Decimal128Array>()
            .unwrap()
            .value(2),
        123
    );
    assert!(
        encode(table, ResultFormat::Csv, output_limits())
            .unwrap()
            .windows(4)
            .any(|s| s == b"1.23")
    );
}

#[test]
fn invalid_decimal_declarations_and_inexact_values_are_explicit_errors() {
    for engine in [
        "decimal(39,2)",
        "decimal(0,0)",
        "decimal(5,6)",
        "decimal(5,-1)",
    ] {
        assert!(matches!(
            convert(outcome(&[("d", engine, "decimal", "json-number")], vec![])),
            Err(ConversionError::UnsupportedType { .. })
        ));
    }
    for text in [
        "1.001",
        "1000",
        "999999999999999999999999999999999999999999",
        "1e100",
        "1e-100",
        "NaN",
        "01",
        "1.",
    ] {
        assert!(
            matches!(
                convert(outcome(
                    &[("d", "decimal(5,2)", "decimal", "json-number")],
                    vec![vec![NativeValue::Decimal(text.into())]]
                )),
                Err(ConversionError::Value { .. })
            ),
            "{text}"
        );
    }
}

#[test]
fn date_naive_time_datetime_and_duration_retain_precision_in_shared_parquet() {
    let result = convert(outcome(
        &[
            ("day", "DateOnly", "date", "iso8601"),
            ("when", "DateTime", "datetime", "iso8601"),
            ("clock", "TimeOnly", "time", "iso8601"),
            ("elapsed", "TimeSpan", "duration", "text"),
        ],
        vec![vec![
            NativeValue::Date("1969-12-31".into()),
            NativeValue::DateTime("1969-12-31T23:59:59.999999999".into()),
            NativeValue::Time("23:59:59.123456789".into()),
            NativeValue::Duration("-1.02:03:04.1234567".into()),
        ]],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    assert_eq!(table.schema().field(0).data_type(), &DataType::Date32);
    assert_eq!(
        table.schema().field(1).data_type(),
        &DataType::Timestamp(TimeUnit::Nanosecond, None)
    );
    assert_eq!(
        json_table(table)["rows"][0],
        json!(["-1", "-1", "86399123456789", "-93784123456700"])
    );
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
}

#[test]
fn timezone_offsets_and_temporal_overflow_never_normalize_or_round_silently() {
    let empty_offset = convert(outcome(
        &[("when", "DateTimeOffset", "datetimeoffset", "iso8601")],
        vec![],
    ))
    .unwrap();
    assert_eq!(
        empty_offset.datasets[0].table.schema().field(0).data_type(),
        &DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()))
    );
    for value in [
        NativeValue::DateTime("9999-12-31T23:59:59.9999999".into()),
        NativeValue::DateTime("2026-01-01T12:00:00.1234567891".into()),
        NativeValue::DateTime("2016-12-31T23:59:60".into()),
    ] {
        assert!(matches!(
            convert(outcome(
                &[("when", "DateTime", "datetime", "iso8601")],
                vec![vec![value]]
            )),
            Err(ConversionError::Value { .. })
        ));
    }
}

#[test]
fn binary_guid_unicode_null_empty_and_booleans_are_independently_typed() {
    let guid = "ABCDEF01-2345-6789-ABCD-EF0123456789";
    let result = convert(outcome(
        &[
            ("bytes", "Byte[]", "binary", "base64"),
            ("id", "Guid", "guid", "text"),
            ("label", "vendor-extension", "string", "text"),
            ("flag", "Boolean", "boolean", "json-boolean"),
        ],
        vec![
            vec![
                NativeValue::Binary("AP8i".into()),
                NativeValue::Guid(guid.into()),
                NativeValue::Text("h\u{e9},\"\n\u{4e16}\u{754c}".into()),
                NativeValue::Boolean(true),
            ],
            vec![
                NativeValue::Binary("".into()),
                NativeValue::Null,
                NativeValue::Text("".into()),
                NativeValue::Boolean(false),
            ],
            vec![NativeValue::Null; 4],
        ],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    let json = json_table(table);
    assert_eq!(
        json["rows"][0],
        json!(["AP8i", guid, "h\u{e9},\"\n\u{4e16}\u{754c}", true])
    );
    assert_eq!(json["rows"][1], json!(["", null, "", false]));
    assert_eq!(json["rows"][2], json!([null, null, null, null]));
    assert_eq!(json["schema"][1]["encoding"], "guid-string");
    let csv =
        String::from_utf8(encode(table, ResultFormat::Csv, output_limits()).unwrap()).unwrap();
    assert!(csv.contains("\"\",,\"\",\"false\"\r\n"));
    assert!(csv.ends_with(",,,\r\n"));
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    let bytes = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<BinaryArray>()
        .unwrap();
    assert_eq!(bytes.value(0), &[0, 255, b'"']);
    assert!(!bytes.is_null(1));
    assert!(bytes.value(1).is_empty());
    assert!(bytes.is_null(2));
}

#[test]
fn typed_empty_all_null_duplicate_names_and_no_resultset_are_not_inferred() {
    let columns = [
        ("same", "Int64", "integer", "json-number"),
        ("same", "String", "string", "text"),
    ];
    for rows in [vec![], vec![vec![NativeValue::Null; 2]]] {
        let result = convert(outcome(&columns, rows)).unwrap();
        let table = &result.datasets[0].table;
        assert_eq!(table.schema().field(0).data_type(), &DataType::Int64);
        assert_eq!(table.schema().field(1).data_type(), &DataType::Utf8);
        assert_eq!(json_table(table)["schema"][0]["name"], "same");
        assert_eq!(json_table(table)["schema"][1]["name"], "same");
        assert!(matches!(
            encode(table, ResultFormat::Parquet, output_limits()),
            Err(TabularError::UnsupportedOutputSchema { .. })
        ));
    }
    let mut source = native(outcome(&columns, vec![]));
    source.resultsets.clear();
    assert!(matches!(
        convert(QueryOutcome::Complete(source)),
        Err(ConversionError::UncleanResult)
    ));
}

#[test]
fn every_template_result_and_all_original_non_rowset_evidence_survive() {
    let mut source = native(outcome(
        &[("x", "Int32", "integer", "json-number")],
        vec![vec![NativeValue::Integer("7".into())]],
    ));
    let mut second = native(outcome(
        &[("other", "String", "string", "text")],
        vec![vec![NativeValue::Text("second".into())]],
    ))
    .resultsets
    .remove(0);
    second.result_id = "22222222-3333-4444-8555-666666666666".into();
    second.uri = "opaque:second-resource".into();
    source.resultsets.push(second);
    let result = convert(QueryOutcome::Complete(source)).unwrap();
    assert_eq!(result.datasets.len(), 2);
    assert_eq!(result.datasets[0].ordinal, 0);
    assert_eq!(result.datasets[1].ordinal, 1);
    assert_eq!(result.datasets[1].uri, "opaque:second-resource");
    assert_eq!(
        json_table(&result.datasets[1].table)["rows"],
        json!([["second"]])
    );
    assert_eq!(result.datasets[0].total_row_count, None);
    assert_eq!(result.datasets[0].max_rows, Some(100));
    assert!(!result.datasets[0].truncated);
    assert_eq!(
        result.resources[0].uri,
        "https://never-fetch.example.invalid/template"
    );
    assert_eq!(
        result.resources[0].text.as_deref(),
        Some("synthetic template")
    );
    assert_eq!(
        result.resources[0].metadata.as_deref(),
        Some("{\"synthetic\":true}")
    );
    assert_eq!(
        result.messages,
        ["synthetic service message, not table data"]
    );
    assert_eq!(result.protocol_version, "2025-06-18");
    assert_eq!(result.completeness, Completeness::Complete);
}

#[test]
fn incomplete_truncated_inconsistent_and_forged_complete_observations_are_refused() {
    let columns = [("x", "Int32", "integer", "json-number")];
    let incomplete = native(outcome(&columns, vec![]));
    assert!(matches!(
        convert(QueryOutcome::Incomplete(incomplete)),
        Err(ConversionError::UncleanResult)
    ));
    for case in 0..7 {
        let mut source = native(outcome(&columns, vec![]));
        match case {
            0 => source.completeness = Completeness::Truncated,
            1 => source.resultsets[0].truncated = true,
            2 => source.resultsets[0].row_count = 1,
            3 => source.resultsets[0].schema_version = 2,
            4 => source.resultsets[0].max_rows = Some(0),
            5 => source.resultsets[0].total_row_count = Some(10),
            _ => source.protocol_version.clear(),
        }
        assert!(matches!(
            convert(QueryOutcome::Complete(source)),
            Err(ConversionError::UncleanResult)
        ));
    }
}

#[test]
fn unknown_schemas_bad_native_variants_nonfinite_values_and_width_overflow_fail() {
    for field in [
        ("x", "unknown", "integer", "json-number"),
        ("x", "unknown", "number", "json-number"),
        ("x", "unknown", "unknown", "text"),
        ("x", "Int64", "integer", "text"),
        ("x", "unknown", "boolean", "json-boolean"),
    ] {
        assert!(matches!(
            convert(outcome(&[field], vec![])),
            Err(ConversionError::UnsupportedType { .. })
        ));
    }
    for (field, value) in [
        (
            ("x", "Int64", "integer", "json-number"),
            NativeValue::Text("1".into()),
        ),
        (
            ("x", "UInt64", "integer", "json-number"),
            NativeValue::Integer("18446744073709551616".into()),
        ),
        (
            ("x", "Int64", "integer", "json-number"),
            NativeValue::Integer("9223372036854775808".into()),
        ),
        (
            ("x", "UInt8", "integer", "json-number"),
            NativeValue::Integer("-1".into()),
        ),
        (
            ("x", "Single", "number", "json-number"),
            NativeValue::Number("1e100".into()),
        ),
        (
            ("x", "Double", "number", "json-number"),
            NativeValue::Number("1e9999".into()),
        ),
        (
            ("x", "Double", "number", "json-number"),
            NativeValue::Number("NaN".into()),
        ),
        (
            ("x", "Double", "number", "json-number"),
            NativeValue::Number("1e-9999".into()),
        ),
        (
            ("x", "Single", "number", "json-number"),
            NativeValue::Number("1e-100".into()),
        ),
        (
            ("x", "Guid", "guid", "text"),
            NativeValue::Guid("not-guid".into()),
        ),
        (
            ("x", "Byte[]", "binary", "base64"),
            NativeValue::Binary("invalid***".into()),
        ),
    ] {
        assert!(matches!(
            convert(outcome(&[field], vec![vec![value]])),
            Err(ConversionError::Value { .. })
        ));
    }
}

#[test]
fn genuine_typed_empty_and_all_null_schemas_roundtrip_actual_parquet() {
    let columns = [
        ("id", "UInt64", "integer", "json-number"),
        ("amount", "decimal(38,28)", "decimal", "json-number"),
        ("when", "DateTime", "datetime", "iso8601"),
        ("bytes", "Byte[]", "binary", "base64"),
        ("guid", "Guid", "guid", "text"),
    ];
    for rows in [vec![], vec![vec![NativeValue::Null; 5]]] {
        let result = convert(outcome(&columns, rows)).unwrap();
        let table = &result.datasets[0].table;
        let (schema, batches) = parquet_table(table);
        assert_eq!(&schema, table.schema());
        assert_eq!(schema.field(0).data_type(), &DataType::UInt64);
        assert_eq!(schema.field(1).data_type(), &DataType::Decimal128(38, 28));
        assert_eq!(
            batches.iter().map(RecordBatch::num_rows).sum::<usize>(),
            table.num_rows()
        );
        for batch in batches {
            assert!(
                batch
                    .columns()
                    .iter()
                    .all(|column| column.null_count() == batch.num_rows())
            );
        }
    }
}

#[test]
fn native_approximate_widths_and_negative_zero_remain_declared_arrow_types() {
    let result = convert(outcome(
        &[
            ("f32", "Single", "number", "json-number"),
            ("f64", "Double", "number", "json-number"),
        ],
        vec![
            vec![
                NativeValue::Number("1.25".into()),
                NativeValue::Number("-0.0".into()),
            ],
            vec![NativeValue::Number("0e999".into()), NativeValue::Null],
        ],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    assert_eq!(table.schema().field(0).data_type(), &DataType::Float32);
    assert_eq!(table.schema().field(1).data_type(), &DataType::Float64);
    let json = json_table(table);
    assert_eq!(json["rows"][0][0], 1.25);
    assert!(json["rows"][0][1].as_f64().unwrap().is_sign_negative());
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    assert!(
        batches[0]
            .column(1)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap()
            .value(0)
            .is_sign_negative()
    );
}

#[test]
fn duplicate_resource_identity_aggregate_datasets_batches_and_columns_fail_explicitly() {
    let columns = [("x", "Int32", "integer", "json-number")];
    let cancelled = AtomicBool::new(false);
    let control = ConversionControl::new(Instant::now() + Duration::from_secs(30), &cancelled);
    for duplicate in [true, false] {
        let mut source = native(outcome(&columns, vec![]));
        let mut second = native(outcome(&columns, vec![])).resultsets.remove(0);
        if duplicate {
            second.uri = "opaque:second".into();
        } else {
            second.result_id = "22222222-3333-4444-8555-666666666666".into();
        }
        source.resultsets.push(second);
        assert!(matches!(
            convert(QueryOutcome::Complete(source)),
            Err(ConversionError::DatasetIdentity)
        ));
    }
    for (limits, expected) in [
        (
            ConversionLimits::new(1, 0, 1, 1_000_000, 2, 16).unwrap(),
            ConversionLimit::Datasets,
        ),
        (
            ConversionLimits::new(2, 0, 1, 1_000_000, 1, 16).unwrap(),
            ConversionLimit::Batches,
        ),
    ] {
        let mut source = native(outcome(&columns, vec![]));
        let mut second = native(outcome(&columns, vec![])).resultsets.remove(0);
        second.uri = "opaque:second".into();
        second.result_id = "22222222-3333-4444-8555-666666666666".into();
        source.resultsets.push(second);
        assert!(matches!(
            convert_aihub(QueryOutcome::Complete(source),limits,&control),
            Err(ConversionError::Limit(actual)) if actual == expected
        ));
    }
    assert!(matches!(
        convert_aihub(
            outcome(&[columns[0], columns[0]], vec![]),
            ConversionLimits::new(1, 0, 1, 1_000_000, 1, 16).unwrap(),
            &control
        ),
        Err(ConversionError::Limit(ConversionLimit::Columns))
    ));
}

#[test]
fn bounds_cancellation_deadline_and_arity_are_enforced_without_partial_output() {
    let columns = [("x", "Int32", "integer", "json-number")];
    let cancelled = AtomicBool::new(false);
    let control = ConversionControl::new(Instant::now() + Duration::from_secs(30), &cancelled);
    let source = outcome(&columns, vec![vec![NativeValue::Integer("1".into())]]);
    assert!(matches!(
        convert_aihub(
            source,
            ConversionLimits::new(1, 0, 1, 1_000_000, 1, 16).unwrap(),
            &control
        ),
        Err(ConversionError::Limit(ConversionLimit::Rows))
    ));
    assert!(matches!(
        convert_aihub(
            outcome(&columns, vec![]),
            ConversionLimits::new(1, 0, 1, 1, 1, 16).unwrap(),
            &control
        ),
        Err(ConversionError::Limit(ConversionLimit::Bytes))
    ));
    assert!(matches!(
        convert(outcome(&columns, vec![vec![]])),
        Err(ConversionError::RowArity { .. })
    ));
    let cancelled = AtomicBool::new(true);
    assert!(matches!(
        convert_aihub(
            outcome(&columns, vec![]),
            limits(),
            &ConversionControl::new(Instant::now() + Duration::from_secs(30), &cancelled)
        ),
        Err(ConversionError::Cancelled)
    ));
    let cancelled = AtomicBool::new(false);
    assert!(matches!(
        convert_aihub(
            outcome(&columns, vec![]),
            limits(),
            &ConversionControl::new(Instant::now(), &cancelled)
        ),
        Err(ConversionError::Deadline)
    ));
}

#[test]
fn common_plain_decimal_and_utc_datetime_use_exact_output_types_in_every_codec() {
    let exact = "0.1000000000000000000000000001";
    let result = convert(outcome(
        &[
            ("amount", "decimal", "decimal", "json-number"),
            ("when", "DateTime", "datetime", "iso8601"),
        ],
        vec![
            vec![
                NativeValue::Decimal(exact.into()),
                NativeValue::DateTime("1970-01-01T00:00:00.0000001Z".into()),
            ],
            vec![
                NativeValue::Decimal("1e-28".into()),
                NativeValue::DateTime("1969-12-31T23:59:59.9999999Z".into()),
            ],
            vec![NativeValue::Null; 2],
        ],
    ))
    .unwrap();
    let dataset = &result.datasets[0];
    let table = &dataset.table;
    assert_eq!(dataset.columns[0].engine_type, "decimal");
    assert_eq!(
        table.schema().field(0).metadata()["fabric.tabular.native_type"],
        "decimal"
    );
    assert_eq!(
        table.schema().field(0).data_type(),
        &DataType::Decimal128(28, 28)
    );
    assert_eq!(
        table.schema().field(1).data_type(),
        &DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()))
    );
    let json = json_table(table);
    assert_eq!(json["rows"][0], json!([exact, "100"]));
    assert_eq!(
        json["rows"][1],
        json!(["0.0000000000000000000000000001", "-100"])
    );
    assert_eq!(json["rows"][2], json!([null, null]));
    let csv =
        String::from_utf8(encode(table, ResultFormat::Csv, output_limits()).unwrap()).unwrap();
    assert!(csv.contains(&format!("\"{exact}\",\"100\"\r\n")));
    assert!(csv.ends_with(",\r\n"));
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    let decimals = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Decimal128Array>()
        .unwrap();
    assert_eq!(decimals.value(0), 1_000_000_000_000_000_000_000_000_001);
    assert_eq!(decimals.value(1), 1);
    let timestamps = batches[0]
        .column(1)
        .as_any()
        .downcast_ref::<TimestampNanosecondArray>()
        .unwrap();
    assert_eq!(timestamps.value(0), 100);
    assert_eq!(timestamps.value(1), -100);
}

#[test]
fn plain_decimal_representation_scans_all_scales_exponents_and_leading_zero_coefficients() {
    for engine in ["Decimal", "numeric"] {
        let lexemes = ["1.20", "0.00300", "-4e1", "1.2300e+2", "0e0"];
        for reverse in [false, true] {
            let mut rows = lexemes
                .iter()
                .map(|value| vec![NativeValue::Decimal((*value).into())])
                .collect::<Vec<_>>();
            if reverse {
                rows.reverse();
            }
            let result =
                convert(outcome(&[("d", engine, "decimal", "json-number")], rows)).unwrap();
            let table = &result.datasets[0].table;
            assert_eq!(
                table.schema().field(0).data_type(),
                &DataType::Decimal128(8, 5)
            );
            let mut expected = vec![
                json!(["1.20000"]),
                json!(["0.00300"]),
                json!(["-40.00000"]),
                json!(["123.00000"]),
                json!(["0.00000"]),
            ];
            if reverse {
                expected.reverse();
            }
            assert_eq!(json_table(table)["rows"], json!(expected));
            let (schema, batches) = parquet_table(table);
            assert_eq!(&schema, table.schema());
            assert_eq!(batches, table.batches());
        }
    }
}

#[test]
fn zero_decimal_values_and_extreme_checked_exponents_never_allocate_exponent_sized_buffers() {
    let result = convert(outcome(
        &[("d", "decimal", "decimal", "json-number")],
        vec![
            vec![NativeValue::Decimal("0e9223372036854775807".into())],
            vec![NativeValue::Decimal("-0.000".into())],
            vec![NativeValue::Decimal("0.00000e2".into())],
        ],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    assert_eq!(
        table.schema().field(0).data_type(),
        &DataType::Decimal128(3, 3)
    );
    assert_eq!(
        json_table(table)["rows"],
        json!([["0.000"], ["0.000"], ["0.000"]])
    );
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
}

#[test]
fn plain_decimal_38_digit_boundary_is_exact_and_wider_or_incompatible_scales_fail() {
    let maximum = "99999999999999999999999999999999999999";
    let result = convert(outcome(
        &[("d", "decimal", "decimal", "json-number")],
        vec![
            vec![NativeValue::Decimal(maximum.into())],
            vec![NativeValue::Decimal("1e37".into())],
        ],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    assert_eq!(
        table.schema().field(0).data_type(),
        &DataType::Decimal128(38, 0)
    );
    assert_eq!(json_table(table)["rows"][0][0], maximum);
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    for values in [
        vec!["100000000000000000000000000000000000000"],
        vec!["1e38"],
        vec!["1e-39"],
        vec!["1e9223372036854775808"],
        vec!["1e-9223372036854775808"],
        vec!["0e-100"],
        vec!["1e+-1"],
        vec![
            "0.1000000000000000000000000001",
            "79228162514264337593543950335",
        ],
        vec!["1", "0.00000000000000000000000000000000000001"],
    ] {
        let rows = values
            .iter()
            .map(|value| vec![NativeValue::Decimal((*value).into())])
            .collect();
        assert!(
            matches!(
                convert(outcome(&[("d", "decimal", "decimal", "json-number")], rows)),
                Err(ConversionError::Value { .. })
            ),
            "{values:?}"
        );
    }
}

#[test]
fn empty_and_all_null_plain_decimals_use_canonical_output_without_native_schema_claims() {
    for rows in [vec![], vec![vec![NativeValue::Null]]] {
        let result = convert(outcome(&[("d", "decimal", "decimal", "json-number")], rows)).unwrap();
        let dataset = &result.datasets[0];
        let table = &dataset.table;
        assert_eq!(
            table.schema().field(0).data_type(),
            &DataType::Decimal128(1, 0)
        );
        assert_eq!(dataset.columns[0].engine_type, "decimal");
        assert_eq!(
            table.schema().field(0).metadata()["fabric.tabular.native_type"],
            "decimal"
        );
        let (schema, batches) = parquet_table(table);
        assert_eq!(&schema, table.schema());
        assert_eq!(
            batches.iter().map(RecordBatch::num_rows).sum::<usize>(),
            table.num_rows()
        );
    }
}

#[test]
fn uniform_datetime_offsets_are_proven_across_all_rows_and_retained_in_parquet_schema() {
    for (engine, logical, wrap) in [
        (
            "DateTime",
            "datetime",
            NativeValue::DateTime as fn(String) -> NativeValue,
        ),
        (
            "DateTimeOffset",
            "datetimeoffset",
            NativeValue::DateTimeOffset as fn(String) -> NativeValue,
        ),
    ] {
        for (timezone, values) in [
            ("UTC", ["1970-01-01T00:00:00Z", "1970-01-01T00:00:00+00:00"]),
            (
                "+02:30",
                [
                    "1970-01-01T02:30:00+02:30",
                    "1970-01-01T02:30:00.000000001+02:30",
                ],
            ),
            (
                "-05:45",
                [
                    "1969-12-31T18:15:00-05:45",
                    "1969-12-31T18:15:00.000000001-05:45",
                ],
            ),
        ] {
            let mut rows = vec![vec![NativeValue::Null]];
            rows.extend(values.iter().map(|value| vec![wrap((*value).into())]));
            let result = convert(outcome(&[("when", engine, logical, "iso8601")], rows)).unwrap();
            let table = &result.datasets[0].table;
            assert_eq!(
                table.schema().field(0).data_type(),
                &DataType::Timestamp(TimeUnit::Nanosecond, Some(timezone.into()))
            );
            let expected_second = if timezone == "UTC" { "0" } else { "1" };
            assert_eq!(
                json_table(table)["rows"],
                json!([[null], ["0"], [expected_second]])
            );
            let csv = String::from_utf8(encode(table, ResultFormat::Csv, output_limits()).unwrap())
                .unwrap();
            assert!(csv.ends_with(&format!("\"{expected_second}\"\r\n")));
            let (schema, batches) = parquet_table(table);
            assert_eq!(&schema, table.schema());
            assert_eq!(batches, table.batches());
            let values = batches[0]
                .column(0)
                .as_any()
                .downcast_ref::<TimestampNanosecondArray>()
                .unwrap();
            assert!(values.is_null(0));
            assert_eq!(values.value(1), 0);
            assert_eq!(values.value(2), if timezone == "UTC" { 0 } else { 1 });
        }
    }
}

#[test]
fn mixed_naive_aware_and_differing_offsets_fail_regardless_of_first_nonnull_row() {
    for values in [
        ["1970-01-01T00:00:00", "1970-01-01T00:00:00Z"],
        ["1970-01-01T00:00:00Z", "1970-01-01T00:00:00"],
        ["1970-01-01T01:00:00+01:00", "1970-01-01T02:00:00+02:00"],
        ["1970-01-01T00:00:00Z", "1970-01-01T01:00:00+01:00"],
    ] {
        let rows = values
            .iter()
            .map(|value| vec![NativeValue::DateTime((*value).into())])
            .collect();
        assert!(matches!(
            convert(outcome(
                &[("when", "DateTime", "datetime", "iso8601")],
                rows
            )),
            Err(ConversionError::Value { row: 1, .. })
        ));
    }
    for values in [
        ["1970-01-01T01:00:00+01:00", "1970-01-01T02:00:00+02:00"],
        ["1970-01-01T00:00:00Z", "1970-01-01T00:00:00"],
    ] {
        let rows = values
            .iter()
            .map(|value| vec![NativeValue::DateTimeOffset((*value).into())])
            .collect();
        assert!(matches!(
            convert(outcome(
                &[("when", "DateTimeOffset", "datetimeoffset", "iso8601")],
                rows
            )),
            Err(ConversionError::Value { row: 1, .. })
        ));
    }
}

#[test]
fn null_datetime_columns_use_documented_canonical_output_timezones() {
    for rows in [vec![], vec![vec![NativeValue::Null; 2]]] {
        let result = convert(outcome(
            &[
                ("local", "DateTime", "datetime", "iso8601"),
                ("aware", "DateTimeOffset", "datetimeoffset", "iso8601"),
            ],
            rows,
        ))
        .unwrap();
        let table = &result.datasets[0].table;
        assert_eq!(
            table.schema().field(0).data_type(),
            &DataType::Timestamp(TimeUnit::Nanosecond, None)
        );
        assert_eq!(
            table.schema().field(1).data_type(),
            &DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()))
        );
        assert_eq!(result.datasets[0].columns[1].engine_type, "DateTimeOffset");
        let (schema, batches) = parquet_table(table);
        assert_eq!(&schema, table.schema());
        assert_eq!(
            batches.iter().map(RecordBatch::num_rows).sum::<usize>(),
            table.num_rows()
        );
    }
}

#[test]
fn aware_temporal_precision_range_leaps_unknown_offsets_and_variant_mismatches_fail() {
    for value in [
        "1970-01-01T00:00:00.1234567891Z",
        "1970-01-01T00:00:00.1234567891+02:00",
        "9999-12-31T23:59:59.9999999Z",
        "2016-12-31T23:59:60Z",
        "2017-01-01T01:59:60+02:00",
        "1970-01-01T00:00:00-00:00",
        "1970-01-01T00:00:00+24:00",
    ] {
        assert!(
            matches!(
                convert(outcome(
                    &[("when", "DateTimeOffset", "datetimeoffset", "iso8601")],
                    vec![vec![NativeValue::DateTimeOffset(value.into())]]
                )),
                Err(ConversionError::Value { .. })
            ),
            "{value}"
        );
    }
    for (logical, engine, value) in [
        (
            "datetime",
            "DateTime",
            NativeValue::DateTimeOffset("1970-01-01T00:00:00Z".into()),
        ),
        (
            "datetimeoffset",
            "DateTimeOffset",
            NativeValue::DateTime("1970-01-01T00:00:00Z".into()),
        ),
        (
            "datetimeoffset",
            "DateTimeOffset",
            NativeValue::DateTimeOffset("1970-01-01T00:00:00".into()),
        ),
    ] {
        assert!(matches!(
            convert(outcome(
                &[("when", engine, logical, "iso8601")],
                vec![vec![value]]
            )),
            Err(ConversionError::Value { .. })
        ));
    }
}
