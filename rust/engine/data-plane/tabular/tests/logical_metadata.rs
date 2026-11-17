// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Explicit query logical types do not relax native IPC diagnostic handling.

#![deny(unnameable_test_items)]

use std::{collections::HashMap, sync::Arc};

use arrow_array::{ArrayRef, Int64Array, LargeStringArray, RecordBatch, StringArray};
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, Field, Schema};
use bytes::Bytes;
use fabric_sdk_tabular::{
    LOGICAL_TYPE_METADATA_KEY, NATIVE_TYPE_METADATA_KEY, OutputLimits, ResultFormat, TabularError,
    TabularResult, decode_arrow_stream, encode,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::Value;

fn limits() -> OutputLimits {
    OutputLimits::new(100, 1_048_576, 10).unwrap()
}

fn field(data_type: DataType, logical_type: &str) -> Field {
    Field::new("value", data_type, true).with_metadata(HashMap::from([
        (LOGICAL_TYPE_METADATA_KEY.into(), logical_type.into()),
        (NATIVE_TYPE_METADATA_KEY.into(), "dynamic".into()),
    ]))
}

fn result(field: Field, values: ArrayRef) -> Result<TabularResult, TabularError> {
    let schema = Arc::new(Schema::new(vec![field]));
    let batch = RecordBatch::try_new(schema.clone(), vec![values]).unwrap();
    TabularResult::try_new(schema, vec![batch], limits())
}

#[test]
fn logical_json_preserves_exact_text_nulls_and_metadata_in_every_format() {
    let exact = r#"{"wide":18446744073709551616,"decimal":1.2300,"huge":1e9999}"#;
    let table = result(
        field(DataType::Utf8, "json"),
        Arc::new(StringArray::from(vec![Some(exact), Some("null"), None])),
    )
    .unwrap();
    let json: Value =
        serde_json::from_slice(&encode(&table, ResultFormat::Json, limits()).unwrap()).unwrap();
    assert_eq!(json["schema"][0]["encoding"], "json-text");
    assert_eq!(
        json["schema"][0]["metadata"][LOGICAL_TYPE_METADATA_KEY],
        "json"
    );
    assert_eq!(json["rows"][0][0], exact);
    assert_eq!(json["rows"][1][0], "null");
    assert!(json["rows"][2][0].is_null());

    let csv = String::from_utf8(encode(&table, ResultFormat::Csv, limits()).unwrap()).unwrap();
    assert!(csv.contains("\"\"wide\"\":18446744073709551616"));
    assert!(csv.ends_with("\"null\"\r\n\r\n"));

    let bytes = encode(&table, ResultFormat::Parquet, limits()).unwrap();
    let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes)).unwrap();
    assert_eq!(builder.schema().as_ref(), table.schema().as_ref());
    let batches = builder
        .build()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(batches[0], table.batches()[0]);
}

#[test]
fn guid_text_is_validated_without_reformatting() {
    let original = "ABCDEF01-2345-6789-ABCD-EF0123456789";
    let table = result(
        field(DataType::LargeUtf8, "guid"),
        Arc::new(LargeStringArray::from(vec![Some(original), None])),
    )
    .unwrap();
    let json: Value =
        serde_json::from_slice(&encode(&table, ResultFormat::Json, limits()).unwrap()).unwrap();
    assert_eq!(json["schema"][0]["encoding"], "guid-string");
    assert_eq!(json["rows"][0][0], original);
}

#[test]
fn invalid_logical_values_are_errors_not_repaired_strings() {
    for (logical, value) in [
        ("json", ""),
        ("json", "{\"unfinished\":"),
        ("json", "{} {}"),
        ("guid", "not-a-guid"),
        ("guid", "{abcdef01-2345-6789-abcd-ef0123456789}"),
    ] {
        assert!(matches!(
            result(
                field(DataType::Utf8, logical),
                Arc::new(StringArray::from(vec![Some(value)])),
            ),
            Err(TabularError::UnsupportedValue {
                column: 0,
                row: 0,
                ..
            })
        ));
    }
}

#[test]
fn logical_value_errors_use_absolute_row_positions_across_batches() {
    let schema = Arc::new(Schema::new(vec![field(DataType::Utf8, "json")]));
    let batches = ["{}", "invalid"]
        .into_iter()
        .map(|value| {
            RecordBatch::try_new(
                schema.clone(),
                vec![Arc::new(StringArray::from(vec![value])) as ArrayRef],
            )
            .unwrap()
        })
        .collect();
    assert!(matches!(
        TabularResult::try_new(schema, batches, limits()),
        Err(TabularError::UnsupportedValue {
            column: 0,
            row: 1,
            ..
        })
    ));
}

#[test]
fn ignored_empty_metadata_keeps_existing_json_shape() {
    let table = result(
        Field::new("value", DataType::Utf8, true)
            .with_metadata(HashMap::from([(String::new(), String::new())])),
        Arc::new(StringArray::from(vec![""])),
    )
    .unwrap();
    let json: Value =
        serde_json::from_slice(&encode(&table, ResultFormat::Json, limits()).unwrap()).unwrap();
    assert!(json["schema"][0].get("metadata").is_none());
    assert_eq!(json["schema"][0]["encoding"], "string");
    assert_eq!(json["rows"][0][0], "");
}

#[test]
fn logical_validation_does_not_change_ordinary_json_object_deserialization() {
    let text = r#"{"$serde_json::private::RawValue":"123"}"#;
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        parsed,
        serde_json::json!({"$serde_json::private::RawValue": "123"})
    );
    let table = result(
        field(DataType::Utf8, "json"),
        Arc::new(StringArray::from(vec![text])),
    )
    .unwrap();
    let encoded: Value =
        serde_json::from_slice(&encode(&table, ResultFormat::Json, limits()).unwrap()).unwrap();
    assert_eq!(encoded["rows"][0][0], text);
}

#[test]
fn unsupported_metadata_and_logical_physical_type_mismatches_still_fail() {
    let invalid = [
        HashMap::from([(LOGICAL_TYPE_METADATA_KEY.into(), "decimal".into())]),
        HashMap::from([("pandas".into(), "{}".into())]),
        HashMap::from([(NATIVE_TYPE_METADATA_KEY.into(), "".into())]),
        HashMap::from([(NATIVE_TYPE_METADATA_KEY.into(), "x".repeat(257))]),
        HashMap::from([(NATIVE_TYPE_METADATA_KEY.into(), "long\n".into())]),
    ];
    for metadata in invalid {
        assert!(matches!(
            result(
                Field::new("value", DataType::Utf8, true).with_metadata(metadata),
                Arc::new(StringArray::from(vec!["{}"])),
            ),
            Err(TabularError::Metadata { .. })
        ));
    }
    assert!(matches!(
        result(
            field(DataType::Int64, "json"),
            Arc::new(Int64Array::from(vec![1])),
        ),
        Err(TabularError::Metadata { .. })
    ));
}

#[test]
fn empty_logical_schema_is_preserved_and_metadata_uses_byte_budget() {
    let schema = Arc::new(Schema::new(vec![field(DataType::Utf8, "json")]));
    let table = TabularResult::try_new(schema.clone(), Vec::new(), limits()).unwrap();
    let json: Value =
        serde_json::from_slice(&encode(&table, ResultFormat::Json, limits()).unwrap()).unwrap();
    assert_eq!(
        json["schema"][0]["metadata"][NATIVE_TYPE_METADATA_KEY],
        "dynamic"
    );
    assert_eq!(json["rows"], serde_json::json!([]));
    assert!(
        TabularResult::try_new(schema, Vec::new(), OutputLimits::new(0, 1, 1).unwrap()).is_err()
    );
}

#[test]
fn native_type_hint_does_not_change_physical_values_or_encoding() {
    let table = result(
        Field::new("value", DataType::Int64, true).with_metadata(HashMap::from([(
            NATIVE_TYPE_METADATA_KEY.into(),
            "long".into(),
        )])),
        Arc::new(Int64Array::from(vec![i64::MAX])),
    )
    .unwrap();
    let json: Value =
        serde_json::from_slice(&encode(&table, ResultFormat::Json, limits()).unwrap()).unwrap();
    assert_eq!(json["schema"][0]["encoding"], "integer-string");
    assert_eq!(json["rows"][0][0], i64::MAX.to_string());
}

#[test]
fn native_ipc_metadata_is_still_unknown_even_for_query_logical_keys() {
    let schema = Schema::new(vec![field(DataType::Utf8, "json")]);
    let mut bytes = Vec::new();
    let mut writer = StreamWriter::try_new(&mut bytes, &schema).unwrap();
    writer.finish().unwrap();
    drop(writer);
    assert!(matches!(
        decode_arrow_stream(&bytes, limits()),
        Err(TabularError::Metadata { .. })
    ));
}

#[test]
fn schema_level_logical_metadata_is_never_an_application_success_marker() {
    let schema = Arc::new(
        Schema::new(vec![Field::new("value", DataType::Utf8, true)]).with_metadata(HashMap::from(
            [(LOGICAL_TYPE_METADATA_KEY.into(), "json".into())],
        )),
    );
    assert!(matches!(
        TabularResult::try_new(schema, Vec::new(), limits()),
        Err(TabularError::Metadata { .. })
    ));
}
