// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_sdk_tabular::{OutputLimits, ResultFormat, TabularError, TabularResult, encode};

/// A completed query that cannot be represented by the requested single file.
#[derive(Debug, thiserror::Error)]
pub enum QueryEncodingError {
    /// No implicit first-set selection or invented resultset selector is allowed.
    #[error(
        "Single-file query output requires exactly one resultset, received {0}; use an unambiguous single-result query"
    )]
    ResultSetCount(usize),
    /// Normal multi-set presentation exceeded its aggregate row bound.
    #[error("Query presentation exceeds the total row limit of {0}")]
    AggregateRows(usize),
    /// Normal multi-set presentation exceeded its aggregate batch bound.
    #[error("Query presentation exceeds the total batch limit of {0}")]
    AggregateBatches(usize),
    /// The complete presentation, including its envelope, exceeds its byte bound.
    #[error("Query presentation exceeds the encoded byte limit of {0}")]
    OutputLimit(usize),
    /// Bounded output allocation failed without returning a clipped prefix.
    #[error("Query presentation could not allocate its bounded output")]
    Allocation,
    /// A declared native type, value, metadata or output bound is unsupported.
    #[error(transparent)]
    Codec(#[from] TabularError),
}

/// Encode exactly one already-converted native Arrow dataset using shared codecs.
///
/// The caller retains the complete native/bridge result and execution evidence.
/// A codec or file-delivery error does not authorize query replay. Format is
/// explicit, never inferred from filenames, rendered tables, or JSON objects.
pub fn encode_single(
    tables: &[&TabularResult],
    format: ResultFormat,
    limits: OutputLimits,
) -> Result<Vec<u8>, QueryEncodingError> {
    let [table] = tables else {
        return Err(QueryEncodingError::ResultSetCount(tables.len()));
    };
    Ok(encode(table, format, limits)?)
}

/// Render every completed native dataset as rich JSON for normal no-export output.
///
/// Each table uses the existing SDK codec's ordered schema and positional rows;
/// exact numbers are never parsed back through a JSON row-object representation.
/// The caller retains the original native dataset identities and evidence
/// separately. Unlike single-file export, normal presentation retains every set.
/// Aggregate limits include the envelope and trailing newline; no prefix is
/// returned on a schema, value, allocation or bound failure.
pub fn encode_presentation(
    tables: &[&TabularResult],
    limits: OutputLimits,
) -> Result<Vec<u8>, QueryEncodingError> {
    let mut rows = 0usize;
    let mut batches = 0usize;
    for table in tables {
        rows = rows
            .checked_add(table.num_rows())
            .filter(|&rows| rows <= limits.max_rows())
            .ok_or(QueryEncodingError::AggregateRows(limits.max_rows()))?;
        batches = batches
            .checked_add(table.batches().len())
            .filter(|&batches| batches <= limits.max_batches())
            .ok_or(QueryEncodingError::AggregateBatches(limits.max_batches()))?;
    }
    let mut bytes = Vec::new();
    append(&mut bytes, b"{\"resultsets\":[", limits.max_bytes())?;
    for (index, table) in tables.iter().enumerate() {
        if index != 0 {
            append(&mut bytes, b",", limits.max_bytes())?;
        }
        let encoded = encode(table, ResultFormat::Json, limits)?;
        append(&mut bytes, &encoded, limits.max_bytes())?;
    }
    append(&mut bytes, b"]}\n", limits.max_bytes())?;
    Ok(bytes)
}

fn append(output: &mut Vec<u8>, bytes: &[u8], max_bytes: usize) -> Result<(), QueryEncodingError> {
    output
        .len()
        .checked_add(bytes.len())
        .filter(|&size| size <= max_bytes)
        .ok_or(QueryEncodingError::OutputLimit(max_bytes))?;
    output
        .try_reserve_exact(bytes.len())
        .map_err(|_| QueryEncodingError::Allocation)?;
    output.extend_from_slice(bytes);
    Ok(())
}
