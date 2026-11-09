// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use thiserror::Error;

/// Resource whose finite budget was exceeded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitKind {
    /// Input bytes, retained array bytes, or encoded output bytes.
    Bytes,
    /// Total rows across all record batches.
    Rows,
    /// Total record batches, including empty batches.
    Batches,
}

/// Conservative classification of metadata without a verified service contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataClassification {
    /// Metadata is present, but its application semantics are not documented here.
    Unknown,
}

/// Location of unrecognized metadata; indexes are zero-based.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetadataLocation {
    /// Dataset schema metadata.
    Schema,
    /// Metadata on the indicated top-level field.
    Field(usize),
    /// Custom metadata on the indicated IPC message, including its schema message.
    Message(usize),
}

/// Uninterpreted metadata evidence, not proof of either application success or failure.
///
/// Values may contain service information. Consumers must apply their own
/// disclosure policy before presenting them; this crate does not log them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataEvidence {
    /// Location in the ordered dataset or stream.
    pub location: MetadataLocation,
    /// Metadata key, preserved without guessing its meaning.
    pub key: String,
    /// Metadata value; absent FlatBuffer values are represented as an empty string.
    pub value: String,
    /// Always unknown until an authoritative application metadata contract exists.
    pub classification: MetadataClassification,
}

/// A bounded result could not be decoded, validated, encoded, or delivered.
#[derive(Debug, Error)]
pub enum TabularError {
    /// Byte and batch budgets must be nonzero.
    #[error("byte and batch limits must be nonzero")]
    InvalidLimits,
    /// A finite budget was exceeded. No truncated success is returned.
    #[error("tabular {kind:?} limit exceeded (maximum {maximum})")]
    LimitExceeded {
        /// Resource that exceeded its budget.
        kind: LimitKind,
        /// Caller-supplied maximum.
        maximum: usize,
    },
    /// A schema or array type is outside the explicitly supported type matrix.
    #[error("unsupported tabular type in column {column}: {data_type}")]
    UnsupportedType {
        /// Zero-based column index.
        column: usize,
        /// Arrow type description.
        data_type: String,
    },
    /// A batch schema differs from the declared result schema.
    #[error("record batch {batch} does not match the dataset schema")]
    SchemaMismatch {
        /// Zero-based batch index.
        batch: usize,
    },
    /// A value would not have a faithful representation.
    #[error("unsupported value in column {column} at row {row}: {reason}")]
    UnsupportedValue {
        /// Zero-based column index.
        column: usize,
        /// Zero-based row within the entire dataset.
        row: usize,
        /// Description without including the cell's data.
        reason: &'static str,
    },
    /// Metadata requires an application-aware decision before output can succeed.
    #[error("unrecognized tabular metadata requires application interpretation")]
    Metadata {
        /// Preserved diagnostic evidence; no error/success semantics are inferred.
        evidence: Vec<MetadataEvidence>,
    },
    /// IPC framing, FlatBuffer metadata, or record-batch validation failed.
    #[error("invalid Arrow IPC stream: {reason}")]
    InvalidIpc {
        /// Structural reason, not a copy of the input.
        reason: &'static str,
    },
    /// A second schema/dataset was found instead of silently selecting the first.
    #[error("multiple Arrow datasets are not supported")]
    MultipleDatasets,
    /// This format cannot represent the dataset's schema faithfully.
    #[error("output schema is not supported: {reason}")]
    UnsupportedOutputSchema {
        /// Description of the unsupported shape.
        reason: &'static str,
    },
    /// The Arrow or Parquet encoder rejected an otherwise validated result.
    #[error("tabular encoding failed: {0}")]
    Encoding(String),
    /// Delivery to a caller-owned writer failed; a prefix may have been written.
    #[error("tabular output write failed")]
    Output(#[source] std::io::Error),
    /// A fallible output allocation failed.
    #[error("tabular output allocation failed")]
    Allocation,
}

pub(crate) fn limit(kind: LimitKind, maximum: usize) -> TabularError {
    TabularError::LimitExceeded { kind, maximum }
}
