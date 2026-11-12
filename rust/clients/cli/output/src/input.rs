// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded data-file decoding for explicitly schema-validated input adapters.
//!
//! JSON is a record array. CSV has a header and JSON-valued cells: `null` is
//! null, `""` is an empty string, and strings include their JSON quotes inside
//! normal CSV quoting. No type/null guessing or inverse of lossy CSV output is
//! implied. Parquet admits only primitive, unannotated Arrow columns. Native
//! object frontdoors retain their separate explicit media and byte bounds.

use arrow_array::{
    Array, BooleanArray, Float32Array, Float64Array, Int32Array, Int64Array, StringArray,
    UInt32Array, UInt64Array,
};
use arrow_schema::DataType;
use parquet::{
    arrow::arrow_reader::ParquetRecordBatchReaderBuilder,
    basic::{ConvertedType, LogicalType},
};
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use std::{collections::HashSet, fmt, num::NonZeroUsize};

/// Decodes exactly one bounded UTF-8 JSON object, rejecting duplicate keys at
/// every depth, including escape-equivalent keys.
///
/// The returned JSON is only decoded input, **not** a schema-validated operation.
/// A released representation-specific SDK schema must reject unknown/read-only
/// fields and check body/path identity before approval or dispatch. Native bytes,
/// archives, URI downloads and arbitrary media are not accepted here.
///
/// # Errors
/// Returns a sanitized local classification without reflecting input content.
pub fn decode_json_input(
    bytes: Option<&[u8]>,
    media_type: &str,
    maximum_bytes: NonZeroUsize,
) -> Result<Value, InputError> {
    if media_type != "application/json" {
        return Err(InputError::UnsupportedMedia);
    }
    let bytes = bytes.ok_or(InputError::Missing)?;
    if bytes.len() > maximum_bytes.get() {
        return Err(InputError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| InputError::InvalidUtf8)?;
    if text.trim().is_empty() {
        return Err(InputError::Empty);
    }
    let value =
        strict_json_bounded(bytes, maximum_bytes.get()).map_err(|_| InputError::InvalidJson)?;
    if !value.is_object() {
        return Err(InputError::InvalidJson);
    }
    Ok(value)
}

/// Maximum encoded input and decoded JSON storage.
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_ROWS: usize = 10_000;
const MAX_COLUMNS: usize = 1024;
const MAX_CELLS: usize = 100_000;

/// Sanitized input errors never include file names, cell values or parser text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InputError {
    /// Omitted input differs from an empty supplied object document.
    #[error("An explicit input document is required.")]
    Missing,
    /// Supplied object input contains no JSON document.
    #[error("The supplied input document is empty.")]
    Empty,
    /// Raw object input exceeds the caller's explicit finite bound.
    #[error("The input document exceeds its explicit byte bound.")]
    TooLarge,
    /// The caller selected media not advertised by the object decoder.
    #[error("This input decoder requires application/json.")]
    UnsupportedMedia,
    /// Object input is not strict UTF-8.
    #[error("The input document must be strict UTF-8.")]
    InvalidUtf8,
    /// Object input syntax, duplicate keys, depth, trailing data or root is invalid.
    #[error("The input must be one JSON object without duplicate keys.")]
    InvalidJson,
    /// Input exceeds a finite byte, row, column or allocation bound.
    #[error("Input exceeds the bounded byte, row, column or cell limit.")]
    Limit,
    /// Syntax includes malformed JSON, duplicate keys or trailing content.
    #[error("Input must be strict UTF-8 JSON without duplicate keys or trailing content.")]
    Json,
    /// A record array with unique nonempty column names is required.
    #[error("Input must be a nonempty array of records with unique, nonempty column names.")]
    Records,
    /// CSV cells require explicit JSON scalars, including quoted strings/null.
    #[error(
        "CSV requires a unique header and JSON-valued cells; use null explicitly, not an empty cell. Strings require JSON quotes inside CSV quoting."
    )]
    Csv,
    /// Unsupported or invalid Parquet is not stringified or coerced.
    #[error(
        "Parquet must contain bounded primitive boolean, integer, finite float or UTF-8 columns; logical, nested, decimal, timestamp and binary conversions are not supported."
    )]
    Parquet,
}

/// Decode one JSON value, rejecting duplicate keys at every depth.
///
/// # Errors
/// Rejects oversized input, malformed UTF-8/JSON and duplicate keys. Based on
/// the existing ontology input visitor; no parser text is exposed to callers.
pub fn strict_json(bytes: &[u8]) -> Result<Value, InputError> {
    strict_json_bounded(bytes, MAX_INPUT_BYTES)
}

/// Decode strict JSON using the caller's explicit byte bound.
///
/// # Errors
/// Rejects input over the bound, malformed JSON and duplicate keys.
pub fn strict_json_bounded(bytes: &[u8], maximum_bytes: usize) -> Result<Value, InputError> {
    if bytes.len() > maximum_bytes {
        return Err(InputError::Limit);
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = StrictValue::deserialize(&mut decoder)
        .map_err(|_| InputError::Json)?
        .0;
    decoder.end().map_err(|_| InputError::Json)?;
    Ok(value)
}

/// Decode a JSON, CSV or Parquet record array without guessing model types.
///
/// The caller must validate every column/value against its exact selected
/// schema before inference. Native arbitrary-JSON requests are a separate mode.
///
/// # Errors
/// Rejects unsupported formats/types, ambiguous cells and all exceeded bounds.
pub fn records(
    bytes: &[u8],
    format: crate::tabular::TabularFormat,
) -> Result<Vec<Value>, InputError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(InputError::Limit);
    }
    let rows = match format {
        crate::tabular::TabularFormat::Json => {
            strict_json(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes))?
                .as_array()
                .cloned()
                .ok_or(InputError::Records)?
        }
        crate::tabular::TabularFormat::Csv => csv_records(bytes)?,
        crate::tabular::TabularFormat::Parquet => parquet_records(bytes)?,
    };
    check_rows(&rows)?;
    Ok(rows)
}

fn check_rows(rows: &[Value]) -> Result<(), InputError> {
    if rows.is_empty() {
        return Err(InputError::Records);
    }
    let mut cells = 0_usize;
    for row in rows {
        let row = row.as_object().ok_or(InputError::Records)?;
        check_names(row.keys().map(String::as_str))?;
        cells = cells.saturating_add(row.len());
    }
    if rows.len() > MAX_ROWS
        || cells > MAX_CELLS
        || serde_json::to_vec(rows)
            .map_err(|_| InputError::Records)?
            .len()
            > MAX_INPUT_BYTES
    {
        return Err(InputError::Limit);
    }
    Ok(())
}

fn check_names<'a>(names: impl Iterator<Item = &'a str>) -> Result<(), InputError> {
    let mut seen = HashSet::new();
    for name in names {
        if name.is_empty() || !seen.insert(name) {
            return Err(InputError::Records);
        }
        if seen.len() > MAX_COLUMNS {
            return Err(InputError::Limit);
        }
    }
    Ok(())
}

fn csv_records(bytes: &[u8]) -> Result<Vec<Value>, InputError> {
    strict_csv_quoting(bytes)?;
    let mut reader = csv::ReaderBuilder::new().flexible(false).from_reader(bytes);
    let header = reader.headers().map_err(|_| InputError::Csv)?.clone();
    check_names(header.iter()).map_err(|_| InputError::Csv)?;
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|_| InputError::Csv)?;
        if rows.len() >= MAX_ROWS || (rows.len() + 1).saturating_mul(header.len()) > MAX_CELLS {
            return Err(InputError::Limit);
        }
        let mut row = Map::new();
        for (name, cell) in header.iter().zip(record.iter()) {
            let value = strict_json(cell.as_bytes()).map_err(|_| InputError::Csv)?;
            if value.is_array() || value.is_object() {
                return Err(InputError::Csv);
            }
            row.insert(name.to_owned(), value);
        }
        rows.push(Value::Object(row));
    }
    Ok(rows)
}

fn strict_csv_quoting(bytes: &[u8]) -> Result<(), InputError> {
    // The shared CSV reader deliberately tolerates malformed quotes. Input
    // must not silently repair them before schema-checked inference.
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    enum State {
        Start,
        Bare,
        Quoted,
        Closed,
    }
    let mut state = State::Start;
    let mut row_started = false;
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        if matches!(state, State::Quoted) {
            if byte == b'"' {
                state = State::Closed;
            }
        } else if matches!(state, State::Closed) && byte == b'"' {
            state = State::Quoted;
        } else if matches!(byte, b'\r' | b'\n') {
            if !row_started {
                return Err(InputError::Csv);
            }
            row_started = false;
            state = State::Start;
            if byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                index += 1;
            }
        } else if byte == b',' {
            row_started = true;
            state = State::Start;
        } else {
            row_started = true;
            state = match (state, byte) {
                (State::Start, b'"') => State::Quoted,
                (State::Start | State::Bare, byte) if byte != b'"' => State::Bare,
                _ => return Err(InputError::Csv),
            };
        }
        index += 1;
    }
    if matches!(state, State::Quoted) {
        return Err(InputError::Csv);
    }
    Ok(())
}

fn parquet_records(bytes: &[u8]) -> Result<Vec<Value>, InputError> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::copy_from_slice(bytes))
        .map_err(|_| InputError::Parquet)?;
    let schema = builder.schema().clone();
    check_names(schema.fields().iter().map(|field| field.name().as_str()))?;
    if !schema.metadata().is_empty()
        || schema.fields().iter().any(|field| {
            !field.metadata().is_empty()
                || !matches!(
                    field.data_type(),
                    DataType::Boolean
                        | DataType::Int32
                        | DataType::Int64
                        | DataType::UInt32
                        | DataType::UInt64
                        | DataType::Float32
                        | DataType::Float64
                        | DataType::Utf8
                )
        })
    {
        return Err(InputError::Parquet);
    }
    let metadata = builder.metadata();
    if metadata
        .file_metadata()
        .schema_descr()
        .columns()
        .iter()
        .any(|column| {
            let logical = match column.logical_type_ref() {
                None | Some(LogicalType::String) => true,
                Some(LogicalType::Integer(integer)) => matches!(integer.bit_width, 32 | 64),
                _ => false,
            };
            !logical
                || !matches!(
                    column.converted_type(),
                    ConvertedType::NONE
                        | ConvertedType::UTF8
                        | ConvertedType::INT_32
                        | ConvertedType::INT_64
                        | ConvertedType::UINT_32
                        | ConvertedType::UINT_64
                )
        })
    {
        return Err(InputError::Parquet);
    }
    if metadata.file_metadata().num_rows() < 0
        || metadata.file_metadata().num_rows() > MAX_ROWS as i64
        || metadata
            .file_metadata()
            .num_rows()
            .saturating_mul(schema.fields().len() as i64)
            > MAX_CELLS as i64
        || metadata.row_groups().iter().any(|group| {
            group.total_byte_size() < 0 || group.total_byte_size() > 16 * MAX_INPUT_BYTES as i64
        })
    {
        return Err(InputError::Limit);
    }
    let reader = builder
        .with_batch_size(256)
        .build()
        .map_err(|_| InputError::Parquet)?;
    let mut rows = Vec::new();
    let mut decoded_bytes = 0_usize;
    for batch in reader {
        let batch = batch.map_err(|_| InputError::Parquet)?;
        for index in 0..batch.num_rows() {
            let mut row = Map::new();
            for (field, array) in schema.fields().iter().zip(batch.columns()) {
                let value = parquet_cell(array.as_ref(), index)?;
                decoded_bytes = decoded_bytes
                    .saturating_add(field.name().len())
                    .saturating_add(
                        serde_json::to_vec(&value)
                            .map_err(|_| InputError::Parquet)?
                            .len(),
                    );
                if decoded_bytes > MAX_INPUT_BYTES {
                    return Err(InputError::Limit);
                }
                row.insert(field.name().clone(), value);
            }
            rows.push(Value::Object(row));
        }
    }
    Ok(rows)
}

fn parquet_cell(array: &dyn Array, index: usize) -> Result<Value, InputError> {
    if array.is_null(index) {
        return Ok(Value::Null);
    }
    macro_rules! cell {
        ($array:ty) => {
            array
                .as_any()
                .downcast_ref::<$array>()
                .ok_or(InputError::Parquet)?
                .value(index)
        };
    }
    Ok(match array.data_type() {
        DataType::Boolean => Value::Bool(cell!(BooleanArray)),
        DataType::Int32 => Value::from(cell!(Int32Array)),
        DataType::Int64 => Value::from(cell!(Int64Array)),
        DataType::UInt32 => Value::from(cell!(UInt32Array)),
        DataType::UInt64 => Value::from(cell!(UInt64Array)),
        DataType::Float32 => Value::Number(
            Number::from_f64(f64::from(cell!(Float32Array))).ok_or(InputError::Parquet)?,
        ),
        DataType::Float64 => {
            Value::Number(Number::from_f64(cell!(Float64Array)).ok_or(InputError::Parquet)?)
        }
        DataType::Utf8 => Value::String(cell!(StringArray).to_owned()),
        _ => return Err(InputError::Parquet),
    })
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON without duplicate keys")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Number::from_f64(value)
            .map(|number| StrictValue(Value::Number(number)))
            .ok_or_else(|| E::custom("nonfinite JSON number"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.into())))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(StrictValue(Value::Array(values)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Self::Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom("duplicate JSON key"));
            }
            values.insert(key, object.next_value::<StrictValue>()?.0);
        }
        Ok(StrictValue(Value::Object(values)))
    }
}
