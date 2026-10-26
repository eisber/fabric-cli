// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Focused, handwritten Fabric item LIST/GET SDK.
//!
//! Callers inject Azure credentials, account/tenant selection, execution and
//! cancellation. No runtime, transport, identity backend or ambient state is
//! installed. See the crate README for Python source mapping and safety exceptions.

mod collections;
mod items;
pub mod models;
pub mod mutations;

pub use collections::ItemCollection;
pub use fabric_core::{CredentialSelection, RequestContext, TokenCredential};
pub use fabric_sdk_workspaces::{FabricServiceError, Page};
pub use items::{
    ItemError, ItemErrorKind, ItemExecutionError, ItemExecutor, ItemGetOptions, ItemList,
    ItemListError, ItemListLimits, ItemListOptions, ItemRequest, ItemResponse,
};
pub use models::{Item, ItemMetadata};
use std::{fmt, sync::Arc};

/// A generic item service client, independent of the facade and identity backend.
///
/// The caller must bind the injected provider to the selected account and tenant.
/// Selection metadata neither configures the provider nor validates token claims.
#[derive(Clone)]
pub struct ItemClient {
    context: RequestContext,
}

impl ItemClient {
    /// Stores a caller-owned provider and explicit selection without performing I/O.
    pub fn new(credential: Arc<dyn TokenCredential>, selection: CredentialSelection) -> Self {
        Self::with_context(RequestContext::new(credential, selection))
    }

    /// Uses an explicit immutable endpoint and the caller's existing credential.
    pub fn with_context(context: RequestContext) -> Self {
        Self { context }
    }

    /// Returns shared request configuration without acquiring credentials.
    pub fn context(&self) -> &RequestContext {
        &self.context
    }

    /// Returns the injected provider without acquiring a token.
    pub fn credential(&self) -> &Arc<dyn TokenCredential> {
        self.context.credential()
    }

    /// Returns the explicit account, tenant and audience selection.
    pub fn selection(&self) -> &CredentialSelection {
        self.context.selection()
    }
}

impl fmt::Debug for ItemClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ItemClient")
            .field("credential", &"<injected>")
            .field("context", &self.context)
            .finish()
    }
}
