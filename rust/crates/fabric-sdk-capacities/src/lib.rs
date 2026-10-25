// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Focused Fabric capacity listing, without ARM operations or a transport backend.
//!
//! Consumers inject an Azure credential, explicit selection, an executor, and
//! cancellation. Construction performs no I/O and installs no runtime.

pub mod capacities;
pub mod models;

pub use capacities::{
    CapacityExecutionError, CapacityExecutor, CapacityList, CapacityListError,
    CapacityListErrorKind, CapacityListLimits, CapacityRequest, CapacityResponse,
};
pub use fabric_core::{CredentialSelection, TokenCredential};
pub use fabric_sdk_workspaces::{FabricServiceError, Page};
pub use models::Capacity;
use std::{fmt, sync::Arc};

/// A read-only capacity service client with caller-bound credentials.
///
/// Selection describes intent; the caller must bind the provider to that account
/// and tenant. It does not validate token claims or configure the provider.
#[derive(Clone)]
pub struct CapacityClient {
    credential: Arc<dyn TokenCredential>,
    selection: CredentialSelection,
}

impl CapacityClient {
    /// Stores the provider and selection without acquiring a token or doing I/O.
    pub fn new(credential: Arc<dyn TokenCredential>, selection: CredentialSelection) -> Self {
        Self {
            credential,
            selection,
        }
    }

    /// Returns the caller's provider without probing it.
    pub fn credential(&self) -> &Arc<dyn TokenCredential> {
        &self.credential
    }

    /// Returns the explicit selection without changing the provider.
    pub fn selection(&self) -> &CredentialSelection {
        &self.selection
    }
}

impl fmt::Debug for CapacityClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapacityClient")
            .field("credential", &"<injected>")
            .field("selection", &self.selection)
            .finish()
    }
}
