// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Independent Fabric SDK scaffold with no identity implementation or CLI dependency.
//!
//! Construction does not request tokens or contact a service. Workspace listing
//! uses caller-injected credentials, execution, and cancellation; no production
//! transport, identity backend, retry policy, or runtime is installed.

pub mod models;
pub mod workspaces;

pub use fabric_core::{CredentialSelection, TokenCredential};
use serde::{Deserialize, Serialize};
use std::{fmt, sync::Arc};

/// A service client boundary with explicitly injected credentials.
///
/// The provider must be configured for the selected account and tenant.
/// Selection metadata does not validate token claims or bind a provider to a tenant.
#[derive(Clone)]
pub struct Client {
    credential: Arc<dyn TokenCredential>,
    selection: CredentialSelection,
}

impl Client {
    /// Stores a caller-provided credential and selection without performing I/O.
    pub fn new(credential: Arc<dyn TokenCredential>, selection: CredentialSelection) -> Self {
        Self {
            credential,
            selection,
        }
    }

    /// Returns the injected provider; token acquisition remains explicitly caller-driven.
    pub fn credential(&self) -> &Arc<dyn TokenCredential> {
        &self.credential
    }

    /// Returns the explicit selection, without probing the identity provider.
    pub fn selection(&self) -> &CredentialSelection {
        &self.selection
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("credential", &"<injected>")
            .field("selection", &self.selection)
            .finish()
    }
}

/// A single Fabric-style JSON page, not an automatic page iterator.
///
/// Continuation values are opaque service data. In particular, a future transport
/// must not follow a continuation URI without validating its origin and bounds.
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

#[cfg(test)]
mod tests {
    use super::*;

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
