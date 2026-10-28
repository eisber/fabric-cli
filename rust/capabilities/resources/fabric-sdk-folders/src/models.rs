// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Folder wire data before path reconstruction, caching or CLI projection.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A folder with opaque service identifiers and unmodified metadata.
///
/// Only `id` and `displayName` must be strings. `parentFolderId` stays raw:
/// absent, null, string and future values remain distinguishable. The SDK does
/// not reconstruct a tree, discard orphans, sort rows or deduplicate identifiers.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    /// Unmodified service identifier, not constrained to UUID syntax.
    pub id: String,
    /// Unmodified name, including case, Unicode and path punctuation.
    pub display_name: String,
    /// Parent, service-specific and future fields, including explicit nulls.
    #[serde(flatten)]
    pub additional_fields: BTreeMap<String, Value>,
}

impl Folder {
    /// Returns the raw parent field, preserving absent versus explicit null.
    pub fn parent_folder_id(&self) -> Option<&Value> {
        self.additional_fields.get("parentFolderId")
    }
}
