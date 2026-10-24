// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Read-only Fabric folder operations with caller-injected credentials and execution.
//!
//! No identity discovery, runtime, transport, mutable context or CLI is installed.
//! See the crate README for the pinned Python contract and explicit safety exceptions.

#![doc = include_str!("../README.md")]

mod folders;
pub mod models;

pub use fabric_core::{CredentialSelection, TokenCredential};
pub use fabric_sdk_items::{
    ItemExecutionError as FolderExecutionError, ItemResponse as FolderResponse,
};
pub use fabric_sdk_workspaces::{FabricServiceError, Page};
pub use folders::{
    FolderError, FolderErrorKind, FolderExecutor, FolderGetOptions, FolderList, FolderListError,
    FolderListLimits, FolderListOptions, FolderQueryValue, FolderRequest,
};
pub use models::Folder;
use std::{fmt, sync::Arc};

/// A focused folder client, independent of the SDK facade and identity backend.
///
/// The caller must bind the credential to the explicitly selected account/tenant.
#[derive(Clone)]
pub struct FolderClient {
    credential: Arc<dyn TokenCredential>,
    selection: CredentialSelection,
}

impl FolderClient {
    /// Stores the provider and selection without requesting a token or doing I/O.
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

impl fmt::Debug for FolderClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FolderClient")
            .field("credential", &"<injected>")
            .field("selection", &self.selection)
            .finish()
    }
}
