// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Data-file contracts through the public shared tabular API.

#![deny(unnameable_test_items)]

use arrow_array::{BooleanArray, Float64Array, Int64Array, StringArray, UInt64Array};
use arrow_schema::DataType;
use bytes::Bytes;
use fabric_cli_output::{
    parse_data,
    tabular::{
        JSON_LOGICAL_TYPE_KEY, TabularColumn, TabularError, TabularFormat, TabularLimits,
        TabularTable, TabularType, TabularValue, encode,
    },
};
use parquet::{
    arrow::arrow_reader::ParquetRecordBatchReaderBuilder,
    basic::{LogicalType, Type as PhysicalType},
    file::{reader::FileReader, serialized_reader::SerializedFileReader},
    record::RowAccessor,
};
use serde_json::{Value, json};

fn object_rows(source: &str) -> Vec<Value> {
    parse_data(source).unwrap().as_array().unwrap().clone()
}

fn column(name: &str, data_type: TabularType, nullable: bool) -> TabularColumn {
    TabularColumn::new(name, data_type, nullable)
}

#[test]
fn formats_and_native_types_are_explicit_and_never_guessed() {
    for (name, format) in [
        ("csv", TabularFormat::Csv),
        ("json", TabularFormat::Json),
        ("parquet", TabularFormat::Parquet),
    ] {
        assert_eq!(TabularFormat::parse(name).unwrap(), format);
        assert_eq!(name.parse::<TabularFormat>().unwrap(), format);
        assert_eq!(format.name(), name);
    }
    for name in ["CSV", ".json", "text", ""] {
        assert!(matches!(
            TabularFormat::parse(name),
            Err(TabularError::UnsupportedFormat(_))
        ));
    }
    for name in [
        "null", "boolean", "int64", "uint64", "float64", "string", "json",
    ] {
        assert_eq!(TabularType::parse(name).unwrap().name(), name);
    }
    for name in ["binary", "decimal128", "date", "timestamp", "float32"] {
        assert_eq!(
            TabularType::parse(name),
            Err(TabularError::UnsupportedType(name.to_owned()))
        );
    }
}

#[test]
fn later_keys_are_retained_in_first_seen_column_order_without_flattening() {
    let rows =
        object_rows(r#"[{"z":1,"a":"001"},{"later":true,"a":"1","z":2},{"a":"","last":null}]"#);
    let table = TabularTable::from_rows(&rows).unwrap();
    assert_eq!(table.row_count(), 3);
    assert_eq!(
        table.columns().iter().map(|c| c.name()).collect::<Vec<_>>(),
        ["z", "a", "later", "last"]
    );
    assert_eq!(
        table
            .columns()
            .iter()
            .map(|c| c.nullable())
            .collect::<Vec<_>>(),
        [true, false, true, true]
    );
    assert_eq!(table.columns()[3].data_type(), TabularType::Null);
    assert_eq!(
        serde_json::to_value(table.columns()).unwrap(),
        json!([
            {"name":"z","type":"int64","nullable":true},
            {"name":"a","type":"string","nullable":false},
            {"name":"later","type":"boolean","nullable":true},
            {"name":"last","type":"null","nullable":true}
        ])
    );
    assert_eq!(
        encode(&table, TabularFormat::Json).unwrap(),
        br#"[{"z":1,"a":"001","later":null,"last":null},{"z":2,"a":"1","later":true,"last":null},{"z":null,"a":"","later":null,"last":null}]"#
    );
    assert_eq!(
        encode(&table, TabularFormat::Csv).unwrap(),
        b"z,a,later,last\r\n1,001,,\r\n2,1,true,\r\n,,,\r\n"
    );
}

#[test]
fn csv_has_rfc4180_quotes_crlf_unicode_and_no_formula_mutation() {
    let table = TabularTable::from_rows(&[json!({
        "text": "comma, \"quote\"\rCR\nLF\r\npair 雪😀",
        "null": null,
        "empty": "",
        "formula": "=1+1",
    })])
    .unwrap();
    let bytes = encode(&table, TabularFormat::Csv).unwrap();
    assert_eq!(
        bytes,
        "text,null,empty,formula\r\n\"comma, \"\"quote\"\"\rCR\nLF\r\npair 雪😀\",,,=1+1\r\n"
            .as_bytes()
    );
    let mut reader = csv::Reader::from_reader(bytes.as_slice());
    assert_eq!(
        reader.headers().unwrap().iter().collect::<Vec<_>>(),
        ["text", "null", "empty", "formula"]
    );
    let row = reader.records().next().unwrap().unwrap();
    assert_eq!(
        row.iter().collect::<Vec<_>>(),
        ["comma, \"quote\"\rCR\nLF\r\npair 雪😀", "", "", "=1+1"]
    );

    let header = TabularTable::new(
        vec![
            column("comma,name", TabularType::String, false),
            column("quote\"name", TabularType::String, false),
            column("line\nname", TabularType::String, false),
        ],
        vec![],
    )
    .unwrap();
    assert_eq!(
        encode(&header, TabularFormat::Csv).unwrap(),
        b"\"comma,name\",\"quote\"\"name\",\"line\nname\"\r\n"
    );
}

#[test]
fn csv_null_and_empty_string_have_the_declared_ambiguity() {
    let table = TabularTable::from_rows(&[json!({"v":null}), json!({"v":""})]).unwrap();
    assert_eq!(
        encode(&table, TabularFormat::Csv).unwrap(),
        b"v\r\n\"\"\r\n\"\"\r\n"
    );
    assert_eq!(
        encode(&table, TabularFormat::Json).unwrap(),
        br#"[{"v":null},{"v":""}]"#
    );
}

#[test]
fn structured_cells_retain_real_json_and_use_compact_csv_cells() {
    let rows = object_rows(
        r#"[{"metadata":{"z":[1,null,{"a":"雪"}],"a":true}},{"metadata":[{"b":2},false]}]"#,
    );
    let table = TabularTable::from_rows(&rows).unwrap();
    assert_eq!(table.columns()[0].data_type(), TabularType::Json);
    assert_eq!(
        encode(&table, TabularFormat::Json).unwrap(),
        "[{\"metadata\":{\"z\":[1,null,{\"a\":\"雪\"}],\"a\":true}},{\"metadata\":[{\"b\":2},false]}]"
            .as_bytes()
    );
    let bytes = encode(&table, TabularFormat::Csv).unwrap();
    let records = csv::Reader::from_reader(bytes.as_slice())
        .records()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(&records[0][0], r#"{"z":[1,null,{"a":"雪"}],"a":true}"#);
    assert_eq!(&records[1][0], r#"[{"b":2},false]"#);
}

#[test]
fn large_integers_and_lossless_unsigned_widening_do_not_round() {
    for source in [
        r#"[{"n":1},{"n":18446744073709551615},{"n":9007199254740993}]"#,
        r#"[{"n":18446744073709551615},{"n":1},{"n":9007199254740993}]"#,
    ] {
        let rows = object_rows(source);
        let table = TabularTable::from_rows(&rows).unwrap();
        assert_eq!(table.columns()[0].data_type(), TabularType::UInt64);
        assert_eq!(
            serde_json::from_slice::<Value>(&encode(&table, TabularFormat::Json).unwrap()).unwrap(),
            Value::Array(rows)
        );
    }
    let table = TabularTable::from_rows(&object_rows(
        r#"[{"n":-9223372036854775808},{"n":9223372036854775807}]"#,
    ))
    .unwrap();
    assert_eq!(
        encode(&table, TabularFormat::Json).unwrap(),
        br#"[{"n":-9223372036854775808},{"n":9223372036854775807}]"#
    );
    assert_eq!(
        encode(&table, TabularFormat::Csv).unwrap(),
        b"n\r\n-9223372036854775808\r\n9223372036854775807\r\n"
    );
}

#[test]
fn inferred_type_conflicts_and_non_object_rows_are_errors() {
    for source in [
        r#"[{"v":1},{"v":1.0}]"#,
        r#"[{"v":1.0},{"v":1}]"#,
        r#"[{"v":-1},{"v":18446744073709551615}]"#,
        r#"[{"v":18446744073709551615},{"v":-1}]"#,
        r#"[{"v":false},{"v":"false"}]"#,
        r#"[{"v":1},{"v":"1"}]"#,
        r#"[{"v":{}},{"v":"{}"}]"#,
        r#"[{"v":[]},{"v":1}]"#,
    ] {
        assert!(
            matches!(
                TabularTable::from_rows(&object_rows(source)),
                Err(TabularError::TypeConflict { row: 1, .. })
            ),
            "{source}"
        );
    }
    for value in [json!(null), json!(1), json!(true), json!("text"), json!([])] {
        assert_eq!(
            TabularTable::from_rows(&[json!({}), value]),
            Err(TabularError::NonObjectRow { row: 1 })
        );
    }
}

#[test]
fn explicit_schema_checks_duplicates_width_types_nullability_and_finiteness() {
    let required = column("v", TabularType::Int64, false);
    assert!(matches!(
        TabularTable::new(vec![required.clone(), required.clone()], vec![]),
        Err(TabularError::DuplicateColumn(_))
    ));
    assert_eq!(
        TabularTable::new(vec![required.clone()], vec![vec![]]),
        Err(TabularError::RowWidth {
            row: 0,
            expected: 1,
            actual: 0
        })
    );
    assert!(matches!(
        TabularTable::new(vec![required.clone()], vec![vec![TabularValue::Null]]),
        Err(TabularError::NullNotAllowed { row: 0, .. })
    ));
    assert!(matches!(
        TabularTable::new(vec![required], vec![vec![TabularValue::String("1".into())]]),
        Err(TabularError::TypeConflict { row: 0, .. })
    ));
    assert!(matches!(
        TabularTable::new(
            vec![column("v", TabularType::UInt64, false)],
            vec![vec![TabularValue::Int64(1)]]
        ),
        Err(TabularError::TypeConflict { .. })
    ));
    assert!(matches!(
        TabularTable::new(vec![column("v", TabularType::Null, false)], vec![]),
        Err(TabularError::NonNullableNullType(_))
    ));
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(matches!(
            TabularTable::new(
                vec![column("v", TabularType::Float64, false)],
                vec![vec![TabularValue::Float64(value)]]
            ),
            Err(TabularError::NonFinite { row: 0, .. })
        ));
    }
}

#[test]
fn explicitly_declared_json_cells_accept_scalars_without_native_coercion() {
    let values = [
        Value::Null,
        json!("long"),
        json!(""),
        json!(1),
        json!(u64::MAX),
        json!(1.25),
        json!(true),
    ];
    let table = TabularTable::new(
        vec![column("v", TabularType::Json, false)],
        values
            .iter()
            .map(|value| vec![TabularValue::Json(value.clone())])
            .collect(),
    )
    .unwrap();
    assert!(!table.columns()[0].nullable());
    assert_eq!(
        serde_json::from_slice::<Value>(&encode(&table, TabularFormat::Json).unwrap()).unwrap(),
        Value::Array(values.into_iter().map(|value| json!({"v":value})).collect())
    );
    assert!(matches!(
        TabularTable::new(
            vec![column("v", TabularType::Json, false)],
            vec![vec![TabularValue::Null]],
        ),
        Err(TabularError::NullNotAllowed { .. })
    ));
    assert!(matches!(
        TabularTable::new(
            vec![column("v", TabularType::Json, false)],
            vec![vec![TabularValue::String("long".into())]],
        ),
        Err(TabularError::TypeConflict { .. })
    ));
}

#[test]
fn opted_json_columns_preserve_mixed_values_and_distinguish_literal_null_from_missing() {
    let rows = object_rows(
        r#"[{"name":"a","type":"long","metadata":{"x":1}},{"name":"b","type":{"type":"array","elementType":"string"},"metadata":null},{"name":"c","type":9007199254740993},{"name":"d","type":true},{"name":"e","type":1.25},{"name":"f","type":null},{"name":"g"}]"#,
    );
    assert!(matches!(
        TabularTable::from_rows(&rows),
        Err(TabularError::TypeConflict { .. })
    ));
    let table = TabularTable::from_rows_with_json_columns(&rows, &["metadata", "type"]).unwrap();
    assert_eq!(table.row_count(), 7);
    assert_eq!(
        table
            .columns()
            .iter()
            .map(|column| column.name())
            .collect::<Vec<_>>(),
        ["name", "type", "metadata"]
    );
    assert_eq!(table.columns()[1].data_type(), TabularType::Json);
    assert_eq!(table.columns()[2].data_type(), TabularType::Json);
    assert!(table.columns()[1].nullable());
    assert_eq!(table.rows()[5][1], TabularValue::Json(Value::Null));
    assert_eq!(table.rows()[6][1], TabularValue::Null);
    let output: Value =
        serde_json::from_slice(&encode(&table, TabularFormat::Json).unwrap()).unwrap();
    assert_eq!(output[0]["type"], json!("long"));
    assert_eq!(output[1]["type"], rows[1]["type"]);
    assert_eq!(output[2]["type"], json!(9_007_199_254_740_993_u64));
    assert_eq!(output[3]["type"], json!(true));
    assert_eq!(output[4]["type"], json!(1.25));
    assert_eq!(output[5]["type"], Value::Null);
    assert_eq!(output[6]["type"], Value::Null);

    let bytes = encode(&table, TabularFormat::Csv).unwrap();
    let csv_rows = csv::Reader::from_reader(bytes.as_slice())
        .records()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(&csv_rows[0][1], r#""long""#);
    assert_eq!(
        &csv_rows[1][1],
        r#"{"type":"array","elementType":"string"}"#
    );
    assert_eq!(&csv_rows[2][1], "9007199254740993");
    assert_eq!(&csv_rows[3][1], "true");
    assert_eq!(&csv_rows[4][1], "1.25");
    assert_eq!(&csv_rows[5][1], "null");
    assert_eq!(&csv_rows[6][1], "");
    assert_eq!(&csv_rows[1][2], "null");
    assert_eq!(&csv_rows[2][2], "");

    let bytes = Bytes::from(encode(&table, TabularFormat::Parquet).unwrap());
    let file = SerializedFileReader::new(bytes.clone()).unwrap();
    let parquet_rows = file
        .get_row_iter(None)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for (index, row) in parquet_rows.iter().enumerate() {
        assert_eq!(
            row.get_string(0).unwrap(),
            rows[index]["name"].as_str().unwrap()
        );
        if index < 6 {
            assert_eq!(row.get_string(1).unwrap(), &csv_rows[index][1]);
            assert!(!row.is_null(1).unwrap());
        } else {
            assert!(row.is_null(1).unwrap());
        }
    }
    assert_eq!(parquet_rows[1].get_string(2).unwrap(), "null");
    assert!(parquet_rows[2].is_null(2).unwrap());
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
    for index in [1, 2] {
        assert_eq!(builder.schema().field(index).data_type(), &DataType::Utf8);
        assert_eq!(
            builder
                .schema()
                .field(index)
                .metadata()
                .get(JSON_LOGICAL_TYPE_KEY)
                .map(String::as_str),
            Some("json")
        );
    }
}

#[test]
fn explicit_json_column_selection_validates_names_bounds_and_unselected_types() {
    let rows = [json!({"v":"long","other":1}), json!({"v":{},"other":2.5})];
    assert!(matches!(
        TabularTable::from_rows_with_json_columns(&rows, &["v"]),
        Err(TabularError::TypeConflict { column, .. }) if column == "other"
    ));
    assert_eq!(
        TabularTable::from_rows_with_json_columns(&[], &["v"]),
        Err(TabularError::UnknownJsonColumn("v".into()))
    );
    assert_eq!(
        TabularTable::from_rows_with_json_columns(&[json!({"v":1})], &["v", "v"]),
        Err(TabularError::DuplicateColumn("v".into()))
    );
    let present_null =
        TabularTable::from_rows_with_json_columns(&[json!({"v":null})], &["v"]).unwrap();
    assert_eq!(present_null.columns()[0].data_type(), TabularType::Json);
    assert!(!present_null.columns()[0].nullable());
    for (value, limits, expected) in [
        (
            json!("four"),
            TabularLimits {
                max_cell_bytes: 3,
                ..Default::default()
            },
            "cell bytes",
        ),
        (
            json!({"x":null}),
            TabularLimits {
                max_json_depth: 1,
                ..Default::default()
            },
            "JSON depth",
        ),
        (
            json!(null),
            TabularLimits {
                max_input_bytes: 0,
                ..Default::default()
            },
            "input bytes",
        ),
    ] {
        assert!(matches!(
            TabularTable::from_rows_with_json_columns_and_limits(
                &[json!({"v":value})],
                &["v"],
                limits,
            ),
            Err(TabularError::LimitExceeded { limit, .. }) if limit == expected
        ));
    }
}

#[test]
fn sparse_inference_charges_the_dense_rectangle_before_allocating_rows() {
    let mut rows = vec![json!({}); 100_000];
    rows[0] = json!({
        "a":0,"b":1,"c":2,"d":3,"e":4,"f":5,"g":6,"h":7,"i":8,"j":9
    });
    for max in [4_096, rows.len() * size_of::<Vec<TabularValue>>() + 4_096] {
        let limits = TabularLimits {
            max_input_bytes: max,
            ..Default::default()
        };
        assert_eq!(
            TabularTable::from_rows_with_limits(&rows, limits),
            Err(TabularError::LimitExceeded {
                limit: "input bytes",
                max
            })
        );
        // Budget rejection must precede even a later invalid row, not happen
        // only after scanning or allocating the sparse-to-dense expansion.
        let last = rows.last_mut().unwrap();
        *last = Value::Null;
        assert_eq!(
            TabularTable::from_rows_with_limits(&rows, limits),
            Err(TabularError::LimitExceeded {
                limit: "input bytes",
                max
            })
        );
        *rows.last_mut().unwrap() = json!({});
    }

    let rows = [json!({"a":1,"b":2}), json!({})];
    let exact = rows.len() * size_of::<Vec<TabularValue>>()
        + rows.len() * 2 * size_of::<TabularValue>()
        + 2 * size_of::<TabularColumn>()
        + 2;
    let limits = TabularLimits {
        max_input_bytes: exact,
        ..Default::default()
    };
    let table = TabularTable::from_rows_with_limits(&rows, limits).unwrap();
    assert_eq!(table.rows()[1], [TabularValue::Null, TabularValue::Null]);
    assert_eq!(
        TabularTable::from_rows_with_limits(
            &rows,
            TabularLimits {
                max_input_bytes: exact - 1,
                ..limits
            },
        ),
        Err(TabularError::LimitExceeded {
            limit: "input bytes",
            max: exact - 1
        })
    );
}

fn complete_typed_table() -> TabularTable {
    TabularTable::new(
        vec![
            column("signed", TabularType::Int64, false),
            column("unsigned", TabularType::UInt64, false),
            column("float", TabularType::Float64, false),
            column("boolean", TabularType::Boolean, false),
            column("text", TabularType::String, false),
            column("metadata", TabularType::Json, true),
            column("nothing", TabularType::Null, true),
        ],
        vec![
            vec![
                TabularValue::Int64(i64::MIN),
                TabularValue::UInt64(u64::MAX),
                TabularValue::Float64(-0.0),
                TabularValue::Boolean(true),
                TabularValue::String("雪😀\r\n\"".into()),
                TabularValue::Json(parse_data(r#"{"z":[1,null],"a":"雪"}"#).unwrap()),
                TabularValue::Null,
            ],
            vec![
                TabularValue::Int64(i64::MAX),
                TabularValue::UInt64(0),
                TabularValue::Float64(1.25),
                TabularValue::Boolean(false),
                TabularValue::String(String::new()),
                TabularValue::Null,
                TabularValue::Null,
            ],
        ],
    )
    .unwrap()
}

#[test]
fn parquet_is_finalized_and_independent_readers_decode_exact_values_and_schema() {
    let table = complete_typed_table();
    let bytes = encode(&table, TabularFormat::Parquet).unwrap();
    assert!(bytes.starts_with(b"PAR1"));
    assert!(bytes.ends_with(b"PAR1"));
    let bytes = Bytes::from(bytes);
    let file = SerializedFileReader::new(bytes.clone()).unwrap();
    assert_eq!(file.metadata().file_metadata().num_rows(), 2);
    let descriptor = file.metadata().file_metadata().schema_descr();
    assert_eq!(descriptor.num_columns(), 7);
    assert_eq!(descriptor.column(0).physical_type(), PhysicalType::INT64);
    assert_eq!(descriptor.column(1).physical_type(), PhysicalType::INT64);
    assert_eq!(
        descriptor.column(1).logical_type_ref(),
        Some(&LogicalType::integer(64, false))
    );
    let marker = file
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .unwrap()
        .iter()
        .find(|entry| entry.key == JSON_LOGICAL_TYPE_KEY)
        .unwrap();
    assert_eq!(marker.value.as_deref(), Some(r#"["metadata"]"#));
    let decoded = file
        .get_row_iter(None)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(decoded[0].get_long(0).unwrap(), i64::MIN);
    assert_eq!(decoded[1].get_long(0).unwrap(), i64::MAX);
    assert_eq!(decoded[0].get_ulong(1).unwrap(), u64::MAX);
    assert_eq!(decoded[1].get_ulong(1).unwrap(), 0);
    assert_eq!(
        decoded[0].get_double(2).unwrap().to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(decoded[1].get_double(2).unwrap(), 1.25);
    assert!(decoded[0].get_bool(3).unwrap());
    assert!(!decoded[1].get_bool(3).unwrap());
    assert_eq!(decoded[0].get_string(4).unwrap(), "雪😀\r\n\"");
    assert_eq!(decoded[1].get_string(4).unwrap(), "");
    assert_eq!(
        decoded[0].get_string(5).unwrap(),
        r#"{"z":[1,null],"a":"雪"}"#
    );
    assert!(matches!(
        decoded[1].get_column_iter().nth(5).unwrap().1,
        parquet::record::Field::Null
    ));
    assert!(decoded[0].is_null(6).unwrap());
    assert!(decoded[1].is_null(6).unwrap());

    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
    let schema = builder.schema();
    for (index, expected) in [
        DataType::Int64,
        DataType::UInt64,
        DataType::Float64,
        DataType::Boolean,
        DataType::Utf8,
        DataType::Utf8,
        DataType::Null,
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(schema.field(index).data_type(), expected);
        assert_eq!(schema.field(index).name(), table.columns()[index].name());
        assert_eq!(
            schema.field(index).is_nullable(),
            table.columns()[index].nullable()
        );
    }
    assert_eq!(
        schema
            .field(5)
            .metadata()
            .get(JSON_LOGICAL_TYPE_KEY)
            .map(String::as_str),
        Some("json")
    );
    assert!(
        !schema
            .field(4)
            .metadata()
            .contains_key(JSON_LOGICAL_TYPE_KEY)
    );
    let batches = builder
        .build()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 2);
    assert_eq!(
        batch
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(0),
        i64::MIN
    );
    assert_eq!(
        batch
            .column(1)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap()
            .value(0),
        u64::MAX
    );
    assert_eq!(
        batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap()
            .value(0)
            .to_bits(),
        (-0.0_f64).to_bits()
    );
    assert!(
        batch
            .column(3)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap()
            .value(0)
    );
    let text = batch
        .column(4)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(text.value(0), "雪😀\r\n\"");
    assert_eq!(text.value(1), "");
    assert!(batch.column(5).is_null(1));
    assert_eq!(batch.column(6).len(), 2);
    // Arrow's NullArray has no physical validity bitmap; count logical nulls.
    assert_eq!(batch.column(6).logical_null_count(), 2);
}

#[test]
fn explicit_zero_rows_keep_the_schema_in_all_formats() {
    let table = TabularTable::new(
        vec![
            column("id", TabularType::UInt64, false),
            column("name", TabularType::String, true),
            column("metadata", TabularType::Json, true),
        ],
        vec![],
    )
    .unwrap();
    assert_eq!(table.row_count(), 0);
    assert_eq!(encode(&table, TabularFormat::Json).unwrap(), b"[]");
    assert_eq!(
        encode(&table, TabularFormat::Csv).unwrap(),
        b"id,name,metadata\r\n"
    );
    let bytes = Bytes::from(encode(&table, TabularFormat::Parquet).unwrap());
    let file = SerializedFileReader::new(bytes.clone()).unwrap();
    assert_eq!(file.metadata().file_metadata().num_rows(), 0);
    assert_eq!(
        file.metadata().file_metadata().schema_descr().num_columns(),
        3
    );
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
    assert_eq!(builder.schema().field(0).data_type(), &DataType::UInt64);
    assert!(!builder.schema().field(0).is_nullable());
    assert!(builder.schema().field(1).is_nullable());
    assert_eq!(
        builder
            .schema()
            .field(2)
            .metadata()
            .get(JSON_LOGICAL_TYPE_KEY)
            .map(String::as_str),
        Some("json")
    );
    assert!(builder.build().unwrap().next().is_none());
}

#[test]
fn zero_column_inference_has_explicit_format_limits() {
    for (rows, expected) in [
        (vec![], b"[]".as_slice()),
        (vec![json!({}), json!({})], b"[{},{}]".as_slice()),
    ] {
        let table = TabularTable::from_rows(&rows).unwrap();
        assert!(table.columns().is_empty());
        assert_eq!(table.row_count(), rows.len());
        assert_eq!(encode(&table, TabularFormat::Json).unwrap(), expected);
        assert_eq!(encode(&table, TabularFormat::Csv).unwrap(), b"");
        assert_eq!(
            encode(&table, TabularFormat::Parquet),
            Err(TabularError::EmptyParquetSchema)
        );
    }
}

#[test]
fn row_groups_preserve_order_across_bounded_batches() {
    let rows = (0..2_050)
        .map(|index| vec![TabularValue::Int64(index)])
        .collect();
    let table = TabularTable::new(vec![column("n", TabularType::Int64, false)], rows).unwrap();
    let bytes = Bytes::from(encode(&table, TabularFormat::Parquet).unwrap());
    let file = SerializedFileReader::new(bytes).unwrap();
    assert_eq!(file.num_row_groups(), 3);
    for (index, row) in file.get_row_iter(None).unwrap().enumerate() {
        assert_eq!(
            row.unwrap().get_long(0).unwrap(),
            i64::try_from(index).unwrap()
        );
    }
}

#[test]
fn input_limits_fail_explicitly_instead_of_truncating() {
    for (rows, limits, expected) in [
        (
            vec![json!({"v":1})],
            TabularLimits {
                max_rows: 0,
                ..Default::default()
            },
            "rows",
        ),
        (
            vec![json!({"v":1,"w":2})],
            TabularLimits {
                max_columns: 1,
                ..Default::default()
            },
            "columns",
        ),
        (
            vec![json!({"v":1}), json!({"v":2})],
            TabularLimits {
                max_cells: 1,
                ..Default::default()
            },
            "cells",
        ),
        (
            vec![json!({"v":1})],
            TabularLimits {
                max_input_bytes: 0,
                ..Default::default()
            },
            "input bytes",
        ),
        (
            vec![json!({"v":"long"})],
            TabularLimits {
                max_cell_bytes: 3,
                ..Default::default()
            },
            "cell bytes",
        ),
        (
            vec![json!({"long":1})],
            TabularLimits {
                max_cell_bytes: 3,
                ..Default::default()
            },
            "cell bytes",
        ),
        (
            vec![json!({"v":{"x":0}})],
            TabularLimits {
                max_cell_bytes: 4,
                ..Default::default()
            },
            "cell bytes",
        ),
        (
            vec![json!({"v":{"x":[]}})],
            TabularLimits {
                max_json_depth: 1,
                ..Default::default()
            },
            "JSON depth",
        ),
    ] {
        let error = TabularTable::from_rows_with_limits(&rows, limits).unwrap_err();
        assert!(
            matches!(
                error, TabularError::LimitExceeded { limit, .. } if limit == expected
            ),
            "{expected}: {error}"
        );
    }
    let limits = TabularLimits {
        max_rows: TabularLimits::default().max_rows + 1,
        ..Default::default()
    };
    assert!(matches!(
        TabularTable::from_rows_with_limits(&[], limits),
        Err(TabularError::InvalidLimit { limit: "rows", .. })
    ));
}

#[test]
fn every_encoder_enforces_its_complete_output_bound() {
    for format in [
        TabularFormat::Json,
        TabularFormat::Csv,
        TabularFormat::Parquet,
    ] {
        let base = complete_typed_table();
        let expected = encode(&base, format).unwrap();
        let bounded = |max| {
            TabularTable::with_limits(
                base.columns().to_vec(),
                base.rows().to_vec(),
                TabularLimits {
                    max_output_bytes: max,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        assert_eq!(encode(&bounded(expected.len()), format).unwrap(), expected);
        assert_eq!(
            encode(&bounded(expected.len() - 1), format),
            Err(TabularError::LimitExceeded {
                limit: "output bytes",
                max: expected.len() - 1
            })
        );
    }
}
