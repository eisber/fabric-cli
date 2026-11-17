// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use fabric_sdk_tabular::TabularError;

/// The explicit conversion budget that was exceeded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConversionLimit {
    /// Number of source datasets.
    Datasets,
    /// Total rows across all datasets.
    Rows,
    /// Columns in any one dataset.
    Columns,
    /// Input payload or retained Arrow bytes.
    Bytes,
    /// Total Arrow record batches.
    Batches,
    /// Nested JSON value depth, including retained non-row metadata.
    ValueDepth,
}

/// Conversion failure without source-sensitive cell values in its display text.
#[derive(Debug, thiserror::Error)]
pub enum ConversionError {
    /// Budgets must be finite and allow at least one dataset, column, and batch.
    #[error("invalid conversion limits")]
    InvalidLimits,
    /// Input execution evidence is not clean, complete, and untruncated.
    #[error("native query result is not clean and complete")]
    UncleanResult,
    /// Dataset ordinals or identifiers are inconsistent.
    #[error("invalid native dataset identity")]
    DatasetIdentity,
    /// An explicit finite budget was exceeded.
    #[error("conversion limit exceeded: {0:?}")]
    Limit(ConversionLimit),
    /// The caller cancelled conversion.
    #[error("conversion cancelled")]
    Cancelled,
    /// The caller's absolute conversion deadline expired.
    #[error("conversion deadline expired")]
    Deadline,
    /// Standard JSON encoding of a logical value failed.
    #[error("JSON logical value encoding failed")]
    Json(#[source] serde_json::Error),
    /// Logical text encoding did not produce valid UTF-8.
    #[error("invalid logical UTF-8 encoding")]
    LogicalEncoding,
    /// A declared native schema has no exact supported representation.
    #[error("unsupported native type at dataset {dataset}, column {column}: {reason}")]
    UnsupportedType {
        /// Positional source dataset index.
        dataset: usize,
        /// Positional source column index.
        column: usize,
        /// Static, value-free explanation.
        reason: &'static str,
    },
    /// A positional source row disagrees with its schema.
    #[error("row arity mismatch at dataset {dataset}, row {row}")]
    RowArity {
        /// Positional source dataset index.
        dataset: usize,
        /// Positional source row index.
        row: usize,
    },
    /// A value does not exactly fit its declared native type.
    #[error("invalid typed value at dataset {dataset}, column {column}, row {row}: {reason}")]
    Value {
        /// Positional source dataset index.
        dataset: usize,
        /// Positional source column index.
        column: usize,
        /// Positional source row index.
        row: usize,
        /// Static, value-free explanation.
        reason: &'static str,
    },
    /// Arrow rejected a constructed schema or batch.
    #[error("Arrow rejected the converted table")]
    Arrow(#[source] arrow_schema::ArrowError),
    /// The shared model rejected the converted table.
    #[error("shared tabular validation rejected the converted table")]
    Tabular(#[from] TabularError),
}
