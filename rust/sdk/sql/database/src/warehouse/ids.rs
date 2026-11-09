// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::WarehouseError;
use crate::{Guid, WorkspaceId};
use serde::{Deserialize, Deserializer, de::Error};
use std::{fmt, str::FromStr};

/// A concrete Warehouse GUID, never a SQLDatabase or logical ID.
#[derive(Clone, Copy, Deserialize, Eq, Hash, PartialEq)]
#[serde(transparent)]
pub struct WarehouseId(Guid);

impl FromStr for WarehouseId {
    type Err = WarehouseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse()
            .map(Self)
            .map_err(|_| WarehouseError::InvalidInput)
    }
}

impl fmt::Display for WarehouseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for WarehouseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WarehouseId(<redacted>)")
    }
}

/// Exact owner coordinates supplied by a resolver, not authorization evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WarehouseTarget {
    workspace_id: WorkspaceId,
    warehouse_id: WarehouseId,
}

impl WarehouseTarget {
    /// Binds native IDs to the exact Warehouse discriminator.
    ///
    /// # Errors
    /// Rejects SQLDatabase, SQLEndpoint, snapshots and other owner types.
    pub fn new(
        workspace_id: WorkspaceId,
        warehouse_id: WarehouseId,
        item_type: &str,
    ) -> Result<Self, WarehouseError> {
        if item_type != "Warehouse" {
            return Err(WarehouseError::WrongOwner);
        }
        Ok(Self {
            workspace_id,
            warehouse_id,
        })
    }

    /// The explicit workspace owner.
    pub const fn workspace_id(self) -> WorkspaceId {
        self.workspace_id
    }

    /// The concrete Warehouse item owner.
    pub const fn warehouse_id(self) -> WarehouseId {
        self.warehouse_id
    }
}

/// Opaque native restore-point key, not a timestamp, UUID or operation handle.
#[derive(Clone, Eq, PartialEq)]
pub struct RestorePointId(String);

impl RestorePointId {
    /// Validates a single native path key without interpreting its contents.
    ///
    /// # Errors
    /// Rejects empty, traversal, separator and control-bearing keys.
    pub fn new(value: impl Into<String>) -> Result<Self, WarehouseError> {
        let value = value.into();
        if value.is_empty()
            || matches!(value.as_str(), "." | "..")
            || value.contains(['/', '\\'])
            || value.chars().any(char::is_control)
        {
            return Err(WarehouseError::InvalidInput);
        }
        Ok(Self(value))
    }

    /// Exact decoded service key; request construction encodes it once.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RestorePointId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

impl fmt::Debug for RestorePointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RestorePointId(<redacted>)")
    }
}
