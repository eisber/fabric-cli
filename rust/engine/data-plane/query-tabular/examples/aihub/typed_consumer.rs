// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Direct SDK consumer of the validated DataAgent typed model and shared codecs.
#![deny(unnameable_test_items)]

use std::{
    error::Error,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use fabric_sdk_aihub_query::{
    Column, Completeness, NativeValue, QueryOutcome, QueryResource, TypedQueryResult,
};
use fabric_sdk_query_tabular::{ConversionControl, ConversionLimits, convert_aihub};
use fabric_sdk_tabular::{OutputLimits, ResultFormat, encode};

fn main() -> Result<(), Box<dyn Error>> {
    // A real caller supplies the complete outcome from its injected AIHub client.
    let source = QueryOutcome::Complete(TypedQueryResult {
        resultsets: vec![QueryResource {
            uri: "opaque:synthetic-result".into(),
            schema_version: 1,
            result_id: "11111111-2222-4333-8444-555555555555".into(),
            columns: vec![
                Column {
                    name: "id".into(),
                    engine_type: "UInt64".into(),
                    logical_type: "integer".into(),
                    encoding: "json-number".into(),
                },
                Column {
                    name: "amount".into(),
                    engine_type: "decimal".into(),
                    logical_type: "decimal".into(),
                    encoding: "json-number".into(),
                },
                Column {
                    name: "bytes".into(),
                    engine_type: "Byte[]".into(),
                    logical_type: "binary".into(),
                    encoding: "base64".into(),
                },
                Column {
                    name: "when".into(),
                    engine_type: "DateTime".into(),
                    logical_type: "datetime".into(),
                    encoding: "iso8601".into(),
                },
            ],
            rows: vec![vec![
                NativeValue::Integer("18446744073709551615".into()),
                NativeValue::Decimal("0.1000000000000000000000000001".into()),
                NativeValue::Binary("AP8i".into()),
                NativeValue::DateTime("1970-01-01T00:00:00.0000001Z".into()),
            ]],
            row_count: 1,
            truncated: false,
            max_rows: Some(100),
            total_row_count: None,
        }],
        resources: vec![],
        messages: vec![],
        completeness: Completeness::Complete,
        protocol_version: "2025-06-18".into(),
    });
    let cancelled = AtomicBool::new(false);
    let control = ConversionControl::new(Instant::now() + Duration::from_secs(5), &cancelled);
    let converted = convert_aihub(
        source,
        ConversionLimits::new(4, 100, 16, 1024 * 1024, 4, 16)?,
        &control,
    )?;
    let limits = OutputLimits::new(100, 1024 * 1024, 4)?;
    for dataset in &converted.datasets {
        for format in [ResultFormat::Json, ResultFormat::Csv, ResultFormat::Parquet] {
            let bytes = encode(&dataset.table, format, limits)?;
            println!(
                "dataset ordinal={}, format={format:?}, bytes={}",
                dataset.ordinal,
                bytes.len()
            );
        }
    }
    Ok(())
}
