// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fabric SDK facade with no identity implementation or CLI dependency.
//!
//! Construction does not request tokens or contact a service. Workspace listing
//! and item, capacity, folder and Notebook operations use injected execution and
//! cancellation; no production transport, identity backend, retry policy, or
//! runtime is installed.

pub mod capacities;
pub mod folders;
pub mod items;
pub mod models;
/// Compatibility namespace for the focused Notebook service package.
pub mod notebooks {
    pub use fabric_sdk_notebooks::*;
}
pub mod workspaces;

pub use fabric_core::{
    Audience, CredentialSelection, EndpointBinding, EndpointError, RequestContext, ServiceOrigin,
    TokenCredential,
};
pub use fabric_sdk_workspaces::Page;
mod client;
pub use client::Client;

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct WorkspaceFixture {
        id: String,
        display_name: String,
    }

    #[test]
    fn page_preserves_wire_names_and_opaque_continuation() {
        let json = include_str!("../../../tests/fixtures/workspaces-page.json");
        let page: Page<WorkspaceFixture> = serde_json::from_str(json).unwrap();
        assert_eq!(page.value[0].display_name, "Offline workspace");
        assert_eq!(page.continuation_token.as_deref(), Some("opaque+/=token"));
        assert_eq!(
            page.continuation_uri.as_deref(),
            Some(
                "https://api.fabric.microsoft.com/v1/workspaces?continuationToken=opaque%2B%2F%3Dtoken"
            )
        );
        assert_eq!(
            serde_json::to_value(&page).unwrap(),
            serde_json::from_str::<serde_json::Value>(json).unwrap()
        );
    }

    #[test]
    fn terminal_page_omits_absent_continuation_fields() {
        let page: Page<WorkspaceFixture> = serde_json::from_str(r#"{"value":[]}"#).unwrap();
        assert!(page.continuation_token.is_none());
        assert!(page.continuation_uri.is_none());
        assert_eq!(serde_json::to_string(&page).unwrap(), r#"{"value":[]}"#);
    }

    #[test]
    fn malformed_page_is_not_a_success_shaped_empty_page() {
        for json in [r#"{}"#, r#"{"value":null}"#, r#"{"value":{}}"#] {
            assert!(serde_json::from_str::<Page<WorkspaceFixture>>(json).is_err());
        }
    }
}
