// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Direct SDK consumer: an offline native result through conversion and shared codecs.
#![deny(unnameable_test_items)]

use std::{
    error::Error,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use fabric_sdk_query::QueryCapability;
use fabric_sdk_query_tabular::{ConversionControl, ConversionLimits, convert_kql};
use fabric_sdk_tabular::{OutputLimits, ResultFormat, encode};
use serde_json::json;

#[path = "../tests/support/mod.rs"]
mod support;

fn main() -> Result<(), Box<dyn Error>> {
    // A real caller passes QueryResult from its existing query executor.
    let native = support::result(
        &[("id", "long"), ("label", "string"), ("utc", "datetime")],
        vec![
            vec![
                json!(9_007_199_254_740_993_i64),
                json!(""),
                json!("2026-01-01T00:00:00.0000001Z"),
            ],
            vec![json!(i64::MAX), json!(null), json!(null)],
        ],
    );
    let cancelled = AtomicBool::new(false);
    let control = ConversionControl::new(Instant::now() + Duration::from_secs(5), &cancelled);
    let converted = convert_kql(
        native,
        ConversionLimits::new(4, 100, 16, 1024 * 1024, 4, 16)?,
        &control,
    )?;
    let output_limits = OutputLimits::new(100, 1024 * 1024, 4)?;
    for dataset in &converted.datasets {
        for format in [ResultFormat::Json, ResultFormat::Csv, ResultFormat::Parquet] {
            let bytes = encode(&dataset.table, format, output_limits)?;
            println!(
                "dataset ordinal={}, native table={}, format={format:?}, bytes={}",
                dataset.ordinal,
                dataset.table_id,
                bytes.len()
            );
        }
    }
    assert!(converted.evidence.completed);
    assert!(converted.evidence.http_eof);
    assert_eq!(
        converted.evidence.adapter_capability,
        QueryCapability::ScriptedOffline
    );
    assert!(!converted.evidence.production_verified);
    Ok(())
}
