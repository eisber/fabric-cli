// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{KqlError, KqlErrorKind, Observed, PageCoverage};
use crate::v2::ItemIdentity;
use serde::{Deserialize, Deserializer, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::fmt;

/// Native database type; future values remain unknown rather than read/write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DatabaseKind {
    /// Native read/write database, not proof of the caller's write permissions.
    ReadWrite,
    /// Database shortcut/follower, not a OneLake table shortcut.
    Shortcut,
    /// Unrecognized native discriminator, never coerced to a supported kind.
    Unknown(String),
}

impl<'de> Deserialize<'de> for DatabaseKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match String::deserialize(deserializer)?.as_str() {
            "ReadWrite" => Self::ReadWrite,
            "Shortcut" => Self::Shortcut,
            value => Self::Unknown(value.into()),
        })
    }
}

/// Observed metadata, not trusted network destinations or authentication scopes.
#[derive(Clone, Eq, PartialEq)]
pub struct KqlProperties {
    /// Parent Eventhouse ID, independent of the KQLDatabase concrete identity.
    pub parent_eventhouse_item_id: Observed<String>,
    /// Native query URI; validation occurs only on explicit context resolution.
    pub query_service_uri: Observed<String>,
    /// Native ingestion URI; never derived from the query URI.
    pub ingestion_service_uri: Observed<String>,
    /// Native kind, with unknown discriminators preserved.
    pub database_type: Observed<DatabaseKind>,
}

impl<'de> Deserialize<'de> for KqlProperties {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = Map::<String, Value>::deserialize(deserializer)?;
        Ok(Self {
            parent_eventhouse_item_id: property(&mut fields, "parentEventhouseItemId")?,
            query_service_uri: property(&mut fields, "queryServiceUri")?,
            ingestion_service_uri: property(&mut fields, "ingestionServiceUri")?,
            database_type: property(&mut fields, "databaseType")?,
        })
    }
}

fn property<T: DeserializeOwned, E: serde::de::Error>(
    fields: &mut Map<String, Value>,
    key: &str,
) -> Result<Observed<T>, E> {
    match fields.remove(key) {
        None => Ok(Observed::Missing),
        Some(value) => {
            serde_json::from_value(value).map_err(|_| E::custom("invalid KQL database property"))
        }
    }
}

impl fmt::Debug for KqlProperties {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KqlProperties(<observed metadata>)")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fields {
    id: String,
    workspace_id: String,
    display_name: String,
    #[serde(rename = "type")]
    owner_type: String,
    #[serde(default)]
    description: Observed<String>,
    #[serde(default)]
    properties: Observed<KqlProperties>,
}

/// Complete native JSON plus a validated read-only KQLDatabase projection.
///
/// Unknown fields, nulls and parsed ordering remain in `data`; known fields are
/// type-checked. Returned URI strings remain inert until explicit resolution.
#[derive(Clone)]
pub struct KqlDatabase {
    identity: ItemIdentity,
    display_name: String,
    description: Observed<String>,
    properties: Observed<KqlProperties>,
    data: Value,
    pub(super) context: crate::RequestContext,
}

impl KqlDatabase {
    pub(super) fn decode(
        data: Value,
        expected: &ItemIdentity,
        context: &crate::RequestContext,
    ) -> Result<Self, KqlError> {
        if !data.is_object() {
            return Err(KqlError::new(KqlErrorKind::InvalidResponse));
        }
        let fields: Fields = serde_json::from_value(data.clone())
            .map_err(|_| KqlError::new(KqlErrorKind::InvalidResponse))?;
        if fields.owner_type != "KQLDatabase"
            || !fields.id.eq_ignore_ascii_case(expected.item_id())
            || !fields
                .workspace_id
                .eq_ignore_ascii_case(expected.workspace_id())
        {
            return Err(KqlError::new(KqlErrorKind::TargetMismatch));
        }
        Ok(Self {
            identity: expected.clone(),
            display_name: fields.display_name,
            description: fields.description,
            properties: fields.properties,
            data,
            context: context.clone(),
        })
    }

    /// Exact selected workspace/item, checked against the metadata response.
    pub fn identity(&self) -> &ItemIdentity {
        &self.identity
    }

    /// Observed display name; not an automatically selected native database.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Observed description, preserving missing versus null.
    pub fn description(&self) -> &Observed<String> {
        &self.description
    }

    /// Observed native properties; missing/null never fabricate a binding.
    pub fn properties(&self) -> &Observed<KqlProperties> {
        &self.properties
    }

    /// Complete native metadata for explicit authorized projection.
    pub fn data(&self) -> &Value {
        &self.data
    }
}

impl fmt::Debug for KqlDatabase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KqlDatabase(<validated metadata>)")
    }
}

/// One native LIST page; no page traversal or complete-inventory assertion.
pub struct KqlDatabasePage {
    /// Rows in service order; duplicate identities are retained.
    pub value: Vec<KqlDatabase>,
    /// Original decoded continuation, preserving missing/null/empty.
    pub continuation_token: Observed<String>,
    /// Original URI metadata; never automatically followed.
    pub continuation_uri: Observed<String>,
    pub(super) data: Value,
}

impl KqlDatabasePage {
    /// A next-page indication is incomplete inventory, not a stable snapshot.
    pub fn coverage(&self) -> PageCoverage {
        let active =
            |field: &Observed<String>| matches!(field, Observed::Value(value) if !value.is_empty());
        if active(&self.continuation_token) || active(&self.continuation_uri) {
            PageCoverage::Incomplete
        } else {
            PageCoverage::TerminalPage
        }
    }

    /// Complete decoded page metadata, including unknown fields.
    pub fn data(&self) -> &Value {
        &self.data
    }
}

impl fmt::Debug for KqlDatabasePage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KqlDatabasePage")
            .field("rows", &self.value.len())
            .field("coverage", &self.coverage())
            .finish_non_exhaustive()
    }
}
