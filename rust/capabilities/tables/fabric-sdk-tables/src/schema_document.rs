// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Additive data-schema documents over the existing Delta JSON-log reader.
//!
//! These documents describe observed native metadata, not JSON Schema, an
//! operation-input contract, or proof of a complete/current Delta snapshot.
//! Existing v1 schema extraction and field projections remain unchanged.

use crate::{TableClient, TableError, TableErrorKind, TableExecutor, TableLimits, TablePath};
use fabric_core_json::{DecimalInteger, PythonValue};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::future::Future;

/// The schema category; operation-input contracts are not data schemas.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DataSchemaKind {
    /// Native backend data metadata.
    Data,
}

/// The existing physical metadata backend, not an inferred SQL/model provider.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DataSchemaBackend {
    /// Metadata extracted from Delta JSON commit files through OneLake.
    DeltaJsonLog,
}

/// One top-level column in native field order.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DataColumn {
    /// Unmodified native field name.
    pub name: String,
    /// Unmodified native type string or object, serialized as `type`.
    ///
    /// Nested types and unknown type properties are not flattened or translated.
    #[serde(rename = "type")]
    pub native_type: Value,
    /// Native nullability; missing or null metadata is unknown, serialized null.
    pub nullable: Option<bool>,
}

/// Limits of the supported extraction, independent of successful JSON decoding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DataSchemaCoverage {
    /// True: the reader searches JSON commits only.
    pub json_commits_only: bool,
    /// False: checkpoint state is not reconstructed.
    pub checkpoints_reconstructed: bool,
    /// False: structural projection does not validate the native type grammar.
    pub native_semantics_validated: bool,
}

/// Evidence available from the existing reader or caller-decoded native schema.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DataSchemaProvenance {
    /// Unavailable from this API; serialized null, never inferred from a filename.
    pub source_revision: Option<String>,
    /// False: listing and reads do not establish an atomic/current snapshot.
    pub snapshot_consistency_established: bool,
}

/// Versioned, lossless native schema with a typed top-level column projection.
///
/// Constructors set `complete` to unknown, not true or false. A successful read
/// identifies metadata in the scanned JSON commits, not the current complete
/// table state. Checkpoint-only/pruned logs can fail to supply metadata, and
/// concurrent changes are not reconciled. Deserializing a document does not
/// independently attest its provenance or coverage claims.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DataSchemaDocument {
    /// Envelope version, the literal JSON number `1`.
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    /// Always `data` for documents constructed by this API.
    pub schema_kind: DataSchemaKind,
    /// Always `delta-json-log` for documents constructed by this API.
    pub backend: DataSchemaBackend,
    /// Top-level columns in original field order.
    pub columns: Vec<DataColumn>,
    /// Entire decoded schema, including metadata, nested and unknown properties.
    ///
    /// This preserves JSON values, not the original JSON string's whitespace.
    pub native_schema: Value,
    /// Unknown completeness, serialized null; this API makes no completeness claim.
    pub complete: Option<bool>,
    /// Explicit backend and validation limitations.
    pub coverage: DataSchemaCoverage,
    /// Explicitly unavailable revision and unestablished snapshot consistency.
    pub provenance: DataSchemaProvenance,
}

impl DataSchemaDocument {
    /// Project caller-decoded native Delta metadata without parsing it again.
    ///
    /// CLI consumers can apply their strict JSON decoder first. The input must
    /// be a native schema object with a `fields` array; an optional root `type`
    /// must be `struct`. Each field must be an object with a nonempty string
    /// name, a native type, and optional boolean/null nullability. A native type
    /// is a nonempty string or an object with a nonempty string `type` tag.
    ///
    /// Type names and nested type properties remain opaque: this is structural
    /// validation, not native semantic or recursive type-grammar validation.
    /// Callers must reject unsupported raw integer tokens before conversion to
    /// `Value`; this method cannot recover an already-rounded input value.
    /// No source revision, timestamp, snapshot or permission is inferred from
    /// caller-supplied values. An empty `fields` array is valid.
    ///
    /// # Errors
    /// Returns [`TableErrorKind::InvalidResponse`] for malformed schema or field
    /// shapes, names, type representations or nullability, never empty success.
    pub fn from_native_schema(native_schema: Value) -> Result<Self, TableError> {
        let schema = native_schema.as_object().ok_or_else(invalid_response)?;
        if schema
            .get("type")
            .is_some_and(|kind| kind.as_str() != Some("struct"))
        {
            return Err(invalid_response());
        }
        let fields = schema
            .get("fields")
            .and_then(Value::as_array)
            .ok_or_else(invalid_response)?;
        let columns = fields
            .iter()
            .map(|field| {
                let field = field.as_object().ok_or_else(invalid_response)?;
                let name = field
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .ok_or_else(invalid_response)?;
                let native_type = field.get("type").ok_or_else(invalid_response)?;
                let type_name = match native_type {
                    Value::String(name) => Some(name.as_str()),
                    Value::Object(object) => object.get("type").and_then(Value::as_str),
                    _ => None,
                };
                if type_name.is_none_or(str::is_empty) {
                    return Err(invalid_response());
                }
                let nullable = match field.get("nullable") {
                    None | Some(Value::Null) => None,
                    Some(Value::Bool(value)) => Some(*value),
                    Some(_) => return Err(invalid_response()),
                };
                Ok(DataColumn {
                    name: name.to_owned(),
                    native_type: native_type.clone(),
                    nullable,
                })
            })
            .collect::<Result<Vec<_>, TableError>>()?;
        Ok(Self {
            schema_version: 1,
            schema_kind: DataSchemaKind::Data,
            backend: DataSchemaBackend::DeltaJsonLog,
            columns,
            native_schema,
            complete: None,
            coverage: DataSchemaCoverage {
                json_commits_only: true,
                checkpoints_reconstructed: false,
                native_semantics_validated: false,
            },
            provenance: DataSchemaProvenance {
                source_revision: None,
                snapshot_consistency_established: false,
            },
        })
    }
}

impl TableClient {
    /// Read native Delta JSON-log metadata and project a typed data-schema document.
    ///
    /// Reuses [`Self::schema_string`] with the caller's executor, credentials,
    /// limits and cancellation. The returned schema string is decoded once.
    /// No separate scanner, pager, transport, checkpoint or SQL fallback is used.
    /// The document exposes unavailable source revision, unestablished snapshot
    /// consistency and unknown completeness, even on a successful read.
    /// Paths containing `.Shortcut` or `.shortcut` are rejected before token
    /// acquisition because the legacy reader removes those substrings.
    ///
    /// # Errors
    /// Preserves all existing read, HTTP, limit, missing-metadata and cancellation
    /// failures. Malformed inner JSON, integer tokens outside the exact i64/u64
    /// domain, or malformed schema structure are
    /// [`TableErrorKind::InvalidResponse`], not a successful empty document.
    /// Normalization-sensitive paths return [`TableErrorKind::InvalidInput`]
    /// rather than selecting a different table.
    pub async fn schema_document(
        &self,
        executor: &dyn TableExecutor,
        table: &TablePath,
        limits: TableLimits,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<DataSchemaDocument, TableError> {
        if table.local_path().contains(".Shortcut") || table.local_path().contains(".shortcut") {
            return Err(TableError::new(TableErrorKind::InvalidInput));
        }
        let schema = self
            .schema_string(executor, table, limits, cancelled)
            .await?;
        let native_schema = decode_native_schema(&schema)?;
        DataSchemaDocument::from_native_schema(native_schema)
    }
}

fn decode_native_schema(schema: &str) -> Result<Value, TableError> {
    let value = serde_json::from_str(schema).map_err(|_| invalid_response())?;
    // Match the existing CLI's quote-aware token boundary before accepting a
    // Value: serde_json otherwise rounds oversized integer tokens into f64.
    let mut quoted = false;
    let mut escaped = false;
    for token in schema.split_inclusive(|character: char| {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            return false;
        }
        if character == '"' {
            quoted = true;
        }
        character.is_ascii_whitespace() || matches!(character, ',' | ']' | '}' | ':' | '[' | '{')
    }) {
        let token = token.trim_matches(|character: char| {
            character.is_ascii_whitespace()
                || matches!(character, ',' | ']' | '}' | ':' | '[' | '{')
        });
        if token.starts_with(|character: char| character == '-' || character.is_ascii_digit())
            && !token.contains(['.', 'e', 'E'])
        {
            let integer = DecimalInteger::parse(token).map_err(|_| invalid_response())?;
            PythonValue::Integer(integer)
                .try_to_json()
                .map_err(|_| invalid_response())?;
        }
    }
    Ok(value)
}

fn invalid_response() -> TableError {
    TableError::new(TableErrorKind::InvalidResponse)
}

fn deserialize_schema_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    match u32::deserialize(deserializer)? {
        1 => Ok(1),
        _ => Err(serde::de::Error::custom("unsupported data-schema version")),
    }
}
