// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{collections::BTreeSet, fmt, sync::Arc};

use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Date32Array, Decimal128Array, DurationNanosecondArray,
    Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array, StringArray,
    Time64NanosecondArray, TimestampNanosecondArray, UInt8Array, UInt16Array, UInt32Array,
    UInt64Array,
};
use arrow_schema::{DataType, Field, Metadata, Schema, TimeUnit};
use fabric_sdk_aihub_query::{
    Column, Completeness, EmbeddedResource, NativeValue, QueryOutcome, QueryResource,
    TypedQueryResult,
};
use fabric_sdk_tabular::{LOGICAL_TYPE_METADATA_KEY, NATIVE_TYPE_METADATA_KEY, TabularResult};

use crate::{
    ConversionControl, ConversionError, ConversionLimit, ConversionLimits, bounds::Budget,
    table::TableBudget,
};

/// One typed DataAgent resource with original native identity and count evidence.
pub struct AihubDataset {
    /// Resource position among all returned typed resultsets.
    pub ordinal: usize,
    /// Original opaque embedded URI; conversion never dereferences it.
    pub uri: String,
    /// Original typed resource version.
    pub schema_version: u32,
    /// Original execution result identifier.
    pub result_id: String,
    /// Original ordered source declarations, including duplicate names.
    pub columns: Vec<Column>,
    /// Validated independently typed Arrow table.
    pub table: TabularResult,
    /// Original returned row count.
    pub row_count: usize,
    /// Original truncation evidence (false for consumable converted results).
    pub truncated: bool,
    /// Original server-recorded row cap, not an inferred execution bound.
    pub max_rows: Option<usize>,
    /// Original total count, or unknown; never manufactured from returned rows.
    pub total_row_count: Option<u64>,
}

impl fmt::Debug for AihubDataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AihubDataset")
            .field("ordinal", &self.ordinal)
            .field("columns", &self.columns.len())
            .field("rows", &self.row_count)
            .finish_non_exhaustive()
    }
}

/// All DataAgent resultsets and unchanged non-rowset evidence.
///
/// Auxiliary resources and messages are retained, not parsed as tables. The
/// native SDK alone owns wire decoding, variant negotiation and dispatch.
pub struct AihubTabularResult {
    /// Every typed resultset, in original content order.
    pub datasets: Vec<AihubDataset>,
    /// Every original auxiliary resource; no resource URI is fetched.
    pub resources: Vec<EmbeddedResource>,
    /// Original service prose, not data columns or a fallback table.
    pub messages: Vec<String>,
    /// Original native completeness evidence.
    pub completeness: Completeness,
    /// Original negotiated protocol version.
    pub protocol_version: String,
}

impl fmt::Debug for AihubTabularResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AihubTabularResult")
            .field("datasets", &self.datasets.len())
            .field("resources", &self.resources.len())
            .field("messages", &self.messages.len())
            .field("completeness", &self.completeness)
            .finish_non_exhaustive()
    }
}

/// Converts all clean typed DataAgent resultsets using their declared schemas.
///
/// Accepts only the native SDK's complete outcome, never its incomplete variant.
/// Missing schemas, unsupported native representations, inferred decimal
/// precision/scale and per-value timezone inference are explicitly refused.
/// There is no CSV/Markdown/Excel parse fallback or direct-Report assumption.
///
/// # Errors
/// Fails atomically on invalid evidence, schema/value mismatch, conversion
/// overflow, finite limits, cancellation, deadline, or shared codec restrictions.
pub fn convert_aihub(
    outcome: QueryOutcome,
    limits: ConversionLimits,
    control: &ConversionControl<'_>,
) -> Result<AihubTabularResult, ConversionError> {
    control.check()?;
    let result = match outcome {
        QueryOutcome::Complete(result) if result.completeness == Completeness::Complete => result,
        _ => return Err(ConversionError::UncleanResult),
    };
    preflight(&result, limits, control)?;
    let mut retained = TableBudget::new(limits);
    let mut datasets = Vec::with_capacity(result.resultsets.len());
    for (ordinal, source) in result.resultsets.into_iter().enumerate() {
        control.check()?;
        let types = source
            .columns
            .iter()
            .enumerate()
            .map(|(column, field)| native_type(field, ordinal, column))
            .collect::<Result<Vec<_>, _>>()?;
        let fields = source
            .columns
            .iter()
            .zip(&types)
            .map(|(field, kind)| {
                let mut metadata =
                    Metadata::new().with(NATIVE_TYPE_METADATA_KEY, &field.engine_type);
                if matches!(kind, NativeType::Guid) {
                    metadata = metadata.with(LOGICAL_TYPE_METADATA_KEY, "guid");
                }
                Field::new(&field.name, kind.arrow(), true).with_metadata(metadata)
            })
            .collect::<Vec<_>>();
        let arrays = types
            .iter()
            .enumerate()
            .map(|(column, kind)| array(*kind, &source.rows, ordinal, column, control))
            .collect::<Result<Vec<_>, _>>()?;
        let table = retained.table(
            Arc::new(Schema::new(fields)),
            arrays,
            source.rows.len(),
            control,
        )?;
        datasets.push(AihubDataset {
            ordinal,
            uri: source.uri,
            schema_version: source.schema_version,
            result_id: source.result_id,
            columns: source.columns,
            table,
            row_count: source.row_count,
            truncated: source.truncated,
            max_rows: source.max_rows,
            total_row_count: source.total_row_count,
        });
    }
    control.check()?;
    Ok(AihubTabularResult {
        datasets,
        resources: result.resources,
        messages: result.messages,
        completeness: result.completeness,
        protocol_version: result.protocol_version,
    })
}

fn preflight(
    source: &TypedQueryResult,
    limits: ConversionLimits,
    control: &ConversionControl<'_>,
) -> Result<(), ConversionError> {
    if source.resultsets.is_empty() || source.protocol_version.is_empty() {
        return Err(ConversionError::UncleanResult);
    }
    if source.resultsets.len() > limits.datasets {
        return Err(ConversionError::Limit(ConversionLimit::Datasets));
    }
    if source.resultsets.len() > limits.batches {
        return Err(ConversionError::Limit(ConversionLimit::Batches));
    }
    let mut budget = Budget::new(limits, control);
    budget.bytes(source.protocol_version.len())?;
    let mut uris = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut rows = 0usize;
    for (ordinal, dataset) in source.resultsets.iter().enumerate() {
        control.check()?;
        validate_resource(dataset)?;
        if !uris.insert(dataset.uri.as_str()) || !ids.insert(dataset.result_id.to_ascii_lowercase())
        {
            return Err(ConversionError::DatasetIdentity);
        }
        budget.bytes(std::mem::size_of_val(dataset))?;
        budget.bytes(dataset.uri.len())?;
        budget.bytes(dataset.result_id.len())?;
        if dataset.columns.len() > limits.columns {
            return Err(ConversionError::Limit(ConversionLimit::Columns));
        }
        rows = rows
            .checked_add(dataset.rows.len())
            .filter(|value| *value <= limits.rows)
            .ok_or(ConversionError::Limit(ConversionLimit::Rows))?;
        for (column, field) in dataset.columns.iter().enumerate() {
            budget.bytes(std::mem::size_of_val(field))?;
            for value in [
                &field.name,
                &field.engine_type,
                &field.logical_type,
                &field.encoding,
            ] {
                budget.bytes(value.len())?;
            }
            native_type(field, ordinal, column)?;
        }
        for (row, values) in dataset.rows.iter().enumerate() {
            if values.len() != dataset.columns.len() {
                return Err(ConversionError::RowArity {
                    dataset: ordinal,
                    row,
                });
            }
            budget.bytes(std::mem::size_of_val(values))?;
            for value in values {
                budget.bytes(std::mem::size_of_val(value))?;
                match value {
                    NativeValue::Null | NativeValue::Boolean(_) => {}
                    NativeValue::Integer(value)
                    | NativeValue::Decimal(value)
                    | NativeValue::Number(value)
                    | NativeValue::Text(value)
                    | NativeValue::Date(value)
                    | NativeValue::DateTime(value)
                    | NativeValue::DateTimeOffset(value)
                    | NativeValue::Time(value)
                    | NativeValue::Duration(value)
                    | NativeValue::Guid(value)
                    | NativeValue::Binary(value) => budget.bytes(value.len())?,
                }
            }
        }
    }
    for resource in &source.resources {
        budget.bytes(std::mem::size_of_val(resource))?;
        if resource.uri.is_empty() || !uris.insert(resource.uri.as_str()) {
            return Err(ConversionError::DatasetIdentity);
        }
        for value in [
            Some(&resource.uri),
            Some(&resource.mime_type),
            resource.text.as_ref(),
            resource.blob.as_ref(),
            resource.metadata.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            budget.bytes(value.len())?;
        }
    }
    for message in &source.messages {
        budget.bytes(std::mem::size_of_val(message))?;
        budget.bytes(message.len())?;
    }
    Ok(())
}

fn validate_resource(source: &QueryResource) -> Result<(), ConversionError> {
    let id =
        uuid::Uuid::parse_str(&source.result_id).map_err(|_| ConversionError::DatasetIdentity)?;
    if source.schema_version != 1
        || source.result_id.len() != 36
        || id.is_nil()
        || source.uri.is_empty()
        || source.truncated
        || source.row_count != source.rows.len()
        || source
            .max_rows
            .is_some_and(|cap| cap == 0 || cap < source.row_count)
        || source
            .total_row_count
            .is_some_and(|total| total != source.row_count as u64)
    {
        return Err(ConversionError::UncleanResult);
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum NativeType {
    Int8,
    Int16,
    Int32,
    Int64,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Float32,
    Float64,
    Decimal(u8, i8),
    Boolean,
    Text,
    Date,
    DateTime,
    Time,
    Duration,
    Guid,
    Binary,
}

impl NativeType {
    fn arrow(self) -> DataType {
        match self {
            Self::Int8 => DataType::Int8,
            Self::Int16 => DataType::Int16,
            Self::Int32 => DataType::Int32,
            Self::Int64 => DataType::Int64,
            Self::UInt8 => DataType::UInt8,
            Self::UInt16 => DataType::UInt16,
            Self::UInt32 => DataType::UInt32,
            Self::UInt64 => DataType::UInt64,
            Self::Float32 => DataType::Float32,
            Self::Float64 => DataType::Float64,
            Self::Decimal(p, s) => DataType::Decimal128(p, s),
            Self::Boolean => DataType::Boolean,
            Self::Text | Self::Guid => DataType::Utf8,
            Self::Date => DataType::Date32,
            Self::DateTime => DataType::Timestamp(TimeUnit::Nanosecond, None),
            Self::Time => DataType::Time64(TimeUnit::Nanosecond),
            Self::Duration => DataType::Duration(TimeUnit::Nanosecond),
            Self::Binary => DataType::Binary,
        }
    }
}

fn native_type(
    field: &Column,
    dataset: usize,
    column: usize,
) -> Result<NativeType, ConversionError> {
    let unsupported = |reason| ConversionError::UnsupportedType {
        dataset,
        column,
        reason,
    };
    if field.engine_type.is_empty()
        || field.engine_type.len() > 256
        || field.engine_type.trim() != field.engine_type
        || field.engine_type.chars().any(char::is_control)
    {
        return Err(unsupported(
            "native type hint cannot be preserved by the shared metadata contract",
        ));
    }
    let native = field.engine_type.to_ascii_lowercase();
    let kind = match (field.logical_type.as_str(), field.encoding.as_str()) {
        ("integer", "json-number") => match native.as_str() {
            "byte" | "tinyint" | "uint8" => NativeType::UInt8,
            "sbyte" | "int8" => NativeType::Int8,
            "int16" | "smallint" | "short" => NativeType::Int16,
            "uint16" => NativeType::UInt16,
            "int32" | "int" | "integer" => NativeType::Int32,
            "uint32" => NativeType::UInt32,
            "int64" | "bigint" | "long" => NativeType::Int64,
            "uint64" => NativeType::UInt64,
            _ => {
                return Err(unsupported(
                    "integer width and sign are not declared by a supported native type",
                ));
            }
        },
        ("decimal", "json-number") => {
            let (precision, scale) =
                crate::typed_scalar::decimal_schema(&native).map_err(unsupported)?;
            NativeType::Decimal(precision, scale)
        }
        ("number", "json-number") => match native.as_str() {
            "single" | "float32" => NativeType::Float32,
            "double" | "float64" => NativeType::Float64,
            _ => return Err(unsupported("approximate numeric width is not explicit")),
        },
        ("boolean", "json-boolean") if matches!(native.as_str(), "boolean" | "bool") => {
            NativeType::Boolean
        }
        ("string", "text") => NativeType::Text,
        ("date", "iso8601") if matches!(native.as_str(), "date" | "dateonly") => NativeType::Date,
        ("datetime", "iso8601") if native == "datetime" => NativeType::DateTime,
        ("datetimeoffset", "iso8601") => {
            return Err(unsupported(
                "source schema supplies no fixed column timezone; per-row offsets cannot be discarded",
            ));
        }
        ("time", "iso8601") if matches!(native.as_str(), "time" | "timeonly") => NativeType::Time,
        ("duration", "text") if matches!(native.as_str(), "timespan" | "duration") => {
            NativeType::Duration
        }
        ("guid", "text") if matches!(native.as_str(), "guid" | "uuid" | "uniqueidentifier") => {
            NativeType::Guid
        }
        ("binary", "base64")
            if matches!(native.as_str(), "binary" | "byte[]" | "varbinary" | "image") =>
        {
            NativeType::Binary
        }
        _ => {
            return Err(unsupported(
                "logical type, native type, and encoding have no supported exact mapping",
            ));
        }
    };
    Ok(kind)
}

fn array(
    kind: NativeType,
    rows: &[Vec<NativeValue>],
    dataset: usize,
    column: usize,
    control: &ConversionControl<'_>,
) -> Result<ArrayRef, ConversionError> {
    macro_rules! typed {
        ($array:ty, $parse:expr) => {{
            let values = rows
                .iter()
                .enumerate()
                .map(|(row, values)| {
                    control.check()?;
                    if matches!(values[column], NativeValue::Null) {
                        Ok(None)
                    } else {
                        ($parse)(&values[column]).map(Some).map_err(|reason| {
                            ConversionError::Value {
                                dataset,
                                column,
                                row,
                                reason,
                            }
                        })
                    }
                })
                .collect::<Result<Vec<_>, ConversionError>>()?;
            <$array>::from(values)
        }};
    }
    use crate::typed_scalar as scalar;
    Ok(match kind {
        NativeType::Int8 => Arc::new(typed!(Int8Array, scalar::integer::<i8>)),
        NativeType::Int16 => Arc::new(typed!(Int16Array, scalar::integer::<i16>)),
        NativeType::Int32 => Arc::new(typed!(Int32Array, scalar::integer::<i32>)),
        NativeType::Int64 => Arc::new(typed!(Int64Array, scalar::integer::<i64>)),
        NativeType::UInt8 => Arc::new(typed!(UInt8Array, scalar::integer::<u8>)),
        NativeType::UInt16 => Arc::new(typed!(UInt16Array, scalar::integer::<u16>)),
        NativeType::UInt32 => Arc::new(typed!(UInt32Array, scalar::integer::<u32>)),
        NativeType::UInt64 => Arc::new(typed!(UInt64Array, scalar::integer::<u64>)),
        NativeType::Float32 => Arc::new(typed!(Float32Array, scalar::float32)),
        NativeType::Float64 => Arc::new(typed!(Float64Array, scalar::float64)),
        NativeType::Decimal(p, s) => Arc::new(
            typed!(Decimal128Array, |value| scalar::decimal(value, p, s))
                .with_precision_and_scale(p, s)
                .map_err(ConversionError::Arrow)?,
        ),
        NativeType::Boolean => Arc::new(typed!(BooleanArray, scalar::boolean)),
        NativeType::Text => Arc::new(typed!(StringArray, scalar::text)),
        NativeType::Guid => Arc::new(typed!(StringArray, scalar::guid)),
        NativeType::Date => Arc::new(typed!(Date32Array, scalar::date)),
        NativeType::DateTime => Arc::new(typed!(TimestampNanosecondArray, scalar::datetime)),
        NativeType::Time => Arc::new(typed!(Time64NanosecondArray, scalar::time)),
        NativeType::Duration => Arc::new(typed!(DurationNanosecondArray, scalar::duration)),
        NativeType::Binary => {
            let decoded = rows
                .iter()
                .enumerate()
                .map(|(row, values)| {
                    control.check()?;
                    if matches!(values[column], NativeValue::Null) {
                        Ok(None)
                    } else {
                        scalar::binary(&values[column]).map(Some).map_err(|reason| {
                            ConversionError::Value {
                                dataset,
                                column,
                                row,
                                reason,
                            }
                        })
                    }
                })
                .collect::<Result<Vec<_>, ConversionError>>()?;
            Arc::new(BinaryArray::from_iter(
                decoded.iter().map(|value| value.as_deref()),
            ))
        }
    })
}
