// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use arrow_array::{
    Date64Array, Decimal128Array, Float32Array, Float64Array, LargeStringArray, RecordBatch,
    StringArray, Time32MillisecondArray, Time32SecondArray, Time64MicrosecondArray,
    Time64NanosecondArray,
};
use arrow_schema::{DataType, Metadata, Schema, SchemaRef, TimeUnit};

use crate::error::limit;
use crate::{LimitKind, MetadataClassification, MetadataEvidence, MetadataLocation, TabularError};

/// Explicit finite budgets applied independently to input, retained arrays, and output.
///
/// The byte budget is not an allocator-wide resident-memory quota: Arrow/Parquet
/// have bounded working buffers and schema overhead in addition to the payload.
/// Compressed IPC is rejected to avoid decompression amplification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputLimits {
    max_rows: usize,
    max_bytes: usize,
    max_batches: usize,
}

impl OutputLimits {
    /// Creates limits; zero rows permits schema-only results, while bytes/batches
    /// must be nonzero. There is deliberately no implicit unlimited default.
    pub fn new(
        max_rows: usize,
        max_bytes: usize,
        max_batches: usize,
    ) -> Result<Self, TabularError> {
        if max_bytes == 0 || max_batches == 0 {
            return Err(TabularError::InvalidLimits);
        }
        Ok(Self {
            max_rows,
            max_bytes,
            max_batches,
        })
    }

    /// Maximum total rows across all batches.
    pub fn max_rows(self) -> usize {
        self.max_rows
    }

    /// Maximum input bytes, retained array bytes, and encoded output bytes,
    /// checked independently rather than added together.
    pub fn max_bytes(self) -> usize {
        self.max_bytes
    }

    /// Maximum batch count, including zero-row batches.
    pub fn max_batches(self) -> usize {
        self.max_batches
    }
}

/// Exactly one immutable dataset with an explicit schema and ordered batches.
///
/// Duplicate field names remain positional, including in JSON. No dataset or
/// batch is silently discarded. Unsupported metadata, types, non-finite floats,
/// and nulls in non-nullable columns fail during construction.
#[derive(Clone, Debug)]
pub struct TabularResult {
    schema: SchemaRef,
    batches: Vec<RecordBatch>,
    rows: usize,
}

impl TabularResult {
    /// Validates a single dataset against finite budgets without copying its arrays.
    ///
    /// Every batch must have exactly the declared schema, including metadata and
    /// nullability. Even an empty dataset must have a supported explicit schema.
    pub fn try_new(
        schema: SchemaRef,
        batches: Vec<RecordBatch>,
        limits: OutputLimits,
    ) -> Result<Self, TabularError> {
        validate_schema(&schema, limits)?;
        if batches.len() > limits.max_batches {
            return Err(limit(LimitKind::Batches, limits.max_batches));
        }
        let mut rows = 0usize;
        let mut bytes = 0usize;
        for (batch_index, batch) in batches.iter().enumerate() {
            if batch.schema().as_ref() != schema.as_ref() {
                return Err(TabularError::SchemaMismatch { batch: batch_index });
            }
            rows = rows
                .checked_add(batch.num_rows())
                .filter(|&value| value <= limits.max_rows)
                .ok_or_else(|| limit(LimitKind::Rows, limits.max_rows))?;
            for (column, array) in batch.columns().iter().enumerate() {
                bytes = bytes
                    .checked_add(array.get_array_memory_size())
                    .filter(|&value| value <= limits.max_bytes)
                    .ok_or_else(|| limit(LimitKind::Bytes, limits.max_bytes))?;
                if !schema.field(column).is_nullable() && array.null_count() != 0 {
                    return Err(TabularError::UnsupportedValue {
                        column,
                        row: rows - batch.num_rows()
                            + (0..array.len())
                                .find(|&row| array.is_null(row))
                                .unwrap_or(0),
                        reason: "null in a non-nullable column",
                    });
                }
                for row in 0..array.len() {
                    if array.is_null(row) {
                        continue;
                    }
                    let finite = match array.data_type() {
                        DataType::Float32 => array
                            .as_any()
                            .downcast_ref::<Float32Array>()
                            .is_some_and(|values| values.value(row).is_finite()),
                        DataType::Float64 => array
                            .as_any()
                            .downcast_ref::<Float64Array>()
                            .is_some_and(|values| values.value(row).is_finite()),
                        _ => true,
                    };
                    if !finite {
                        return Err(TabularError::UnsupportedValue {
                            column,
                            row: rows - batch.num_rows() + row,
                            reason: "non-finite floating point",
                        });
                    }
                    let valid_temporal = match array.data_type() {
                        DataType::Date64 => array
                            .as_any()
                            .downcast_ref::<Date64Array>()
                            .is_some_and(|values| values.value(row) % 86_400_000 == 0),
                        DataType::Time32(TimeUnit::Second) => array
                            .as_any()
                            .downcast_ref::<Time32SecondArray>()
                            .is_some_and(|values| (0..86_400).contains(&values.value(row))),
                        DataType::Time32(TimeUnit::Millisecond) => array
                            .as_any()
                            .downcast_ref::<Time32MillisecondArray>()
                            .is_some_and(|values| (0..86_400_000).contains(&values.value(row))),
                        DataType::Time64(TimeUnit::Microsecond) => array
                            .as_any()
                            .downcast_ref::<Time64MicrosecondArray>()
                            .is_some_and(|values| (0..86_400_000_000).contains(&values.value(row))),
                        DataType::Time64(TimeUnit::Nanosecond) => array
                            .as_any()
                            .downcast_ref::<Time64NanosecondArray>()
                            .is_some_and(|values| {
                                (0..86_400_000_000_000).contains(&values.value(row))
                            }),
                        _ => true,
                    };
                    if !valid_temporal {
                        return Err(TabularError::UnsupportedValue {
                            column,
                            row: rows - batch.num_rows() + row,
                            reason: "invalid date alignment or time-of-day range",
                        });
                    }
                    if let DataType::Decimal128(precision, _) = array.data_type() {
                        let valid = array
                            .as_any()
                            .downcast_ref::<Decimal128Array>()
                            .is_some_and(|values| {
                                values.value(row).unsigned_abs() < 10u128.pow(u32::from(*precision))
                            });
                        if !valid {
                            return Err(TabularError::UnsupportedValue {
                                column,
                                row: rows - batch.num_rows() + row,
                                reason: "decimal value exceeds declared precision",
                            });
                        }
                    }
                }
            }
        }
        Ok(Self {
            schema,
            batches,
            rows,
        })
    }

    /// Ordered Arrow schema, including duplicate field names.
    pub fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    /// Record batches in original order.
    pub fn batches(&self) -> &[RecordBatch] {
        &self.batches
    }

    /// Total row count.
    pub fn num_rows(&self) -> usize {
        self.rows
    }

    /// Rejects non-null values in a caller-declared diagnostic column.
    ///
    /// Engines declare their own reserved column name: for example, Dataflow
    /// Execute Query reports in-stream errors in `PQ Arrow Metadata`. Null
    /// diagnostics do not imply an error. Non-null values retain unknown
    /// evidence rather than guessing an undocumented success/error schema.
    ///
    /// # Errors
    /// Returns the first non-null diagnostic value without writing any output.
    pub fn check_diagnostic_column(&self, name: &str) -> Result<(), TabularError> {
        for (column, field) in self.schema.fields().iter().enumerate() {
            if field.name() != name {
                continue;
            }
            for batch in &self.batches {
                let array = batch.column(column);
                for row in 0..array.len() {
                    if array.is_null(row) {
                        continue;
                    }
                    let value = if let Some(values) = array.as_any().downcast_ref::<StringArray>() {
                        values.value(row).to_owned()
                    } else if let Some(values) = array.as_any().downcast_ref::<LargeStringArray>() {
                        values.value(row).to_owned()
                    } else {
                        "Non-null diagnostic value; native interpretation required".into()
                    };
                    return Err(TabularError::Metadata {
                        evidence: vec![MetadataEvidence {
                            location: MetadataLocation::Field(column),
                            key: name.to_owned(),
                            value,
                            classification: MetadataClassification::Unknown,
                        }],
                    });
                }
            }
        }
        Ok(())
    }

    pub(crate) fn validate(&self, limits: OutputLimits) -> Result<(), TabularError> {
        // Array storage is shared; revalidation does not clone cell buffers.
        Self::try_new(self.schema.clone(), self.batches.clone(), limits).map(|_| ())
    }
}

pub(crate) fn validate_schema(schema: &Schema, limits: OutputLimits) -> Result<(), TabularError> {
    let mut schema_size = metadata_size(schema.metadata(), 0, limits)?;
    let mut unsupported = None;
    for (column, field) in schema.fields().iter().enumerate() {
        schema_size = schema_size
            .checked_add(field.name().len())
            .and_then(|value| value.checked_add(std::mem::size_of::<arrow_schema::Field>()))
            .filter(|&value| value <= limits.max_bytes)
            .ok_or_else(|| limit(LimitKind::Bytes, limits.max_bytes))?;
        schema_size = metadata_size(field.metadata(), schema_size, limits)?;
        if let DataType::Timestamp(_, Some(timezone)) = field.data_type() {
            schema_size = schema_size
                .checked_add(timezone.len())
                .filter(|&value| value <= limits.max_bytes)
                .ok_or_else(|| limit(LimitKind::Bytes, limits.max_bytes))?;
        }
        if !matches!(
            field.data_type(),
            DataType::Boolean
                | DataType::Int8
                | DataType::Int16
                | DataType::Int32
                | DataType::Int64
                | DataType::UInt8
                | DataType::UInt16
                | DataType::UInt32
                | DataType::UInt64
                | DataType::Float32
                | DataType::Float64
                | DataType::Utf8
                | DataType::LargeUtf8
                | DataType::Binary
                | DataType::LargeBinary
                | DataType::Timestamp(_, _)
                | DataType::Date32
                | DataType::Date64
                | DataType::Time32(TimeUnit::Second | TimeUnit::Millisecond)
                | DataType::Time64(TimeUnit::Microsecond | TimeUnit::Nanosecond)
                | DataType::Duration(_)
                | DataType::Decimal128(_, _)
        ) {
            unsupported.get_or_insert_with(|| (column, field.data_type().to_string()));
        }
        if let DataType::Decimal128(precision, scale) = field.data_type()
            && (*precision == 0 || *precision > 38 || scale.unsigned_abs() > *precision)
        {
            unsupported.get_or_insert_with(|| (column, field.data_type().to_string()));
        }
    }
    let mut evidence = Vec::new();
    collect_metadata(schema.metadata(), MetadataLocation::Schema, &mut evidence);
    for (column, field) in schema.fields().iter().enumerate() {
        collect_metadata(
            field.metadata(),
            MetadataLocation::Field(column),
            &mut evidence,
        );
    }
    if !evidence.is_empty() {
        return Err(TabularError::Metadata { evidence });
    }
    if let Some((column, data_type)) = unsupported {
        return Err(TabularError::UnsupportedType { column, data_type });
    }
    Ok(())
}

fn metadata_size(
    metadata: &Metadata,
    mut size: usize,
    limits: OutputLimits,
) -> Result<usize, TabularError> {
    for (key, value) in metadata {
        size = size
            .checked_add(key.len())
            .and_then(|size| size.checked_add(value.len()))
            .and_then(|size| size.checked_add(std::mem::size_of::<MetadataEvidence>()))
            .filter(|&size| size <= limits.max_bytes)
            .ok_or_else(|| limit(LimitKind::Bytes, limits.max_bytes))?;
    }
    Ok(size)
}

fn collect_metadata(
    metadata: &Metadata,
    location: MetadataLocation,
    evidence: &mut Vec<MetadataEvidence>,
) {
    let mut entries: Vec<_> = metadata.iter().collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    for (key, value) in entries {
        if !key.is_empty() || !value.is_empty() {
            evidence.push(MetadataEvidence {
                location: location.clone(),
                key: key.clone(),
                value: value.clone(),
                classification: MetadataClassification::Unknown,
            });
        }
    }
}
