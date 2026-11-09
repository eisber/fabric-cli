// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded, pure data-file encoding for explicitly tabular command adapters.
//!
//! [`TabularTable`] validates one ordered schema for every format. JSON is an
//! array of row objects, without an execution envelope. CSV has a header, CRLF
//! records, RFC 4180 quoting, and no spreadsheet-formula rewriting. CSV cannot
//! distinguish null from an empty string and is **not** a typed/lossless format.
//! Neither terminal rendering nor column truncation belongs here.
//!
//! Signed/unsigned 64-bit integers, finite 64-bit floats, booleans, exact UTF-8
//! strings and nulls are supported. Integer/float mixtures are rejected rather
//! than rounded. Inference may widen nonnegative signed integers to unsigned
//! integers, but never changes their value. Native decimals, binary, dates,
//! timestamps and other unlisted types are unsupported; adapters must reject
//! them, not convert them to a supported type behind the caller's back.
//!
//! Structured object/array cells infer [`TabularType::Json`], not strings. Explicit
//! JSON columns can additionally contain scalar JSON values. JSON output retains
//! the actual value; CSV and Parquet store compact JSON text.
//! Parquet uses UTF-8 fields with [`JSON_LOGICAL_TYPE_KEY`] = `"json"` in the
//! embedded Arrow field metadata, and the same key in file metadata contains an
//! ordered JSON array of those column names. It is not a native Parquet nested
//! struct/list type or a claim that arbitrary native objects are supported.
//!
//! [`TabularTable::from_rows`] is a convenience for **declared tabular adapters**,
//! not a classifier for arbitrary command JSON. Object keys are unioned in
//! first-seen insertion order, including later-only keys, without flattening.
//! Missing keys infer nullable columns. All-null columns infer [`TabularType::Null`].
//! [`TabularTable::from_rows_with_json_columns`] explicitly opts named columns
//! into logical JSON, allowing heterogeneous JSON values without relaxing
//! inference for any other column. A present JSON literal null becomes
//! [`TabularValue::Json`]`(Value::Null)`, whereas a missing key becomes
//! [`TabularValue::Null`]. The former is a nonnull JSON cell (text `null` in CSV
//! and Parquet); the latter is an absent/SQL-null cell. JSON data files express
//! both as `null`, so that distinction cannot be recovered from JSON alone.
//! Empty/zero-column tables encode as JSON objects/arrays or empty CSV bytes;
//! Parquet rejects a zero-column schema. An explicit nonempty schema with zero
//! rows works in all three formats.
//!
//! Limits bound rows, columns, cells, modeled input allocation, individual
//! values, JSON nesting, and encoded output. They do not account for allocator
//! overhead or impose a process-wide memory quota. Input `Value`s already
//! converted from non-finite floats or oversized integer tokens cannot recover
//! lost information; raw adapters should use [`crate::parse_data`] and
//! [`crate::finite_number`] before constructing a table.

mod codecs;

use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use thiserror::Error;

/// Metadata key marking compact JSON cells in Parquet (see the module contract).
pub const JSON_LOGICAL_TYPE_KEY: &str = "fabric.tabular.logical_type";

/// Supported data-file formats; these do not select terminal rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TabularFormat {
    /// RFC 4180 CSV with a header and CRLF records.
    Csv,
    /// A compact JSON array of ordered row objects.
    Json,
    /// A finalized, uncompressed Apache Parquet file.
    Parquet,
}

impl TabularFormat {
    /// Parse an exact lowercase format name; no extension guessing is performed.
    pub fn parse(name: &str) -> Result<Self, TabularError> {
        match name {
            "csv" => Ok(Self::Csv),
            "json" => Ok(Self::Json),
            "parquet" => Ok(Self::Parquet),
            _ => Err(TabularError::UnsupportedFormat(name.to_owned())),
        }
    }

    /// Return the stable lowercase name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
            Self::Parquet => "parquet",
        }
    }
}

impl FromStr for TabularFormat {
    type Err = TabularError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// Exact logical cell types; serialized names match [`Self::name`].
///
/// Text is never heuristically converted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TabularType {
    /// A column containing only nulls (necessarily nullable).
    Null,
    /// A native boolean.
    Boolean,
    /// A signed 64-bit integer.
    Int64,
    /// An unsigned 64-bit integer, including values larger than `i64::MAX`.
    UInt64,
    /// A finite 64-bit floating-point number.
    Float64,
    /// An exact UTF-8 string.
    String,
    /// A JSON value, retained as JSON rather than flattened/stringified.
    ///
    /// Inference selects this type only for objects/arrays. Explicit declaration
    /// also permits scalars, including JSON literal null.
    Json,
}

impl TabularType {
    /// Parse an exact logical type name, explicitly rejecting unsupported types.
    pub fn parse(name: &str) -> Result<Self, TabularError> {
        match name {
            "null" => Ok(Self::Null),
            "boolean" => Ok(Self::Boolean),
            "int64" => Ok(Self::Int64),
            "uint64" => Ok(Self::UInt64),
            "float64" => Ok(Self::Float64),
            "string" => Ok(Self::String),
            "json" => Ok(Self::Json),
            _ => Err(TabularError::UnsupportedType(name.to_owned())),
        }
    }

    /// Return the stable logical type name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Boolean => "boolean",
            Self::Int64 => "int64",
            Self::UInt64 => "uint64",
            Self::Float64 => "float64",
            Self::String => "string",
            Self::Json => "json",
        }
    }
}

/// A named, ordered column; a table constructor validates schema-wide invariants.
///
/// Serializes as `{"name": "...", "type": "int64", "nullable": false}` for
/// schema receipts. Deserialization is deliberately not exposed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TabularColumn {
    name: String,
    #[serde(rename = "type")]
    data_type: TabularType,
    nullable: bool,
}

impl TabularColumn {
    /// Declare a column without altering its name or guessing its type.
    pub fn new(name: impl Into<String>, data_type: TabularType, nullable: bool) -> Self {
        Self {
            name: name.into(),
            data_type,
            nullable,
        }
    }

    /// Borrow the exact column name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the declared logical type.
    pub const fn data_type(&self) -> TabularType {
        self.data_type
    }

    /// Whether a cell may be null.
    pub const fn nullable(&self) -> bool {
        self.nullable
    }
}

/// Explicit cell values; table construction rejects type and nullability errors.
#[derive(Clone, Debug, PartialEq)]
pub enum TabularValue {
    /// An absent value, not an empty string or a JSON text sentinel.
    Null,
    /// A boolean.
    Boolean(bool),
    /// A signed integer.
    Int64(i64),
    /// An unsigned integer.
    UInt64(u64),
    /// A float, validated as finite by table construction.
    Float64(f64),
    /// Exact text, without numeric/date/formula interpretation.
    String(String),
    /// An actual JSON value, including explicitly declared scalars.
    ///
    /// `Json(Value::Null)` is a nonnull JSON literal, unlike [`Self::Null`].
    Json(Value),
}

impl TabularValue {
    fn data_type(&self) -> TabularType {
        match self {
            Self::Null => TabularType::Null,
            Self::Boolean(_) => TabularType::Boolean,
            Self::Int64(_) => TabularType::Int64,
            Self::UInt64(_) => TabularType::UInt64,
            Self::Float64(_) => TabularType::Float64,
            Self::String(_) => TabularType::String,
            Self::Json(_) => TabularType::Json,
        }
    }
}

/// Resource ceilings. Custom limits may only lower the documented defaults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TabularLimits {
    /// Maximum rows (default 100,000).
    pub max_rows: usize,
    /// Maximum columns (default 1,024).
    pub max_columns: usize,
    /// Maximum rectangular cells, including nulls (default 1,000,000).
    pub max_cells: usize,
    /// Maximum modeled input storage, including JSON nodes (default 64 MiB).
    pub max_input_bytes: usize,
    /// Maximum encoded file size, including footer/header (default 64 MiB).
    pub max_output_bytes: usize,
    /// Maximum text/name or compact JSON cell size (default 1 MiB).
    pub max_cell_bytes: usize,
    /// Maximum structured JSON depth, with its root at depth one (default 64).
    pub max_json_depth: usize,
}

impl Default for TabularLimits {
    fn default() -> Self {
        Self {
            max_rows: 100_000,
            max_columns: 1_024,
            max_cells: 1_000_000,
            max_input_bytes: 64 * 1024 * 1024,
            max_output_bytes: 64 * 1024 * 1024,
            max_cell_bytes: 1024 * 1024,
            max_json_depth: 64,
        }
    }
}

impl TabularLimits {
    fn validate(self) -> Result<(), TabularError> {
        let hard = Self::default();
        for (name, value, max) in [
            ("rows", self.max_rows, hard.max_rows),
            ("columns", self.max_columns, hard.max_columns),
            ("cells", self.max_cells, hard.max_cells),
            ("input bytes", self.max_input_bytes, hard.max_input_bytes),
            ("output bytes", self.max_output_bytes, hard.max_output_bytes),
            ("cell bytes", self.max_cell_bytes, hard.max_cell_bytes),
            ("JSON depth", self.max_json_depth, hard.max_json_depth),
        ] {
            if value > max {
                return Err(TabularError::InvalidLimit { limit: name, max });
            }
        }
        Ok(())
    }
}

/// Structured validation/encoding failures; errors are never encoded as data.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum TabularError {
    /// The caller selected an unsupported file format.
    #[error("unsupported tabular format: {0}")]
    UnsupportedFormat(String),
    /// The source type has no supported lossless native representation.
    #[error("unsupported tabular type: {0}")]
    UnsupportedType(String),
    /// An inferred row was not an object.
    #[error("tabular row {row} must be an object")]
    NonObjectRow {
        /// Zero-based row index.
        row: usize,
    },
    /// Two explicit schema columns or JSON-column declarations shared a name.
    #[error("duplicate tabular column: {0}")]
    DuplicateColumn(String),
    /// An explicit JSON-column declaration did not match any source key.
    #[error("declared JSON column is absent from every row: {0}")]
    UnknownJsonColumn(String),
    /// A null-only column was incorrectly declared nonnullable.
    #[error("null-only column must be nullable: {0}")]
    NonNullableNullType(String),
    /// Explicit row width did not match the schema.
    #[error("tabular row {row} has width {actual}; expected {expected}")]
    RowWidth {
        /// Zero-based row index.
        row: usize,
        /// Declared column count.
        expected: usize,
        /// Supplied cell count.
        actual: usize,
    },
    /// A null violated the explicit schema.
    #[error("null in nonnullable tabular column {column} at row {row}")]
    NullNotAllowed {
        /// Zero-based row index.
        row: usize,
        /// Column name.
        column: String,
    },
    /// A value conflicted with another value or the declared schema.
    #[error(
        "tabular type conflict in {column} at row {row}: expected {expected:?}, got {actual:?}"
    )]
    TypeConflict {
        /// Zero-based row index.
        row: usize,
        /// Column name.
        column: String,
        /// Declared/inferred type.
        expected: TabularType,
        /// Encountered type.
        actual: TabularType,
    },
    /// Non-finite floats have no supported representation.
    #[error("non-finite float in tabular column {column} at row {row}")]
    NonFinite {
        /// Zero-based row index.
        row: usize,
        /// Column name.
        column: String,
    },
    /// A format cannot represent zero-column tables.
    #[error("Parquet requires at least one declared column")]
    EmptyParquetSchema,
    /// A lowerable limit was set above the hard ceiling.
    #[error("tabular limit {limit} cannot exceed {max}")]
    InvalidLimit {
        /// Limit name.
        limit: &'static str,
        /// Hard ceiling.
        max: usize,
    },
    /// Input or output exceeded a resource bound.
    #[error("tabular {limit} limit exceeded ({max})")]
    LimitExceeded {
        /// Limit name.
        limit: &'static str,
        /// Configured ceiling.
        max: usize,
    },
    /// A fallible allocation failed.
    #[error("tabular allocation failed")]
    AllocationFailed,
    /// A real encoder failed; no success-looking fallback is generated.
    #[error("{format:?} tabular encoding failed: {message}")]
    Encoding {
        /// Selected file format.
        format: TabularFormat,
        /// Encoder diagnostic.
        message: String,
    },
}

/// Immutable, validated rectangular table shared by all three encoders.
#[derive(Clone, Debug, PartialEq)]
pub struct TabularTable {
    columns: Vec<TabularColumn>,
    rows: Vec<Vec<TabularValue>>,
    limits: TabularLimits,
}

impl TabularTable {
    /// Validate an explicit ordered schema and typed rows using default limits.
    ///
    /// Width, duplicate names, exact cell types, finite floats and nullability
    /// are checked, including for zero-row tables. No implicit coercion occurs.
    pub fn new(
        columns: Vec<TabularColumn>,
        rows: Vec<Vec<TabularValue>>,
    ) -> Result<Self, TabularError> {
        Self::with_limits(columns, rows, TabularLimits::default())
    }

    /// Construct an explicit table with resource ceilings no larger than defaults.
    pub fn with_limits(
        columns: Vec<TabularColumn>,
        rows: Vec<Vec<TabularValue>>,
        limits: TabularLimits,
    ) -> Result<Self, TabularError> {
        limits.validate()?;
        check_shape(columns.len(), rows.len(), limits)?;
        let mut budget = Budget::new(limits);
        budget.add(rows.len().saturating_mul(size_of::<Vec<TabularValue>>()))?;
        let mut names = HashSet::new();
        for column in &columns {
            if !names.insert(column.name.as_str()) {
                return Err(TabularError::DuplicateColumn(column.name.clone()));
            }
            if column.data_type == TabularType::Null && !column.nullable {
                return Err(TabularError::NonNullableNullType(column.name.clone()));
            }
            budget.text(&column.name)?;
            budget.add(size_of::<TabularColumn>())?;
        }
        for (index, row) in rows.iter().enumerate() {
            if row.len() != columns.len() {
                return Err(TabularError::RowWidth {
                    row: index,
                    expected: columns.len(),
                    actual: row.len(),
                });
            }
            for (column, value) in columns.iter().zip(row) {
                validate_cell(column, value, index, &mut budget)?;
            }
        }
        Ok(Self {
            columns,
            rows,
            limits,
        })
    }

    /// Infer one lossless schema from object rows of an explicitly tabular adapter.
    ///
    /// Unions names in first-seen insertion order. Missing/null cells infer
    /// nullability. Rejects non-object rows and incompatible cell types. Positive
    /// integers beyond `i64::MAX` widen an otherwise nonnegative integer column
    /// to `UInt64`; negative/large-unsigned or integer/float mixtures fail.
    pub fn from_rows(rows: &[Value]) -> Result<Self, TabularError> {
        Self::from_rows_with_limits(rows, TabularLimits::default())
    }

    /// Infer a table with resource ceilings no larger than defaults.
    pub fn from_rows_with_limits(
        rows: &[Value],
        limits: TabularLimits,
    ) -> Result<Self, TabularError> {
        Self::from_rows_with_json_columns_and_limits(rows, &[], limits)
    }

    /// Infer object rows while explicitly treating selected columns as JSON cells.
    ///
    /// Present values in those columns retain any actual JSON type, including
    /// scalar strings, numbers, booleans and literal null. Missing keys remain
    /// absent/SQL nulls and infer nullability. Other columns retain strict
    /// inference. Order remains first-seen, not declaration order. Duplicate
    /// declarations or names absent from every row are errors; use [`Self::new`]
    /// for a known schema with no rows.
    pub fn from_rows_with_json_columns(
        rows: &[Value],
        json_columns: &[&str],
    ) -> Result<Self, TabularError> {
        Self::from_rows_with_json_columns_and_limits(rows, json_columns, TabularLimits::default())
    }

    /// Infer explicitly selected JSON columns using lowerable resource ceilings.
    ///
    /// Charges schema, row-vector, every dense cell (including synthesized
    /// nulls), and retained payload storage before cloning any dense row.
    pub fn from_rows_with_json_columns_and_limits(
        rows: &[Value],
        json_columns: &[&str],
        limits: TabularLimits,
    ) -> Result<Self, TabularError> {
        limits.validate()?;
        check_bound(rows.len(), limits.max_rows, "rows")?;
        check_bound(json_columns.len(), limits.max_columns, "columns")?;
        let mut budget = Budget::new(limits);
        budget.add(rows.len().saturating_mul(size_of::<Vec<TabularValue>>()))?;
        let mut explicit_json = HashSet::new();
        for &name in json_columns {
            check_bound(name.len(), limits.max_cell_bytes, "cell bytes")?;
            if !explicit_json.insert(name) {
                return Err(TabularError::DuplicateColumn(name.to_owned()));
            }
        }
        let mut columns: Vec<InferredColumn> = Vec::new();
        let mut indices = HashMap::new();
        for (row_index, row) in rows.iter().enumerate() {
            let object = row
                .as_object()
                .ok_or(TabularError::NonObjectRow { row: row_index })?;
            for (name, value) in object {
                let index = if let Some(index) = indices.get(name.as_str()) {
                    *index
                } else {
                    check_shape(columns.len() + 1, rows.len(), limits)?;
                    budget.add(size_of::<TabularColumn>())?;
                    budget.add(rows.len().saturating_mul(size_of::<TabularValue>()))?;
                    budget.text(name)?;
                    indices.insert(name.as_str(), columns.len());
                    let is_explicit_json = explicit_json.contains(name.as_str());
                    columns.push(InferredColumn {
                        column: TabularColumn::new(
                            name,
                            if is_explicit_json {
                                TabularType::Json
                            } else {
                                TabularType::Null
                            },
                            false,
                        ),
                        seen: 0,
                        negative: false,
                        explicit_json: is_explicit_json,
                    });
                    columns.len() - 1
                };
                if columns[index].explicit_json || value.is_object() || value.is_array() {
                    budget.json(value, 1)?;
                    codecs::check_json_cell_size(value, limits.max_cell_bytes)?;
                } else if let Value::String(value) = value {
                    budget.text(value)?;
                }
                columns[index].observe(value, row_index)?;
            }
        }
        for &name in json_columns {
            if !indices.contains_key(name) {
                return Err(TabularError::UnknownJsonColumn(name.to_owned()));
            }
        }
        let columns: Vec<_> = columns
            .into_iter()
            .map(|mut inferred| {
                inferred.column.nullable |= inferred.seen < rows.len();
                inferred.column
            })
            .collect();
        // Validate the entire source before cloning structured values or creating
        // the rectangular representation, including later-only nullable columns.
        let typed_rows = rows
            .iter()
            .map(|row| {
                columns
                    .iter()
                    .map(|column| {
                        cell_from_json(
                            row.get(column.name()),
                            column.data_type,
                            explicit_json.contains(column.name()),
                        )
                    })
                    .collect()
            })
            .collect();
        Self::with_limits(columns, typed_rows, limits)
    }

    /// Borrow columns in encoding order.
    pub fn columns(&self) -> &[TabularColumn] {
        &self.columns
    }

    /// Return the exact validated row count, including zero-column rows.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Borrow validated rows in source order.
    pub fn rows(&self) -> &[Vec<TabularValue>] {
        &self.rows
    }

    /// Return the table's validated resource ceilings.
    pub const fn limits(&self) -> TabularLimits {
        self.limits
    }
}

/// Encode a validated table into a complete data file without any external I/O.
///
/// The returned file is never terminal-formatted or truncated. Errors propagate
/// instead of producing fallback text. Parquet is closed/finalized before return.
pub fn encode(table: &TabularTable, format: TabularFormat) -> Result<Vec<u8>, TabularError> {
    codecs::encode(table, format)
}

fn check_bound(value: usize, max: usize, limit: &'static str) -> Result<(), TabularError> {
    if value > max {
        return Err(TabularError::LimitExceeded { limit, max });
    }
    Ok(())
}

fn check_shape(columns: usize, rows: usize, limits: TabularLimits) -> Result<(), TabularError> {
    check_bound(columns, limits.max_columns, "columns")?;
    check_bound(rows, limits.max_rows, "rows")?;
    check_bound(rows.saturating_mul(columns), limits.max_cells, "cells")
}

struct Budget {
    used: usize,
    limits: TabularLimits,
}

impl Budget {
    fn new(limits: TabularLimits) -> Self {
        Self { used: 0, limits }
    }

    fn add(&mut self, amount: usize) -> Result<(), TabularError> {
        self.used = self.used.saturating_add(amount);
        check_bound(self.used, self.limits.max_input_bytes, "input bytes")
    }

    fn text(&mut self, value: &str) -> Result<(), TabularError> {
        check_bound(value.len(), self.limits.max_cell_bytes, "cell bytes")?;
        self.add(value.len())
    }

    fn json(&mut self, value: &Value, depth: usize) -> Result<(), TabularError> {
        check_bound(depth, self.limits.max_json_depth, "JSON depth")?;
        self.add(size_of::<Value>())?;
        match value {
            Value::String(value) => self.text(value)?,
            Value::Array(values) => {
                for value in values {
                    self.json(value, depth + 1)?;
                }
            }
            Value::Object(values) => {
                for (name, value) in values {
                    self.text(name)?;
                    self.json(value, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn validate_cell(
    column: &TabularColumn,
    value: &TabularValue,
    row: usize,
    budget: &mut Budget,
) -> Result<(), TabularError> {
    budget.add(size_of::<TabularValue>())?;
    if matches!(value, TabularValue::Null) {
        if !column.nullable {
            return Err(TabularError::NullNotAllowed {
                row,
                column: column.name.clone(),
            });
        }
        return Ok(());
    }
    if value.data_type() != column.data_type {
        return Err(TabularError::TypeConflict {
            row,
            column: column.name.clone(),
            expected: column.data_type,
            actual: value.data_type(),
        });
    }
    match value {
        TabularValue::Float64(value) if !value.is_finite() => {
            return Err(TabularError::NonFinite {
                row,
                column: column.name.clone(),
            });
        }
        TabularValue::String(value) => budget.text(value)?,
        TabularValue::Json(value) => {
            budget.json(value, 1)?;
            codecs::check_json_cell_size(value, budget.limits.max_cell_bytes)?;
        }
        _ => {}
    }
    Ok(())
}

struct InferredColumn {
    column: TabularColumn,
    seen: usize,
    negative: bool,
    explicit_json: bool,
}

impl InferredColumn {
    fn observe(&mut self, value: &Value, row: usize) -> Result<(), TabularError> {
        self.seen += 1;
        if self.explicit_json {
            return Ok(());
        }
        let actual = json_type(value);
        if actual == TabularType::Null {
            self.column.nullable = true;
            return Ok(());
        }
        let expected = self.column.data_type;
        if expected == TabularType::Null {
            self.column.data_type = actual;
        } else if expected == TabularType::Int64 && actual == TabularType::UInt64 && !self.negative
        {
            self.column.data_type = TabularType::UInt64;
        } else if expected != actual
            && !(expected == TabularType::UInt64
                && actual == TabularType::Int64
                && value.as_u64().is_some())
        {
            return Err(TabularError::TypeConflict {
                row,
                column: self.column.name.clone(),
                expected,
                actual,
            });
        }
        self.negative |= value.as_i64().is_some_and(|number| number < 0);
        Ok(())
    }
}

fn json_type(value: &Value) -> TabularType {
    match value {
        Value::Null => TabularType::Null,
        Value::Bool(_) => TabularType::Boolean,
        Value::Number(number) if number.is_i64() => TabularType::Int64,
        Value::Number(number) if number.is_u64() => TabularType::UInt64,
        Value::Number(_) => TabularType::Float64,
        Value::String(_) => TabularType::String,
        Value::Array(_) | Value::Object(_) => TabularType::Json,
    }
}

fn cell_from_json(
    value: Option<&Value>,
    data_type: TabularType,
    explicit_json: bool,
) -> TabularValue {
    let Some(value) = value else {
        return TabularValue::Null;
    };
    if explicit_json {
        return TabularValue::Json(value.clone());
    }
    match value {
        Value::Null => TabularValue::Null,
        Value::Bool(value) => TabularValue::Boolean(*value),
        Value::Number(value) => {
            // Inference above has already proved these conversions lossless.
            if data_type == TabularType::UInt64 {
                TabularValue::UInt64(value.as_u64().expect("validated uint64"))
            } else if data_type == TabularType::Int64 {
                TabularValue::Int64(value.as_i64().expect("validated int64"))
            } else {
                TabularValue::Float64(value.as_f64().expect("validated float64"))
            }
        }
        Value::String(value) => TabularValue::String(value.clone()),
        Value::Array(_) | Value::Object(_) => TabularValue::Json(value.clone()),
    }
}
