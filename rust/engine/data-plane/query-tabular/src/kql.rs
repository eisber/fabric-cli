// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{collections::BTreeSet, fmt, sync::Arc};

use arrow_array::{
    ArrayRef, BooleanArray, DurationNanosecondArray, Float64Array, Int32Array, Int64Array,
    StringArray, TimestampNanosecondArray,
};
use arrow_schema::{DataType, Field, Metadata, Schema, TimeUnit};
use fabric_sdk_query::{QueryColumn, QueryDiagnostic, QueryEvidence, QueryResult};
use fabric_sdk_tabular::{LOGICAL_TYPE_METADATA_KEY, NATIVE_TYPE_METADATA_KEY, TabularResult};
use serde_json::Value;

use crate::{
    ConversionControl, ConversionError, ConversionLimit, ConversionLimits, bounds::Budget,
    table::TableBudget,
};

/// One independently typed native dataset with its original identity.
#[derive(Clone)]
pub struct KqlDataset {
    /// Service-order primary-result ordinal.
    pub ordinal: usize,
    /// Original native table identifier.
    pub table_id: u64,
    /// Original native table name.
    pub name: String,
    /// Original ordered native columns, including duplicate names.
    pub columns: Vec<QueryColumn>,
    /// Validated shared Arrow table, never a row-object projection.
    pub table: TabularResult,
}

impl fmt::Debug for KqlDataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KqlDataset")
            .field("ordinal", &self.ordinal)
            .field("columns", &self.columns.len())
            .field("rows", &self.table.num_rows())
            .finish_non_exhaustive()
    }
}

/// All converted datasets plus unchanged, unprojected execution evidence.
///
/// An empty `datasets` vector means no primary resultset, not a zero-row table.
/// There is deliberately no first-dataset accessor or automatic export choice.
#[derive(Clone)]
pub struct KqlTabularResult {
    /// Every primary dataset in service order.
    pub datasets: Vec<KqlDataset>,
    /// Original non-primary native metadata, not row columns.
    pub metadata: Vec<Value>,
    /// Original diagnostics (empty for clean consumable results).
    pub diagnostics: Vec<QueryDiagnostic>,
    /// Original native terminal frame.
    pub completion: Value,
    /// Original protected execution evidence, never manufactured by conversion.
    pub evidence: QueryEvidence,
}

impl fmt::Debug for KqlTabularResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KqlTabularResult")
            .field("datasets", &self.datasets.len())
            .field("metadata", &self.metadata.len())
            .field("diagnostics", &self.diagnostics.len())
            .field("evidence", &self.evidence)
            .finish()
    }
}

/// Converts all datasets only after validating clean completion and finite budgets.
///
/// Schema mapping is explicit: `bool`, `int`, `long`, `real`, `string`,
/// `datetime` (UTC nanoseconds), `timespan` (nanoseconds), and GUID/dynamic
/// (annotated logical UTF-8). Unknown types,
/// unspecified decimal precision/scale, and unsupported logical types fail even
/// for empty or all-null columns. Temporal overflow never rounds or stringifies.
///
/// # Errors
/// Rejects missing HTTP EOF, partial/error/warning/truncated observations, invalid values and
/// identities, budget overflow, cancellation, deadline, and shared model failures.
pub fn convert_kql(
    result: QueryResult,
    limits: ConversionLimits,
    control: &ConversionControl<'_>,
) -> Result<KqlTabularResult, ConversionError> {
    control.check()?;
    clean(&result)?;
    preflight(&result, limits, control)?;
    let mut datasets = Vec::with_capacity(result.datasets.len());
    let mut retained = TableBudget::new(limits);
    for dataset in result.datasets {
        control.check()?;
        let fields = dataset
            .columns
            .iter()
            .enumerate()
            .map(|(column, field)| {
                let mut metadata =
                    Metadata::new().with(NATIVE_TYPE_METADATA_KEY, &field.native_type);
                let logical = match field.native_type.as_str() {
                    "dynamic" => Some("json"),
                    "guid" => Some("guid"),
                    _ => None,
                };
                if let Some(logical) = logical {
                    metadata = metadata.with(LOGICAL_TYPE_METADATA_KEY, logical);
                }
                Ok(Field::new(
                    &field.name,
                    data_type(&field.native_type, dataset.ordinal, column)?,
                    true,
                )
                .with_metadata(metadata))
            })
            .collect::<Result<Vec<_>, ConversionError>>()?;
        let schema = Arc::new(Schema::new(fields));
        let arrays = dataset
            .columns
            .iter()
            .enumerate()
            .map(|(column, field)| {
                array(
                    &field.native_type,
                    &dataset.rows,
                    dataset.ordinal,
                    column,
                    control,
                    limits,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let table = retained.table(schema, arrays, dataset.rows.len(), control)?;
        control.check()?;
        datasets.push(KqlDataset {
            ordinal: dataset.ordinal,
            table_id: dataset.table_id,
            name: dataset.name,
            columns: dataset.columns,
            table,
        });
    }
    control.check()?;
    Ok(KqlTabularResult {
        datasets,
        metadata: result.metadata,
        diagnostics: result.diagnostics,
        completion: result.completion,
        evidence: result.evidence,
    })
}

fn clean(result: &QueryResult) -> Result<(), ConversionError> {
    let completion = &result.completion;
    if !result.evidence.completed
        || !result.evidence.http_eof
        || result.evidence.locally_truncated
        || !result.diagnostics.is_empty()
        || completion["FrameType"] != "DataSetCompletion"
        || completion["HasErrors"].as_bool() != Some(false)
        || completion["Cancelled"].as_bool() != Some(false)
        || completion
            .get("OneApiErrors")
            .is_some_and(|value| value.as_array().is_none_or(|errors| !errors.is_empty()))
    {
        return Err(ConversionError::UncleanResult);
    }
    Ok(())
}

fn preflight(
    result: &QueryResult,
    limits: ConversionLimits,
    control: &ConversionControl<'_>,
) -> Result<(), ConversionError> {
    if result.datasets.len() > limits.datasets {
        return Err(ConversionError::Limit(ConversionLimit::Datasets));
    }
    if result.datasets.len() > limits.batches {
        return Err(ConversionError::Limit(ConversionLimit::Batches));
    }
    let mut budget = Budget::new(limits, control);
    let evidence = &result.evidence;
    let binding = &evidence.binding;
    budget.bytes(std::mem::size_of_val(evidence))?;
    for text in [
        binding.id(),
        binding.version(),
        binding.owner().workspace_id(),
        binding.owner().item_id(),
        binding.endpoint().base().as_str(),
        binding.database(),
        binding.audience_scope(),
        binding.account(),
        binding.tenant(),
        evidence.language,
        evidence.transport,
        evidence.protocol_version,
        evidence.read_policy,
        &evidence.client_request_id,
    ] {
        budget.bytes(text.len())?;
    }
    for text in [&evidence.content_type, &evidence.activity_id]
        .into_iter()
        .flatten()
    {
        budget.bytes(text.len())?;
    }
    budget.value(&result.completion, 1)?;
    for value in &result.metadata {
        budget.value(value, 1)?;
    }
    let mut ids = BTreeSet::new();
    let mut rows = 0usize;
    for (index, dataset) in result.datasets.iter().enumerate() {
        control.check()?;
        if dataset.ordinal != index || !ids.insert(dataset.table_id) {
            return Err(ConversionError::DatasetIdentity);
        }
        if dataset.columns.len() > limits.columns {
            return Err(ConversionError::Limit(ConversionLimit::Columns));
        }
        budget.bytes(std::mem::size_of_val(dataset) + dataset.name.len())?;
        for (column, field) in dataset.columns.iter().enumerate() {
            budget.bytes(std::mem::size_of_val(field))?;
            budget.bytes(field.name.len())?;
            budget.bytes(field.native_type.len())?;
            data_type(&field.native_type, index, column)?;
        }
        rows = rows
            .checked_add(dataset.rows.len())
            .filter(|count| *count <= limits.rows)
            .ok_or(ConversionError::Limit(ConversionLimit::Rows))?;
        for (row, values) in dataset.rows.iter().enumerate() {
            if values.len() != dataset.columns.len() {
                return Err(ConversionError::RowArity {
                    dataset: index,
                    row,
                });
            }
            budget.bytes(std::mem::size_of_val(values))?;
            for value in values {
                budget.value(value, 1)?;
            }
        }
    }
    Ok(())
}

fn data_type(native: &str, dataset: usize, column: usize) -> Result<DataType, ConversionError> {
    Ok(match native {
        "bool" => DataType::Boolean,
        "int" => DataType::Int32,
        "long" => DataType::Int64,
        "real" => DataType::Float64,
        "string" | "guid" | "dynamic" => DataType::Utf8,
        "datetime" => DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
        "timespan" => DataType::Duration(TimeUnit::Nanosecond),
        _ => {
            return Err(ConversionError::UnsupportedType {
                dataset,
                column,
                reason: match native {
                    "decimal" => "native schema supplies no decimal precision or scale",
                    _ => "native schema has no supported exact Arrow mapping",
                },
            });
        }
    })
}

fn array(
    native: &str,
    rows: &[Vec<Value>],
    dataset: usize,
    column: usize,
    control: &ConversionControl<'_>,
    limits: ConversionLimits,
) -> Result<ArrayRef, ConversionError> {
    macro_rules! typed {
        ($array:ty, $parse:expr) => {{
            let values = rows
                .iter()
                .enumerate()
                .map(|(row, values)| {
                    control.check()?;
                    let value = &values[column];
                    if value.is_null() {
                        Ok(None)
                    } else {
                        ($parse)(value)
                            .map(Some)
                            .map_err(|reason| ConversionError::Value {
                                dataset,
                                column,
                                row,
                                reason,
                            })
                    }
                })
                .collect::<Result<Vec<_>, ConversionError>>()?;
            <$array>::from(values)
        }};
    }
    Ok(match native {
        "bool" => Arc::new(typed!(BooleanArray, crate::scalar::boolean)),
        "int" => Arc::new(typed!(Int32Array, crate::scalar::int32)),
        "long" => Arc::new(typed!(Int64Array, crate::scalar::signed)),
        "real" => Arc::new(typed!(Float64Array, crate::scalar::real)),
        "string" => Arc::new(typed!(StringArray, crate::scalar::text)),
        "guid" => Arc::new(typed!(StringArray, crate::scalar::guid)),
        "dynamic" => {
            let mut remaining = limits.bytes;
            let values = rows
                .iter()
                .map(|values| {
                    control.check()?;
                    if values[column].is_null() {
                        Ok(None)
                    } else {
                        let value = crate::bounds::json_text(&values[column], remaining, control)?;
                        remaining -= value.len();
                        Ok(Some(value))
                    }
                })
                .collect::<Result<Vec<_>, ConversionError>>()?;
            Arc::new(StringArray::from(values))
        }
        "datetime" => {
            Arc::new(typed!(TimestampNanosecondArray, crate::scalar::datetime).with_timezone("UTC"))
        }
        "timespan" => Arc::new(typed!(DurationNanosecondArray, crate::scalar::timespan)),
        _ => {
            return Err(ConversionError::UnsupportedType {
                dataset,
                column,
                reason: "native schema has no supported exact Arrow mapping",
            });
        }
    })
}
