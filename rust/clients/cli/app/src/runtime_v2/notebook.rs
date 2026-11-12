// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit non-secret Notebook binding and connector over the native foundation.

use super::{OwnerKind, OwnerPath, ReadOptions, ResolvedTarget, TypedPath, host::ExplicitResolver};
use fabric_sdk::{
    Audience, CredentialSelection, EndpointBinding, RequestContext,
    notebook_runtime::{NotebookRuntime, content::ContentLimits},
    runtime::{NativeDeadline, NativeError},
};
use fabric_sdk_http::{
    HttpExecutor,
    notebook_workload::{
        NativeNotebookExecutor, NotebookEnvironment, NotebookOrigin, NotebookReadScope,
        NotebookWorkloadSelection,
    },
};
use serde::Deserialize;
use std::{io::Read, path::Path, time::Instant};

/// Maximum encoded non-secret binding file size.
pub const BINDING_BYTES: usize = 16 * 1024;
/// Explicit non-secret binding file selection; never searched or auto-created.
pub const BINDING_VARIABLE: &str = "FAB_RUST_NOTEBOOK_BINDING";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Document {
    schema_version: u32,
    owner: OwnerPath,
    environment: Environment,
    catalog_origin: String,
    cluster_origin: String,
    workload_origin: String,
    capacity_id: String,
    workspace_id: String,
    notebook_id: String,
    account: String,
    tenant: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Environment {
    Production,
    Msit,
    Daily,
    Dxt,
}

/// Validated caller selection, never an inferred provider response or credential.
///
/// Debug output deliberately omits origins, identifiers and identity selectors.
pub struct NotebookBinding {
    owner: OwnerPath,
    identity: CredentialSelection,
    catalog: EndpointBinding,
    selection: NotebookWorkloadSelection,
}

impl std::fmt::Debug for NotebookBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NotebookBinding(<explicit>)")
    }
}

impl NotebookBinding {
    /// Read exactly one bounded regular JSON file on a caller-owned setup worker.
    ///
    /// # Errors
    /// Rejects relative paths, directories, symlinks, unavailable or oversized files
    /// and malformed/inconsistent selections. No file or environment discovery.
    pub fn read(path: &Path, selected: &TypedPath) -> Result<Self, NativeError> {
        if !path.is_absolute() {
            return Err(NativeError::InvalidInput);
        }
        let metadata = path
            .symlink_metadata()
            .map_err(|_| NativeError::InvalidInput)?;
        if !metadata.file_type().is_file() || metadata.len() > BINDING_BYTES as u64 {
            return Err(NativeError::InvalidInput);
        }
        let file = std::fs::File::open(path).map_err(|_| NativeError::InvalidInput)?;
        if !file
            .metadata()
            .map_err(|_| NativeError::InvalidInput)?
            .is_file()
        {
            return Err(NativeError::InvalidInput);
        }
        let mut bytes = Vec::new();
        file.take(BINDING_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| NativeError::InvalidInput)?;
        Self::parse(&bytes, selected)
    }

    /// Parse an exact non-secret binding and verify the selected typed owner.
    ///
    /// # Errors
    /// Rejects unknown fields (including token fields), mismatched owners, invalid
    /// origins, GUIDs, account/tenant selections and unsupported catalogs.
    pub fn parse(bytes: &[u8], selected: &TypedPath) -> Result<Self, NativeError> {
        if bytes.len() > BINDING_BYTES {
            return Err(NativeError::Limit);
        }
        let document: Document =
            serde_json::from_slice(bytes).map_err(|_| NativeError::InvalidInput)?;
        if document.schema_version != 1
            || selected.owner != document.owner
            || selected.owner.kind != OwnerKind::Notebook
        {
            return Err(NativeError::InvalidInput);
        }
        let owner_path = TypedPath::parse(&format!(
            "/{}/{}",
            document.owner.workspace, document.owner.artifact
        ))
        .map_err(|_| NativeError::InvalidInput)?;
        if owner_path.owner != document.owner {
            return Err(NativeError::InvalidInput);
        }
        let identity =
            CredentialSelection::new(document.account, document.tenant, Audience::Fabric)
                .map_err(|_| NativeError::InvalidInput)?;
        let catalog_origin = NotebookOrigin::new(&document.catalog_origin)?;
        let catalog_url = azure_core::http::Url::parse(&document.catalog_origin)
            .map_err(|_| NativeError::InvalidInput)?;
        let catalog = EndpointBinding::with_host(
            fabric_sdk::ServiceOrigin::Fabric,
            catalog_url.host_str().ok_or(NativeError::InvalidInput)?,
        )
        .map_err(|_| NativeError::InvalidInput)?;
        let environment = match document.environment {
            Environment::Production => NotebookEnvironment::Production,
            Environment::Msit => NotebookEnvironment::Msit,
            Environment::Daily => NotebookEnvironment::Daily,
            Environment::Dxt => NotebookEnvironment::Dxt,
        };
        let selection = NotebookWorkloadSelection::new(
            identity.clone(),
            environment,
            catalog_origin,
            NotebookOrigin::new(&document.cluster_origin)?,
            NotebookOrigin::new(&document.workload_origin)?,
            NotebookReadScope::new(
                &document.capacity_id,
                &document.workspace_id,
                &document.notebook_id,
            )?,
        )?;
        Ok(Self {
            owner: document.owner,
            identity,
            catalog,
            selection,
        })
    }

    /// Validate existing native account/tenant/catalog before constructing credentials.
    ///
    /// # Errors
    /// Exact mismatch fails locally; configuration never chooses another identity.
    pub fn check_identity(
        &self,
        identity: &CredentialSelection,
        catalog: &EndpointBinding,
    ) -> Result<(), NativeError> {
        if &self.identity != identity || &self.catalog != catalog {
            return Err(NativeError::InvalidInput);
        }
        Ok(())
    }

    /// Compose actual native authorization and the existing SDK resolver, locally only.
    ///
    /// The caller must preflight output first and explicitly opt its existing
    /// provider into PBI bootstrap. No token, HTTP or returned-origin selection
    /// occurs in this constructor. Acquisition belongs on the async read future.
    ///
    /// # Errors
    /// Rejects context/identity/catalog, budgets or deadline mismatches before I/O.
    pub fn connect(
        self,
        context: RequestContext,
        http: HttpExecutor,
        options: ReadOptions,
        expires: Instant,
    ) -> Result<NotebookConnection, NativeError> {
        self.check_identity(context.selection(), context.endpoint())?;
        options.validate().map_err(|_| NativeError::InvalidInput)?;
        let scope = self.selection.scope();
        let runtime = NotebookRuntime::new(
            self.selection.workload_endpoint(),
            scope.capacity(),
            scope.workspace(),
            scope.notebook(),
            ContentLimits::default(),
            options.max_response_bytes,
            options.max_records as usize,
        )?;
        let resolver = ExplicitResolver::new(self.owner, ResolvedTarget::Notebook(runtime))
            .map_err(|_| NativeError::InvalidInput)?;
        let executor = NativeNotebookExecutor::new(
            context,
            http,
            self.selection,
            NativeDeadline::until(expires)?,
        )?;
        Ok(NotebookConnection { executor, resolver })
    }
}

/// Actual native executor and typed resolver consumed by the existing shared head.
pub struct NotebookConnection {
    /// Verified-principal/MwcToken adapter; never a scripted production placeholder.
    pub executor: NativeNotebookExecutor,
    /// Exact owner binding to the existing Notebook SDK.
    pub resolver: ExplicitResolver,
}
