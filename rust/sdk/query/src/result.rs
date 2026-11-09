// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{EngineBinding, QueryError, QueryErrorKind, QueryLimits};
use fabric_sdk::runtime::NativeDeadline;
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeSet, fmt};

/// Native column metadata; unknown type names are retained, not guessed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct QueryColumn {
    /// Original name, including duplicate names.
    pub name: String,
    /// Original Kusto scalar type name.
    pub native_type: String,
}

/// One native primary table, with positional values and ordered schema.
#[derive(Clone, PartialEq, Serialize)]
pub struct QueryDataset {
    /// Primary-result ordinal in service order.
    pub ordinal: usize,
    /// Native table identifier.
    pub table_id: u64,
    /// Original table name.
    pub name: String,
    /// Native schema in column order, including empty datasets.
    pub columns: Vec<QueryColumn>,
    /// Bounded positional rows; no keyed-object conversion or type inference.
    pub rows: Vec<Vec<Value>>,
}

impl fmt::Debug for QueryDataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueryDataset")
            .field("ordinal", &self.ordinal)
            .field("columns", &self.columns.len())
            .field("rows", &self.rows.len())
            .finish()
    }
}

/// Unprojected execution evidence, distinct from data rows and engine safety.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryEvidence {
    /// Exact binding used for this request.
    pub binding: EngineBinding,
    /// Dialect fixed by the binding, not inferred from text.
    pub language: &'static str,
    /// Actual native protocol, not an inferred Fabric REST operation.
    pub transport: &'static str,
    /// Requested native response protocol version.
    pub protocol_version: &'static str,
    /// Adapter-owned read-only request policy; not live enforcement proof.
    pub read_policy: &'static str,
    /// Production enforcement is not established by an offline fixture.
    pub production_verified: bool,
    /// Actual response status, if headers were returned.
    pub http_status: Option<u16>,
    /// Actual response media metadata, not an inferred output format.
    pub content_type: Option<String>,
    /// Caller-supplied client request identifier, not an asynchronous handle.
    pub client_request_id: String,
    /// Provider correlation identifier, if returned.
    pub activity_id: Option<String>,
    /// True only after actual HTTP EOF and a valid terminal native frame.
    pub completed: bool,
    /// Local row cap dropped native rows; it did not limit scans or remote costs.
    pub locally_truncated: bool,
}

impl fmt::Debug for QueryEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueryEvidence")
            .field("language", &self.language)
            .field("http_status", &self.http_status)
            .field("completed", &self.completed)
            .field("locally_truncated", &self.locally_truncated)
            .finish_non_exhaustive()
    }
}

/// Complete or partial native data plus unprojected execution metadata.
///
/// Non-complete observations are carried inside [`QueryError::partial`], not
/// returned as successful queries. No continuation or cancel handle is invented.
/// A native partial failure can flush incorrect values: retained rows are forensic
/// observations, not safe query results to consume.
#[derive(Clone, Serialize)]
pub struct QueryResult {
    /// Ordered primary tables.
    pub datasets: Vec<QueryDataset>,
    /// Native non-primary tables/diagnostics, never appended as data rows.
    pub metadata: Vec<Value>,
    /// Native in-data errors and completion warnings/errors, never data rows.
    pub diagnostics: Vec<QueryDiagnostic>,
    /// Original terminal status frame, or null if no terminal frame was received.
    pub completion: Value,
    /// Protected evidence, kept separately from data projection.
    pub evidence: QueryEvidence,
}

impl fmt::Debug for QueryResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueryResult")
            .field("datasets", &self.datasets.len())
            .field("metadata", &self.metadata.len())
            .field("diagnostics", &self.diagnostics.len())
            .field("evidence", &self.evidence)
            .finish()
    }
}

/// Source-grounded native failure/warning classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum QueryDiagnosticKind {
    /// An in-data OneApiErrors record instead of a positional row.
    InlineError,
    /// QueryCompletionInformation Level is 2 or smaller.
    CompletionError,
    /// QueryCompletionInformation Level is 3.
    CompletionWarning,
}

/// A native diagnostic with exact table/row coordinates and original contents.
#[derive(Clone, Serialize)]
pub struct QueryDiagnostic {
    /// Native table identifier, not a primary dataset ordinal.
    pub table_id: u64,
    /// Row position in the native table, including in-data error records.
    pub row_index: usize,
    /// Numeric/protocol classification, not a text heuristic.
    pub kind: QueryDiagnosticKind,
    /// Original source-sensitive error bag or completion-status row.
    pub record: Value,
}

impl fmt::Debug for QueryDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueryDiagnostic")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

pub(crate) fn decode(
    bytes: Vec<u8>,
    limits: QueryLimits,
    evidence: QueryEvidence,
    deadline: &NativeDeadline,
) -> Result<QueryResult, QueryError> {
    let frames: Vec<Value> = serde_json::from_slice(&bytes).map_err(|_| {
        QueryError::observed(QueryErrorKind::InvalidResponse, evidence.clone())
            .with_provider_body(bytes.clone())
    })?;
    let mut result = QueryResult {
        datasets: Vec::new(),
        metadata: Vec::new(),
        diagnostics: Vec::new(),
        completion: frames
            .last()
            .filter(|frame| frame["FrameType"] == "DataSetCompletion")
            .cloned()
            .unwrap_or(Value::Null),
        evidence,
    };
    match decode_frames(&frames, limits, &mut result, deadline) {
        Ok(()) => {
            result.evidence.completed = true;
            Ok(result)
        }
        Err(kind) => Err(QueryError::with_partial(kind, result).with_provider_body(bytes)),
    }
}

fn decode_frames(
    frames: &[Value],
    limits: QueryLimits,
    result: &mut QueryResult,
    deadline: &NativeDeadline,
) -> Result<(), QueryErrorKind> {
    if frames.len() > limits.max_frames() {
        return Err(QueryErrorKind::ResponseLimit);
    }
    if frames.is_empty() {
        return Err(QueryErrorKind::InvalidResponse);
    }
    let header = &frames[0];
    if header["FrameType"] != "DataSetHeader"
        || header["Version"] != "v2.0"
        || header["IsProgressive"].as_bool() != Some(false)
    {
        return Err(QueryErrorKind::UnsupportedResponse);
    }
    let mut ids = BTreeSet::new();
    let mut remaining = limits.max_rows();
    let last_table = frames.len() - usize::from(!result.completion.is_null());
    for frame in &frames[1..last_table] {
        deadline.remaining().map_err(QueryErrorKind::Native)?;
        if frame["FrameType"] != "DataTable" {
            return Err(QueryErrorKind::UnsupportedResponse);
        }
        let primary = frame["TableKind"] == "PrimaryResult";
        if !primary {
            result.metadata.push(frame.clone());
        }
        let table = table_header(frame, limits)?;
        if !ids.insert(table.table_id) {
            return Err(QueryErrorKind::InvalidResponse);
        }
        let table_id = table.table_id;
        let columns = table.columns.clone();
        let status = if frame["TableKind"] == "QueryCompletionInformation" {
            Some(CompletionColumns::new(&columns)?)
        } else {
            None
        };
        if primary {
            if result.datasets.len() == limits.max_tables() {
                return Err(QueryErrorKind::ResponseLimit);
            }
            let mut table = table;
            table.ordinal = result.datasets.len();
            result.datasets.push(table);
        }
        let rows = frame["Rows"]
            .as_array()
            .ok_or(QueryErrorKind::InvalidResponse)?;
        for (row_index, row) in rows.iter().enumerate() {
            deadline.remaining().map_err(QueryErrorKind::Native)?;
            if inline_error(row) {
                result.diagnostics.push(QueryDiagnostic {
                    table_id,
                    row_index,
                    kind: QueryDiagnosticKind::InlineError,
                    record: row.clone(),
                });
                continue;
            }
            let values = row.as_array().ok_or(QueryErrorKind::InvalidResponse)?;
            if values.len() != columns.len() {
                return Err(QueryErrorKind::InvalidResponse);
            }
            if let Some(status) = &status
                && let Some(kind) = status.classify(values)?
            {
                result.diagnostics.push(QueryDiagnostic {
                    table_id,
                    row_index,
                    kind,
                    record: row.clone(),
                });
            }
            if primary {
                if remaining == 0 {
                    result.evidence.locally_truncated = true;
                } else {
                    result
                        .datasets
                        .last_mut()
                        .expect("primary table inserted before its rows")
                        .rows
                        .push(values.clone());
                    remaining -= 1;
                }
            }
        }
    }
    let completion = &result.completion;
    let has_errors = completion["HasErrors"].as_bool();
    let cancelled = completion["Cancelled"].as_bool();
    if completion["FrameType"] != "DataSetCompletion"
        || has_errors.is_none()
        || cancelled.is_none()
        || completion
            .get("OneApiErrors")
            .is_some_and(|errors| !errors.is_array())
    {
        return Err(QueryErrorKind::InvalidResponse);
    }
    deadline.remaining().map_err(QueryErrorKind::Native)?;
    if cancelled == Some(true) {
        Err(QueryErrorKind::BackendCancelled)
    } else if has_errors == Some(true)
        || completion
            .get("OneApiErrors")
            .is_some_and(|errors| errors.as_array().is_some_and(|errors| !errors.is_empty()))
        || result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind != QueryDiagnosticKind::CompletionWarning)
    {
        Err(QueryErrorKind::BackendFailure)
    } else if !result.diagnostics.is_empty() {
        Err(QueryErrorKind::BackendWarning)
    } else if result.evidence.locally_truncated {
        Err(QueryErrorKind::RowLimit)
    } else {
        Ok(())
    }
}

fn inline_error(row: &Value) -> bool {
    row.get("OneApiErrors")
        .and_then(Value::as_array)
        .is_some_and(|errors| {
            !errors.is_empty()
                && errors.iter().all(|entry| {
                    entry
                        .get("error")
                        .and_then(|error| error.get("code"))
                        .and_then(Value::as_str)
                        .is_some_and(|code| !code.trim().is_empty())
                })
        })
}

struct CompletionColumns {
    level: usize,
    payload: usize,
    request_id: usize,
}

impl CompletionColumns {
    fn new(columns: &[QueryColumn]) -> Result<Self, QueryErrorKind> {
        let index = |name: &str, native_type: &str| {
            let mut matches = columns
                .iter()
                .enumerate()
                .filter(|(_, col)| col.name == name);
            let (index, column) = matches.next().ok_or(QueryErrorKind::UnsupportedResponse)?;
            if matches.next().is_some() || column.native_type != native_type {
                return Err(QueryErrorKind::UnsupportedResponse);
            }
            Ok(index)
        };
        Ok(Self {
            level: index("Level", "int")?,
            payload: index("Payload", "string")?,
            request_id: index("ClientRequestId", "string")?,
        })
    }

    fn classify(&self, row: &[Value]) -> Result<Option<QueryDiagnosticKind>, QueryErrorKind> {
        if !row[self.payload].is_string() || !row[self.request_id].is_string() {
            return Err(QueryErrorKind::InvalidResponse);
        }
        match row[self.level].as_i64() {
            Some(0..=2) => Ok(Some(QueryDiagnosticKind::CompletionError)),
            Some(3) => Ok(Some(QueryDiagnosticKind::CompletionWarning)),
            Some(4..=6) => Ok(None),
            Some(_) => Err(QueryErrorKind::UnsupportedResponse),
            None => Err(QueryErrorKind::InvalidResponse),
        }
    }
}

fn table_header(frame: &Value, limits: QueryLimits) -> Result<QueryDataset, QueryErrorKind> {
    let invalid = QueryErrorKind::InvalidResponse;
    let table_id = frame["TableId"].as_u64().ok_or(invalid)?;
    let name = frame["TableName"].as_str().ok_or(invalid)?;
    frame["TableKind"].as_str().ok_or(invalid)?;
    let columns = frame["Columns"].as_array().ok_or(invalid)?;
    if columns.len() > limits.max_columns() {
        return Err(QueryErrorKind::ResponseLimit);
    }
    let columns = columns
        .iter()
        .map(|column| {
            Ok(QueryColumn {
                name: column["ColumnName"].as_str().ok_or(invalid)?.into(),
                native_type: column["ColumnType"].as_str().ok_or(invalid)?.into(),
            })
        })
        .collect::<Result<Vec<_>, QueryErrorKind>>()?;
    Ok(QueryDataset {
        ordinal: 0,
        table_id,
        name: name.into(),
        columns,
        rows: Vec::new(),
    })
}
