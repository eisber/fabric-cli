// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{Completeness, Limits, QueryErrorKind, QueryOutcome, protocol};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::value::RawValue;
use std::{collections::BTreeSet, fmt};
use uuid::Uuid;

/// Authoritative ordered column; duplicate names and unknown engine types survive.
#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Column {
    /// Exact provider column name, including empty or duplicate names.
    pub name: String,
    /// Exact native engine type; never inferred from row values.
    pub engine_type: String,
    /// Portable logical type declared by the resource.
    pub logical_type: String,
    /// Explicit wire encoding declared by the resource.
    pub encoding: String,
}

impl fmt::Debug for Column {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Column(<redacted>)")
    }
}

/// Native cell representation. Numbers retain their JSON lexeme, without `f64`.
#[derive(Clone, PartialEq)]
pub enum NativeValue {
    /// Explicit JSON null; no value or type inference.
    Null,
    /// Signed or unsigned exact integer text.
    Integer(String),
    /// Exact decimal JSON number text, including scale and exponent.
    Decimal(String),
    /// Native approximate numeric type, with its original JSON number text.
    Number(String),
    /// JSON boolean.
    Boolean(bool),
    /// Exact text, including unknown native engine types explicitly encoded as text.
    Text(String),
    /// ISO 8601 date, preserving its original spelling.
    Date(String),
    /// ISO 8601 datetime, preserving any original offset.
    DateTime(String),
    /// ISO 8601 datetime with a required offset, never normalized to UTC.
    DateTimeOffset(String),
    /// ISO 8601 time, preserving fractional digits.
    Time(String),
    /// Native constant-format duration text.
    Duration(String),
    /// GUID text, preserving its original spelling.
    Guid(String),
    /// Validated base64 text; no string coercion of arbitrary bytes.
    Binary(String),
}

impl fmt::Debug for NativeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeValue(<redacted>)")
    }
}

/// One versioned query result; positional rows retain empty/all-null schemas.
pub struct QueryResource {
    /// Opaque embedded identifier only; the SDK never downloads this URI.
    pub uri: String,
    /// Validated schema version (currently 1).
    pub schema_version: u32,
    /// Execution identity matched against `fabric.queryResult`.
    pub result_id: String,
    /// Ordered authoritative columns, without deduplication.
    pub columns: Vec<Column>,
    /// Positional native rows with validated arity.
    pub rows: Vec<Vec<NativeValue>>,
    /// Validated returned row count.
    pub row_count: usize,
    /// Explicit server truncation evidence.
    pub truncated: bool,
    /// Server-recorded cap, or no recorded evidence.
    pub max_rows: Option<usize>,
    /// Exact total, or unknown (never fabricated from the returned count).
    pub total_row_count: Option<u64>,
}

impl fmt::Debug for QueryResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueryResource")
            .field("columns", &self.columns.len())
            .field("rows", &self.rows.len())
            .field("truncated", &self.truncated)
            .finish_non_exhaustive()
    }
}

/// A retained non-rowset embedded template resource; URI is never dereferenced.
pub struct EmbeddedResource {
    /// Opaque resource identifier.
    pub uri: String,
    /// Declared media type, not guessed from a file extension.
    pub mime_type: String,
    /// Original resource text, if it is a text resource.
    pub text: Option<String>,
    /// Original validated base64, if it is a blob resource.
    pub blob: Option<String>,
    /// Original resource metadata, retained as JSON text without interpretation.
    pub metadata: Option<String>,
}

impl fmt::Debug for EmbeddedResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EmbeddedResource(<redacted>)")
    }
}

/// Validated typed data and retained auxiliary resources, without local export.
pub struct TypedQueryResult {
    /// Every typed resultset, in content order. No implicit first-set selection.
    pub resultsets: Vec<QueryResource>,
    /// Every auxiliary embedded template resource, in content order.
    pub resources: Vec<EmbeddedResource>,
    /// Original service prose, retained but never interpreted as typed data.
    pub messages: Vec<String>,
    /// Explicit aggregate completeness.
    pub completeness: Completeness,
    /// Actual negotiated MCP protocol, not an assumed server version.
    pub protocol_version: String,
}

impl fmt::Debug for TypedQueryResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypedQueryResult")
            .field("resultsets", &self.resultsets.len())
            .field("resources", &self.resources.len())
            .field("completeness", &self.completeness)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolResult {
    content: Vec<Box<RawValue>>,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    task: Option<Box<RawValue>>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Content {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "resource")]
    Resource { resource: Resource },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Resource {
    uri: String,
    mime_type: String,
    text: Option<String>,
    blob: Option<String>,
    #[serde(rename = "_meta")]
    meta: Option<Box<RawValue>>,
}

#[derive(Deserialize)]
struct ResourceMeta {
    #[serde(rename = "fabric.queryResult")]
    query_result: Option<Box<RawValue>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Rowset {
    schema_version: u32,
    result_id: String,
    columns: Vec<Column>,
    row_count: usize,
    truncated: bool,
    #[serde(deserialize_with = "required_nullable")]
    max_rows: Option<usize>,
    #[serde(deserialize_with = "required_nullable")]
    total_row_count: Option<u64>,
    rows: Vec<Vec<Box<RawValue>>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Descriptor {
    schema_version: u32,
    result_id: String,
    columns: Vec<Column>,
    row_count: usize,
    truncated: bool,
    #[serde(deserialize_with = "required_nullable")]
    max_rows: Option<usize>,
    #[serde(deserialize_with = "required_nullable")]
    total_row_count: Option<u64>,
    format: String,
    nulls: Vec<[usize; 2]>,
}

pub(crate) fn decode(
    raw: &RawValue,
    limits: Limits,
    requested_rows: usize,
    protocol_version: String,
) -> Result<QueryOutcome, QueryErrorKind> {
    let tool: ToolResult = protocol::parse(raw.get())?;
    if tool.is_error {
        return Err(QueryErrorKind::ToolError);
    }
    if tool.task.is_some() {
        return Err(QueryErrorKind::Protocol);
    }
    if tool.content.len() > limits.content_blocks {
        return Err(QueryErrorKind::Limit);
    }
    let mut result = TypedQueryResult {
        resultsets: Vec::new(),
        resources: Vec::new(),
        messages: Vec::new(),
        completeness: Completeness::Complete,
        protocol_version,
    };
    let (mut uris, mut ids) = (BTreeSet::new(), BTreeSet::new());
    let (mut rows, mut cells, mut bytes) = (0_usize, 0_usize, 0_usize);
    for block in tool.content {
        match protocol::parse(block.get())? {
            Content::Text { text } => result.messages.push(text),
            Content::Resource { resource } => {
                if resource.uri.is_empty()
                    || resource.uri.len() > 4096
                    || resource.uri.chars().any(char::is_control)
                    || resource.mime_type.len() > 256
                    || resource.text.is_some() == resource.blob.is_some()
                {
                    return Err(QueryErrorKind::InvalidTypedResource);
                }
                if !uris.insert(resource.uri.clone()) {
                    return Err(QueryErrorKind::DuplicateResource);
                }
                bytes = bytes
                    .checked_add(resource.text.as_ref().map_or(0, String::len))
                    .and_then(|n| n.checked_add(resource.blob.as_ref().map_or(0, String::len)))
                    .and_then(|n| {
                        n.checked_add(resource.meta.as_ref().map_or(0, |v| v.get().len()))
                    })
                    .ok_or(QueryErrorKind::Limit)?;
                if bytes > limits.resource_bytes {
                    return Err(QueryErrorKind::Limit);
                }
                let descriptor = match &resource.meta {
                    Some(meta) => protocol::parse::<ResourceMeta>(meta.get())?.query_result,
                    None => None,
                };
                if let Some(descriptor) = descriptor {
                    if resource.mime_type != "application/json" || resource.blob.is_some() {
                        return Err(QueryErrorKind::UnsupportedRepresentation);
                    }
                    let text = resource
                        .text
                        .as_ref()
                        .ok_or(QueryErrorKind::InvalidTypedResource)?;
                    let rowset = typed_resource(
                        resource.uri,
                        text,
                        descriptor.get(),
                        limits,
                        requested_rows,
                    )?;
                    if !ids.insert(rowset.result_id.to_ascii_lowercase()) {
                        return Err(QueryErrorKind::DuplicateResource);
                    }
                    rows += rowset.rows.len();
                    cells += rowset.rows.len() * rowset.columns.len();
                    if rows > limits.rows
                        || cells > limits.cells
                        || result.resultsets.len() == limits.resultsets
                    {
                        return Err(QueryErrorKind::Limit);
                    }
                    if rowset.truncated {
                        result.completeness = Completeness::Truncated;
                    }
                    result.resultsets.push(rowset);
                } else {
                    if matches!(
                        resource.mime_type.as_str(),
                        "text/csv"
                            | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                    ) {
                        return Err(QueryErrorKind::MissingTypedJson);
                    }
                    if let Some(blob) = &resource.blob {
                        STANDARD
                            .decode(blob)
                            .map_err(|_| QueryErrorKind::InvalidTypedResource)?;
                    }
                    result.resources.push(EmbeddedResource {
                        uri: resource.uri,
                        mime_type: resource.mime_type,
                        text: resource.text,
                        blob: resource.blob,
                        metadata: resource.meta.map(|meta| meta.get().to_owned()),
                    });
                }
            }
        }
    }
    if result.resultsets.is_empty() {
        return Err(QueryErrorKind::MissingTypedJson);
    }
    Ok(match result.completeness {
        Completeness::Complete => QueryOutcome::Complete(result),
        Completeness::Truncated => QueryOutcome::Incomplete(result),
    })
}

fn typed_resource(
    uri: String,
    text: &str,
    descriptor: &str,
    limits: Limits,
    requested_rows: usize,
) -> Result<QueryResource, QueryErrorKind> {
    let invalid = QueryErrorKind::InvalidTypedResource;
    let rowset: Rowset = protocol::parse(text).map_err(|_| invalid)?;
    let meta: Descriptor = protocol::parse(descriptor).map_err(|_| invalid)?;
    if rowset.columns.len() > limits.columns
        || rowset.rows.len() > limits.rows
        || rowset.rows.len().saturating_mul(rowset.columns.len()) > limits.cells
    {
        return Err(QueryErrorKind::Limit);
    }
    if rowset.schema_version != 1
        || rowset.result_id.len() != 36
        || Uuid::parse_str(&rowset.result_id).map_or(true, |id| id.is_nil())
        || rowset.columns.len() > limits.columns
        || rowset.rows.len() > limits.rows.min(requested_rows)
        || rowset.rows.len().saturating_mul(rowset.columns.len()) > limits.cells
        || rowset.rows.len() != rowset.row_count
        || rowset
            .max_rows
            .is_some_and(|cap| cap == 0 || cap > requested_rows || cap < rowset.row_count)
        || rowset.total_row_count.is_some_and(|total| {
            total < rowset.row_count as u64
                || (!rowset.truncated && total != rowset.row_count as u64)
                || (rowset.truncated && total == rowset.row_count as u64)
        })
        || rowset.schema_version != meta.schema_version
        || rowset.result_id != meta.result_id
        || rowset.columns != meta.columns
        || rowset.row_count != meta.row_count
        || rowset.truncated != meta.truncated
        || rowset.max_rows != meta.max_rows
        || rowset.total_row_count != meta.total_row_count
        || meta.format != "json"
    {
        return Err(invalid);
    }
    for column in &rowset.columns {
        if column.name.len() > 32_000
            || column.engine_type.is_empty()
            || column.engine_type.len() > 32_000
        {
            return Err(invalid);
        }
        validate_representation(column)?;
    }
    let mut nulls = Vec::new();
    let mut rows = Vec::with_capacity(rowset.rows.len());
    for (row_index, row) in rowset.rows.into_iter().enumerate() {
        if row.len() != rowset.columns.len() {
            return Err(invalid);
        }
        let mut values = Vec::with_capacity(row.len());
        for (index, (raw, column)) in row.into_iter().zip(&rowset.columns).enumerate() {
            let value = cell(&raw, column)?;
            if value == NativeValue::Null {
                nulls.push([row_index, index]);
            }
            values.push(value);
        }
        rows.push(values);
    }
    if nulls != meta.nulls {
        return Err(invalid);
    }
    Ok(QueryResource {
        uri,
        schema_version: rowset.schema_version,
        result_id: rowset.result_id,
        columns: rowset.columns,
        rows,
        row_count: rowset.row_count,
        truncated: rowset.truncated,
        max_rows: rowset.max_rows,
        total_row_count: rowset.total_row_count,
    })
}

fn validate_representation(column: &Column) -> Result<(), QueryErrorKind> {
    match (column.logical_type.as_str(), column.encoding.as_str()) {
        ("integer" | "decimal" | "number", "json-number")
        | ("boolean", "json-boolean")
        | ("date" | "datetime" | "datetimeoffset" | "time", "iso8601")
        | ("duration" | "guid" | "string", "text")
        | ("binary", "base64") => Ok(()),
        _ => Err(QueryErrorKind::UnsupportedRepresentation),
    }
}

fn cell(raw: &RawValue, column: &Column) -> Result<NativeValue, QueryErrorKind> {
    let invalid = QueryErrorKind::InvalidTypedResource;
    let text = raw.get();
    if text == "null" {
        return Ok(NativeValue::Null);
    }
    if text.len() > 32_000 * 6 + 2 {
        return Err(QueryErrorKind::Limit);
    }
    let logical = column.logical_type.as_str();
    if matches!(logical, "integer" | "decimal" | "number") {
        if !text
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_digit() || *b == b'-')
            || text.len() > 32_000
        {
            return Err(invalid);
        }
        return match logical {
            "integer" => {
                if text.contains(['.', 'e', 'E']) {
                    return Err(invalid);
                }
                let native = column.engine_type.to_ascii_lowercase();
                let range = match native.as_str() {
                    "byte" | "tinyint" => Some((0, u8::MAX as i128)),
                    "sbyte" => Some((i8::MIN as i128, i8::MAX as i128)),
                    "int16" | "smallint" | "short" => Some((i16::MIN as i128, i16::MAX as i128)),
                    "uint16" => Some((0, u16::MAX as i128)),
                    "int32" | "int" | "integer" => Some((i32::MIN as i128, i32::MAX as i128)),
                    "uint32" => Some((0, u32::MAX as i128)),
                    "uint64" => Some((0, u64::MAX as i128)),
                    "int64" | "bigint" | "long" => Some((i64::MIN as i128, i64::MAX as i128)),
                    _ => None,
                };
                if let Some((minimum, maximum)) = range {
                    let value = text.parse::<i128>().map_err(|_| invalid)?;
                    if !(minimum..=maximum).contains(&value) {
                        return Err(invalid);
                    }
                }
                Ok(NativeValue::Integer(text.into()))
            }
            "decimal" => Ok(NativeValue::Decimal(text.into())),
            _ => Ok(NativeValue::Number(text.into())),
        };
    }
    if logical == "boolean" {
        return serde_json::from_str::<bool>(text)
            .map(NativeValue::Boolean)
            .map_err(|_| invalid);
    }
    let text: String = serde_json::from_str(text).map_err(|_| invalid)?;
    if text.encode_utf16().count() > 32_000 {
        return Err(QueryErrorKind::Limit);
    }
    let value = match logical {
        "string" => NativeValue::Text(text),
        "guid" if text.len() == 36 && Uuid::parse_str(&text).is_ok() => NativeValue::Guid(text),
        "binary" if STANDARD.decode(&text).is_ok() => NativeValue::Binary(text),
        "date" if chrono::NaiveDate::parse_from_str(&text, "%Y-%m-%d").is_ok() => {
            NativeValue::Date(text)
        }
        "datetimeoffset" if datetime(&text, true) => NativeValue::DateTimeOffset(text),
        "datetime" if datetime(&text, false) => NativeValue::DateTime(text),
        "time" if chrono::NaiveTime::parse_from_str(&text, "%H:%M:%S%.f").is_ok() => {
            NativeValue::Time(text)
        }
        "duration" if duration(&text) => NativeValue::Duration(text),
        _ => return Err(invalid),
    };
    Ok(value)
}

fn datetime(text: &str, offset_required: bool) -> bool {
    chrono::DateTime::parse_from_rfc3339(text).is_ok()
        || (!offset_required
            && chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").is_ok())
}

fn duration(text: &str) -> bool {
    let text = text.strip_prefix('-').unwrap_or(text);
    let Some((hours, rest)) = text.split_once(':') else {
        return false;
    };
    let hours = match hours.split_once('.') {
        Some((days, hours)) if !days.is_empty() && days.bytes().all(|b| b.is_ascii_digit()) => {
            hours
        }
        Some(_) => return false,
        None => hours,
    };
    let Some((minutes, seconds)) = rest.split_once(':') else {
        return false;
    };
    let (seconds, fraction) = match seconds.split_once('.') {
        Some((_, "")) => return false,
        Some(parts) => parts,
        None => (seconds, ""),
    };
    [hours, minutes, seconds]
        .into_iter()
        .all(|s| s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit()))
        && hours.parse::<u8>().is_ok_and(|n| n < 24)
        && minutes.parse::<u8>().is_ok_and(|n| n < 60)
        && seconds.parse::<u8>().is_ok_and(|n| n < 60)
        && fraction.len() <= 7
        && fraction.bytes().all(|b| b.is_ascii_digit())
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}
