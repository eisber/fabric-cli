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

/// Full source repository snapshot, including nested and parent-managed items.
#[derive(Clone, Debug, Default)]
pub struct DeploymentRepository {
    /// Source directory traversal order, not a type-name sort.
    pub items: Vec<crate::model::RepositoryItem>,
    /// Nonempty workspace folders, in first-discovered order.
    pub folders: Vec<String>,
}

impl DeploymentRepository {
    /// Adapt the original injected Notebook-only capability without new I/O.
    pub fn from_notebooks(repository: NotebookRepository) -> Self {
        Self {
            items: repository
                .notebooks
                .into_iter()
                .map(LocalNotebook::into_repository_item)
                .collect(),
            folders: Vec::new(),
        }
    }

    /// Collect all actual source items before any remote write.
    ///
    /// The capability rejects symlinks/reparse points and unsafe paths. The
    /// traversal never follows ambient roots or silently drops read failures.
    ///
    /// # Errors
    /// Invalid metadata, duplicate identities and finite resource bounds fail
    /// before a partial snapshot can be used as evidence for orphan deletion.
    pub fn discover(
        files: &mut dyn DefinitionImportFileSystem,
        root: &Path,
        limits: DefinitionLimits,
        max_entries: usize,
    ) -> Result<Self, RepositoryError> {
        use crate::model::{ItemFile, RepositoryItem, invalid};
        let mut pending = vec![String::new()];
        let mut directories = Vec::new();
        let mut count = 0usize;
        let mut remaining = limits.max_bytes.get();
        while let Some(relative) = pending.pop() {
            let entries = files
                .entries(root, &relative)
                .map_err(RepositoryError::FileSystem)?;
            count = count
                .checked_add(entries.len())
                .filter(|count| *count <= max_entries)
                .ok_or(RepositoryError::Configuration(ConfigurationError::Limit))?;
            for entry in &entries {
                if entry.name.is_empty()
                    || matches!(entry.name.as_str(), "." | "..")
                    || entry.name.contains(['/', '\\', ':', '\0'])
                {
                    return Err(RepositoryError::Configuration(invalid(
                        "Unsafe repository entry",
                    )));
                }
            }
            for entry in entries
                .iter()
                .rev()
                .filter(|entry| entry.kind == DefinitionEntryKind::Directory)
            {
                pending.push(if relative.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{relative}/{}", entry.name)
                });
            }
            directories.push((relative, entries));
        }
        let item_paths: HashSet<_> = directories
            .iter()
            .filter(|(_, files)| {
                files
                    .iter()
                    .any(|file| file.kind == DefinitionEntryKind::File && file.name == ".platform")
            })
            .map(|(path, _)| path.clone())
            .collect();
        let mut items = Vec::new();
        let mut names = HashSet::new();
        let mut folders = Vec::new();
        for path in directories
            .iter()
            .map(|(path, _)| path)
            .filter(|path| item_paths.contains(*path))
        {
            let mut parent = path.as_str();
            while let Some((ancestor, _)) = parent.rsplit_once('/') {
                if !item_paths.contains(ancestor)
                    && !ancestor.ends_with("/.children")
                    && ancestor != ".children"
                {
                    let folder = format!("/{ancestor}");
                    if !folders.contains(&folder) {
                        folders.push(folder);
                    }
                }
                parent = ancestor;
            }
        }
        folders.sort_by_key(|folder| folder.bytes().filter(|ch| *ch == b'/').count());
        let mut loaded = Vec::new();
        for (directory, entries) in directories {
            if !item_paths.iter().any(|item| {
                directory == *item
                    || item.is_empty()
                    || directory
                        .strip_prefix(item)
                        .is_some_and(|tail| tail.starts_with('/'))
            }) {
                continue;
            }
            let mut contents = Vec::new();
            for entry in entries
                .into_iter()
                .filter(|entry| entry.kind == DefinitionEntryKind::File)
            {
                let path = if directory.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{directory}/{}", entry.name)
                };
                let bytes = files
                    .read(root, &path, remaining)
                    .map_err(RepositoryError::FileSystem)?;
                remaining = remaining
                    .checked_sub(bytes.len())
                    .ok_or(RepositoryError::Configuration(ConfigurationError::Limit))?;
                contents.push(ItemFile::new(entry.name, bytes));
            }
            loaded.push((directory, contents));
        }
        let mut snapshot_bytes = 0usize;
        for path in loaded
            .iter()
            .map(|(path, _)| path)
            .filter(|path| item_paths.contains(*path))
        {
            let mut contents = Vec::new();
            for (directory, files) in &loaded {
                if path.is_empty()
                    || directory == path
                    || directory
                        .strip_prefix(path)
                        .is_some_and(|tail| tail.starts_with('/'))
                {
                    for file in files {
                        snapshot_bytes = snapshot_bytes
                            .checked_add(file.bytes.len())
                            .filter(|bytes| *bytes <= limits.max_bytes.get())
                            .ok_or(RepositoryError::Configuration(ConfigurationError::Limit))?;
                        let prefix = directory
                            .strip_prefix(path)
                            .unwrap_or("")
                            .trim_start_matches('/');
                        let mut file = file.clone();
                        if !prefix.is_empty() {
                            file.path = format!("{prefix}/{}", file.path);
                        }
                        contents.push(file);
                    }
                }
            }
            if contents.len() > max_entries {
                return Err(RepositoryError::Configuration(ConfigurationError::Limit));
            }
            let item = RepositoryItem::from_files(path.clone(), contents)
                .map_err(RepositoryError::Configuration)?;
            if !names.insert((item.item_type.clone(), item.name.clone())) {
                return Err(RepositoryError::Configuration(invalid(
                    "Duplicate display name/type in repository",
                )));
            }
            items.push(item);
        }
        Ok(Self { items, folders })
    }
}

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
