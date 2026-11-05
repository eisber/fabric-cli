// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{FolderId, Guid, SqlDatabaseId, WorkspaceId};
use serde::{
    Deserialize, Deserializer,
    de::{DeserializeOwned, Error},
};
use serde_json::{Map, Value};
use std::fmt;

/// Preserves absence, explicit null and a typed value in service metadata.
#[derive(Clone, Eq, PartialEq)]
pub enum Observed<T> {
    /// The service omitted the field.
    Missing,
    /// The service explicitly returned null.
    Null,
    /// The service returned a correctly typed value.
    Value(T),
}

impl<T> Default for Observed<T> {
    fn default() -> Self {
        Self::Missing
    }
}

impl<T> fmt::Debug for Observed<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "Missing",
            Self::Null => "Null",
            Self::Value(_) => "Value(<redacted>)",
        })
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Observed<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

/// Inert connection-string metadata, not an authenticated or trusted SQL binding.
#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(transparent)]
pub struct ConnectionStringMetadata(String);

impl ConnectionStringMetadata {
    /// Returns explicit service data without parsing, connecting or acquiring tokens.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ConnectionStringMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConnectionStringMetadata(<redacted>)")
    }
}

/// Named service properties, projected without granting connection capability.
///
/// Missing connection fields never fall back to the display name. Timestamp
/// strings remain observed metadata, not validated restore inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlDatabaseProperties {
    /// Authoritative service catalog metadata when present.
    pub database_name: Observed<String>,
    /// Inert service host metadata, potentially including a port.
    pub server_fqdn: Observed<String>,
    /// Inert provider connection string; Debug is redacted.
    pub connection_string: Observed<ConnectionStringMetadata>,
    /// Service-provided earliest restore timestamp, without a restore promise.
    pub earliest_restore_point: Observed<String>,
    /// Service-provided latest restore timestamp, without a restore promise.
    pub latest_restore_point: Observed<String>,
    /// Native int32 metadata; no create-input range or default is imposed.
    pub backup_retention_days: Observed<i32>,
    /// Service collation, not a selectable mutation in this crate.
    pub collation: Observed<String>,
}

impl<'de> Deserialize<'de> for SqlDatabaseProperties {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = object_fields(deserializer)?;
        Ok(Self {
            database_name: optional_field::<_, D::Error>(&mut fields, "databaseName")?,
            server_fqdn: optional_field::<_, D::Error>(&mut fields, "serverFqdn")?,
            connection_string: optional_field::<_, D::Error>(&mut fields, "connectionString")?,
            earliest_restore_point: optional_field::<_, D::Error>(
                &mut fields,
                "earliestRestorePoint",
            )?,
            latest_restore_point: optional_field::<_, D::Error>(&mut fields, "latestRestorePoint")?,
            backup_retention_days: optional_field::<_, D::Error>(
                &mut fields,
                "backupRetentionDays",
            )?,
            collation: optional_field::<_, D::Error>(&mut fields, "collation")?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SqlDatabaseKind {
    SqlDatabase,
}

impl<'de> Deserialize<'de> for SqlDatabaseKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "SQLDatabase" => Ok(Self::SqlDatabase),
            _ => Err(D::Error::custom("invalid SQLDatabase item type")),
        }
    }
}

/// Minimal typed SQLDatabase metadata, not a lossless raw-metadata round trip.
///
/// Unknown response fields are ignored for forward-compatible projection.
/// Known field types and required concrete identity are checked. No returned
/// field is accepted as mutation input; this crate implements no mutations.
#[derive(Clone, Eq, PartialEq)]
pub struct SqlDatabase {
    /// Concrete service item identity.
    pub id: SqlDatabaseId,
    /// Owning workspace, checked against the selected request.
    pub workspace_id: WorkspaceId,
    /// Unmodified service display name, never used as a catalog fallback.
    pub display_name: String,
    kind: SqlDatabaseKind,
    /// Optional description with omission/null preserved.
    pub description: Observed<String>,
    /// Optional folder, never inferred from another field.
    pub folder_id: Observed<FolderId>,
    /// Optional logical ID, never substituted for the concrete item.
    pub logical_id: Observed<Guid>,
    /// Optional properties; absence/null do not synthesize a SQL binding.
    pub properties: Observed<SqlDatabaseProperties>,
}

impl<'de> Deserialize<'de> for SqlDatabase {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = object_fields(deserializer)?;
        Ok(Self {
            id: required_field::<_, D::Error>(&mut fields, "id")?,
            workspace_id: required_field::<_, D::Error>(&mut fields, "workspaceId")?,
            display_name: required_field::<_, D::Error>(&mut fields, "displayName")?,
            kind: required_field::<_, D::Error>(&mut fields, "type")?,
            description: optional_field::<_, D::Error>(&mut fields, "description")?,
            folder_id: optional_field::<_, D::Error>(&mut fields, "folderId")?,
            logical_id: optional_field::<_, D::Error>(&mut fields, "logicalId")?,
            properties: optional_field::<_, D::Error>(&mut fields, "properties")?,
        })
    }
}

impl SqlDatabase {
    /// Returns the exact validated native item discriminator.
    pub const fn item_type(&self) -> &'static str {
        match &self.kind {
            SqlDatabaseKind::SqlDatabase => "SQLDatabase",
        }
    }
}

impl fmt::Debug for SqlDatabase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqlDatabase")
            .field("metadata", &"<redacted>")
            .finish()
    }
}

/// Service-reported traversal state for a single page, not a snapshot guarantee.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageCoverage {
    /// Neither returned continuation field contains a nonempty string.
    TerminalPage,
    /// A nonempty continuation token or URI remains; enumeration is incomplete.
    Incomplete,
}

/// One typed service page; it never follows a continuation URI or fetches a batch.
#[derive(Clone, Eq, PartialEq)]
pub struct SqlDatabasePage {
    /// Complete decoded rows from this response in service order, with duplicates.
    pub value: Vec<SqlDatabase>,
    /// Opaque service token, preserving absent/null/empty; never logged implicitly.
    pub continuation_token: Observed<String>,
    /// Inert service URI, never followed by this crate.
    pub continuation_uri: Observed<String>,
}

impl<'de> Deserialize<'de> for SqlDatabasePage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = object_fields(deserializer)?;
        Ok(Self {
            value: required_field::<_, D::Error>(&mut fields, "value")?,
            continuation_token: optional_field::<_, D::Error>(&mut fields, "continuationToken")?,
            continuation_uri: optional_field::<_, D::Error>(&mut fields, "continuationUri")?,
        })
    }
}

impl SqlDatabasePage {
    /// Reports whether this page leaves a continuation unconsumed.
    ///
    /// Even a terminal page is not a guarantee of transactional enumeration or
    /// complete CLI list behavior. No pagination is performed by this method.
    pub fn coverage(&self) -> PageCoverage {
        let active =
            |field: &Observed<String>| matches!(field, Observed::Value(value) if !value.is_empty());
        if active(&self.continuation_token) || active(&self.continuation_uri) {
            PageCoverage::Incomplete
        } else {
            PageCoverage::TerminalPage
        }
    }
}

impl fmt::Debug for SqlDatabasePage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqlDatabasePage")
            .field("rows", &self.value.len())
            .field("coverage", &self.coverage())
            .finish()
    }
}

fn object_fields<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Map<String, Value>, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Object(fields) => Ok(fields),
        _ => Err(D::Error::custom("expected SQLDatabase metadata object")),
    }
}

fn required_field<T: DeserializeOwned, E: Error>(
    fields: &mut Map<String, Value>,
    name: &'static str,
) -> Result<T, E> {
    let value = fields
        .remove(name)
        .ok_or_else(|| E::custom("missing required SQLDatabase metadata field"))?;
    serde_json::from_value(value).map_err(|_| E::custom("invalid SQLDatabase metadata field type"))
}

fn optional_field<T: DeserializeOwned, E: Error>(
    fields: &mut Map<String, Value>,
    name: &'static str,
) -> Result<Observed<T>, E> {
    match fields.remove(name) {
        None => Ok(Observed::Missing),
        Some(value) => serde_json::from_value(value)
            .map_err(|_| E::custom("invalid SQLDatabase metadata field type")),
    }
}
