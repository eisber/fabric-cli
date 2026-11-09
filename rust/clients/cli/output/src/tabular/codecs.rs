// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    JSON_LOGICAL_TYPE_KEY, TabularError, TabularFormat, TabularTable, TabularType, TabularValue,
};
use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, NullArray, RecordBatch, UInt64Array,
    builder::StringBuilder,
};
use arrow_schema::{DataType, Field, Schema};
use parquet::{
    arrow::ArrowWriter,
    errors::ParquetError,
    file::{metadata::KeyValue, properties::WriterProperties},
};
use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;

pub(super) fn encode(table: &TabularTable, format: TabularFormat) -> Result<Vec<u8>, TabularError> {
    let mut output = BoundedWriter::new(table.limits.max_output_bytes, "output bytes");
    match format {
        TabularFormat::Json => {
            let result = serde_json::to_writer(&mut output, &JsonTable(table));
            output.check(result, format)?;
        }
        TabularFormat::Csv => csv(table, &mut output)?,
        TabularFormat::Parquet => parquet(table, &mut output)?,
    }
    Ok(output.bytes)
}

struct JsonTable<'a>(&'a TabularTable);
struct JsonRow<'a> {
    table: &'a TabularTable,
    row: &'a [TabularValue],
}

impl Serialize for JsonTable<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut rows = serializer.serialize_seq(Some(self.0.rows.len()))?;
        for row in &self.0.rows {
            rows.serialize_element(&JsonRow { table: self.0, row })?;
        }
        rows.end()
    }
}

impl Serialize for JsonRow<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut object = serializer.serialize_map(Some(self.table.columns.len()))?;
        for (column, cell) in self.table.columns.iter().zip(self.row) {
            object.serialize_entry(&column.name, cell)?;
        }
        object.end()
    }
}

impl Serialize for TabularValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_none(),
            Self::Boolean(value) => serializer.serialize_bool(*value),
            Self::Int64(value) => serializer.serialize_i64(*value),
            Self::UInt64(value) => serializer.serialize_u64(*value),
            Self::Float64(value) => serializer.serialize_f64(*value),
            Self::String(value) => serializer.serialize_str(value),
            Self::Json(value) => value.serialize(serializer),
        }
    }
}

fn csv(table: &TabularTable, output: &mut BoundedWriter) -> Result<(), TabularError> {
    if table.columns.is_empty() {
        return Ok(());
    }
    let result = (|| {
        let mut writer = csv::WriterBuilder::new()
            .terminator(csv::Terminator::CRLF)
            .from_writer(&mut *output);
        writer.write_record(table.columns.iter().map(|column| &column.name))?;
        for row in &table.rows {
            let values = row
                .iter()
                .map(|value| csv_cell(value, table.limits.max_cell_bytes))
                .collect::<Result<Vec<_>, _>>()?;
            writer.write_record(&values)?;
        }
        writer.flush()?;
        Ok::<(), CsvFailure>(())
    })();
    match result {
        Ok(()) => Ok(()),
        Err(CsvFailure::Cell(error)) => Err(error),
        Err(CsvFailure::Csv(error)) => output.check(Err(error), TabularFormat::Csv),
        Err(CsvFailure::Io(error)) => output.check(Err(error), TabularFormat::Csv),
    }
}

enum CsvFailure {
    Cell(TabularError),
    Csv(csv::Error),
    Io(io::Error),
}

impl From<TabularError> for CsvFailure {
    fn from(error: TabularError) -> Self {
        Self::Cell(error)
    }
}

impl From<csv::Error> for CsvFailure {
    fn from(error: csv::Error) -> Self {
        Self::Csv(error)
    }
}

impl From<io::Error> for CsvFailure {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn csv_cell(value: &TabularValue, max: usize) -> Result<String, TabularError> {
    match value {
        TabularValue::Null => Ok(String::new()),
        TabularValue::String(value) => Ok(value.clone()),
        TabularValue::Json(_) => compact_json(value, max),
        // JSON's shortest round-trippable float spelling also preserves -0.0.
        _ => compact_json(value, 32),
    }
}

fn compact_json(value: &impl Serialize, max: usize) -> Result<String, TabularError> {
    let mut output = BoundedWriter::new(max, "cell bytes");
    let result = serde_json::to_writer(&mut output, value);
    output.check(result, TabularFormat::Json)?;
    String::from_utf8(output.bytes).map_err(|error| TabularError::Encoding {
        format: TabularFormat::Json,
        message: error.to_string(),
    })
}

pub(super) fn check_json_cell_size(value: &Value, max: usize) -> Result<(), TabularError> {
    // Measure escaped JSON before retaining the table, without allocating a
    // second serialized cell merely to check its size.
    let mut counter = SizeCounter { written: 0, max };
    serde_json::to_writer(&mut counter, value).map_err(|_| TabularError::LimitExceeded {
        limit: "cell bytes",
        max,
    })
}

struct SizeCounter {
    written: usize,
    max: usize,
}

impl Write for SizeCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.written = self.written.saturating_add(bytes.len());
        if self.written > self.max {
            return Err(io::Error::other("tabular cell limit exceeded"));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn parquet(table: &TabularTable, output: &mut BoundedWriter) -> Result<(), TabularError> {
    if table.columns.is_empty() {
        return Err(TabularError::EmptyParquetSchema);
    }
    let fields = table
        .columns
        .iter()
        .map(|column| {
            let data_type = match column.data_type {
                TabularType::Null => DataType::Null,
                TabularType::Boolean => DataType::Boolean,
                TabularType::Int64 => DataType::Int64,
                TabularType::UInt64 => DataType::UInt64,
                TabularType::Float64 => DataType::Float64,
                TabularType::String | TabularType::Json => DataType::Utf8,
            };
            let field = Field::new(&column.name, data_type, column.nullable);
            if column.data_type == TabularType::Json {
                field.with_metadata(HashMap::from([(
                    JSON_LOGICAL_TYPE_KEY.to_owned(),
                    "json".to_owned(),
                )]))
            } else {
                field
            }
        })
        .collect::<Vec<_>>();
    let schema = Arc::new(Schema::new(fields));
    let json_columns: Vec<_> = table
        .columns
        .iter()
        .filter(|column| column.data_type == TabularType::Json)
        .map(|column| &column.name)
        .collect();
    let properties = WriterProperties::builder()
        .set_dictionary_enabled(false)
        .set_key_value_metadata(Some(vec![KeyValue::new(
            JSON_LOGICAL_TYPE_KEY.to_owned(),
            compact_json(&json_columns, table.limits.max_output_bytes)?,
        )]))
        .build();
    let result = (|| {
        let mut writer = ArrowWriter::try_new(&mut *output, Arc::clone(&schema), Some(properties))?;
        // Flush each bounded batch to avoid retaining a whole table-sized row
        // group inside the Parquet encoder.
        for rows in table.rows.chunks(1_024) {
            let arrays = table
                .columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    arrow_column(rows, index, column.data_type, table.limits.max_cell_bytes)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| ParquetError::General(error.to_string()))?;
            let batch = RecordBatch::try_new(Arc::clone(&schema), arrays)?;
            writer.write(&batch)?;
            writer.flush()?;
        }
        writer.close()?;
        Ok::<(), ParquetError>(())
    })();
    output.check(result, TabularFormat::Parquet)
}

fn arrow_column(
    rows: &[Vec<TabularValue>],
    column: usize,
    data_type: TabularType,
    max: usize,
) -> Result<ArrayRef, TabularError> {
    let array: ArrayRef = match data_type {
        TabularType::Null => Arc::new(NullArray::new(rows.len())),
        TabularType::Boolean => {
            Arc::new(BooleanArray::from_iter(rows.iter().map(|row| {
                match &row[column] {
                    TabularValue::Boolean(value) => Some(*value),
                    TabularValue::Null => None,
                    _ => unreachable!("table constructor validates exact cell types"),
                }
            })))
        }
        TabularType::Int64 => Arc::new(Int64Array::from_iter(rows.iter().map(|row| {
            match &row[column] {
                TabularValue::Int64(value) => Some(*value),
                TabularValue::Null => None,
                _ => unreachable!("table constructor validates exact cell types"),
            }
        }))),
        TabularType::UInt64 => Arc::new(UInt64Array::from_iter(rows.iter().map(|row| match &row
            [column]
        {
            TabularValue::UInt64(value) => Some(*value),
            TabularValue::Null => None,
            _ => unreachable!("table constructor validates exact cell types"),
        }))),
        TabularType::Float64 => {
            Arc::new(Float64Array::from_iter(rows.iter().map(|row| {
                match &row[column] {
                    TabularValue::Float64(value) => Some(*value),
                    TabularValue::Null => None,
                    _ => unreachable!("table constructor validates exact cell types"),
                }
            })))
        }
        TabularType::String | TabularType::Json => {
            let mut builder = StringBuilder::new();
            for row in rows {
                match &row[column] {
                    TabularValue::Null => builder.append_null(),
                    TabularValue::String(value) => builder.append_value(value),
                    TabularValue::Json(value) => builder.append_value(compact_json(value, max)?),
                    _ => unreachable!("table constructor validates exact cell types"),
                }
            }
            Arc::new(builder.finish())
        }
    };
    Ok(array)
}

struct BoundedWriter {
    bytes: Vec<u8>,
    max: usize,
    limit: &'static str,
    exceeded: bool,
    allocation_failed: bool,
}

impl BoundedWriter {
    fn new(max: usize, limit: &'static str) -> Self {
        Self {
            bytes: Vec::new(),
            max,
            limit,
            exceeded: false,
            allocation_failed: false,
        }
    }

    fn check<E: std::fmt::Display>(
        &self,
        result: Result<(), E>,
        format: TabularFormat,
    ) -> Result<(), TabularError> {
        if self.exceeded {
            return Err(TabularError::LimitExceeded {
                limit: self.limit,
                max: self.max,
            });
        }
        if self.allocation_failed {
            return Err(TabularError::AllocationFailed);
        }
        result.map_err(|error| TabularError::Encoding {
            format,
            message: error.to_string(),
        })
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let required = self.bytes.len().saturating_add(bytes.len());
        if required > self.max {
            self.exceeded = true;
            return Err(io::Error::other("tabular output limit exceeded"));
        }
        if required > self.bytes.capacity() {
            let capacity = required
                .max(self.bytes.capacity().saturating_mul(2))
                .min(self.max);
            if self
                .bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .is_err()
            {
                self.allocation_failed = true;
                return Err(io::Error::other("tabular allocation failed"));
            }
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
