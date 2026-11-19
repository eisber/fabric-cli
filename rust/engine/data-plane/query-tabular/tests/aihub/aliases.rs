// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Known native v1 aliases retain their explicit widths, values and schemas.
#![deny(unnameable_test_items)]

use arrow_array::{
    Array, BooleanArray, Decimal128Array, Float32Array, Float64Array, RecordBatch,
    TimestampNanosecondArray,
};
use arrow_schema::{DataType, TimeUnit};
use fabric_sdk_aihub_query::NativeValue;
use fabric_sdk_query_tabular::ConversionError;
use fabric_sdk_tabular::{ResultFormat, encode};
use serde_json::json;

use super::{convert, json_table, outcome, output_limits, parquet_table};

#[test]
fn kusto_real_uses_float64_with_null_signed_zero_maximum_and_subnormal_values() {
    let result = convert(outcome(
        &[("value", "real", "number", "json-number")],
        vec![
            vec![NativeValue::Number("1.25".into())],
            vec![NativeValue::Number("-0.0".into())],
            vec![NativeValue::Number("1.7976931348623157e308".into())],
            vec![NativeValue::Number("5e-324".into())],
            vec![NativeValue::Null],
        ],
    ))
    .unwrap();
    let dataset = &result.datasets[0];
    let table = &dataset.table;
    assert_eq!(dataset.columns[0].engine_type, "real");
    assert_eq!(
        table.schema().field(0).metadata()["fabric.tabular.native_type"],
        "real"
    );
    assert_eq!(table.schema().field(0).data_type(), &DataType::Float64);
    let json = json_table(table);
    assert_eq!(json["rows"][0][0], 1.25);
    assert!(json["rows"][1][0].as_f64().unwrap().is_sign_negative());
    assert_eq!(json["rows"][2][0].as_f64().unwrap(), f64::MAX);
    assert_eq!(json["rows"][3][0].as_f64().unwrap().to_bits(), 1);
    assert!(json["rows"][4][0].is_null());
    let csv =
        String::from_utf8(encode(table, ResultFormat::Csv, output_limits()).unwrap()).unwrap();
    assert!(csv.contains("\"1.25\"\r\n"));
    assert!(csv.contains("\"-0"));
    assert!(csv.ends_with("\r\n\r\n"));
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    let values = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Float64Array>()
        .unwrap();
    assert_eq!(values.value(0), 1.25);
    assert!(values.value(1).is_sign_negative());
    assert_eq!(values.value(2), f64::MAX);
    assert_eq!(values.value(3).to_bits(), 1);
    assert!(values.is_null(4));
}

#[test]
fn known_approximate_aliases_keep_single_and_double_widths() {
    let result = convert(outcome(
        &[
            ("kusto", "REAL", "number", "json-number"),
            ("single", "Single", "number", "json-number"),
            ("double", "Double", "number", "json-number"),
            ("f32", "Float32", "number", "json-number"),
            ("f64", "Float64", "number", "json-number"),
        ],
        vec![vec![NativeValue::Number("1.25".into()); 5]],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    let expected = [
        DataType::Float64,
        DataType::Float32,
        DataType::Float64,
        DataType::Float32,
        DataType::Float64,
    ];
    for (field, expected) in table.schema().fields().iter().zip(expected) {
        assert_eq!(field.data_type(), &expected);
    }
    assert_eq!(
        json_table(table)["rows"][0],
        json!([1.25, 1.25, 1.25, 1.25, 1.25])
    );
    assert!(
        encode(table, ResultFormat::Csv, output_limits())
            .unwrap()
            .ends_with(b"\"1.25\",\"1.25\",\"1.25\",\"1.25\",\"1.25\"\r\n")
    );
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    assert_eq!(
        batches[0]
            .column(1)
            .as_any()
            .downcast_ref::<Float32Array>()
            .unwrap()
            .value(0),
        1.25
    );
    assert_eq!(
        batches[0]
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap()
            .value(0),
        1.25
    );
}

#[test]
fn real_overflow_underflow_nonfinite_and_non_number_variants_fail_explicitly() {
    for value in [
        NativeValue::Number("1.7976931348623159e308".into()),
        NativeValue::Number("-1.7976931348623159e308".into()),
        NativeValue::Number("1e-9999".into()),
        NativeValue::Number("-1e-9999".into()),
        NativeValue::Number("NaN".into()),
        NativeValue::Number("Infinity".into()),
        NativeValue::Text("1.25".into()),
        NativeValue::Integer("1".into()),
    ] {
        assert!(matches!(
            convert(outcome(
                &[("value", "real", "number", "json-number")],
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

#[test]
fn ambiguous_numeric_and_unknown_monetary_aliases_do_not_acquire_defaults() {
    for (native, logical, encoding) in [
        ("float", "number", "json-number"),
        ("vendor-number", "number", "json-number"),
        ("currency", "decimal", "json-number"),
        ("vendor-money", "decimal", "json-number"),
        ("real", "integer", "json-number"),
        ("bit", "integer", "json-number"),
        ("money", "number", "json-number"),
    ] {
        for rows in [vec![], vec![vec![NativeValue::Null]]] {
            assert!(matches!(
                convert(outcome(&[("value", native, logical, encoding)], rows)),
                Err(ConversionError::UnsupportedType { .. })
            ));
        }
    }
}

#[test]
fn bit_alias_remains_a_nullable_boolean_without_integer_or_text_coercion() {
    let result = convert(outcome(
        &[("flag", "bit", "boolean", "json-boolean")],
        vec![
            vec![NativeValue::Boolean(true)],
            vec![NativeValue::Boolean(false)],
            vec![NativeValue::Null],
        ],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    assert_eq!(table.schema().field(0).data_type(), &DataType::Boolean);
    assert_eq!(
        table.schema().field(0).metadata()["fabric.tabular.native_type"],
        "bit"
    );
    assert_eq!(json_table(table)["rows"], json!([[true], [false], [null]]));
    assert_eq!(
        encode(table, ResultFormat::Csv, output_limits()).unwrap(),
        b"\"flag\"\r\n\"true\"\r\n\"false\"\r\n\r\n"
    );
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    let values = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .unwrap();
    assert!(values.value(0));
    assert!(!values.value(1));
    assert!(values.is_null(2));
    for value in [
        NativeValue::Integer("1".into()),
        NativeValue::Text("true".into()),
    ] {
        assert!(matches!(
            convert(outcome(
                &[("flag", "bit", "boolean", "json-boolean")],
                vec![vec![value]]
            )),
            Err(ConversionError::Value { .. })
        ));
    }
}

#[test]
fn datetime_aliases_preserve_all_value_naive_utc_and_uniform_fixed_offsets() {
    for native in ["datetime2", "smalldatetime"] {
        for (timezone, values) in [
            (None, ["1970-01-01T00:00:00", "1970-01-01T00:00:00.0000001"]),
            (
                Some("UTC"),
                ["1970-01-01T00:00:00Z", "1970-01-01T00:00:00.0000001+00:00"],
            ),
            (
                Some("+02:30"),
                [
                    "1970-01-01T02:30:00+02:30",
                    "1970-01-01T02:30:00.0000001+02:30",
                ],
            ),
        ] {
            let result = convert(outcome(
                &[("when", native, "datetime", "iso8601")],
                vec![
                    vec![NativeValue::DateTime(values[0].into())],
                    vec![NativeValue::DateTime(values[1].into())],
                    vec![NativeValue::Null],
                ],
            ))
            .unwrap();
            let table = &result.datasets[0].table;
            assert_eq!(result.datasets[0].columns[0].engine_type, native);
            assert_eq!(
                table.schema().field(0).metadata()["fabric.tabular.native_type"],
                native
            );
            assert_eq!(
                table.schema().field(0).data_type(),
                &DataType::Timestamp(TimeUnit::Nanosecond, timezone.map(Into::into))
            );
            assert_eq!(json_table(table)["rows"], json!([["0"], ["100"], [null]]));
            assert_eq!(
                encode(table, ResultFormat::Csv, output_limits()).unwrap(),
                b"\"when\"\r\n\"0\"\r\n\"100\"\r\n\r\n"
            );
            let (schema, batches) = parquet_table(table);
            assert_eq!(&schema, table.schema());
            assert_eq!(batches, table.batches());
            let ticks = batches[0]
                .column(0)
                .as_any()
                .downcast_ref::<TimestampNanosecondArray>()
                .unwrap();
            assert_eq!(ticks.value(0), 0);
            assert_eq!(ticks.value(1), 100);
            assert!(ticks.is_null(2));
        }
    }
}

#[test]
fn datetime_aliases_reject_mixed_offsets_oversized_precision_and_range() {
    for native in ["datetime2", "smalldatetime"] {
        for values in [
            vec!["1970-01-01T00:00:00", "1970-01-01T00:00:00Z"],
            vec!["1970-01-01T01:00:00+01:00", "1970-01-01T02:00:00+02:00"],
            vec!["1970-01-01T00:00:00.1234567891Z"],
            vec!["9999-12-31T23:59:59Z"],
            vec!["2016-12-31T23:59:60Z"],
        ] {
            let rows = values
                .iter()
                .map(|value| vec![NativeValue::DateTime((*value).into())])
                .collect();
            assert!(matches!(
                convert(outcome(&[("when", native, "datetime", "iso8601")], rows)),
                Err(ConversionError::Value { .. })
            ));
        }
    }
}

#[test]
fn sql_monetary_aliases_preserve_exact_minimum_maximum_scale_and_scaled_integer_bounds() {
    let result = convert(outcome(
        &[
            ("large", "money", "decimal", "json-number"),
            ("small", "smallmoney", "decimal", "json-number"),
        ],
        vec![
            vec![
                NativeValue::Decimal("-922337203685477.5808".into()),
                NativeValue::Decimal("-214748.3648".into()),
            ],
            vec![
                NativeValue::Decimal("922337203685477.5807".into()),
                NativeValue::Decimal("214748.3647".into()),
            ],
            vec![
                NativeValue::Decimal("1e-4".into()),
                NativeValue::Decimal("1.230000".into()),
            ],
            vec![NativeValue::Null; 2],
        ],
    ))
    .unwrap();
    let table = &result.datasets[0].table;
    assert_eq!(
        table.schema().field(0).data_type(),
        &DataType::Decimal128(19, 4)
    );
    assert_eq!(
        table.schema().field(1).data_type(),
        &DataType::Decimal128(10, 4)
    );
    assert_eq!(
        table.schema().field(0).metadata()["fabric.tabular.native_type"],
        "money"
    );
    assert_eq!(
        table.schema().field(1).metadata()["fabric.tabular.native_type"],
        "smallmoney"
    );
    assert_eq!(result.datasets[0].columns[0].engine_type, "money");
    assert_eq!(
        json_table(table)["rows"],
        json!([
            ["-922337203685477.5808", "-214748.3648"],
            ["922337203685477.5807", "214748.3647"],
            ["0.0001", "1.2300"],
            [null, null]
        ])
    );
    let csv =
        String::from_utf8(encode(table, ResultFormat::Csv, output_limits()).unwrap()).unwrap();
    assert!(csv.contains("\"-922337203685477.5808\",\"-214748.3648\"\r\n"));
    assert!(csv.ends_with("\"0.0001\",\"1.2300\"\r\n,\r\n"));
    let (schema, batches) = parquet_table(table);
    assert_eq!(&schema, table.schema());
    assert_eq!(batches, table.batches());
    let large = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Decimal128Array>()
        .unwrap();
    let small = batches[0]
        .column(1)
        .as_any()
        .downcast_ref::<Decimal128Array>()
        .unwrap();
    assert_eq!(large.value(0), i128::from(i64::MIN));
    assert_eq!(large.value(1), i128::from(i64::MAX));
    assert_eq!(small.value(0), i128::from(i32::MIN));
    assert_eq!(small.value(1), i128::from(i32::MAX));
    assert_eq!(large.value(2), 1);
    assert_eq!(small.value(2), 12_300);
    assert!(large.is_null(3));
    assert!(small.is_null(3));
}

#[test]
fn monetary_aliases_reject_out_of_range_coefficients_and_nonzero_extra_scale() {
    for (native, values) in [
        (
            "money",
            [
                "922337203685477.5808",
                "-922337203685477.5809",
                "999999999999999.9999",
                "0.00001",
                "1e100",
                "1e-100",
            ],
        ),
        (
            "smallmoney",
            [
                "214748.3648",
                "-214748.3649",
                "999999.9999",
                "0.00001",
                "1e100",
                "1e-100",
            ],
        ),
    ] {
        for value in values {
            assert!(
                matches!(
                    convert(outcome(
                        &[("amount", native, "decimal", "json-number")],
                        vec![vec![NativeValue::Decimal(value.into())]],
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
        assert!(matches!(
            convert(outcome(
                &[("amount", native, "decimal", "json-number")],
                vec![vec![NativeValue::Number("1.25".into())]],
            )),
            Err(ConversionError::Value { .. })
        ));
    }
}

#[test]
fn alias_schemas_are_preserved_for_empty_and_all_null_columns() {
    let columns = [
        ("r", "real", "number", "json-number"),
        ("b", "bit", "boolean", "json-boolean"),
        ("d", "datetime2", "datetime", "iso8601"),
        ("s", "smalldatetime", "datetime", "iso8601"),
        ("m", "money", "decimal", "json-number"),
        ("sm", "smallmoney", "decimal", "json-number"),
    ];
    for rows in [vec![], vec![vec![NativeValue::Null; 6]]] {
        let result = convert(outcome(&columns, rows)).unwrap();
        let table = &result.datasets[0].table;
        let types = [
            DataType::Float64,
            DataType::Boolean,
            DataType::Timestamp(TimeUnit::Nanosecond, None),
            DataType::Timestamp(TimeUnit::Nanosecond, None),
            DataType::Decimal128(19, 4),
            DataType::Decimal128(10, 4),
        ];
        for (field, expected) in table.schema().fields().iter().zip(types) {
            assert_eq!(field.data_type(), &expected);
        }
        assert_eq!(json_table(table)["schema"].as_array().unwrap().len(), 6);
        assert!(
            encode(table, ResultFormat::Csv, output_limits())
                .unwrap()
                .starts_with(b"\"r\",\"b\",\"d\",\"s\",\"m\",\"sm\"\r\n")
        );
        let (schema, batches) = parquet_table(table);
        assert_eq!(&schema, table.schema());
        assert_eq!(
            batches.iter().map(RecordBatch::num_rows).sum::<usize>(),
            table.num_rows()
        );
        for batch in batches {
            assert!(
                batch
                    .columns()
                    .iter()
                    .all(|array| array.null_count() == batch.num_rows())
            );
        }
    }
}
