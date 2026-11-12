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
        Self::read_checked(path, selected, || {})
    }

    fn read_checked(
        path: &Path,
        selected: &TypedPath,
        before_open: impl FnOnce(),
    ) -> Result<Self, NativeError> {
        if !path.is_absolute() {
            return Err(NativeError::InvalidInput);
        }
        let metadata = path
            .symlink_metadata()
            .map_err(|_| NativeError::InvalidInput)?;
        if !metadata.file_type().is_file() || metadata.len() > BINDING_BYTES as u64 {
            return Err(NativeError::InvalidInput);
        }
        before_open();
        let file = open_no_follow(path).map_err(|_| NativeError::InvalidInput)?;
        let opened = file.metadata().map_err(|_| NativeError::InvalidInput)?;
        if !opened.is_file() || linked(&opened) || opened.len() > BINDING_BYTES as u64 {
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

#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags, open};
    Ok(std::fs::File::from(open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?))
}

#[cfg(windows)]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    // Same OPEN_REPARSE_POINT and SHARE_READ policy as assertion_file.rs.
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const SHARE_READ: u32 = 1;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ)
        .custom_flags(OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_no_follow(_: &Path) -> std::io::Result<std::fs::File> {
    Err(std::io::ErrorKind::Unsupported.into())
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Actual native executor and typed resolver consumed by the existing shared head.
pub struct NotebookConnection {
    /// Verified-principal/MwcToken adapter; never a scripted production placeholder.
    pub executor: NativeNotebookExecutor,
    /// Exact owner binding to the existing Notebook SDK.
    pub resolver: ExplicitResolver,
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;
    use serde_json::json;

    fn document(notebook: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schemaVersion":1,
            "owner":{"workspace":"W.Workspace","artifact":"N.Notebook","kind":"notebook"},
            "environment":"production","catalogOrigin":"https://api.fabric.microsoft.com",
            "clusterOrigin":"https://selected-cluster.analysis.windows.net",
            "workloadOrigin":"https://selected-notebook.fabric.microsoft.com",
            "capacityId":"44444444-4444-4444-4444-444444444444",
            "workspaceId":"55555555-5555-5555-5555-555555555555",
            "notebookId":notebook,
            "account":"22222222-2222-2222-2222-222222222222",
            "tenant":"11111111-1111-1111-1111-111111111111"
        }))
        .unwrap()
    }

    #[cfg(any(unix, windows))]
    fn symlink(target: &Path, path: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, path).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(target, path)
            .expect("native Windows link tests require developer-mode/symlink privilege");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn replacement_after_metadata_cannot_follow_a_foreign_binding_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selected.json");
        let foreign = temp.path().join("foreign.json");
        let selected = TypedPath::parse("/W.Workspace/N.Notebook/src/content").unwrap();
        std::fs::write(&path, document("66666666-6666-6666-6666-666666666666")).unwrap();
        std::fs::write(&foreign, document("77777777-7777-7777-7777-777777777777")).unwrap();
        assert!(NotebookBinding::read(&path, &selected).is_ok());
        let result = NotebookBinding::read_checked(&path, &selected, || {
            std::fs::remove_file(&path).unwrap();
            symlink(&foreign, &path);
        });
        assert!(matches!(result, Err(NativeError::InvalidInput)));
        // No validated binding is published, so no credential/HTTP connector
        // can be constructed from the substituted file.
        assert!(NotebookBinding::read(&path, &selected).is_err());
    }

    #[test]
    fn binding_reader_preserves_regular_file_directory_and_size_guards() {
        let temp = tempfile::tempdir().unwrap();
        let selected = TypedPath::parse("/W.Workspace/N.Notebook/src/content").unwrap();
        assert!(NotebookBinding::read(temp.path(), &selected).is_err());
        assert!(NotebookBinding::read(Path::new("relative.json"), &selected).is_err());
        let path = temp.path().join("selected.json");
        std::fs::write(&path, vec![b' '; BINDING_BYTES + 1]).unwrap();
        assert!(NotebookBinding::read(&path, &selected).is_err());
        std::fs::write(&path, document("66666666-6666-6666-6666-666666666666")).unwrap();
        assert!(NotebookBinding::read(&path, &selected).is_ok());
    }
}
