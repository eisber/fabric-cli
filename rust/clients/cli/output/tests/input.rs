// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//! Strict typed input decoding, quoting, precision and unsupported schema tests.
#![deny(unnameable_test_items)]

use arrow_array::types::Int32Type;
use arrow_array::{
    ArrayRef, Decimal128Array, Float64Array, Int64Array, ListArray, RecordBatch, StringArray,
    TimestampMillisecondArray,
};
use arrow_schema::{Field, Schema};
use fabric_cli_output::{
    input::{InputError, decode_json_input, records, strict_json, strict_json_bounded},
    tabular::TabularFormat,
};
use parquet::arrow::ArrowWriter;
use serde_json::json;
use std::{num::NonZeroUsize, sync::Arc};

fn parquet(array: ArrayRef) -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "x",
        array.data_type().clone(),
        true,
    )]));
    let batch = RecordBatch::try_new(schema.clone(), vec![array]).unwrap();
    let mut bytes = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut bytes, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
    bytes
}

#[test]
fn strict_json_rejects_duplicate_nested_and_escape_equivalent_keys() {
    for input in [
        br#"{"x":0,"x":1}"#.as_slice(),
        br#"[{"x":0,"\u0078":1}]"#,
        br#"{}{}"#,
    ] {
        assert_eq!(strict_json(input), Err(InputError::Json));
    }
}

#[test]
fn object_and_record_frontdoors_keep_distinct_bounds_and_diagnostics() {
    let bytes = br#"{"x":0}"#;
    let bound = NonZeroUsize::new(bytes.len()).unwrap();
    assert_eq!(
        decode_json_input(Some(bytes), "application/json", bound).unwrap(),
        strict_json_bounded(bytes, bound.get()).unwrap()
    );
    assert_eq!(
        decode_json_input(None, "application/json", bound),
        Err(InputError::Missing)
    );
    assert_eq!(
        decode_json_input(Some(b" "), "application/json", bound),
        Err(InputError::Empty)
    );
    assert_eq!(
        decode_json_input(Some(&[0xff]), "application/json", bound),
        Err(InputError::InvalidUtf8)
    );
    assert_eq!(
        decode_json_input(Some(bytes), "text/plain", bound),
        Err(InputError::UnsupportedMedia)
    );
    let smaller = NonZeroUsize::new(bytes.len() - 1).unwrap();
    assert_eq!(
        decode_json_input(Some(bytes), "application/json", smaller),
        Err(InputError::TooLarge)
    );
    assert_eq!(
        strict_json_bounded(bytes, smaller.get()),
        Err(InputError::Limit)
    );
    for bytes in [
        br#"{"x":0,"\u0078":1}"#.as_slice(),
        br#"{"nested":{"x":0,"x":1}}"#,
        br#"{}{}"#,
    ] {
        let bound = NonZeroUsize::new(bytes.len()).unwrap();
        assert_eq!(
            decode_json_input(Some(bytes), "application/json", bound),
            Err(InputError::InvalidJson)
        );
        assert_eq!(strict_json(bytes), Err(InputError::Json));
    }
    assert_eq!(strict_json(b"null").unwrap(), serde_json::Value::Null);
    assert_eq!(
        decode_json_input(Some(b"null"), "application/json", bound),
        Err(InputError::InvalidJson)
    );
    assert!(records(bytes, TabularFormat::Json).is_err());
}

#[test]
fn csv_quoting_types_and_nulls_are_explicit_not_guessed() {
    let bytes = b"x,flag,nothing,text\r\n0,false,null,\"\"\"a,b\"\"\"\r\n";
    assert_eq!(
        records(bytes, TabularFormat::Csv).unwrap(),
        vec![json!({"x":0,"flag":false,"nothing":null,"text":"a,b"})]
    );
    for bytes in [
        b"x,x\r\n0,1\r\n".as_slice(),
        b"x\r\n\r\n",
        b"x,y\r\n0,\r\n",
        b"x\r\nnot-JSON\r\n",
        b"x,y\r\n0\r\n",
        b"x\r\n\"0",
        b"x\r\n0\r\n\r\n",
        b"x\r\n\"0\"junk\r\n",
    ] {
        assert!(records(bytes, TabularFormat::Csv).is_err());
    }
}

#[test]
fn text_file_bom_is_optional_only_at_the_document_boundary() {
    assert_eq!(
        records(b"\xef\xbb\xbf\"x\"\r\n0\r\n", TabularFormat::Csv).unwrap(),
        vec![json!({"x":0})]
    );
    assert_eq!(
        records(b"\xef\xbb\xbf[{\"x\":0}]", TabularFormat::Json).unwrap(),
        vec![json!({"x":0})]
    );
    assert_eq!(
        records(b"x\r\n\"\"\"\xef\xbb\xbftext\"\"\"\r\n", TabularFormat::Csv).unwrap(),
        vec![json!({"x":"\u{feff}text"})]
    );
    assert_eq!(strict_json(b"\xef\xbb\xbf{}"), Err(InputError::Json));
}

#[test]
fn parquet_preserves_i64_precision_and_rejects_nonfinite_logical_and_nested_types() {
    let bytes = parquet(Arc::new(Int64Array::from(vec![
        Some(9007199254740993),
        None,
    ])));
    assert_eq!(
        records(&bytes, TabularFormat::Parquet).unwrap(),
        vec![json!({"x":9007199254740993_i64}), json!({"x":null})]
    );
    let bytes = parquet(Arc::new(Float64Array::from(vec![f64::INFINITY])));
    assert_eq!(
        records(&bytes, TabularFormat::Parquet),
        Err(InputError::Parquet)
    );
    let bytes = parquet(Arc::new(
        Decimal128Array::from(vec![123_i128])
            .with_precision_and_scale(10, 2)
            .unwrap(),
    ));
    assert_eq!(
        records(&bytes, TabularFormat::Parquet),
        Err(InputError::Parquet)
    );
    let bytes = parquet(Arc::new(StringArray::from(vec!["exact"])));
    assert_eq!(
        records(&bytes, TabularFormat::Parquet).unwrap(),
        vec![json!({"x":"exact"})]
    );
    for array in [
        Arc::new(TimestampMillisecondArray::from(vec![1])) as ArrayRef,
        Arc::new(ListArray::from_iter_primitive::<Int32Type, _, _>(vec![
            Some(vec![Some(1)]),
        ])) as ArrayRef,
    ] {
        assert_eq!(
            records(&parquet(array), TabularFormat::Parquet),
            Err(InputError::Parquet)
        );
    }
}

#[test]
fn shared_parquet_json_logical_cells_are_not_silently_stringified() {
    use fabric_cli_output::tabular::{
        TabularColumn, TabularTable, TabularType, TabularValue, encode,
    };
    let table = TabularTable::new(
        vec![TabularColumn::new("x", TabularType::Json, false)],
        vec![vec![TabularValue::Json(json!({"nested":1}))]],
    )
    .unwrap();
    assert_eq!(
        records(
            &encode(&table, TabularFormat::Parquet).unwrap(),
            TabularFormat::Parquet
        ),
        Err(InputError::Parquet)
    );
}
