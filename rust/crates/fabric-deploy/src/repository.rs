// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded repository snapshot discovery over caller-injected file capabilities.

use crate::{
    definitions::{DefinitionEntryKind, DefinitionImportFileSystem},
    deployment_config::ConfigurationError,
    notebook::{DefinitionLimits, DeploymentError, LocalNotebook},
    transforms::DEFAULT_GUID,
};
use std::{collections::HashSet, fmt, io, path::Path};

/// Local preflight error; no remote side effects have occurred.
#[derive(Debug)]
pub enum RepositoryError {
    /// Original filesystem failure.
    FileSystem(io::Error),
    /// Invalid or explicitly unsupported repository shape.
    Configuration(ConfigurationError),
    /// Existing native Notebook codec failure.
    Notebook(DeploymentError),
}

impl fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileSystem(_) => f.write_str("Could not read the deployment repository"),
            Self::Configuration(error) => error.fmt(f),
            Self::Notebook(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RepositoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::FileSystem(error) => Some(error),
            Self::Configuration(error) => Some(error),
            Self::Notebook(error) => Some(error),
        }
    }
}

/// A complete bounded snapshot, not an iterator that can fail after mutation.
pub struct NotebookRepository {
    /// Loaded root-level Notebooks in the injected directory listing order.
    pub notebooks: Vec<LocalNotebook>,
}

impl NotebookRepository {
    /// Discover and load every item before any service work.
    ///
    /// The caller supplies an already-confined stable root and bounded entries.
    /// Out-of-scope item processors/folder placement fail explicitly rather than
    /// being silently filtered out of a successful partial deployment.
    ///
    /// # Errors
    /// Rejects unknown processors, nested workspace folders, duplicate names or
    /// logical IDs and aggregate entry/content bounds before remote mutations.
    pub fn discover(
        files: &mut dyn DefinitionImportFileSystem,
        root: &Path,
        limits: DefinitionLimits,
        max_entries: usize,
    ) -> Result<Self, RepositoryError> {
        let entries = files
            .entries(root, "")
            .map_err(RepositoryError::FileSystem)?;
        let mut count = entries.len();
        let mut remaining = limits.max_bytes.get();
        let mut notebooks = Vec::new();
        let mut names = HashSet::new();
        let mut logical_ids = HashSet::new();
        if count > max_entries {
            return Err(RepositoryError::Configuration(ConfigurationError::Limit));
        }
        if entries.iter().any(|entry| entry.name == ".platform") {
            return Err(RepositoryError::Configuration(
                ConfigurationError::Unsupported("repository root as an item directory".into()),
            ));
        }
        for entry in entries {
            if entry.kind != DefinitionEntryKind::Directory {
                continue;
            }
            let children = files
                .entries(root, &entry.name)
                .map_err(RepositoryError::FileSystem)?;
            count = count
                .checked_add(children.len())
                .filter(|count| *count <= max_entries)
                .ok_or(RepositoryError::Configuration(ConfigurationError::Limit))?;
            if !children.iter().any(|entry| entry.name == ".platform") {
                return Err(RepositoryError::Configuration(
                    ConfigurationError::Unsupported(
                        "workspace folders or non-item repository directories".into(),
                    ),
                ));
            }
            let mut contents = Vec::new();
            for child in children {
                if child.kind != DefinitionEntryKind::File {
                    return Err(RepositoryError::Configuration(
                        ConfigurationError::Unsupported(
                            "nested Notebook resources or item-specific child processing".into(),
                        ),
                    ));
                }
                let path = format!("{}/{}", entry.name, child.name);
                let bytes = files
                    .read(root, &path, remaining)
                    .map_err(RepositoryError::FileSystem)?;
                remaining = remaining
                    .checked_sub(bytes.len())
                    .ok_or(RepositoryError::Configuration(ConfigurationError::Limit))?;
                contents.push((child.name, bytes));
            }
            let notebook =
                LocalNotebook::from_files(contents, limits).map_err(RepositoryError::Notebook)?;
            if !names.insert(notebook.name().to_owned())
                || (notebook.logical_id() != DEFAULT_GUID
                    && !logical_ids.insert(notebook.logical_id().to_owned()))
            {
                return Err(RepositoryError::Configuration(ConfigurationError::Invalid(
                    vec![
                        "Duplicate Notebook display name or logicalId in deployment repository"
                            .into(),
                    ],
                )));
            }
            notebooks.push(notebook);
        }
        Ok(Self { notebooks })
    }
}
