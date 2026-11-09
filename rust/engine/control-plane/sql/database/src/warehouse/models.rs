// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{RestorePointId, WarehouseId, WarehouseTarget};
use crate::{ConnectionStringMetadata, FolderId, Guid, Observed, PageCoverage, WorkspaceId};
use serde::{Deserialize, Deserializer, de::Error};
use serde_json::{Map, Value};
use std::fmt;

/// Data tied to the selected request owner, not fabricated service identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Owned<T> {
    /// Exact workspace/item selected for the request.
    pub owner: WarehouseTarget,
    /// Native response; owner IDs omitted by that API remain omitted.
    pub data: T,
}

/// Native Warehouse properties, not a configured or trusted SQL connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WarehouseProperties {
    /// Inert connection metadata; Debug never discloses it.
    pub connection_string: Observed<ConnectionStringMetadata>,
    /// Native creation string, not a parsed restore timestamp.
    pub created_date: Observed<String>,
    /// Native update string, not a concurrency token.
    pub last_updated_time: Observed<String>,
    /// Native collation; no mutation is implied.
    pub collation_type: Observed<String>,
}

impl<'de> Deserialize<'de> for WarehouseProperties {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = crate::models::object_fields(deserializer)?;
        Ok(Self {
            connection_string: crate::models::optional_field::<_, D::Error>(
                &mut fields,
                "connectionString",
            )?,
            created_date: crate::models::optional_field::<_, D::Error>(&mut fields, "createdDate")?,
            last_updated_time: crate::models::optional_field::<_, D::Error>(
                &mut fields,
                "lastUpdatedTime",
            )?,
            collation_type: crate::models::optional_field::<_, D::Error>(
                &mut fields,
                "collationType",
            )?,
        })
    }
}

/// Forward-compatible Warehouse metadata projection with validated native IDs.
#[derive(Clone, Eq, PartialEq)]
pub struct Warehouse {
    /// Concrete native item identity.
    pub id: WarehouseId,
    /// Native workspace identity, checked against the request.
    pub workspace_id: WorkspaceId,
    /// Display name, never inferred to be a SQL database/catalog.
    pub display_name: String,
    /// Absent, null or native description.
    pub description: Observed<String>,
    /// Native folder identity, not substituted for the item.
    pub folder_id: Observed<FolderId>,
    /// Native logical identity, not substituted for the concrete item.
    pub logical_id: Observed<Guid>,
    /// Native properties without fallback or SQL connection setup.
    pub properties: Observed<WarehouseProperties>,
}

impl<'de> Deserialize<'de> for Warehouse {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = crate::models::object_fields(deserializer)?;
        let kind: String = crate::models::required_field::<_, D::Error>(&mut fields, "type")?;
        if kind != "Warehouse" {
            return Err(D::Error::custom("invalid Warehouse owner type"));
        }
        Ok(Self {
            id: crate::models::required_field::<_, D::Error>(&mut fields, "id")?,
            workspace_id: crate::models::required_field::<_, D::Error>(&mut fields, "workspaceId")?,
            display_name: crate::models::required_field::<_, D::Error>(&mut fields, "displayName")?,
            description: crate::models::optional_field::<_, D::Error>(&mut fields, "description")?,
            folder_id: crate::models::optional_field::<_, D::Error>(&mut fields, "folderId")?,
            logical_id: crate::models::optional_field::<_, D::Error>(&mut fields, "logicalId")?,
            properties: crate::models::optional_field::<_, D::Error>(&mut fields, "properties")?,
        })
    }
}

impl Warehouse {
    /// Exact validated native item type.
    pub const fn item_type(&self) -> &'static str {
        "Warehouse"
    }
}

impl fmt::Debug for Warehouse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Warehouse(<redacted>)")
    }
}

/// Connection API response: inert host/string, not a SQL database name or token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionMetadata {
    /// Exact provider value. A hostname alone is a valid native representation.
    pub connection_string: ConnectionStringMetadata,
}

impl<'de> Deserialize<'de> for ConnectionMetadata {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = crate::models::object_fields(deserializer)?;
        Ok(Self {
            connection_string: crate::models::required_field::<_, D::Error>(
                &mut fields,
                "connectionString",
            )?,
        })
    }
}

/// Restore-point metadata; unknown creation modes and event fields stay data.
#[derive(Clone, Eq, PartialEq)]
pub struct RestorePoint {
    /// Exact native opaque key, not interpreted numerically or sorted as time.
    pub id: RestorePointId,
    /// Native optional name.
    pub display_name: Observed<String>,
    /// Native optional description.
    pub description: Observed<String>,
    /// Native mode, including future modes; not a deletion capability.
    pub creation_mode: Observed<String>,
    /// Native event data with unknown fields preserved, not a retention promise.
    pub creation_details: Observed<Map<String, Value>>,
}

impl<'de> Deserialize<'de> for RestorePoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = crate::models::object_fields(deserializer)?;
        Ok(Self {
            id: crate::models::required_field::<_, D::Error>(&mut fields, "id")?,
            display_name: crate::models::optional_field::<_, D::Error>(&mut fields, "displayName")?,
            description: crate::models::optional_field::<_, D::Error>(&mut fields, "description")?,
            creation_mode: crate::models::optional_field::<_, D::Error>(
                &mut fields,
                "creationMode",
            )?,
            creation_details: crate::models::optional_field::<_, D::Error>(
                &mut fields,
                "creationDetails",
            )?,
        })
    }
}

impl fmt::Debug for RestorePoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RestorePoint(<redacted>)")
    }
}

macro_rules! page {
    ($name:ident, $row:ty, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Eq, PartialEq)]
        pub struct $name {
            /// Native rows in service order, including duplicates.
            pub value: Vec<$row>,
            /// Inert native continuation, preserving missing/null/empty.
            pub continuation_token: Observed<String>,
            /// Inert native URI, never followed by this module.
            pub continuation_uri: Observed<String>,
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let mut fields = crate::models::object_fields(deserializer)?;
                Ok(Self {
                    value: crate::models::required_field::<_, D::Error>(&mut fields, "value")?,
                    continuation_token: crate::models::optional_field::<_, D::Error>(
                        &mut fields,
                        "continuationToken",
                    )?,
                    continuation_uri: crate::models::optional_field::<_, D::Error>(
                        &mut fields,
                        "continuationUri",
                    )?,
                })
            }
        }

        impl $name {
            /// Native page state, not a full-list or transactional snapshot guarantee.
            pub fn coverage(&self) -> PageCoverage {
                let active = |field: &Observed<String>| {
                    matches!(field, Observed::Value(value) if !value.is_empty())
                };
                if active(&self.continuation_token) || active(&self.continuation_uri) {
                    PageCoverage::Incomplete
                } else {
                    PageCoverage::TerminalPage
                }
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($name))
                    .field("rows", &self.value.len())
                    .field("coverage", &self.coverage())
                    .finish()
            }
        }
    };
}

page!(
    WarehousePage,
    Warehouse,
    "One native Warehouse metadata page."
);
page!(
    RestorePointPage,
    RestorePoint,
    "One native restore-point page."
);
