// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Generic item wire data, before any CLI type filtering or projection.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

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

/// Validated generic item metadata retaining the parsed HTTP object's key order.
///
/// Unlike [`Item`], this representation never separates known fields from extras.
/// Required `id` and `displayName` remain strings; unknown, missing and null fields,
/// nested object order and JSON number types are retained. This preserves parsed
/// JSON data, not original whitespace, escape spelling or numeric token spelling.
/// Debug deliberately excludes all service metadata.
#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ItemMetadata {
    value: Value,
}

impl ItemMetadata {
    /// Returns the validated, unmodified service identifier.
    pub fn id(&self) -> &str {
        self.value["id"]
            .as_str()
            .expect("validated item identifier")
    }

    /// Returns the validated, unmodified service display name.
    pub fn display_name(&self) -> &str {
        self.value["displayName"]
            .as_str()
            .expect("validated item display name")
    }

    /// Returns a string-valued service type without restricting future types.
    ///
    /// Use [`Self::as_value`] to distinguish missing, null and non-string types.
    pub fn item_type(&self) -> Option<&str> {
        self.value.get("type").and_then(Value::as_str)
    }

    /// Borrows the original parsed object without reordering or normalizing it.
    pub fn as_value(&self) -> &Value {
        &self.value
    }

    /// Consumes the metadata without rebuilding its original parsed object.
    pub fn into_value(self) -> Value {
        self.value
    }
}

impl<'de> Deserialize<'de> for ItemMetadata {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if !value.get("id").is_some_and(Value::is_string)
            || !value.get("displayName").is_some_and(Value::is_string)
        {
            return Err(serde::de::Error::custom(
                "item metadata requires string id and displayName",
            ));
        }
        Ok(Self { value })
    }
}

impl fmt::Debug for ItemMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ItemMetadata")
            .field("value", &"<redacted>")
            .finish()
    }
}
