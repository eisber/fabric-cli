// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::collections::HashSet;
use std::io::{self, Write};

use arrow_array::{
    Array, BinaryArray, BooleanArray, Decimal128Array, Float32Array, Float64Array, Int8Array,
    Int16Array, Int32Array, Int64Array, LargeBinaryArray, LargeStringArray, StringArray,
    TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
    TimestampSecondArray, UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow_schema::{DataType, TimeUnit};
use base64::display::Base64Display;
use base64::engine::general_purpose::STANDARD;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

use crate::error::limit;
use crate::{LimitKind, OutputLimits, TabularError, TabularResult};

/// Supported portable result formats. Excel is intentionally not implemented.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultFormat {
    /// UTF-8 RFC 4180-style CSV with CRLF, quoted non-null cells, and bare nulls.
    Csv,
    /// Ordered schema and positional rows; exact numeric/timestamp encodings.
    Json,
    /// Real uncompressed Parquet with embedded Arrow schema.
    Parquet,
}

/// Encodes one complete dataset without returning a silently clipped prefix.
///
/// JSON has `{"schema":[{"name","type","nullable","encoding"}],"rows":[[...]]}`.
/// Integer and decimal cells are exact strings; timestamps are signed tick
/// strings with their unit/timezone retained in the Arrow type description.
/// Binary uses standard padded base64; finite floats and booleans are JSON values.
///
/// CSV quotes every non-null cell, including empty strings, and represents null
/// as a bare empty field. Generic CSV readers may conflate these; a schema-aware
/// reader must preserve quotedness. CSV carries names but not a full type schema.
///
/// Parquet rejects duplicate names, zero-column schemas, and timestamps in seconds
/// (which require conversion to Parquet's millisecond unit). Other supported
/// types retain Arrow type metadata, values, nulls, and row/column order.
pub fn encode(
    result: &TabularResult,
    format: ResultFormat,
    limits: OutputLimits,
) -> Result<Vec<u8>, TabularError> {
    result.validate(limits)?;
    validate_output_schema(result, format)?;
    let mut output = BoundedBuffer::new(limits.max_bytes());
    let encoded = match format {
        ResultFormat::Csv => write_csv(result, &mut output),
        ResultFormat::Json => write_json(result, &mut output),
        ResultFormat::Parquet => write_parquet(result, &mut output),
    };
    if output.exceeded {
        return Err(limit(LimitKind::Bytes, limits.max_bytes()));
    }
    if output.allocation_failed {
        return Err(TabularError::Allocation);
    }
    encoded?;
    Ok(output.bytes)
}

/// Encodes completely before writing to a caller-owned sink, then flushes it.
///
/// Encoding/limit errors write no bytes. An I/O error can leave a prefix in the
/// sink, and flush errors also fail: callers own no-clobber/atomic-file handling,
/// terminal safety, cleanup, and ensuring receipts do not contaminate data bytes.
pub fn encode_to_writer<W: Write>(
    result: &TabularResult,
    format: ResultFormat,
    limits: OutputLimits,
    writer: &mut W,
) -> Result<(), TabularError> {
    let bytes = encode(result, format, limits)?;
    writer.write_all(&bytes).map_err(TabularError::Output)?;
    writer.flush().map_err(TabularError::Output)
}

fn validate_output_schema(
    result: &TabularResult,
    format: ResultFormat,
) -> Result<(), TabularError> {
    if format != ResultFormat::Json && result.schema().fields().is_empty() {
        return Err(TabularError::UnsupportedOutputSchema {
            reason: "CSV and Parquet require at least one column",
        });
    }
    if format == ResultFormat::Parquet {
        let mut names = HashSet::new();
        for field in result.schema().fields() {
            if !names.insert(field.name()) {
                return Err(TabularError::UnsupportedOutputSchema {
                    reason: "Parquet output requires unique column names",
                });
            }
            if matches!(field.data_type(), DataType::Timestamp(TimeUnit::Second, _)) {
                return Err(TabularError::UnsupportedOutputSchema {
                    reason: "Parquet cannot preserve second-unit timestamps without conversion",
                });
            }
            if matches!(field.data_type(), DataType::Decimal128(_, scale) if *scale < 0) {
                return Err(TabularError::UnsupportedOutputSchema {
                    reason: "Parquet requires a nonnegative decimal scale",
                });
            }
        }
    }
    Ok(())
}

fn write_csv(result: &TabularResult, output: &mut BoundedBuffer) -> Result<(), TabularError> {
    for (column, field) in result.schema().fields().iter().enumerate() {
        if column != 0 {
            output.write_all(b",").map_err(encoding)?;
        }
        quoted_csv(output, field.name()).map_err(encoding)?;
    }
    output.write_all(b"\r\n").map_err(encoding)?;
    for batch in result.batches() {
        for row in 0..batch.num_rows() {
            for (column, array) in batch.columns().iter().enumerate() {
                if column != 0 {
                    output.write_all(b",").map_err(encoding)?;
                }
                if array.is_null(row) {
                    continue;
                }
                match cell(array.as_ref(), column, row)? {
                    Cell::Text(value) => quoted_csv(output, value).map_err(encoding)?,
                    value => {
                        output.write_all(b"\"").map_err(encoding)?;
                        write_plain_cell(output, &value).map_err(encoding)?;
                        output.write_all(b"\"").map_err(encoding)?;
                    }
                }
            }
            output.write_all(b"\r\n").map_err(encoding)?;
        }
    }
    Ok(())
}

fn quoted_csv(output: &mut BoundedBuffer, value: &str) -> io::Result<()> {
    output.write_all(b"\"")?;
    let mut start = 0;
    for (index, byte) in value.bytes().enumerate() {
        if byte == b'"' {
            output.write_all(&value.as_bytes()[start..index])?;
            output.write_all(b"\"\"")?;
            start = index + 1;
        }
    }
    output.write_all(&value.as_bytes()[start..])?;
    output.write_all(b"\"")
}

fn write_json(result: &TabularResult, output: &mut BoundedBuffer) -> Result<(), TabularError> {
    output.write_all(b"{\"schema\":[").map_err(encoding)?;
    for (column, field) in result.schema().fields().iter().enumerate() {
        if column != 0 {
            output.write_all(b",").map_err(encoding)?;
        }
        let descriptor = serde_json::json!({
            "name": field.name(),
            "type": field.data_type().to_string(),
            "nullable": field.is_nullable(),
            "encoding": json_encoding(field.data_type()),
        });
        serde_json::to_writer(&mut *output, &descriptor).map_err(encoding)?;
    }
    output.write_all(b"],\"rows\":[").map_err(encoding)?;
    let mut first = true;
    for batch in result.batches() {
        for row in 0..batch.num_rows() {
            if !first {
                output.write_all(b",").map_err(encoding)?;
            }
            first = false;
            output.write_all(b"[").map_err(encoding)?;
            for (column, array) in batch.columns().iter().enumerate() {
                if column != 0 {
                    output.write_all(b",").map_err(encoding)?;
                }
                if array.is_null(row) {
                    output.write_all(b"null").map_err(encoding)?;
                    continue;
                }
                match cell(array.as_ref(), column, row)? {
                    Cell::Text(value) => {
                        serde_json::to_writer(&mut *output, value).map_err(encoding)?
                    }
                    Cell::Exact(value) => {
                        serde_json::to_writer(&mut *output, &value).map_err(encoding)?
                    }
                    Cell::Float32(value) => {
                        serde_json::to_writer(&mut *output, &value).map_err(encoding)?
                    }
                    Cell::Float64(value) => {
                        serde_json::to_writer(&mut *output, &value).map_err(encoding)?
                    }
                    value => {
                        let binary = matches!(value, Cell::Binary(_));
                        if binary {
                            output.write_all(b"\"").map_err(encoding)?;
                        }
                        write_plain_cell(output, &value).map_err(encoding)?;
                        if binary {
                            output.write_all(b"\"").map_err(encoding)?;
                        }
                    }
                }
            }
            output.write_all(b"]").map_err(encoding)?;
        }
    }
    output.write_all(b"]}\n").map_err(encoding)
}

fn json_encoding(data_type: &DataType) -> &'static str {
    match data_type {
        DataType::Boolean => "boolean",
        DataType::Float32 | DataType::Float64 => "number",
        DataType::Binary | DataType::LargeBinary => "base64",
        DataType::Timestamp(_, _) => "signed-ticks-string",
        DataType::Decimal128(_, _) => "decimal-string",
        DataType::Utf8 | DataType::LargeUtf8 => "string",
        _ => "integer-string",
    }
}

fn write_parquet(
    result: &TabularResult,
    output: &mut BoundedBuffer,
) -> Result<(), TabularError> {
    let properties = WriterProperties::builder()
        .set_compression(Compression::UNCOMPRESSED)
        .set_write_batch_size(1024)
        .set_max_row_group_row_count(Some(1024))
        .set_max_row_group_bytes(Some(output.maximum))
        .build();
    let mut writer =
        ArrowWriter::try_new(output, result.schema().clone(), Some(properties)).map_err(encoding)?;
    for batch in result.batches() {
        writer.write(batch).map_err(encoding)?;
        writer.flush().map_err(encoding)?;
    }
    writer.close().map_err(encoding)?;
    Ok(())
}

enum Cell<'a> {
    Text(&'a str),
    Exact(String),
    Boolean(bool),
    Float32(f32),
    Float64(f64),
    Binary(&'a [u8]),
}

fn cell(array: &dyn Array, column: usize, row: usize) -> Result<Cell<'_>, TabularError> {
    macro_rules! typed {
        ($array_type:ty) => {
            array
                .as_any()
                .downcast_ref::<$array_type>()
                .ok_or(TabularError::UnsupportedValue {
                    column,
                    row,
                    reason: "array implementation does not match the declared Arrow type",
                })?
                .value(row)
        };
    }
    macro_rules! exact {
        ($array_type:ty) => {
            Cell::Exact(typed!($array_type).to_string())
        };
    }
    Ok(match array.data_type() {
        DataType::Boolean => Cell::Boolean(typed!(BooleanArray)),
        DataType::Int8 => exact!(Int8Array),
        DataType::Int16 => exact!(Int16Array),
        DataType::Int32 => exact!(Int32Array),
        DataType::Int64 => exact!(Int64Array),
        DataType::UInt8 => exact!(UInt8Array),
        DataType::UInt16 => exact!(UInt16Array),
        DataType::UInt32 => exact!(UInt32Array),
        DataType::UInt64 => exact!(UInt64Array),
        DataType::Float32 => Cell::Float32(typed!(Float32Array)),
        DataType::Float64 => Cell::Float64(typed!(Float64Array)),
        DataType::Utf8 => Cell::Text(typed!(StringArray)),
        DataType::LargeUtf8 => Cell::Text(typed!(LargeStringArray)),
        DataType::Binary => Cell::Binary(typed!(BinaryArray)),
        DataType::LargeBinary => Cell::Binary(typed!(LargeBinaryArray)),
        DataType::Timestamp(TimeUnit::Second, _) => exact!(TimestampSecondArray),
        DataType::Timestamp(TimeUnit::Millisecond, _) => exact!(TimestampMillisecondArray),
        DataType::Timestamp(TimeUnit::Microsecond, _) => exact!(TimestampMicrosecondArray),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => exact!(TimestampNanosecondArray),
        DataType::Decimal128(_, scale) => {
            Cell::Exact(decimal(typed!(Decimal128Array), *scale))
        }
        data_type => {
            return Err(TabularError::UnsupportedType {
                column,
                data_type: data_type.to_string(),
            });
        }
    })
}

fn decimal(value: i128, scale: i8) -> String {
    let digits = value.unsigned_abs().to_string();
    let sign = if value < 0 { "-" } else { "" };
    if scale <= 0 {
        return format!("{sign}{digits}{}", "0".repeat(usize::from(scale.unsigned_abs())));
    }
    let scale = scale as usize;
    if digits.len() <= scale {
        format!("{sign}0.{}{digits}", "0".repeat(scale - digits.len()))
    } else {
        let split = digits.len() - scale;
        format!("{sign}{}.{}", &digits[..split], &digits[split..])
    }
}

fn write_plain_cell(output: &mut BoundedBuffer, cell: &Cell<'_>) -> io::Result<()> {
    match cell {
        Cell::Text(value) => output.write_all(value.as_bytes()),
        Cell::Exact(value) => output.write_all(value.as_bytes()),
        Cell::Boolean(value) => write!(output, "{value}"),
        Cell::Float32(value) => write!(output, "{value}"),
        Cell::Float64(value) => write!(output, "{value}"),
        Cell::Binary(value) => write!(output, "{}", Base64Display::new(value, &STANDARD)),
    }
}

fn encoding(error: impl std::fmt::Display) -> TabularError {
    TabularError::Encoding(error.to_string())
}

struct BoundedBuffer {
    bytes: Vec<u8>,
    maximum: usize,
    exceeded: bool,
    allocation_failed: bool,
}

impl BoundedBuffer {
    fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            exceeded: false,
            allocation_failed: false,
        }
    }
}

impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|length| length > self.maximum)
        {
            self.exceeded = true;
            return Err(io::Error::other("tabular output byte limit exceeded"));
        }
        if self.bytes.try_reserve_exact(bytes.len()).is_err() {
            self.allocation_failed = true;
            return Err(io::Error::other("tabular output allocation failed"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
