// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{EngineBinding, QueryError, QueryErrorKind, QueryLimits};
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
#[derive(Clone, Serialize)]
pub struct QueryResult {
    /// Ordered primary tables.
    pub datasets: Vec<QueryDataset>,
    /// Native non-primary tables/diagnostics, never appended as data rows.
    pub metadata: Vec<Value>,
    /// Native terminal status frame.
    pub completion: Value,
    /// Protected evidence, kept separately from data projection.
    pub evidence: QueryEvidence,
}

impl fmt::Debug for QueryResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueryResult")
            .field("datasets", &self.datasets.len())
            .field("metadata", &self.metadata.len())
            .field("evidence", &self.evidence)
            .finish()
    }
}

pub(crate) fn decode(
    bytes: &[u8],
    limits: QueryLimits,
    mut evidence: QueryEvidence,
) -> Result<QueryResult, QueryError> {
    let frames: Vec<Value> = serde_json::from_slice(bytes)
        .map_err(|_| QueryError::observed(QueryErrorKind::InvalidResponse, evidence.clone()))?;
    if frames.len() > limits.max_frames() {
        return Err(QueryError::observed(
            QueryErrorKind::ResponseLimit,
            evidence,
        ));
    }
    if frames.len() < 2 {
        return Err(QueryError::observed(
            QueryErrorKind::InvalidResponse,
            evidence,
        ));
    }
    let header = &frames[0];
    if header["FrameType"] != "DataSetHeader"
        || header["Version"] != "v2.0"
        || header["IsProgressive"].as_bool() != Some(false)
    {
        return Err(QueryError::observed(
            QueryErrorKind::UnsupportedResponse,
            evidence,
        ));
    }
    let completion = frames
        .last()
        .expect("validated nonempty frame array")
        .clone();
    let has_errors = completion["HasErrors"].as_bool();
    let cancelled = completion["Cancelled"].as_bool();
    if completion["FrameType"] != "DataSetCompletion"
        || has_errors.is_none()
        || cancelled.is_none()
        || completion
            .get("OneApiErrors")
            .is_some_and(|errors| !errors.is_array())
    {
        return Err(QueryError::observed(
            QueryErrorKind::InvalidResponse,
            evidence,
        ));
    }
    let mut datasets = Vec::new();
    let mut metadata = Vec::new();
    let mut ids = BTreeSet::new();
    let mut remaining = limits.max_rows();
    for frame in &frames[1..frames.len() - 1] {
        if frame["FrameType"] != "DataTable" {
            return Err(QueryError::observed(
                QueryErrorKind::UnsupportedResponse,
                evidence,
            ));
        }
        let table =
            table(frame, limits).map_err(|kind| QueryError::observed(kind, evidence.clone()))?;
        if !ids.insert(table.table_id) {
            return Err(QueryError::observed(
                QueryErrorKind::InvalidResponse,
                evidence,
            ));
        }
        if frame["TableKind"] == "PrimaryResult" {
            if datasets.len() == limits.max_tables() {
                return Err(QueryError::observed(
                    QueryErrorKind::ResponseLimit,
                    evidence,
                ));
            }
            let mut table = table;
            table.ordinal = datasets.len();
            if table.rows.len() > remaining {
                table.rows.truncate(remaining);
                evidence.locally_truncated = true;
            }
            remaining -= table.rows.len();
            datasets.push(table);
        } else {
            metadata.push(frame.clone());
        }
    }
    evidence.completed = has_errors == Some(false)
        && cancelled == Some(false)
        && !evidence.locally_truncated
        && completion
            .get("OneApiErrors")
            .is_none_or(|errors| errors.as_array().is_some_and(Vec::is_empty));
    let result = QueryResult {
        datasets,
        metadata,
        completion,
        evidence,
    };
    if cancelled == Some(true) {
        Err(QueryError::with_partial(
            QueryErrorKind::BackendCancelled,
            result,
        ))
    } else if has_errors == Some(true)
        || result
            .completion
            .get("OneApiErrors")
            .is_some_and(|errors| errors.as_array().is_some_and(|errors| !errors.is_empty()))
    {
        Err(QueryError::with_partial(
            QueryErrorKind::BackendFailure,
            result,
        ))
    } else if result.evidence.locally_truncated {
        Err(QueryError::with_partial(QueryErrorKind::RowLimit, result))
    } else {
        Ok(result)
    }
}

fn table(frame: &Value, limits: QueryLimits) -> Result<QueryDataset, QueryErrorKind> {
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
    let rows = frame["Rows"]
        .as_array()
        .ok_or(invalid)?
        .iter()
        .map(|row| {
            let row = row.as_array().ok_or(invalid)?;
            if row.len() != columns.len() {
                return Err(invalid);
            }
            Ok(row.clone())
        })
        .collect::<Result<Vec<_>, QueryErrorKind>>()?;
    Ok(QueryDataset {
        ordinal: 0,
        table_id,
        name: name.into(),
        columns,
        rows,
    })
}
