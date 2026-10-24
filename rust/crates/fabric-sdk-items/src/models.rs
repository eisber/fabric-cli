// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Generic item wire data, before any CLI type filtering or projection.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A generic Fabric item with opaque identifiers and unmodified service data.
///
/// Only `id` and `displayName` are required strings. All other fields, including
/// `type`, preserve absent versus null, future values and nested JSON. No enum
/// whitelist, sorting, normalization or client-side filtering is applied.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    /// Unmodified service identifier; no new UUID restriction is imposed.
    pub id: String,
    /// Unmodified service display name, including case, Unicode and separators.
    pub display_name: String,
    /// Type, description, folder, service-specific and future properties.
    #[serde(flatten)]
    pub additional_fields: BTreeMap<String, Value>,
}

impl Item {
    /// Returns a string-valued service type without restricting future types.
    ///
    /// Inspect `additional_fields` to distinguish absent, null and non-string type.
    pub fn item_type(&self) -> Option<&str> {
        self.additional_fields.get("type").and_then(Value::as_str)
    }
}
