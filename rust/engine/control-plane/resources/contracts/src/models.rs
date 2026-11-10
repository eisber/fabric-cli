// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared paging and service-error wire models.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A single Fabric-style JSON page, not an automatic page iterator.
///
/// Continuation values are opaque service data. A transport must not follow a
/// continuation URI without validating its origin and traversal bounds.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    /// Items in this page.
    pub value: Vec<T>,
    /// Opaque service continuation token, when another page exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_token: Option<String>,
    /// Service-provided next-page URI; this type never fetches it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_uri: Option<String>,
}

/// Fabric's structured error response, retained as data rather than log text.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FabricServiceError {
    /// Service error identifier.
    pub error_code: String,
    /// Service-provided message. Treat it as untrusted data when displaying it.
    pub message: String,
    /// Optional details, request ID, resource information, and future fields.
    #[serde(flatten)]
    pub additional_fields: BTreeMap<String, Value>,
}
