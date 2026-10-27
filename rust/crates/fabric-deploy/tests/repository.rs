// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic caller-owned file snapshots; no ambient paths or service access.

use fabric_deploy::{
    definitions::{DefinitionEntry, DefinitionEntryKind, DefinitionImportFileSystem},
    deployment_config::ConfigurationError,
    notebook::{DefinitionLimits, DeploymentError, LocalNotebook},
    repository::{NotebookRepository, RepositoryError},
    transforms::DEFAULT_GUID,
};
use serde_json::json;
use std::{collections::BTreeMap, io, num::NonZeroUsize, path::Path};

#[derive(Default)]
struct Files {
    entries: BTreeMap<String, Vec<DefinitionEntry>>,
    contents: BTreeMap<String, Vec<u8>>,
}

impl Files {
    fn notebook(&mut self, directory: &str, name: &str, logical: &str) {
        self.entries
            .entry(String::new())
            .or_default()
            .push(DefinitionEntry {
                name: directory.into(),
                kind: DefinitionEntryKind::Directory,
            });
        self.entries.insert(
            directory.into(),
            vec![
                DefinitionEntry {
                    name: "notebook-content.py".into(),
                    kind: DefinitionEntryKind::File,
                },
                DefinitionEntry {
                    name: ".platform".into(),
                    kind: DefinitionEntryKind::File,
                },
            ],
        );
        self.contents.insert(
            format!("{directory}/.platform"),
            json!({
                "metadata": {"displayName": name, "type": "Notebook"},
                "config": {"logicalId": logical},
            })
            .to_string()
            .into_bytes(),
        );
        self.contents.insert(
            format!("{directory}/notebook-content.py"),
            b"# Fabric notebook source\r\nprint('synthetic')\r\n".to_vec(),
        );
    }

    fn discover(&mut self) -> Result<NotebookRepository, RepositoryError> {
        NotebookRepository::discover(
            self,
            Path::new("caller-owned-root"),
            DefinitionLimits::default(),
            100,
        )
    }
}

impl DefinitionImportFileSystem for Files {
    fn entries(&mut self, _: &Path, relative: &str) -> io::Result<Vec<DefinitionEntry>> {
        self.entries
            .get(relative)
            .cloned()
            .ok_or_else(|| io::Error::other("injected missing directory"))
    }
    fn read(&mut self, _: &Path, relative: &str, max_bytes: usize) -> io::Result<Vec<u8>> {
        let bytes = self
            .contents
            .get(relative)
            .ok_or_else(|| io::Error::other("injected read failure"))?;
        if bytes.len() > max_bytes {
            return Err(io::Error::other("injected byte bound"));
        }
        Ok(bytes.clone())
    }
}

#[test]
fn complete_snapshot_retains_listing_order_and_platform_names() {
    let mut files = Files::default();
    files.notebook("z.Notebook", "First", "logical-z");
    files.notebook("a.Notebook", "Second", "logical-a");
    let snapshot = files.discover().unwrap();
    assert_eq!(
        snapshot
            .notebooks
            .iter()
            .map(LocalNotebook::name)
            .collect::<Vec<_>>(),
        ["First", "Second"]
    );
    assert_eq!(snapshot.notebooks[0].logical_id(), "logical-z");
}

#[test]
fn duplicate_real_logical_ids_and_names_fail_before_service_work() {
    for duplicate_name in [true, false] {
        let mut files = Files::default();
        files.notebook("one.Notebook", "First", "logical-first");
        files.notebook(
            "two.Notebook",
            if duplicate_name { "First" } else { "Second" },
            if duplicate_name {
                "logical-second"
            } else {
                "logical-first"
            },
        );
        assert!(matches!(
            files.discover(),
            Err(RepositoryError::Configuration(ConfigurationError::Invalid(
                _
            )))
        ));
    }
}

#[test]
fn default_logical_id_is_not_a_duplicate() {
    let mut files = Files::default();
    files.notebook("one.Notebook", "First", DEFAULT_GUID);
    files.notebook("two.Notebook", "Second", DEFAULT_GUID);
    assert_eq!(files.discover().unwrap().notebooks.len(), 2);
}

#[test]
fn a_later_invalid_item_prevents_a_partial_successful_snapshot() {
    let mut files = Files::default();
    files.notebook("one.Notebook", "First", "logical-first");
    files.notebook("two.Notebook", "Second", "logical-second");
    files.contents.remove("two.Notebook/notebook-content.py");
    assert!(matches!(
        files.discover(),
        Err(RepositoryError::FileSystem(_))
    ));
}

#[test]
fn folder_placement_and_nested_resources_are_explicit_gaps() {
    let mut files = Files::default();
    files.notebook("one.Notebook", "First", "logical-first");
    files
        .entries
        .get_mut("one.Notebook")
        .unwrap()
        .push(DefinitionEntry {
            name: "resources".into(),
            kind: DefinitionEntryKind::Directory,
        });
    assert!(matches!(
        files.discover(),
        Err(RepositoryError::Configuration(
            ConfigurationError::Unsupported(_)
        ))
    ));
}

#[test]
fn traversal_and_aggregate_content_have_finite_bounds() {
    let mut files = Files::default();
    files.notebook("one.Notebook", "First", "logical-first");
    assert!(matches!(
        NotebookRepository::discover(
            &mut files,
            Path::new("owned"),
            DefinitionLimits::default(),
            1
        ),
        Err(RepositoryError::Configuration(ConfigurationError::Limit))
    ));
    let limits = DefinitionLimits {
        max_bytes: NonZeroUsize::new(1).unwrap(),
    };
    assert!(matches!(
        NotebookRepository::discover(&mut files, Path::new("owned"), limits, 100),
        Err(RepositoryError::FileSystem(_))
    ));
}

#[test]
fn injected_notebook_files_cannot_smuggle_duplicate_or_unsupported_paths() {
    for files in [
        vec![
            (".platform".into(), b"{}".to_vec()),
            (".platform".into(), b"{}".to_vec()),
        ],
        vec![("../notebook-content.py".into(), b"print('unsafe')".to_vec())],
    ] {
        assert!(matches!(
            LocalNotebook::from_files(files, DefinitionLimits::default()),
            Err(DeploymentError::UnsupportedContent)
        ));
    }
}
