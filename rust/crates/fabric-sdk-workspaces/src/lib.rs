// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Focused Fabric workspace SDK, independent of the SDK facade and identity backend.
//!
//! Workspace listing and ordered enriched metadata use caller-injected Azure
//! credentials, execution, and cancellation. No production transport, retry/LRO
//! policy, ambient credential lookup, or async runtime is installed.

pub mod metadata;
pub mod models;
pub mod workspaces;

pub use fabric_core::{CredentialSelection, TokenCredential};
pub use metadata::{
    WorkspaceEnrichmentOutcome, WorkspaceMetadata, WorkspaceMetadataError,
    WorkspaceMetadataErrorKind, WorkspaceMetadataResult, WorkspaceMetadataStep, WorkspaceOmission,
};
pub use models::{FabricServiceError, Page, Workspace};
use std::{fmt, sync::Arc};
pub use workspaces::{
    WorkspaceExecutionError, WorkspaceExecutor, WorkspaceList, WorkspaceListError,
    WorkspaceListErrorKind, WorkspaceListLimits, WorkspaceRequest, WorkspaceResponse,
};

/// A workspace service client with explicitly injected credentials.
///
/// The provider must be bound by the caller to the selected account and tenant.
/// Selection metadata does not validate token claims or configure the provider.
#[derive(Clone)]
pub struct WorkspaceClient {
    credential: Arc<dyn TokenCredential>,
    selection: CredentialSelection,
}

impl WorkspaceClient {
    /// Stores the caller's credential and selection without token acquisition or I/O.
    pub fn new(credential: Arc<dyn TokenCredential>, selection: CredentialSelection) -> Self {
        Self {
            credential,
            selection,
        }
    }

    /// Returns the injected provider without acquiring a token.
    pub fn credential(&self) -> &Arc<dyn TokenCredential> {
        &self.credential
    }

    /// Returns the explicit selection, without probing or reconfiguring the provider.
    pub fn selection(&self) -> &CredentialSelection {
        &self.selection
    }
}

impl fmt::Debug for WorkspaceClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceClient")
            .field("credential", &"<injected>")
            .field("selection", &self.selection)
            .finish()
    }
}
