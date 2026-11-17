// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit schema-version-1 signed int64 logical declarations stay exact.
#![deny(unnameable_test_items)]

use arrow_array::{Array, Int64Array, RecordBatch};
use arrow_schema::DataType;
use fabric_sdk_aihub_query::NativeValue;
use fabric_sdk_query_tabular::ConversionError;
use fabric_sdk_tabular::{ResultFormat, encode};
use serde_json::json;

use super::{convert, json_table, outcome, output_limits, parquet_table};

#[test]
fn logical_int64_constant_uses_signed_arrow_and_preserves_original_declarations() {
    let result = convert(outcome(
        &[("constant", "Int64", "int64", "json-number")],
        vec![vec![NativeValue::Integer("1".into())]],
    ))
    .unwrap();
    let dataset = &result.datasets[0];
    let column = &dataset.columns[0];
    let table = &dataset.table;
    assert_eq!(dataset.schema_version, 1);
    assert_eq!(column.name, "constant");
    assert_eq!(column.engine_type, "Int64");
    assert_eq!(column.logical_type, "int64");
    assert_eq!(column.encoding, "json-number");
    assert_eq!(table.schema().field(0).data_type(), &DataType::Int64);
    assert_eq!(
        table.schema().field(0).metadata()["fabric.tabular.native_type"],
        "Int64"
    );
    assert_eq!(json_table(table)["rows"], json!([["1"]]));
    assert_eq!(
        encode(table, ResultFormat::Csv, output_limits()).unwrap(),
        b"\"constant\"\r\n\"1\"\r\n"
    );
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    let values = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap();
    assert_eq!(values.value(0), 1);
}

#[test]
fn logical_int64_signed_bounds_wide_integer_and_null_roundtrip_all_codecs() {
    let result = convert(outcome(
        &[("value", "Int64", "int64", "json-number")],
        vec![
            vec![NativeValue::Integer(i64::MIN.to_string())],
            vec![NativeValue::Integer(i64::MAX.to_string())],
            vec![NativeValue::Integer("9007199254740993".into())],
            vec![NativeValue::Null],
        ],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    assert_eq!(table.schema().field(0).data_type(), &DataType::Int64);
    assert_eq!(
        json_table(table)["rows"],
        json!([
            ["-9223372036854775808"],
            ["9223372036854775807"],
            ["9007199254740993"],
            [null]
        ])
    );
    assert_eq!(
        encode(table, ResultFormat::Csv, output_limits()).unwrap(),
        b"\"value\"\r\n\"-9223372036854775808\"\r\n\"9223372036854775807\"\r\n\"9007199254740993\"\r\n\r\n"
    );
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    let values = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap();
    assert_eq!(values.value(0), i64::MIN);
    assert_eq!(values.value(1), i64::MAX);
    assert_eq!(values.value(2), 9_007_199_254_740_993_i64);
    assert!(values.is_null(3));
}

#[test]
fn logical_int64_empty_and_all_null_columns_retain_the_declared_schema() {
    for rows in [vec![], vec![vec![NativeValue::Null]]] {
        let expected_rows = rows.len();
        let result = convert(outcome(&[("value", "Int64", "int64", "json-number")], rows)).unwrap();
        let table = &result.datasets[0].table;
        assert_eq!(table.num_rows(), expected_rows);
        assert_eq!(table.schema().field(0).data_type(), &DataType::Int64);
        assert_eq!(result.datasets[0].columns[0].logical_type, "int64");
        let json = json_table(table);
        assert_eq!(json["schema"][0]["type"], "Int64");
        assert_eq!(json["rows"].as_array().unwrap().len(), expected_rows);
        let csv = encode(table, ResultFormat::Csv, output_limits()).unwrap();
        assert_eq!(
            csv,
            if expected_rows == 0 {
                &b"\"value\"\r\n"[..]
            } else {
                &b"\"value\"\r\n\r\n"[..]
            }
        );
        let (schema, batches) = parquet_table(table);
        assert_eq!(&schema, table.schema());
        assert_eq!(
            batches.iter().map(RecordBatch::num_rows).sum::<usize>(),
            expected_rows
        );
        for batch in batches {
            assert_eq!(batch.column(0).null_count(), batch.num_rows());
        }
    }
}

#[test]
fn logical_int64_rejects_mismatched_native_width_sign_encoding_and_unknown_aliases() {
    for (native, logical, encoding) in [
        ("Int32", "int64", "json-number"),
        ("UInt64", "int64", "json-number"),
        ("UInt32", "int64", "json-number"),
        ("Double", "int64", "json-number"),
        ("long", "int64", "json-number"),
        ("bigint", "int64", "json-number"),
        ("vendor-int64", "int64", "json-number"),
        ("Int64", "int64", "text"),
        ("Int64", "int64", "json-boolean"),
        ("Int64", "vendor-int64", "json-number"),
        ("Int64", "uint64", "json-number"),
    ] {
        for rows in [
            vec![],
            vec![vec![NativeValue::Null]],
            vec![vec![NativeValue::Integer("1".into())]],
        ] {
            assert!(
                matches!(
                    convert(outcome(&[("value", native, logical, encoding)], rows)),
                    Err(ConversionError::UnsupportedType {
                        dataset: 0,
                        column: 0,
                        ..
                    })
                ),
                "{native}/{logical}/{encoding}"
            );
        }
    }
}

#[test]
fn logical_int64_rejects_fractional_overflow_and_noninteger_native_values() {
    for value in [
        NativeValue::Integer("1.0".into()),
        NativeValue::Integer("1e0".into()),
        NativeValue::Integer("0.5".into()),
        NativeValue::Integer("9223372036854775808".into()),
        NativeValue::Integer("-9223372036854775809".into()),
        NativeValue::Integer("18446744073709551615".into()),
        NativeValue::Integer("01".into()),
        NativeValue::Integer("+1".into()),
        NativeValue::Integer("".into()),
        NativeValue::Number("1.0".into()),
        NativeValue::Decimal("1".into()),
        NativeValue::Text("1".into()),
        NativeValue::Boolean(true),
    ] {
        assert!(matches!(
            convert(outcome(
                &[("value", "Int64", "int64", "json-number")],
                vec![vec![value]]
            )),
            Err(ConversionError::Value {
                dataset: 0,
                column: 0,
                row: 0,
                ..
            })
        ));
    }
}
