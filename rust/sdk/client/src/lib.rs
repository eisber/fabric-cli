// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Fabric SDK facade with no identity implementation or CLI dependency.
//!
//! Construction does not request tokens or contact a service. Workspace listing
//! and item, capacity, folder and Notebook operations use injected execution and
//! cancellation; no production transport, identity backend, retry policy, or
//! runtime is installed.

pub mod capacities;
pub mod dataagents;
pub mod documents;
pub mod folders;
pub mod items;
pub mod livy;
#[cfg(feature = "livy-arrow")]
pub mod livy_arrow;
pub mod metadata;
pub mod models;
pub mod notebook_observability;
pub mod notebook_runtime;
pub mod notebooks;
pub mod observe;
pub mod representations;
pub mod runtime;
pub mod workspaces;

/// Read-only model analysis contracts; .NET activation is separately feature-gated.
pub use fabric_sdk_semantic_model as semantic_model;
/// Local operation contracts and schema projections, with no identity or service I/O.
pub mod discovery {
    pub use fabric_sdk_discovery::*;
}
/// Bounded, caller-composed catalog and relevance search with source evidence.
pub mod search {
    pub use fabric_sdk_search::*;
}
/// Caller-injected request execution used by catalog and relevance clients.
pub mod api {
    pub use fabric_sdk_api::*;
}
/// Native Catalog requests and bounded metadata pages.
pub mod catalog {
    pub use fabric_sdk_catalog::*;
}
/// Direct Relevance Engine discovery using caller-injected service execution.
pub mod relevance {
    pub use fabric_sdk_relevance::*;
}
/// DataAgent-owned Ontology attachment and source-guidance workflows.
pub use fabric_sdk_dataagent_ontology as dataagent_ontology;
/// Public Ontology metadata/model/native-definition operations.
pub use fabric_sdk_ontology as ontology;
/// Explicitly caller-injected private Ontology workload operations.
pub use fabric_sdk_ontology_workload as ontology_workload;
/// Typed SQLDatabase metadata requests using caller-owned shared execution.
pub use fabric_sdk_sql_database as sql_databases;
/// Transport-independent native SQLDatabase definition validation and encoding.
pub use fabric_sdk_sql_definition as sql_definitions;
/// Host-neutral v2 registration, preparation and receipt contracts.
///
/// CLI/MCP consumers use this SDK-layer reexport rather than acquiring a direct
/// dependency on core implementation crates.
pub use fabric_v2_contracts as v2;

pub use fabric_core::{
    Audience, CredentialSelection, EndpointBinding, EndpointError, RequestContext, ServiceOrigin,
    TokenCredential,
};
pub use fabric_sdk_workspaces::Page;
use std::{fmt, sync::Arc};

/// A service client boundary with explicitly injected credentials.
///
/// The provider must be configured for the selected account and tenant.
/// Selection metadata does not validate token claims or bind a provider to a tenant.
#[derive(Clone)]
pub struct Client {
    context: RequestContext,
}

impl Client {
    /// Stores a caller-provided credential and selection without performing I/O.
    pub fn new(credential: Arc<dyn TokenCredential>, selection: CredentialSelection) -> Self {
        Self::with_context(RequestContext::new(credential, selection))
    }

    /// Uses explicit request configuration for context-enabled service methods.
    ///
    /// Workspace, item, folder and capacity reads share this binding. Other
    /// facade families retain public defaults until their separate migration.
    pub fn with_context(context: RequestContext) -> Self {
        Self { context }
    }

    /// Returns the immutable caller-owned request configuration.
    pub fn context(&self) -> &RequestContext {
        &self.context
    }

    /// Returns the injected provider; token acquisition remains explicitly caller-driven.
    pub fn credential(&self) -> &Arc<dyn TokenCredential> {
        self.context.credential()
    }

    /// Returns the explicit selection, without probing the identity provider.
    pub fn selection(&self) -> &CredentialSelection {
        self.context.selection()
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("credential", &"<injected>")
            .field("context", &self.context)
            .finish()
    }
}

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
