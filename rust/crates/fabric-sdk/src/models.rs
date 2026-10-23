// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Wire models for the bounded workspace-list operation.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A workspace returned by Fabric, before CLI filtering or name/path projection.
///
/// IDs and workspace types remain opaque strings, as in the Python source.
/// Unknown types must not make an otherwise valid service page disappear.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    /// Service workspace identifier, without imposing a new UUID restriction.
    pub id: String,
    /// Unmodified service display name, including Unicode and slash characters.
    pub display_name: String,
    /// Unmodified service type; the CLI separately recognizes Workspace and Personal.
    #[serde(rename = "type")]
    pub workspace_type: String,
    /// Other service fields, preserving missing versus null and future properties.
    ///
    /// Includes fields such as `capacityId` and `description` used by consumers.
    #[serde(flatten)]
    pub additional_fields: BTreeMap<String, Value>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Page;
    use serde_json::json;

    #[test]
    fn workspace_preserves_opaque_values_and_optional_field_presence() {
        let value = json!({
            "id": "opaque-id",
            "displayName": "Research/\u{65e5}\u{672c}",
            "type": "FutureWorkspace",
            "capacityId": null,
            "futureProperty": {"value": 1}
        });
        let workspace: Workspace = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(workspace.workspace_type, "FutureWorkspace");
        assert_eq!(workspace.additional_fields["capacityId"], Value::Null);
        assert!(!workspace.additional_fields.contains_key("description"));
        assert_eq!(serde_json::to_value(workspace).unwrap(), value);
    }

    #[test]
    fn required_workspace_fields_do_not_default_to_empty_values() {
        for value in [
            json!({"value": [{}]}),
            json!({"value": [{"id": "a", "displayName": "A"}]}),
            json!({"value": [{"id": null, "displayName": "A", "type": "Workspace"}]}),
            json!({"value": [{"id": "a", "displayName": 1, "type": "Workspace"}]}),
        ] {
            assert!(serde_json::from_value::<Page<Workspace>>(value).is_err());
        }
    }

    #[test]
    fn structured_error_preserves_details_and_request_id() {
        let value = json!({
            "errorCode": "WorkspaceDenied",
            "message": "Service message",
            "requestId": "offline-request",
            "moreDetails": [{"errorCode": "Detail", "message": "detail"}]
        });
        let error: FabricServiceError = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(error.error_code, "WorkspaceDenied");
        assert_eq!(serde_json::to_value(error).unwrap(), value);
    }
}
