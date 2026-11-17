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
