// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Raw capacity rows before CLI hierarchy, sorting, or enrichment.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A capacity returned by Fabric, not an ARM capacity resource.
///
/// Only the two fields directly indexed by Python's hierarchy consumer are
/// required. Optional service fields preserve missing, null, and future values.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capacity {
    /// Opaque identifier, with no invented UUID or ARM-resource restriction.
    pub id: String,
    /// Unmodified name, including case, Unicode, and path-like characters.
    pub display_name: String,
    /// All other properties, including optional SKU, state, and region.
    #[serde(flatten)]
    pub additional_fields: BTreeMap<String, Value>,
}

impl Capacity {
    /// Returns the exact display name used by hierarchy and enrichment consumers.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Returns the raw state, preserving unknown enum values and explicit null.
    pub fn state(&self) -> Option<&Value> {
        self.additional_fields.get("state")
    }

    /// Returns the raw region without introducing a default or case conversion.
    pub fn region(&self) -> Option<&Value> {
        self.additional_fields.get("region")
    }

    /// Returns the raw SKU; Fabric-only filtering belongs to the caller.
    pub fn sku(&self) -> Option<&Value> {
        self.additional_fields.get("sku")
    }
}
