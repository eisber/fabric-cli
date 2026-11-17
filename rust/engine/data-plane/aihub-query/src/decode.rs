// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    Completeness, Limits, QueryErrorKind, QueryOutcome,
    json::{self, RawValue},
    media, protocol,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use std::{collections::BTreeSet, fmt};
use uuid::Uuid;

#[cfg(test)]
std::thread_local! {
    static OWNED_ROWSET_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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
    content: Vec<RawValue>,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    task: Option<Box<RawValue>>,
}

#[derive(Deserialize)]
struct Content {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    resource: Option<Resource>,
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
    rows: Vec<Vec<RawValue>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExpandedDescriptor {
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompactDescriptor {
    schema_version: u32,
    result_id: String,
    format: String,
}

enum Descriptor {
    Compact(CompactDescriptor),
    Expanded(ExpandedDescriptor),
}

#[derive(Clone, Copy)]
enum DescriptorProfile {
    Compact,
    Expanded,
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
        let content: Content = protocol::parse(block.get())?;
        match (content.kind.as_str(), content.text, content.resource) {
            ("text", Some(text), None) => result.messages.push(text),
            ("resource", None, Some(resource)) => {
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
                let media = media::essence(&resource.mime_type)?;
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
                    if !media.eq_ignore_ascii_case("application/json") || resource.blob.is_some() {
                        return Err(QueryErrorKind::UnsupportedRepresentation);
                    }
                    let text = resource
                        .text
                        .as_ref()
                        .ok_or(QueryErrorKind::InvalidTypedResource)?;
                    if result.resultsets.len() == limits.resultsets {
                        return Err(QueryErrorKind::Limit);
                    }
                    let rowset = typed_resource(
                        resource.uri,
                        text,
                        descriptor.get(),
                        Limits {
                            rows: limits.rows - rows,
                            cells: limits.cells - cells,
                            ..limits
                        },
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
                    if media.eq_ignore_ascii_case("text/csv")
                        || media.eq_ignore_ascii_case(
                            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                        )
                    {
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
            _ => return Err(QueryErrorKind::Protocol),
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
    let profile = preflight(text, descriptor, limits, requested_rows)?;
    #[cfg(test)]
    OWNED_ROWSET_DECODES.set(OWNED_ROWSET_DECODES.get() + 1);
    let rowset: Rowset = protocol::parse(text).map_err(|_| invalid)?;
    let meta = match profile {
        DescriptorProfile::Compact => {
            Descriptor::Compact(protocol::parse(descriptor).map_err(|_| invalid)?)
        }
        DescriptorProfile::Expanded => {
            Descriptor::Expanded(protocol::parse(descriptor).map_err(|_| invalid)?)
        }
    };
    let (version, result_id, format) = match &meta {
        Descriptor::Compact(meta) => (
            meta.schema_version,
            meta.result_id.as_str(),
            meta.format.as_str(),
        ),
        Descriptor::Expanded(meta) => (
            meta.schema_version,
            meta.result_id.as_str(),
            meta.format.as_str(),
        ),
    };
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
        || rowset.schema_version != version
        || rowset.result_id != result_id
        || format != "json"
    {
        return Err(invalid);
    }
    if let Descriptor::Expanded(meta) = &meta
        && (rowset.columns != meta.columns
            || rowset.row_count != meta.row_count
            || rowset.truncated != meta.truncated
            || rowset.max_rows != meta.max_rows
            || rowset.total_row_count != meta.total_row_count)
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
    if let Descriptor::Expanded(meta) = &meta
        && nulls != meta.nulls
    {
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

fn preflight(
    text: &str,
    descriptor: &str,
    limits: Limits,
    requested_rows: usize,
) -> Result<DescriptorProfile, QueryErrorKind> {
    let invalid = QueryErrorKind::InvalidTypedResource;
    let body = json::borrowed(text).map_err(|error| {
        if error == QueryErrorKind::Limit {
            error
        } else {
            invalid
        }
    })?;
    let meta = json::borrowed(descriptor).map_err(|error| {
        if error == QueryErrorKind::Limit {
            error
        } else {
            invalid
        }
    })?;
    let profile = descriptor_profile(meta)?;
    let bounded_count = |value: json::BorrowedValue<'_>, cap: usize| {
        let mut count = 0_usize;
        for value in value.elements().map_err(|_| invalid)? {
            value.map_err(|_| invalid)?;
            count = count.checked_add(1).ok_or(QueryErrorKind::Limit)?;
            if count > cap {
                return Err(QueryErrorKind::Limit);
            }
        }
        Ok(count)
    };
    let columns = bounded_count(required_field(body, "columns")?, limits.columns)?;
    if matches!(profile, DescriptorProfile::Expanded)
        && bounded_count(required_field(meta, "columns")?, limits.columns)? != columns
    {
        return Err(invalid);
    }
    let advertised: usize = required_field(body, "rowCount")?
        .deserialize()
        .map_err(|_| invalid)?;
    if advertised > limits.rows.min(requested_rows)
        || advertised
            .checked_mul(columns)
            .ok_or(QueryErrorKind::Limit)?
            > limits.cells
    {
        return Err(QueryErrorKind::Limit);
    }
    if matches!(profile, DescriptorProfile::Expanded) {
        let metadata_rows: usize = required_field(meta, "rowCount")?
            .deserialize()
            .map_err(|_| invalid)?;
        if advertised != metadata_rows {
            return Err(invalid);
        }
    }
    let mut rows = 0_usize;
    let mut cells = 0_usize;
    for row in required_field(body, "rows")?
        .elements()
        .map_err(|_| invalid)?
    {
        rows = rows.checked_add(1).ok_or(QueryErrorKind::Limit)?;
        if rows > limits.rows.min(requested_rows) {
            return Err(QueryErrorKind::Limit);
        }
        let mut arity = 0_usize;
        for cell in row.map_err(|_| invalid)?.elements().map_err(|_| invalid)? {
            cell.map_err(|_| invalid)?;
            cells = cells.checked_add(1).ok_or(QueryErrorKind::Limit)?;
            if cells > limits.cells {
                return Err(QueryErrorKind::Limit);
            }
            arity += 1;
            if arity > columns {
                return Err(invalid);
            }
        }
        if arity != columns {
            return Err(invalid);
        }
    }
    if rows != advertised {
        return Err(invalid);
    }
    if matches!(profile, DescriptorProfile::Expanded) {
        let mut nulls = 0_usize;
        for coordinate in required_field(meta, "nulls")?
            .elements()
            .map_err(|_| invalid)?
        {
            nulls += 1;
            if nulls > cells {
                return Err(QueryErrorKind::Limit);
            }
            let [row, column]: [usize; 2] = coordinate
                .map_err(|_| invalid)?
                .deserialize()
                .map_err(|_| invalid)?;
            if row >= rows || column >= columns {
                return Err(invalid);
            }
        }
    }
    Ok(profile)
}

fn descriptor_profile(meta: json::BorrowedValue<'_>) -> Result<DescriptorProfile, QueryErrorKind> {
    let invalid = QueryErrorKind::InvalidTypedResource;
    for field in ["schemaVersion", "resultId", "format"] {
        required_field(meta, field)?;
    }
    let expanded = [
        "columns",
        "rowCount",
        "truncated",
        "maxRows",
        "totalRowCount",
        "nulls",
    ];
    let mut expanded_present = 0;
    for field in expanded {
        if meta.field(field).map_err(|_| invalid)?.is_some() {
            expanded_present += 1;
        }
    }
    match (meta.object_len().map_err(|_| invalid)?, expanded_present) {
        (3, 0) => Ok(DescriptorProfile::Compact),
        (9, 6) => Ok(DescriptorProfile::Expanded),
        _ => Err(invalid),
    }
}

fn required_field<'a>(
    value: json::BorrowedValue<'a>,
    name: &str,
) -> Result<json::BorrowedValue<'a>, QueryErrorKind> {
    value
        .field(name)
        .map_err(|_| QueryErrorKind::InvalidTypedResource)?
        .ok_or(QueryErrorKind::InvalidTypedResource)
}

fn validate_representation(column: &Column) -> Result<(), QueryErrorKind> {
    match (column.logical_type.as_str(), column.encoding.as_str()) {
        ("integer" | "int64" | "decimal" | "number", "json-number")
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
    if matches!(logical, "integer" | "int64" | "decimal" | "number") {
        if !text
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_digit() || *b == b'-')
            || text.len() > 32_000
        {
            return Err(invalid);
        }
        return match logical {
            "integer" | "int64" => {
                if text.contains(['.', 'e', 'E']) {
                    return Err(invalid);
                }
                let native = column.engine_type.to_ascii_lowercase();
                let range = if logical == "int64" {
                    Some((i64::MIN as i128, i64::MAX as i128))
                } else {
                    match native.as_str() {
                        "byte" | "tinyint" => Some((0, u8::MAX as i128)),
                        "sbyte" => Some((i8::MIN as i128, i8::MAX as i128)),
                        "int16" | "smallint" | "short" => {
                            Some((i16::MIN as i128, i16::MAX as i128))
                        }
                        "uint16" => Some((0, u16::MAX as i128)),
                        "int32" | "int" | "integer" => Some((i32::MIN as i128, i32::MAX as i128)),
                        "uint32" => Some((0, u32::MAX as i128)),
                        "uint64" => Some((0, u64::MAX as i128)),
                        "int64" | "bigint" | "long" => Some((i64::MIN as i128, i64::MAX as i128)),
                        _ => None,
                    }
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

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;
    use serde_json::json;

    const ID: &str = "11111111-1111-4111-8111-111111111111";
    const COLUMN: &str =
        r#"{"name":"n","engineType":"int64","logicalType":"integer","encoding":"json-number"}"#;

    fn body(columns: &str, rows: &str, count: usize, rows_first: bool) -> String {
        let fields = format!(
            r#""schemaVersion":1,"resultId":"{ID}","columns":{columns},"rowCount":{count},"truncated":false,"maxRows":1000,"totalRowCount":null"#
        );
        if rows_first {
            format!(r#"{{"rows":{rows},{fields}}}"#)
        } else {
            format!(r#"{{{fields},"rows":{rows}}}"#)
        }
    }

    fn descriptor(columns: &str, count: usize, nulls: &str, nulls_first: bool) -> String {
        let fields = format!(
            r#""schemaVersion":1,"resultId":"{ID}","columns":{columns},"rowCount":{count},"truncated":false,"maxRows":1000,"totalRowCount":null,"format":"json""#
        );
        if nulls_first {
            format!(r#"{{"nulls":{nulls},{fields}}}"#)
        } else {
            format!(r#"{{{fields},"nulls":{nulls}}}"#)
        }
    }

    fn reject_before_owned_decode(
        body: &str,
        metadata: &str,
        limits: Limits,
        expected: QueryErrorKind,
    ) {
        let raw_before = json::raw_materializations();
        let rowsets_before = OWNED_ROWSET_DECODES.get();
        let error = typed_resource("opaque:test".into(), body, metadata, limits, 1000).unwrap_err();
        assert_eq!(error, expected);
        assert_eq!(
            json::raw_materializations(),
            raw_before,
            "shape rejection must precede every owned raw-cell allocation"
        );
        assert_eq!(
            OWNED_ROWSET_DECODES.get(),
            rowsets_before,
            "shape rejection must precede owned rowset and descriptor deserialization"
        );
    }

    #[test]
    fn two_million_cell_row_is_rejected_before_owned_cells_in_any_field_order() {
        let columns = format!("[{COLUMN}]");
        let mut row = String::with_capacity(10_000_004);
        row.push_str("[[");
        row.push_str(&"null,".repeat(1_999_999));
        row.push_str("null]]");
        assert_eq!(row.matches("null").count(), 2_000_000);
        assert!(row.len() < 16 * 1024 * 1024);
        let limits = Limits {
            cells: 1,
            ..Limits::default()
        };
        let valid = body(&columns, "[[null]]", 1, false);
        let metadata = descriptor(&columns, 1, "[[0,0]]", false);
        let before = json::raw_materializations();
        typed_resource("opaque:control".into(), &valid, &metadata, limits, 1000).unwrap();
        assert_eq!(
            json::raw_materializations() - before,
            1,
            "counter observes real cell ownership"
        );
        for rows_first in [false, true] {
            for nulls_first in [false, true] {
                reject_before_owned_decode(
                    &body(&columns, &row, 1, rows_first),
                    &descriptor(&columns, 1, "[[0,0]]", nulls_first),
                    limits,
                    QueryErrorKind::Limit,
                );
            }
            reject_before_owned_decode(
                &body(&columns, &row, 1, rows_first),
                &format!(r#"{{"schemaVersion":1,"resultId":"{ID}","format":"json"}}"#),
                limits,
                QueryErrorKind::Limit,
            );
        }
    }

    #[test]
    fn schema_rows_null_coordinates_and_arity_are_bounded_before_owned_vectors() {
        let columns = format!("[{COLUMN}]");
        let many_columns = format!(
            "[{}]",
            std::iter::repeat_n(COLUMN, 5000)
                .collect::<Vec<_>>()
                .join(",")
        );
        let metadata = descriptor(&columns, 1, "[[0,0]]", false);
        reject_before_owned_decode(
            &body(&many_columns, "[[null]]", 1, true),
            &metadata,
            Limits {
                columns: 1,
                ..Limits::default()
            },
            QueryErrorKind::Limit,
        );
        reject_before_owned_decode(
            &body(&columns, "[[null]]", 1, true),
            &descriptor(&many_columns, 1, "[[0,0]]", true),
            Limits {
                columns: 1,
                ..Limits::default()
            },
            QueryErrorKind::Limit,
        );
        reject_before_owned_decode(
            &body(&columns, "[[null],[null]]", 1, true),
            &metadata,
            Limits {
                rows: 1,
                ..Limits::default()
            },
            QueryErrorKind::Limit,
        );
        reject_before_owned_decode(
            &body(&columns, "[[null,null]]", 1, true),
            &metadata,
            Limits::default(),
            QueryErrorKind::InvalidTypedResource,
        );
        reject_before_owned_decode(
            &body(&columns, "[[]]", 1, false),
            &metadata,
            Limits::default(),
            QueryErrorKind::InvalidTypedResource,
        );
        reject_before_owned_decode(
            &body(&columns, "[[null]]", 1, false),
            &descriptor(&columns, 1, "[[0,0],[0,0]]", true),
            Limits::default(),
            QueryErrorKind::Limit,
        );
        let many_nulls = format!("[{}]", "[0,0],".repeat(99_999) + "[0,0]");
        reject_before_owned_decode(
            &body(&columns, "[[null]]", 1, true),
            &descriptor(&columns, 1, &many_nulls, true),
            Limits::default(),
            QueryErrorKind::Limit,
        );
    }

    #[test]
    fn aggregate_row_and_cell_budget_prevents_second_resultset_materialization() {
        let columns = format!("[{COLUMN}]");
        let text = body(&columns, "[[null]]", 1, false);
        let metadata: serde_json::Value =
            serde_json::from_str(&descriptor(&columns, 1, "[[0,0]]", false)).unwrap();
        let make_block = |uri: &str| {
            json!({"type":"resource","resource":{"uri":uri,"mimeType":"application/json","text":text,
                "_meta":{"fabric.queryResult":metadata}}})
        };
        let tool =
            json!({"content":[make_block("opaque:first"),make_block("opaque:second")]}).to_string();
        for limits in [
            Limits {
                cells: 1,
                ..Limits::default()
            },
            Limits {
                rows: 1,
                ..Limits::default()
            },
        ] {
            let raw = json::parse::<RawValue>(&tool).unwrap();
            let before = OWNED_ROWSET_DECODES.get();
            let error = decode(&raw, limits, 1000, "2025-06-18".into()).unwrap_err();
            assert_eq!(error, QueryErrorKind::Limit);
            assert_eq!(OWNED_ROWSET_DECODES.get() - before, 1);
        }
    }
}
