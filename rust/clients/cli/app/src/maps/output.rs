// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_cli_output::tabular::{TabularColumn, TabularLimits, TabularTable, TabularType};
use fabric_sdk::maps::{MAX_OUTPUT_BYTES, ProcessingResult};
use serde_json::{Value, json};

/// Render complete nested results or a type-preserving explicit projection.
///
/// Text is labelled JSON rather than a lossy flattened table. Failure status is
/// never projected away, even if a query selects a null or empty value.
pub fn render(
    result: &ProcessingResult,
    query: Option<&str>,
    text: bool,
    offline: bool,
) -> Result<Vec<u8>, &'static str> {
    let mut document = result.document().clone();
    document["validation"] = json!(if offline {
        "offline-synthetic-fixture"
    } else {
        "live-unqualified-private-contract"
    });
    if let Some(query) = query {
        document["data"] = super::projection::project(&document["data"], query)?;
        document["projected"] = Value::Bool(true);
    }
    let mut output = BoundedOutput(Vec::new());
    use std::io::Write;
    if text {
        output
            .write_all(
                b"Maps processing data (nested JSON; HTTP success is not location-quality proof)\n",
            )
            .map_err(|_| "Maps output byte budget exceeded.")?;
    }
    serde_json::to_writer_pretty(&mut output, &document)
        .map_err(|_| "Maps result serialization or output byte budget failed.")?;
    output
        .write_all(b"\n")
        .map_err(|_| "Maps output byte budget exceeded.")?;
    Ok(output.0)
}

/// Declare real result rows using the repository's logical-JSON table codecs.
///
/// CSV/Parquet preserve nested result/context values as documented JSON cells;
/// they never silently flatten coordinates, confidence, units or timestamps.
pub fn export_table(
    result: &ProcessingResult,
    offline: bool,
) -> Result<TabularTable, &'static str> {
    let mut rows = result
        .export_rows()
        .map_err(|_| "Maps expanded result output budget exceeded.")?;
    for row in &mut rows {
        row["validation"] = json!(if offline {
            "offline-synthetic-fixture"
        } else {
            "live-unqualified-private-contract"
        });
    }

    let limits = TabularLimits {
        max_output_bytes: MAX_OUTPUT_BYTES,
        ..TabularLimits::default()
    };
    if rows.is_empty() {
        return TabularTable::with_limits(
            vec![
                TabularColumn::new("resultIndex", TabularType::UInt64, false),
                TabularColumn::new("result", TabularType::Json, true),
                TabularColumn::new("operation", TabularType::String, false),
                TabularColumn::new("provider", TabularType::String, false),
                TabularColumn::new("apiVersion", TabularType::String, false),
                TabularColumn::new("contract", TabularType::String, false),
                TabularColumn::new("context", TabularType::Json, false),
                TabularColumn::new("responseMetadata", TabularType::Json, false),
                TabularColumn::new("complete", TabularType::Boolean, false),
                TabularColumn::new("validation", TabularType::String, false),
            ],
            Vec::new(),
            limits,
        )
        .map_err(|_| "Maps empty result schema failed.");
    }
    TabularTable::from_rows_with_json_columns_and_limits(
        &rows,
        &["result", "context", "responseMetadata"],
        limits,
    )
    .map_err(|_| "Maps results cannot be represented losslessly in the requested table.")
}

struct BoundedOutput(Vec<u8>);

impl std::io::Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_OUTPUT_BYTES - self.0.len() {
            return Err(std::io::Error::other("Maps output byte budget exceeded"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
