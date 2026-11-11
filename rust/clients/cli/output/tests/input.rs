// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
#![deny(unnameable_test_items)]

use arrow_array::{ArrayRef, Decimal128Array, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use fabric_cli_output::{
    input::{InputError, records, strict_json},
    tabular::TabularFormat,
};
use parquet::arrow::ArrowWriter;
use serde_json::json;
use std::sync::Arc;

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
    ] {
        assert!(records(bytes, TabularFormat::Csv).is_err());
    }
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
    assert!(matches!(
        DataType::Timestamp(arrow_schema::TimeUnit::Millisecond, None),
        DataType::Timestamp(..)
    ));
}
