// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Focused, handwritten Fabric item LIST/GET SDK.
//!
//! Callers inject Azure credentials, account/tenant selection, execution and
//! cancellation. No runtime, transport, identity backend or ambient state is
//! installed. See the crate README for Python source mapping and safety exceptions.

mod items;
pub mod models;

pub use fabric_core::{CredentialSelection, TokenCredential};
pub use fabric_sdk_workspaces::{FabricServiceError, Page};
pub use items::{
    ItemError, ItemErrorKind, ItemExecutionError, ItemExecutor, ItemGetOptions, ItemList,
    ItemListError, ItemListLimits, ItemListOptions, ItemRequest, ItemResponse,
};
pub use models::Item;
use std::{fmt, sync::Arc};

/// A generic item service client, independent of the facade and identity backend.
///
/// The caller must bind the injected provider to the selected account and tenant.
/// Selection metadata neither configures the provider nor validates token claims.
#[derive(Clone)]
pub struct ItemClient {
    credential: Arc<dyn TokenCredential>,
    selection: CredentialSelection,
}

impl ItemClient {
    /// Stores a caller-owned provider and explicit selection without performing I/O.
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

    /// Returns the explicit account, tenant and audience selection.
    pub fn selection(&self) -> &CredentialSelection {
        &self.selection
    }
}

impl fmt::Debug for ItemClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ItemClient")
            .field("credential", &"<injected>")
            .field("selection", &self.selection)
            .finish()
    }
}
