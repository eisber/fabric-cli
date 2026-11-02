// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit local metadata and filesystem capabilities for physical resource copy.

use fabric_cli_context::{ContextError, LocalAccess, LocalLookup, LocalMetadata, LocalProbe};
use fabric_cli_mutations::{CommandFuture, MutationError};
use fabric_cli_storage_files::LocalFileSystem;
use std::{path::Path, sync::Arc};

/// One caller-owned local classification, distinct from permission to copy.
///
/// The metadata owner requests this snapshot only after Fabric resolution
/// selects local fallback. Its filesystem remains confined to the exact input.
pub struct ResourceLocalSnapshot {
    /// Exact original input, basename, opaque identity and actual object kind.
    pub metadata: LocalMetadata,
    /// Whether the original path exists.
    pub exists: bool,
    /// Whether the selected input is readable by the source permission policy.
    pub readable: bool,
    /// Source OS dirname, without Fabric normalization.
    pub directory: String,
    /// Source dirname write predicate.
    pub directory_writable: bool,
    /// Source existence check for an empty dirname.
    pub empty_path_exists: bool,
    /// Source boolean-as-file-descriptor existence, indexed by false/true.
    pub descriptor_exists: [bool; 2],
    /// Deferred filesystem capability for this alias, never a directory tree.
    pub files: Arc<dyn LocalFileSystem>,
}

impl LocalLookup for ResourceLocalSnapshot {
    fn exists(&self, probe: LocalProbe<'_>) -> Result<bool, ContextError> {
        match probe {
            LocalProbe::Path(path) if path == self.metadata.path => Ok(self.exists),
            LocalProbe::Path("") => Ok(self.empty_path_exists),
            LocalProbe::Boolean(value) => Ok(self.descriptor_exists[usize::from(value)]),
            _ => Err(local_snapshot_error()),
        }
    }

    fn access(&self, path: &str, mode: LocalAccess) -> Result<bool, ContextError> {
        match mode {
            LocalAccess::Read if path == self.metadata.path => Ok(self.readable),
            LocalAccess::Write if path == self.directory => Ok(self.directory_writable),
            _ => Err(local_snapshot_error()),
        }
    }

    fn dirname(&self, path: &str) -> Result<String, ContextError> {
        if path != self.metadata.path {
            return Err(local_snapshot_error());
        }
        Ok(self.directory.clone())
    }

    fn describe(&self, path: &str) -> Result<LocalMetadata, ContextError> {
        if path != self.metadata.path {
            return Err(local_snapshot_error());
        }
        Ok(self.metadata.clone())
    }
}

fn local_snapshot_error() -> ContextError {
    ContextError::lookup("InvalidContext", "Local metadata belongs to another input")
}

/// Asynchronous local classification supplied by the physical-filesystem owner.
///
/// Implementations must not reinterpret service, permission, paging or
/// cancellation failures as local paths. The caller invokes this only when
/// the existing path resolver requests a local fallback snapshot.
pub trait ResourceFileInputs: Send + Sync {
    /// Load one explicit input's metadata and its exact-alias filesystem policy.
    fn snapshot<'a>(
        &'a self,
        input: &'a str,
    ) -> CommandFuture<'a, Result<ResourceLocalSnapshot, MutationError>>;
}

/// Bind a native candidate to the caller's captured directory without probing it.
///
/// This helper never reads or changes the process working directory.
pub fn selected_local_path(base: &Path, input: &str) -> Result<std::path::PathBuf, MutationError> {
    if !base.is_absolute() || input.is_empty() || input.contains('\0') {
        return Err(MutationError::new(
            "InvalidPath",
            "Invalid explicit local input",
        ));
    }
    let path = Path::new(input);
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(MutationError::new(
            "InvalidPath",
            "Local input contains a parent traversal",
        ));
    }
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    })
}
