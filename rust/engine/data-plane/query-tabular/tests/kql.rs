// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Public native-result conversion and existing shared codec round trips.
#![deny(unnameable_test_items)]

mod support;

use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use arrow_array::{Array, Int32Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, SchemaRef, TimeUnit};
use bytes::Bytes;
use fabric_sdk_query::{QueryDiagnostic, QueryDiagnosticKind, QueryResult};
use fabric_sdk_query_tabular::{
    ConversionControl, ConversionError, ConversionLimit, ConversionLimits, KqlTabularResult,
    convert_kql,
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

fn convert(result: QueryResult) -> Result<KqlTabularResult, ConversionError> {
    let cancelled = AtomicBool::new(false);
    convert_kql(
        result,
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
    let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes)).unwrap();
    let schema = reader.schema().clone();
    let batches = reader
        .build()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (schema, batches)
}

#[test]
fn declared_integer_widths_and_exact_boundaries_survive_all_shared_codecs() {
    let result = convert(support::result(
        &[("int", "int"), ("long", "long")],
        vec![
            vec![json!(i32::MIN), json!(i64::MIN)],
            vec![json!(i32::MAX), json!(i64::MAX)],
            vec![Value::Null, json!(9_007_199_254_740_993_i64)],
        ],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    assert_eq!(table.schema().field(0).data_type(), &DataType::Int32);
    assert_eq!(table.schema().field(1).data_type(), &DataType::Int64);
    let json = json_table(table);
    assert_eq!(
        json["rows"][0],
        json!([i32::MIN.to_string(), i64::MIN.to_string()])
    );
    assert_eq!(
        json["rows"][1],
        json!([i32::MAX.to_string(), i64::MAX.to_string()])
    );
    assert_eq!(json["rows"][2], json!([null, "9007199254740993"]));
    let csv =
        String::from_utf8(encode(table, ResultFormat::Csv, output_limits()).unwrap()).unwrap();
    assert!(csv.contains("\"9223372036854775807\""));
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    assert_eq!(
        batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap()
            .value(0),
        i32::MIN
    );
    assert_eq!(
        batches[0]
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(1),
        i64::MAX
    );
}

#[test]
fn unicode_boolean_null_empty_and_signed_zero_survive_without_string_inference() {
    let result = convert(support::result(
        &[("text", "string"), ("flag", "bool"), ("real", "real")],
        vec![
            vec![
                json!("h\u{e9},\"\n\u{4e16}\u{754c}"),
                json!(true),
                json!(-0.0),
            ],
            vec![json!(""), json!(false), json!(1.25)],
            vec![Value::Null, Value::Null, Value::Null],
        ],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    let json = json_table(table);
    assert_eq!(json["rows"][0][0], "h\u{e9},\"\n\u{4e16}\u{754c}");
    assert!(json["rows"][0][2].as_f64().unwrap().is_sign_negative());
    assert_eq!(json["rows"][1], json!(["", false, 1.25]));
    assert_eq!(json["rows"][2], json!([null, null, null]));
    let csv =
        String::from_utf8(encode(table, ResultFormat::Csv, output_limits()).unwrap()).unwrap();
    assert!(csv.contains("\"\",\"false\",\"1.25\"\r\n"));
    assert!(csv.ends_with(",,\r\n"));
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    let strings = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(strings.value(1), "");
    assert!(!strings.is_null(1));
    assert!(strings.is_null(2));
}

#[test]
fn typed_empty_all_null_and_no_resultset_are_distinct() {
    let source = support::result(&[("i", "int"), ("s", "string"), ("t", "datetime")], vec![]);
    let empty = convert(source.clone()).unwrap();
    let table = &empty.datasets[0].table;
    assert_eq!(table.num_rows(), 0);
    assert_eq!(table.schema().fields().len(), 3);
    assert_eq!(json_table(table)["rows"], json!([]));
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 0);

    let mut source = source;
    source.datasets[0].rows = vec![vec![Value::Null; 3]];
    let nulls = convert(source.clone()).unwrap();
    assert_eq!(nulls.datasets[0].table.schema(), table.schema());
    assert_eq!(
        json_table(&nulls.datasets[0].table)["rows"],
        json!([[null, null, null]])
    );
    let (_, read) = parquet_table(&nulls.datasets[0].table);
    assert_eq!(read, nulls.datasets[0].table.batches());
    source.datasets.clear();
    let absent = convert(source).unwrap();
    assert!(absent.datasets.is_empty());
}

#[test]
fn duplicate_names_remain_positional_and_parquet_refuses_ambiguous_names() {
    let result = convert(support::result(
        &[("same", "int"), ("same", "string")],
        vec![vec![json!(7), json!("seven")]],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    let json = json_table(table);
    assert_eq!(json["schema"][0]["name"], "same");
    assert_eq!(json["schema"][1]["name"], "same");
    assert_eq!(json["rows"][0], json!(["7", "seven"]));
    assert_eq!(
        encode(table, ResultFormat::Csv, output_limits()).unwrap(),
        b"\"same\",\"same\"\r\n\"7\",\"seven\"\r\n"
    );
    assert!(matches!(
        encode(table, ResultFormat::Parquet, output_limits()),
        Err(TabularError::UnsupportedOutputSchema { .. })
    ));
}

#[test]
fn every_resultset_identity_and_unprojected_evidence_are_preserved() {
    let mut source = support::result(&[("value", "int")], vec![vec![json!(1)]]);
    let mut second = source.datasets[0].clone();
    second.ordinal = 1;
    second.table_id = 99;
    second.name = "second".into();
    second.rows = vec![vec![json!(2)]];
    source.datasets.push(second);
    let expected_evidence = serde_json::to_value(&source.evidence).unwrap();
    let expected_metadata = source.metadata.clone();
    let expected_completion = source.completion.clone();
    let result = convert(source).unwrap();
    assert_eq!(result.datasets.len(), 2);
    assert_eq!(
        (result.datasets[0].ordinal, result.datasets[0].table_id),
        (0, 17)
    );
    assert_eq!(
        (result.datasets[1].ordinal, result.datasets[1].table_id),
        (1, 99)
    );
    assert_eq!(result.datasets[1].name, "second");
    assert_eq!(
        json_table(&result.datasets[1].table)["rows"],
        json!([["2"]])
    );
    assert_eq!(
        serde_json::to_value(result.evidence).unwrap(),
        expected_evidence
    );
    assert_eq!(result.metadata, expected_metadata);
    assert_eq!(result.completion, expected_completion);
}

#[test]
fn native_temporal_precision_and_negative_epoch_survive_parquet() {
    let result = convert(support::result(
        &[("utc", "datetime"), ("elapsed", "timespan")],
        vec![
            vec![
                json!("1970-01-01T00:00:00.0000001Z"),
                json!("1.02:03:04.1234567"),
            ],
            vec![
                json!("1969-12-31T23:59:59.9999999Z"),
                json!("-00:00:00.0000001"),
            ],
        ],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    assert_eq!(
        table.schema().field(0).data_type(),
        &DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()))
    );
    assert_eq!(
        json_table(table)["rows"],
        json!([["100", "93784123456700"], ["-100", "-100"]])
    );
    let (schema, read) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(read, table.batches());
}

#[test]
fn invalid_values_fail_instead_of_coercing_or_rounding() {
    for (native, value) in [
        ("int", json!(i64::from(i32::MAX) + 1)),
        ("int", json!(-2_147_483_649_i64)),
        ("long", json!(u64::MAX)),
        ("long", json!(1.0)),
        ("long", json!("9007199254740993")),
        ("bool", json!("true")),
        ("string", json!({"not":"text"})),
        ("real", json!("NaN")),
        ("real", json!("Infinity")),
        ("real", json!(9_007_199_254_740_993_u64)),
        ("datetime", json!("2026-01-01T12:00:00+02:00")),
        ("datetime", json!("2026-01-01T12:00:00+00:00")),
        ("datetime", json!("2026-01-01T12:00:00.12345678Z")),
        ("datetime", json!("9999-12-31T23:59:59.9999999Z")),
        ("datetime", json!("2016-12-31T23:59:60Z")),
        ("timespan", json!("00:60:00")),
        ("timespan", json!("24:00:00")),
        ("timespan", json!("00:00:00.00000001")),
        (
            "timespan",
            json!("99999999999999999999999999999999999999.00:00:00"),
        ),
    ] {
        assert!(
            matches!(
                convert(support::result(
                    &[("value", native)],
                    vec![vec![value.clone()]]
                )),
                Err(ConversionError::Value {
                    dataset: 0,
                    column: 0,
                    row: 0,
                    ..
                })
            ),
            "{native}: {value}"
        );
    }
}

#[test]
fn missing_schema_details_and_unsupported_logical_types_fail_even_if_empty() {
    for native in [
        "decimal",
        "dynamic",
        "guid",
        "binary",
        "unsigned",
        "future-type",
    ] {
        for rows in [vec![], vec![vec![Value::Null]]] {
            assert!(matches!(
                convert(support::result(&[("value", native)], rows)),
                Err(ConversionError::UnsupportedType {
                    dataset: 0,
                    column: 0,
                    ..
                })
            ));
        }
    }
    for decimal in [
        "0.0000000000000000000000000001",
        "79228162514264337593543950335",
        "1e100",
    ] {
        assert!(matches!(
            convert(support::result(
                &[("decimal", "decimal")],
                vec![vec![json!(decimal)]]
            )),
            Err(ConversionError::UnsupportedType { .. })
        ));
    }
}

#[test]
fn partial_failures_warnings_cancellation_truncation_and_missing_terminal_are_refused() {
    let source = support::result(&[("value", "int")], vec![vec![json!(1)]]);
    let mut cases = vec![];
    let mut value = source.clone();
    value.evidence.completed = false;
    cases.push(value);
    let mut value = source.clone();
    value.evidence.locally_truncated = true;
    cases.push(value);
    for kind in [
        QueryDiagnosticKind::InlineError,
        QueryDiagnosticKind::CompletionError,
        QueryDiagnosticKind::CompletionWarning,
    ] {
        let mut value = source.clone();
        value.diagnostics.push(QueryDiagnostic {
            table_id: 17,
            row_index: 0,
            kind,
            record: json!({}),
        });
        cases.push(value);
    }
    for (key, field) in [
        ("HasErrors", json!(true)),
        ("Cancelled", json!(true)),
        ("OneApiErrors", json!([{"code":"E_PARTIAL"}])),
        ("OneApiErrors", Value::Null),
        ("FrameType", json!("DataTable")),
    ] {
        let mut value = source.clone();
        value.completion[key] = field;
        cases.push(value);
    }
    let mut value = source;
    value.completion = Value::Null;
    cases.push(value);
    for value in cases {
        assert!(matches!(
            convert(value),
            Err(ConversionError::UncleanResult)
        ));
    }
}

#[test]
fn invalid_identity_and_row_shapes_do_not_return_a_converted_prefix() {
    let mut source = support::result(&[("value", "int")], vec![vec![]]);
    assert!(matches!(
        convert(source.clone()),
        Err(ConversionError::RowArity { .. })
    ));
    source.datasets[0].rows = vec![vec![json!(1), json!(2)]];
    assert!(matches!(
        convert(source.clone()),
        Err(ConversionError::RowArity { .. })
    ));
    source.datasets[0].rows = vec![vec![json!(1)]];
    source.datasets[0].ordinal = 2;
    assert!(matches!(
        convert(source.clone()),
        Err(ConversionError::DatasetIdentity)
    ));
    source.datasets[0].ordinal = 0;
    let mut second = source.datasets[0].clone();
    second.ordinal = 1;
    source.datasets.push(second);
    assert!(matches!(
        convert(source.clone()),
        Err(ConversionError::DatasetIdentity)
    ));
    source.datasets[1].table_id += 1;
    source.datasets[1].rows = vec![vec![json!("invalid")]];
    assert!(matches!(
        convert(source),
        Err(ConversionError::Value { dataset: 1, .. })
    ));
}

#[test]
fn zero_column_rows_are_preserved_in_shared_json_not_invented_for_parquet() {
    let result = convert(support::result(&[], vec![vec![], vec![]])).unwrap();
    let table = &result.datasets[0].table;
    assert_eq!(table.num_rows(), 2);
    assert_eq!(json_table(table), json!({"schema":[], "rows":[[],[]]}));
    assert!(matches!(
        encode(table, ResultFormat::Parquet, output_limits()),
        Err(TabularError::UnsupportedOutputSchema { .. })
    ));
}

#[test]
fn aggregate_bounds_and_retained_metadata_depth_are_enforced() {
    let source = support::result(
        &[("a", "int"), ("b", "string")],
        vec![vec![json!(1), json!("x")]],
    );
    let cancelled = AtomicBool::new(false);
    let control = ConversionControl::new(Instant::now() + Duration::from_secs(30), &cancelled);
    for (limits, expected) in [
        (
            ConversionLimits::new(1, 0, 2, 1_000_000, 1, 16).unwrap(),
            ConversionLimit::Rows,
        ),
        (
            ConversionLimits::new(1, 1, 1, 1_000_000, 1, 16).unwrap(),
            ConversionLimit::Columns,
        ),
        (
            ConversionLimits::new(1, 1, 2, 1, 1, 16).unwrap(),
            ConversionLimit::Bytes,
        ),
    ] {
        assert!(matches!(
            convert_kql(source.clone(), limits, &control),
            Err(ConversionError::Limit(actual)) if actual == expected
        ));
    }
    let mut two = source.clone();
    let mut next = two.datasets[0].clone();
    next.ordinal = 1;
    next.table_id += 1;
    two.datasets.push(next);
    for (limits, expected) in [
        (
            ConversionLimits::new(1, 2, 2, 1_000_000, 2, 16).unwrap(),
            ConversionLimit::Datasets,
        ),
        (
            ConversionLimits::new(2, 2, 2, 1_000_000, 1, 16).unwrap(),
            ConversionLimit::Batches,
        ),
        (
            ConversionLimits::new(2, 1, 2, 1_000_000, 2, 16).unwrap(),
            ConversionLimit::Rows,
        ),
    ] {
        assert!(matches!(
            convert_kql(two.clone(), limits, &control),
            Err(ConversionError::Limit(actual)) if actual == expected
        ));
    }
    let mut nested = source;
    nested.metadata = vec![json!([[[[[[[true]]]]]]])];
    assert!(matches!(
        convert_kql(
            nested,
            ConversionLimits::new(1, 1, 2, 1_000_000, 1, 4).unwrap(),
            &control
        ),
        Err(ConversionError::Limit(ConversionLimit::ValueDepth))
    ));
}

#[test]
fn caller_cancellation_absolute_deadline_and_invalid_budgets_are_explicit() {
    let source = support::result(&[("value", "int")], vec![]);
    let cancelled = AtomicBool::new(true);
    assert!(matches!(
        convert_kql(
            source.clone(),
            limits(),
            &ConversionControl::new(Instant::now() + Duration::from_secs(30), &cancelled)
        ),
        Err(ConversionError::Cancelled)
    ));
    let cancelled = AtomicBool::new(false);
    assert!(matches!(
        convert_kql(
            source,
            limits(),
            &ConversionControl::new(Instant::now(), &cancelled)
        ),
        Err(ConversionError::Deadline)
    ));
    for args in [
        (0, 1, 1, 1, 1, 1),
        (1, 1, 0, 1, 1, 1),
        (1, 1, 1, 0, 1, 1),
        (1, 1, 1, 1, 0, 1),
        (1, 1, 1, 1, 1, 0),
        (1, 1, 1, 1, 1, 129),
    ] {
        assert!(matches!(
            ConversionLimits::new(args.0, args.1, args.2, args.3, args.4, args.5),
            Err(ConversionError::InvalidLimits)
        ));
    }
}
